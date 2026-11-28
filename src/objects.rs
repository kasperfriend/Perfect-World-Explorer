//! Streaming access to placed lit-model geometry and textures.
//!
//! Texture references are resolved exactly the way the retail client does
//! (see [`crate::texture_resolution`] for the native call chain and
//! addresses): archive-first across the mounted packages in mount order,
//! then a plain `fopen(name, "rb")` disk fallback, with `.sdr.` shader
//! registry names rejected by name and every unresolved reference recorded
//! as a register row.
//!
//! Decoded textures live in a content-hash deduplicated store with a byte
//! budget and LRU eviction (P3.08): the full-res source textures of the
//! whole world are never all resident at once, even though the whole-world
//! preload walks every placement. The budget defaults to 32 MiB and can be
//! overridden with `PW_TEX_BUDGET_MIB` (tests and measurements use both).
//!
//! The Graphics tab's **All to VRAM** switch calls [`set_unbounded_textures`],
//! which lifts the budget: every decoded texture stays cached, so revisiting an
//! area (or rebuilding the whole-world preload) never decodes the same source
//! twice. That is the residency the optimisation passes removed, restored as an
//! explicit option because it costs the whole decoded corpus in RAM.
use crate::{
    bmd, pck,
    scene::Ornament,
    texture_resolution::{
        litmodel_basename_index_from_map, litmodel_entry_key_indexed, litmodel_key,
        litmodel_partition_flags, normalize, MountedArchive, Resolution, TextureResolver,
    },
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};
pub struct Texture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}
impl Texture {
    pub fn sample(&self, u: f32, v: f32) -> [f32; 3] {
        let x = (u.rem_euclid(1.) * self.width as f32) as usize % self.width;
        let y = (v.rem_euclid(1.) * self.height as f32) as usize % self.height;
        let p = (y * self.width + x) * 4;
        [
            self.rgba[p] as f32,
            self.rgba[p + 1] as f32,
            self.rgba[p + 2] as f32,
        ]
    }
    pub fn sample_rgba(&self, u: f32, v: f32) -> [u8; 4] {
        let x = (u.rem_euclid(1.) * self.width as f32) as usize % self.width;
        let y = (v.rem_euclid(1.) * self.height as f32) as usize % self.height;
        let p = (y * self.width + x) * 4;
        self.rgba[p..p + 4].try_into().unwrap()
    }
}
fn content_hash(tex: &Texture) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325 ^ ((tex.width as u64) << 32) ^ (tex.height as u64);
    for chunk in tex.rgba.chunks(8) {
        let mut v = [0u8; 8];
        v[..chunk.len()].copy_from_slice(chunk);
        h = (h ^ u64::from_le_bytes(v)).wrapping_mul(0x100000001b3);
    }
    h
}
/// Measured residency counters (P3.08 acceptance: measured numbers, not
/// estimates).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResidencyStats {
    /// Textures actually decoded from archive/disk bytes.
    pub decodes: usize,
    /// References whose content was already resident under another name;
    /// the decode and the second resident copy were both avoided.
    pub alias_hits: usize,
    /// Bytes of resident copies avoided through content deduplication.
    pub alias_bytes_saved: u64,
    /// LRU evictions performed to stay inside the byte budget.
    pub evictions: usize,
    /// Bytes freed by eviction.
    pub evicted_bytes: u64,
    /// Peak resident decoded-texture bytes since start.
    pub peak_resident_bytes: u64,
    /// Resident decoded-texture bytes right now.
    pub resident_bytes: u64,
    /// Batches where the pinned (in-use) textures alone exceeded the budget,
    /// so eviction could not reach it.
    pub pinned_over_budget: usize,
    /// Configured byte budget.
    pub budget_bytes: u64,
}
struct CacheEntry {
    tex: Texture,
    last_used: u64,
    bytes: usize,
}
/// Content-hash deduplicated, byte-budgeted LRU store of decoded textures.
/// Entries used by the batch in progress are pinned (their `last_used`
/// equals the current batch clock) and are never evicted mid-batch.
pub struct TextureCache {
    budget: usize,
    clock: u64,
    over_budget_clock: u64,
    by_name: HashMap<String, u64>,
    by_hash: HashMap<u64, CacheEntry>,
    /// Reverse index: the reference names pointing at one content hash.
    ///
    /// Eviction used to find them with a `retain` over every name in the cache,
    /// which is the whole world's name table (tens of thousands of entries) for
    /// every single evicted texture, on every store past the budget. Keeping the
    /// names next to the hash makes an eviction cost one lookup plus its own
    /// aliases, and the resolved set of names is identical.
    aliases: HashMap<u64, Vec<String>>,
    pub stats: ResidencyStats,
}
impl TextureCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget: budget_bytes,
            clock: 0,
            over_budget_clock: u64::MAX,
            by_name: HashMap::new(),
            by_hash: HashMap::new(),
            aliases: HashMap::new(),
            stats: ResidencyStats {
                budget_bytes: budget_bytes as u64,
                ..Default::default()
            },
        }
    }
    /// Starts a new pin generation; call once per `load_ids` batch.
    pub fn begin_batch(&mut self) {
        self.clock += 1;
    }
    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }
    /// Content key used for cross-name deduplication downstream (the
    /// whole-world builder keys its atlas slots by this instead of the
    /// reference name, so alias names share one packed slot).
    pub fn key(&self, name: &str) -> Option<u64> {
        self.by_name.get(name).copied()
    }
    pub fn get(&self, name: &str) -> Option<&Texture> {
        let hash = self.by_name.get(name)?;
        Some(&self.by_hash.get(hash)?.tex)
    }
    /// Marks a reference as used by the current batch (pins its content).
    pub fn touch(&mut self, name: &str) {
        if let Some(&hash) = self.by_name.get(name) {
            if let Some(entry) = self.by_hash.get_mut(&hash) {
                entry.last_used = self.clock;
            }
        }
    }
    /// Inserts a decoded texture under a reference name. If identical
    /// content is already resident under another name, only the alias is
    /// recorded (deduplication). Evicts unpinned LRU entries afterwards
    /// until the byte budget holds again.
    pub fn store(&mut self, name: String, tex: Texture) {
        let bytes = tex.rgba.len() + tex.width * tex.height / 1024;
        let hash = content_hash(&tex);
        if self.by_hash.contains_key(&hash) {
            self.aliases.entry(hash).or_default().push(name.clone());
            self.by_name.insert(name, hash);
            self.stats.alias_hits += 1;
            self.stats.alias_bytes_saved += bytes as u64;
            self.touch_internal(hash);
            return;
        }
        self.stats.decodes += 1;
        self.by_hash.insert(
            hash,
            CacheEntry {
                tex,
                last_used: self.clock,
                bytes,
            },
        );
        self.aliases.entry(hash).or_default().push(name.clone());
        self.by_name.insert(name, hash);
        self.stats.resident_bytes += bytes as u64;
        self.stats.peak_resident_bytes = self
            .stats
            .peak_resident_bytes
            .max(self.stats.resident_bytes);
        self.evict_unpinned();
    }
    fn touch_internal(&mut self, hash: u64) {
        if let Some(entry) = self.by_hash.get_mut(&hash) {
            entry.last_used = self.clock;
        }
    }
    fn evict_unpinned(&mut self) {
        while self.stats.resident_bytes > self.budget as u64 {
            let victim = self
                .by_hash
                .iter()
                .filter(|(_, e)| e.last_used < self.clock)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(&h, _)| h);
            let Some(hash) = victim else {
                // Counted once per batch, not once per over-budget store.
                if self.over_budget_clock != self.clock {
                    self.stats.pinned_over_budget += 1;
                    self.over_budget_clock = self.clock;
                }
                return;
            };
            let entry = self.by_hash.remove(&hash).unwrap();
            match self.aliases.remove(&hash) {
                Some(names) => {
                    for name in names {
                        self.by_name.remove(&name);
                    }
                }
                // Defensive: an entry with no recorded aliases can only mean the
                // two tables disagree, in which case the slow but exact answer
                // still has to be right.
                None => self.by_name.retain(|_, &mut h| h != hash),
            }
            self.stats.evictions += 1;
            self.stats.evicted_bytes += entry.bytes as u64;
            self.stats.resident_bytes -= entry.bytes as u64;
        }
    }
    /// Drops all decoded CPU textures and shrinks internal maps. Used once
    /// whole-world preload owns scenery so local-streaming textures are not
    /// kept resident in RAM alongside uploaded GPU pages.
    pub fn clear(&mut self) {
        self.by_name.clear();
        self.by_name.shrink_to_fit();
        self.by_hash.clear();
        self.by_hash.shrink_to_fit();
        self.aliases.clear();
        self.aliases.shrink_to_fit();
        self.stats.resident_bytes = 0;
    }
}
/// One register row for a texture reference the client could not have
/// loaded either, or that this renderer refuses by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnresolvedTexture {
    pub reference: String,
    pub kind: String,
    pub detail: String,
}
pub struct Objects {
    pub ornaments: Arc<[Ornament]>,
    /// Canonical in-memory keys built once instead of formatting and lowercasing
    /// the same placement path on every streamed batch.
    model_names: Vec<String>,
    models: HashMap<String, bmd::Model>,
    cache: TextureCache,
    resolver: TextureResolver,
    /// Scenery archives are optional: an instance map can still show terrain,
    /// water, trees and the models it does have when one package is absent.
    litfile: Option<SceneryArchive>,
    texfile: Option<SceneryArchive>,
    lit_archive_error: Option<String>,
    tex_archive_error: Option<String>,
    lit_archive_entries: usize,
    litmodel_map_id: String,
    litentries: Arc<[pck::Entry]>,
    litindex: Arc<HashMap<String, usize>>,
    lit_has_map_prefix: bool,
    lit_has_other_map_partition: bool,
    litbasenames: HashMap<String, Vec<String>>,
    texentries: Arc<[pck::Entry]>,
    texindex: Arc<HashMap<String, usize>>,
    /// Missing or unparsable BMDs are skipped once and negative-cached, so a
    /// single bad placement cannot abort the whole world or retry every frame.
    unavailable_models: HashSet<String>,
    unavailable_examples: Vec<String>,
    unresolved: Vec<UnresolvedTexture>,
    unresolved_seen: HashSet<String>,
}
/// RAM-first decoded-texture residency budget. Re-entry may re-decode evicted
/// sources; source resolution and all visible textures remain unchanged.
/// The whole corpus of
/// building.pck textures is ~445 MiB compressed on disk and far larger
/// decoded, so the full world never fits under this budget by design.
pub const DEFAULT_TEX_BUDGET_MIB: usize = 16;
/// Optional host-provided logger for scenery BMD failures. `objects.rs` is also
/// compiled directly into standalone survey binaries, so it cannot depend on
/// the viewer's diagnostics module.
static MODEL_LOAD_LOGGER: std::sync::OnceLock<fn(&str, &str)> = std::sync::OnceLock::new();
pub fn set_model_load_logger(logger: fn(&str, &str)) {
    let _ = MODEL_LOAD_LOGGER.set(logger);
}
/// Process-wide "All to VRAM" switch. `Objects` instances read it when their
/// cache evicts and when a new one is opened, so the whole-world preload and
/// later streamed batches follow a live toggle without a restart.
static UNBOUNDED_TEXTURES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Keep every decoded texture resident instead of evicting to the budget.
pub fn set_unbounded_textures(on: bool) {
    UNBOUNDED_TEXTURES.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// Whether decoded textures are kept permanently resident.
pub fn unbounded_textures() -> bool {
    UNBOUNDED_TEXTURES.load(std::sync::atomic::Ordering::Relaxed)
}
fn open_optional_archive(
    path: &Path,
) -> (
    Option<File>,
    Arc<[pck::Entry]>,
    Arc<HashMap<String, usize>>,
    Option<String>,
) {
    let empty_entries = || Arc::<[pck::Entry]>::from(Vec::new());
    let empty_index = || Arc::new(HashMap::new());
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            let reason = format!("{}: {error}", path.display());
            eprintln!("Optional scenery archive unavailable: {reason}");
            return (None, empty_entries(), empty_index(), Some(reason));
        }
    };
    match pck::normalized_index_arc(&mut file) {
        Ok((entries, indexed)) => {
            let bmd_count = indexed.keys().filter(|name| name.ends_with(".bmd")).count();
            eprintln!(
                "Indexed scenery archive {}: {} members, {} BMDs",
                path.display(),
                indexed.len(),
                bmd_count
            );
            (Some(file), entries, indexed, None)
        }
        Err(error) => {
            let reason = format!("{}: {error}", path.display());
            eprintln!("Optional scenery archive could not be read: {reason}");
            (None, empty_entries(), empty_index(), Some(reason))
        }
    }
}

fn tex_budget_bytes() -> usize {
    if unbounded_textures() {
        // Not literally infinite: the LRU comparison adds bytes to this, so it
        // stays a value the arithmetic cannot overflow.
        return usize::MAX / 4;
    }
    std::env::var("PW_TEX_BUDGET_MIB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_TEX_BUDGET_MIB)
        << 20
}
enum PendingTexture {
    Archive {
        mount: usize,
        key: String,
    },
    Disk {
        key: String,
        path: std::path::PathBuf,
    },
}

fn decode_texture_bytes(key: &str, data: &[u8]) -> Result<Texture, String> {
    let decoded = if key.ends_with(".tga") {
        image::load_from_memory_with_format(data, image::ImageFormat::Tga)
    } else {
        image::load_from_memory(data)
    };
    match decoded {
        Ok(img) => {
            let img = img.into_rgba8();
            let (w, h) = img.dimensions();
            Ok(Texture {
                width: w as usize,
                height: h as usize,
                rgba: img.into_raw(),
            })
        }
        Err(err) => Err(err.to_string()),
    }
}

impl Objects {
    pub fn open<O: Into<Arc<[Ornament]>>>(element: &Path, ornaments: O) -> Result<Self, String> {
        let ornaments = ornaments.into();
        let litmodel_map_id = ornaments
            .first()
            .map(|ornament| ornament.map_id.clone())
            .unwrap_or_else(|| "world".to_string());
        if ornaments
            .iter()
            .any(|ornament| !ornament.map_id.eq_ignore_ascii_case(&litmodel_map_id))
        {
            return Err("scenery placements from multiple map namespaces were mixed".into());
        }
        let (
            (litfile, litentries, litindex, lit_archive_error),
            (texfile, texentries, texindex, _tex_archive_error),
        ) = std::thread::scope(|s| {
            let lit_path = element.join("litmodels.pck");
            let tex_path = element.join("building.pck");
            let lit_task = s.spawn(move || open_optional_archive(&lit_path));
            let tex_task = s.spawn(move || open_optional_archive(&tex_path));
            (lit_task.join().unwrap(), tex_task.join().unwrap())
        });
        let lit_archive_entries = litindex.len();
        let (lit_has_map_prefix, lit_has_other_map_partition) =
            litmodel_partition_flags(&litindex, &litmodel_map_id);
        let litbasenames = litmodel_basename_index_from_map(&litindex);
        // Mount order matches the open order above. The retail mount order
        // of the two packages is not recoverable from the executable alone,
        // but it is also unobservable in this corpus: the census found 0
        // texture names present in both archives and litmodels.pck contains
        // no texture members at all (28858/28858 are .bmd).
        let model_names = ornaments
            .iter()
            .map(|ornament| litmodel_key(&ornament.path))
            .collect();
        let resolver = TextureResolver::new(
            vec![
                MountedArchive::from_shared_index("litmodels".into(), Arc::clone(&litindex)),
                MountedArchive::from_shared_index("building".into(), Arc::clone(&texindex)),
            ],
            element.to_path_buf(),
        );
        Ok(Self {
            ornaments,
            model_names,
            models: HashMap::new(),
            cache: TextureCache::new(tex_budget_bytes()),
            resolver,
            litfile,
            texfile,
            lit_archive_error,
            tex_archive_error,
            lit_archive_entries,
            litmodel_map_id,
            litentries,
            litindex,
            lit_has_map_prefix,
            lit_has_other_map_partition,
            litbasenames,
            texentries,
            texindex,
            unavailable_models: HashSet::new(),
            unavailable_examples: Vec::new(),
            unresolved: Vec::new(),
            unresolved_seen: HashSet::new(),
        })
    }
    pub fn update(&mut self, world_x: f32, world_z: f32, range: f32) -> Result<Vec<usize>, String> {
        let ids: Vec<usize> = self
            .ornaments
            .iter()
            .enumerate()
            .filter(|(_, o)| {
                let dx = o.position[0] - world_x;
                let dz = o.position[2] - world_z;
                dx * dx + dz * dz < range * range
            })
            .map(|(i, _)| i)
            .collect();
        self.load_ids(&ids)?;
        Ok(ids)
    }
    /// Loads a small explicit batch for the independent world preload worker.
    pub fn load_ids(&mut self, ids: &[usize]) -> Result<(), String> {
        let mut active = HashSet::with_capacity(ids.len());
        for &i in ids {
            let Some(_) = self.ornaments.get(i) else {
                return Err(format!("invalid ornament index {i}"));
            };
            active.insert(self.model_names[i].clone());
        }
        self.models.retain(|name, _| active.contains(name));
        self.cache.begin_batch();

        // Resolve each absent BMD independently. A missing archive member, a
        // failed archive read or a malformed BMD skips only that prop; the
        // remaining scenery continues to load.
        let mut missing_models: Vec<(usize, String, pck::Entry)> = Vec::new();
        let mut seen_missing = HashSet::new();
        for &i in ids {
            let name = self.model_names[i].clone();
            if self.unavailable_models.contains(&name) {
                continue;
            }
            if self.models.contains_key(&name) || !seen_missing.insert(name.clone()) {
                continue;
            }
            if let Some(error) = self.lit_archive_error.clone() {
                self.skip_model(&name, &format!("litmodels.pck was not indexed: {error}"));
                continue;
            }
            let resolved = litmodel_entry_key_indexed(
                &name,
                &self.litmodel_map_id,
                &self.litindex,
                &self.litbasenames,
                self.lit_has_map_prefix,
                self.lit_has_other_map_partition,
            );
            let entry_key = match resolved {
                Ok(key) => key,
                Err(error) => {
                    self.skip_model(
                        &name,
                        &format!(
                            "{error}; litmodels.pck index has {} members",
                            self.lit_archive_entries
                        ),
                    );
                    continue;
                }
            };
            let Some(entry) = self
                .litindex
                .get(&entry_key)
                .map(|&idx| self.litentries[idx].clone())
            else {
                self.skip_model(&name, &format!("resolver/index mismatch for {entry_key}"));
                continue;
            };
            missing_models.push((i, name, entry));
        }

        if self.litfile.is_none() {
            for (_, name, _) in &missing_models {
                self.skip_model(name, "litmodels.pck unavailable");
            }
            missing_models.clear();
        }
        if !missing_models.is_empty() {
            // Keep several archive ranges per worker so DirectStorage can
            // submit a real batch instead of paying one queue submit/wait
            // per model. Four workers still parse independent BMDs
            // concurrently.
            // Use all available CPU cores for concurrent BMD decompression and
            // parsing (DirectStorage still batches 16 entries per worker call).
            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(32)
                .min(missing_models.len());
            let chunk_size = (missing_models.len() + workers - 1) / workers;
            let parsed: Vec<Vec<(usize, String, Result<bmd::Model, String>)>> = {
                let litfile = self
                    .litfile
                    .as_ref()
                    .ok_or_else(|| "litmodels.pck unavailable".to_string())?;
                // Every piece of the package, so a model stored past the first
                // continuation still reads. A batch is shared by reference and
                // the reads are positioned, so the threads do not interfere.
                let parts = litfile.package().part_list();
                let layout = litfile.package().layout().clone();
                std::thread::scope(|s| {
                    let handles: Vec<_> = missing_models
                        .chunks(chunk_size)
                        .map(|chunk| {
                            // Borrowed for the whole scope: each batch is read
                            // by reference, so no worker takes ownership of the
                            // piece list.
                            let parts = &parts;
                            let layout = &layout;
                            s.spawn(move || {
                                let mut out = Vec::with_capacity(chunk.len());
                                for batch in chunk.chunks(16) {
                                    let entries: Vec<&pck::Entry> =
                                        batch.iter().map(|(_, _, entry)| entry).collect();
                                    match pck::read_entries_in(parts, layout, &entries) {
                                        Ok(data) => {
                                            for ((index, name, _), data) in batch.iter().zip(data) {
                                                let result = bmd::Model::parse(&data)
                                                    .map_err(|error| format!("{name}: {error}"));
                                                out.push((*index, name.clone(), result));
                                            }
                                        }
                                        Err(error) => {
                                            let error = error.to_string();
                                            for (index, name, _) in batch {
                                                out.push((
                                                    *index,
                                                    name.clone(),
                                                    Err(format!("archive read failed: {error}")),
                                                ));
                                            }
                                        }
                                    }
                                }
                                out
                            })
                        })
                        .collect();
                    handles.into_iter().map(|h| h.join().unwrap()).collect()
                })
            };
            for batch in parsed {
                for (_, name, result) in batch {
                    match result {
                        Ok(model) => {
                            self.models.insert(name, model);
                        }
                        Err(error) => self.skip_model(&name, &error),
                    }
                }
            }
        }

        let mut seen_models = HashSet::with_capacity(ids.len());
        let mut seen_refs: HashSet<&str> = HashSet::new();
        let mut unique_refs: Vec<String> = Vec::new();
        for &i in ids {
            let name = self.model_names[i].as_str();
            if self.unavailable_models.contains(name) || !seen_models.insert(name) {
                continue;
            }
            let Some(model) = self.models.get(name) else {
                continue;
            };
            for mesh in &model.meshes {
                if seen_refs.insert(mesh.texture.as_str()) {
                    unique_refs.push(mesh.texture.clone());
                }
            }
        }

        let mut pending = Vec::with_capacity(unique_refs.len());
        let mut queued_keys = HashSet::with_capacity(unique_refs.len());
        for raw in unique_refs {
            if let Some(job) = self.resolve_texture_job(&raw) {
                let key = match &job {
                    PendingTexture::Archive { key, .. } | PendingTexture::Disk { key, .. } => {
                        key.clone()
                    }
                };
                if queued_keys.insert(key) {
                    pending.push(job);
                }
            }
            // Derived normal-map reference (A3DLitMesh::LoadTexture step
            // 4). The client tolerates every failure here (a missing file
            // inside A3DTexture::Load returns TRUE), and this renderer
            // has no normal-map consumer, so the derivation is resolved
            // and counted but never decoded.
            if let Some((nrf, used_default)) = self.resolver.resolve_nrf(&raw) {
                if matches!(nrf, Resolution::Missing { .. }) {
                    self.register(
                        &raw,
                        if used_default {
                            "nrf-default-missing"
                        } else {
                            "nrf-derived-missing"
                        },
                        "derived normal-map reference resolves nowhere; client tolerates (A3DTexture::Load returns TRUE on open failure, 0x78a440)",
                    );
                }
            }
        }

        if !pending.is_empty() {
            enum ResolvedJob {
                Archive {
                    key: String,
                    mount: usize,
                    entry: pck::Entry,
                },
                Disk {
                    key: String,
                    path: std::path::PathBuf,
                },
            }

            let mut jobs = Vec::with_capacity(pending.len());
            for job in pending {
                match job {
                    PendingTexture::Archive { mount, key } => {
                        let entry = if mount == 0 {
                            self.litindex
                                .get(&key)
                                .map(|&idx| self.litentries[idx].clone())
                        } else {
                            self.texindex
                                .get(&key)
                                .map(|&idx| self.texentries[idx].clone())
                        };
                        let entry =
                            entry.ok_or_else(|| format!("resolver/index mismatch for {key}"))?;
                        jobs.push(ResolvedJob::Archive { key, mount, entry });
                    }
                    PendingTexture::Disk { key, path } => {
                        jobs.push(ResolvedJob::Disk { key, path });
                    }
                }
            }

            // Use all available CPU cores for concurrent texture decompression
            // and DXT/TGA/BMP/PNG/JPEG decoding.
            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(32)
                .min(jobs.len());
            let chunk_size = (jobs.len() + workers - 1) / workers;
            let decoded: Vec<Vec<(String, Result<Texture, String>)>> = {
                let litfile = self.litfile.as_ref();
                let texfile = self.texfile.as_ref();
                std::thread::scope(|s| {
                    let handles: Vec<_> = jobs
                        .chunks(chunk_size)
                        .map(|chunk| {
                            s.spawn(move || {
                                let mut out = Vec::with_capacity(chunk.len());
                                for jobs_batch in chunk.chunks(16) {
                                    let mut lit_jobs: Vec<(String, pck::Entry)> = Vec::new();
                                    let mut tex_jobs: Vec<(String, pck::Entry)> = Vec::new();
                                    for job in jobs_batch {
                                        if let ResolvedJob::Archive { key, mount, entry } = job {
                                            let file = if *mount == 0 { litfile } else { texfile };
                                            if file.is_none() {
                                                let archive = if *mount == 0 {
                                                    "litmodels.pck"
                                                } else {
                                                    "building.pck"
                                                };
                                                out.push((
                                                    key.clone(),
                                                    Err(format!(
                                                        "scenery archive {archive} unavailable"
                                                    )),
                                                ));
                                            } else if *mount == 0 {
                                                lit_jobs.push((key.clone(), entry.clone()));
                                            } else {
                                                tex_jobs.push((key.clone(), entry.clone()));
                                            }
                                        }
                                    }
                                    for (file, archive_jobs) in
                                        [(litfile, lit_jobs), (texfile, tex_jobs)]
                                    {
                                        let Some(file) = file else { continue };
                                        if archive_jobs.is_empty() {
                                            continue;
                                        }
                                        let read = {
                                            let entries: Vec<&pck::Entry> = archive_jobs
                                                .iter()
                                                .map(|(_, entry)| entry)
                                                .collect();
                                            pck::read_entries_in(
                                                &file.package().part_list(),
                                                file.package().layout(),
                                                &entries,
                                            )
                                        };
                                        match read {
                                            Ok(bytes) => {
                                                for ((key, _), data) in
                                                    archive_jobs.into_iter().zip(bytes)
                                                {
                                                    out.push((
                                                        key.clone(),
                                                        decode_texture_bytes(&key, &data),
                                                    ));
                                                }
                                            }
                                            Err(error) => {
                                                let error = error.to_string();
                                                for (key, _) in archive_jobs {
                                                    out.push((
                                                        key,
                                                        Err(format!(
                                                            "archive read failed: {error}"
                                                        )),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                    for job in jobs_batch {
                                        if let ResolvedJob::Disk { key, path } = job {
                                            let result = std::fs::read(path)
                                                .map_err(|error| {
                                                    format!(
                                                        "disk texture {}: {error}",
                                                        path.display()
                                                    )
                                                })
                                                .and_then(|data| decode_texture_bytes(key, &data));
                                            out.push((key.clone(), result));
                                        }
                                    }
                                }
                                out
                            })
                        })
                        .collect();
                    handles.into_iter().map(|h| h.join().unwrap()).collect()
                })
            };

            for batch in decoded {
                for (key, res) in batch {
                    match res {
                        Ok(texture) => {
                            self.cache.store(key, texture);
                        }
                        Err(err) => {
                            self.register(
                                &key,
                                "decode-failed",
                                &format!("bytes read but image decode failed: {err}"),
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }
    fn skip_model(&mut self, name: &str, reason: &str) {
        if self.unavailable_models.insert(name.to_string()) {
            let line = format!("world BMD unavailable: {name}: {reason}");
            if let Some(logger) = MODEL_LOAD_LOGGER.get() {
                logger(name, &line);
            }
            eprintln!("Skipping unavailable scenery model {name}: {reason}");
            if self.unavailable_examples.len() < 3 {
                self.unavailable_examples.push(format!("{name}: {reason}"));
            }
        }
    }
    fn load_texture_reference(&mut self, raw: &str) -> Result<(), String> {
        match self.resolve_texture_job(raw) {
            None => Ok(()),
            Some(PendingTexture::Archive { mount, key }) => self.load_from_archive(mount, &key),
            Some(PendingTexture::Disk { key, path }) => {
                let data = std::fs::read(&path)
                    .map_err(|e| format!("disk texture {}: {e}", path.display()))?;
                self.decode_and_store(&key, &data)
            }
        }
    }

    fn resolve_texture_job(&mut self, raw: &str) -> Option<PendingTexture> {
        let resolution = self.resolver.resolve(raw);
        let key = normalize(raw);
        match resolution {
            Resolution::Trivial => None,
            Resolution::ShaderRegistry { key: registry_key } => {
                // The client looks the truncated key up in the runtime
                // shader-texture registry (0x77c2e4 -> 0x785d30); a miss is
                // terminal with no file fallback (0x77c320). The registry is
                // populated by the effect system (outside this lane) from
                // the same-named archive member, so the approximation here
                // is: resolve the FULL name as a plain file and record a
                // register row either way.
                // `key` (outer) is the FULL normalized reference name —
                // the truncated registry key only exists at runtime.
                match self.resolver.resolve_file_path(&key) {
                    Resolution::Archive { mount, key: fkey } => {
                        self.resolver.stats.shader_registry_file_backed += 1;
                        self.register(
                            raw,
                            "shader-registry-file-backed",
                            &format!(
                                "registry key {registry_key:?} is runtime state (0x785d30); loaded the same-named archive member {fkey:?} as the registry object's source pixels"
                            ),
                        );
                        if self.cache.contains(&fkey) {
                            self.cache.touch(&fkey);
                            return None;
                        }
                        Some(PendingTexture::Archive { mount, key: fkey })
                    }
                    Resolution::Disk { path } => {
                        self.resolver.stats.shader_registry_file_backed += 1;
                        self.register(
                            raw,
                            "shader-registry-file-backed",
                            &format!(
                                "registry key {registry_key:?} is runtime state (0x785d30); loaded the same-named disk file as the registry object's source pixels"
                            ),
                        );
                        if self.cache.contains(&key) {
                            self.cache.touch(&key);
                            return None;
                        }
                        Some(PendingTexture::Disk { key, path })
                    }
                    _ => {
                        self.register(
                            raw,
                            "shader-registry",
                            &format!(
                                "name truncates to registry key {registry_key:?} (0x77c2e4 -> 0x785d30); the registry is runtime state and no same-named file exists in any staged archive or on disk"
                            ),
                        );
                        None
                    }
                }
            }
            Resolution::Missing { key, quiet } => {
                self.register(
                    raw,
                    if quiet { "missing-quiet" } else { "missing" },
                    &format!(
                        "{key:?} not in any mounted archive or on disk; client {}",
                        if quiet {
                            "fails silently (name contains Textures/Tex_, 0x76d88d/0x76d8a6)"
                        } else {
                            "logs \"AFileImage::Can not open file [%s] from disk!\" (0x986f28)"
                        }
                    ),
                );
                None
            }
            Resolution::Archive { mount, key } => {
                let archive = if mount == 0 {
                    "litmodels.pck"
                } else {
                    "building.pck"
                };
                let available = if mount == 0 {
                    self.litfile.is_some()
                } else {
                    self.texfile.is_some()
                };
                if !available {
                    self.register(
                        raw,
                        "texture-load-failed",
                        &format!("{archive} unavailable; retaining model geometry"),
                    );
                    return None;
                }
                if self.cache.contains(&key) {
                    self.cache.touch(&key);
                    return None;
                }
                Some(PendingTexture::Archive { mount, key })
            }
            Resolution::Disk { path } => {
                if self.cache.contains(&key) {
                    self.cache.touch(&key);
                    return None;
                }
                Some(PendingTexture::Disk { key, path })
            }
        }
    }
    fn load_from_archive(&mut self, mount: usize, key: &str) -> Result<(), String> {
        let data = if mount == 0 {
            let entry = self
                .litindex
                .get(key)
                .map(|&idx| self.litentries[idx].clone())
                .ok_or_else(|| format!("resolver/index mismatch for {key}"))?;
            let package = self
                .litfile
                .as_ref()
                .ok_or_else(|| "litmodels.pck unavailable".to_string())?;
            package.package().read(&entry)?
        } else {
            let entry = self
                .texindex
                .get(key)
                .map(|&idx| self.texentries[idx].clone())
                .ok_or_else(|| format!("resolver/index mismatch for {key}"))?;
            let package = self
                .texfile
                .as_ref()
                .ok_or_else(|| "building.pck unavailable".to_string())?;
            package.package().read(&entry)?
        };
        self.decode_and_store(key, &data)
    }
    fn decode_and_store(&mut self, key: &str, data: &[u8]) -> Result<(), String> {
        match decode_texture_bytes(key, data) {
            Ok(tex) => {
                self.cache.store(key.to_string(), tex);
                Ok(())
            }
            Err(err) => {
                self.register(
                    key,
                    "decode-failed",
                    &format!("bytes read but image decode failed: {err}"),
                );
                Ok(())
            }
        }
    }
    fn register(&mut self, reference: &str, kind: &str, detail: &str) {
        let dedup = format!("{kind}:{reference}");
        if !self.unresolved_seen.insert(dedup) {
            return;
        }
        eprintln!("Unresolved texture reference [{kind}] {reference}: {detail}");
        self.unresolved.push(UnresolvedTexture {
            reference: reference.to_string(),
            kind: kind.to_string(),
            detail: detail.to_string(),
        });
    }
    pub fn get(&self, idx: usize) -> Option<&bmd::Model> {
        self.models
            .get(&litmodel_key(&self.ornaments.get(idx)?.path))
    }
    /// How the ornament set resolved: models that can draw, names this client
    /// cannot resolve, and the first few reasons.
    ///
    /// A map with no visible buildings has three very different causes -- it
    /// authors none, its archive is not indexed, or every name misses -- and
    /// they read identically on screen. This is what tells them apart, without
    /// needing the ModelLoads channel switched on.
    pub fn model_census(&self) -> (usize, usize, Vec<String>) {
        let mut available = 0usize;
        let mut unavailable = 0usize;
        let mut seen = std::collections::HashSet::new();
        for ornament in self.ornaments.iter() {
            let name = litmodel_key(&ornament.path);
            if !seen.insert(name.clone()) {
                continue;
            }
            if self.models.contains_key(&name) {
                available += 1;
            } else {
                unavailable += 1;
            }
        }
        (available, unavailable, self.unavailable_examples.clone())
    }

    /// Distinct BMDs that could not be resolved or parsed for this map.
    pub fn unavailable_model_count(&self) -> usize {
        self.unavailable_models.len()
    }
    /// First failure causes, retained for the whole-world status summary.
    pub fn unavailable_examples(&self) -> &[String] {
        &self.unavailable_examples
    }
    /// Sorted paths of scenery models that failed archive lookup or parsing.
    pub fn unavailable_models(&self) -> Vec<String> {
        let mut names: Vec<_> = self.unavailable_models.iter().cloned().collect();
        names.sort();
        names
    }

    pub fn unindexed_archives(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(error) = &self.lit_archive_error {
            out.push(("litmodels.pck".into(), error.clone()));
        }
        if let Some(error) = &self.tex_archive_error {
            out.push(("building.pck".into(), error.clone()));
        }
        out
    }
    pub fn texture(&self, path: &str) -> Option<&Texture> {
        self.cache.get(&normalize(path))
    }
    /// Content key of a resolved texture, for cross-name deduplication in
    /// downstream packers. `None` when the reference did not resolve to
    /// resident pixel data.
    pub fn texture_key(&self, path: &str) -> Option<u64> {
        self.cache.key(&normalize(path))
    }
    /// Register rows for every unresolved / by-name-rejected reference.
    pub fn unresolved(&self) -> &[UnresolvedTexture] {
        &self.unresolved
    }
    /// Measured residency counters plus the resolver's own census.
    pub fn residency(&self) -> ResidencyStats {
        self.cache.stats
    }
    pub fn resolver_stats(&self) -> crate::texture_resolution::ResolverStats {
        self.resolver.stats
    }
    /// Releases decoded CPU ornament models and RGBA textures when local
    /// scenery streaming is superseded by whole-world GPU pages. Reloads
    /// transparently on demand if whole-world mode is later disabled.
    pub fn release_resident_caches(&mut self) {
        if !self.models.is_empty() {
            self.models.clear();
            self.models.shrink_to_fit();
        }
        if self.cache.stats.resident_bytes > 0 {
            self.cache.clear();
        }
    }
}

#[cfg(test)]
mod container_tests {
    use super::*;

    /// Build an Angelica package holding one stored member.
    ///
    /// This is the container the 1.5.2 client files `litmodels.pck` and
    /// `building.pck` in, and the one a scenery archive used to be refused for:
    /// the retail read fails, `open_optional_archive` returned `None`, and the
    /// map drew no buildings while its terrain and effects loaded normally.
    fn angelica(name: &str, payload: &[u8]) -> Vec<u8> {
        use flate2::write::ZlibEncoder;
        use std::io::Write;
        let mut record = vec![0u8; 276];
        record[..name.len()].copy_from_slice(name.as_bytes());
        record[260..264].copy_from_slice(&12u32.to_le_bytes());
        record[264..268].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        record[268..272].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&record).unwrap();
        let stream = encoder.finish().unwrap();

        let mut out = Vec::new();
        out.extend_from_slice(&0x4dca23efu32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0x56a089b7u32.to_le_bytes());
        // The member lives at offset 12, straight after the prefix.
        out.extend_from_slice(payload);
        out.extend_from_slice(&[0u8; 8]);
        out.extend_from_slice(&stream);
        let mut header = vec![0u8; 280];
        header[0..4].copy_from_slice(&crate::pck::ANGELICA_GUARD_0.to_le_bytes());
        header[268..272].copy_from_slice(&crate::pck::ANGELICA_GUARD_1.to_le_bytes());
        header[272..276].copy_from_slice(&1u32.to_le_bytes());
        header[276..280].copy_from_slice(&0x0002_0002u32.to_le_bytes());
        out.extend_from_slice(&header);
        let size = out.len() as u32;
        out[4..8].copy_from_slice(&size.to_le_bytes());
        out
    }

    fn temp_package(bytes: &[u8], name: &str) -> std::path::PathBuf {
        static UNIQUE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "pwe-scenery-{}-{}-{name}",
            std::process::id(),
            UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn an_angelica_scenery_archive_opens_and_reads() {
        let payload = b"MOXFBRAME: scenery";
        let path = temp_package(
            &angelica("litmodels/a01/1/litmodel_7.bmd", payload),
            "litmodels.pck",
        );
        let archive = open_scenery_package(&path).expect("Angelica scenery archive must open");
        assert_eq!(archive.container(), "Angelica");
        assert_eq!(archive.directory().len(), 1);
        // The whole point: the member is readable, not just listed.
        let entry = &archive.directory()[0];
        assert_eq!(entry.name, "litmodels/a01/1/litmodel_7.bmd");
        assert_eq!(archive.package().read(entry).unwrap(), payload);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn open_optional_archive_indexes_an_angelica_scenery_archive() {
        let path = temp_package(&angelica("textures/a/beam.dds", b"DDS "), "building.pck");
        let (archive, indexed, error) = open_optional_archive(&path);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(archive.as_ref().unwrap().container(), "Angelica");
        assert!(indexed.contains_key("textures/a/beam.dds"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_neither_container_archive_names_both_readers() {
        let path = temp_package(&[0x11; 4096], "litmodels.pck");
        let reason = match open_scenery_package(&path) {
            Ok(_) => panic!("a garbage archive must not open"),
            Err(reason) => reason,
        };
        assert!(reason.contains("retail"), "{reason}");
        assert!(reason.contains("Angelica"), "{reason}");
        std::fs::remove_file(path).unwrap();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const MASK: u32 = 0xa8937462;
    const CHECK: u32 = 0x59374231;

    fn put_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    /// A minimal, valid BMD with one empty mesh. The scenery path resolver is
    /// what these tests exercise, not model geometry.
    fn empty_bmd() -> Vec<u8> {
        let mut b = Vec::new();
        put_u32(&mut b, 0x42584f4d);
        put_u32(&mut b, 0x10000002);
        b.extend_from_slice(&[0; 48]);
        put_u32(&mut b, 1);
        put_u32(&mut b, 0x10000004);
        b.extend_from_slice(&[0; 320]);
        put_u32(&mut b, 0);
        put_u32(&mut b, 0);
        b.extend_from_slice(&[0; 48]);
        b
    }
    /// Write an uncompressed v2 PCK whose entries have the given normalized
    /// names. `compressed == size`, so the production batched reader returns
    /// the payload bytes without needing a compression fixture.
    fn write_pck(path: &std::path::Path, entries: &[(&str, &[u8])]) {
        let mut bytes = Vec::new();
        put_u32(&mut bytes, 0x4dca23ef);
        put_u32(&mut bytes, 0); // patched with the logical package end
        put_u32(&mut bytes, 0x56a089b7);
        let mut records = Vec::new();
        for (name, payload) in entries {
            records.push((bytes.len() as u32, *name, payload.len() as u32));
            bytes.extend_from_slice(payload);
        }
        let directory_at = bytes.len() as u32;
        for (offset, name, size) in records {
            let mut record = vec![0u8; 276];
            assert!(name.len() <= 260);
            record[..name.len()].copy_from_slice(name.as_bytes());
            record[260..264].copy_from_slice(&offset.to_le_bytes());
            record[264..268].copy_from_slice(&size.to_le_bytes());
            record[268..272].copy_from_slice(&size.to_le_bytes());
            put_u32(&mut bytes, 276 ^ MASK);
            put_u32(&mut bytes, 276 ^ MASK ^ CHECK);
            bytes.extend_from_slice(&record);
        }
        let end = bytes.len() as u32 + 280;
        let mut footer = vec![0u8; 280];
        footer[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
        footer[8..12].copy_from_slice(&(directory_at ^ MASK).to_le_bytes());
        // The client refuses a package whose description lacks "lica File
        // Package" (`strstr(m_header.szDescription, "lica File Package")`), so
        // a fixture without it is not a package it would ever open.
        footer[16..16 + 45].copy_from_slice(b"Angelica File Package, Perfect World Co. Ltd.");
        footer[268..272].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
        footer[272..276].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        footer[276..280].copy_from_slice(&0x20002u32.to_le_bytes());
        bytes.extend_from_slice(&footer);
        bytes[4..8].copy_from_slice(&end.to_le_bytes());
        std::fs::write(path, &bytes).unwrap();
    }
    fn fixture_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "pw-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
    fn ornament(map_id: &str) -> Ornament {
        Ornament {
            export_id: 77,
            offset: 0,
            position: [0.; 3],
            direction: [0., 0., 1.],
            up: [0., 1., 0.],
            extra: 0,
            path: "1/litmodel_1399.bmd".into(),
            map_id: map_id.into(),
        }
    }
    #[test]
    fn map_partitioned_litmodels_load_from_the_selected_instance_namespace() {
        let root = fixture_root("map-scoped-litmodels");
        let bmd = empty_bmd();
        write_pck(
            &root.join("litmodels.pck"),
            &[
                ("litmodels/a01/1/litmodel_1399.bmd", &bmd),
                ("litmodels/a11/1/litmodel_1399.bmd", &bmd),
            ],
        );
        write_pck(&root.join("building.pck"), &[]);
        let mut objects = Objects::open(&root, vec![ornament("a01")]).unwrap();
        objects.load_ids(&[0]).unwrap();
        assert!(objects.get(0).is_some());
        assert_eq!(objects.unavailable_model_count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn the_census_counts_drawable_and_unresolvable_placements_separately() {
        let root = fixture_root("census-partition");
        let bmd = empty_bmd();
        write_pck(
            &root.join("litmodels.pck"),
            &[("litmodels/a01/1/litmodel_1399.bmd", &bmd)],
        );
        write_pck(&root.join("building.pck"), &[]);
        let mut missing = ornament("a01");
        missing.path = "1/litmodel_1400.bmd".into();
        let ornaments = vec![ornament("a01"), missing, ornament("a01")];
        let mut objects = Objects::open(&root, ornaments).unwrap();
        objects.load_ids(&[0, 1, 2]).unwrap();
        // Three placements, two distinct names: 1399 resolves inside this map's
        // namespace, 1400 does not. Duplicates are counted once, so the numbers
        // describe the map rather than how often a prop repeats.
        let (available, unavailable, examples) = objects.model_census();
        assert_eq!((available, unavailable), (1, 1));
        assert!(examples.iter().any(|e| e.contains("litmodel_1400")));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_missing_instance_model_is_skipped_without_aborting_the_world() {
        let root = fixture_root("missing-instance-litmodel");
        let bmd = empty_bmd();
        write_pck(
            &root.join("litmodels.pck"),
            &[("litmodels/a11/1/litmodel_1399.bmd", &bmd)],
        );
        write_pck(&root.join("building.pck"), &[]);
        let mut objects = Objects::open(&root, vec![ornament("a01")]).unwrap();
        objects.load_ids(&[0]).unwrap();
        assert!(objects.get(0).is_none());
        assert_eq!(objects.unavailable_model_count(), 1);
        let reason = &objects.unavailable_examples()[0];
        assert!(reason.contains("litmodels/a01/"));
        assert!(reason.contains("refusing cross-map basename fallback"));
        // A later visible-frame batch must not retry the same absent model.
        objects.load_ids(&[0]).unwrap();
        assert_eq!(objects.unavailable_model_count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_optional_scenery_archives_do_not_block_map_objects() {
        let root = fixture_root("missing-object-archives");
        let mut objects = Objects::open(&root, vec![ornament("a01")]).unwrap();
        objects.load_ids(&[0]).unwrap();
        assert!(objects.get(0).is_none());
        assert_eq!(objects.unavailable_model_count(), 1);
        assert!(objects.unavailable_examples()[0].contains("litmodels.pck was not indexed"));
        std::fs::remove_dir_all(root).unwrap();
    }
    fn tex(fill: u8, side: usize) -> Texture {
        Texture {
            width: side,
            height: side,
            rgba: vec![fill; side * side * 4],
        }
    }
    #[test]
    fn dedup_stores_identical_content_once_under_alias_names() {
        let mut cache = TextureCache::new(64 << 20);
        cache.begin_batch();
        cache.store("a/x.dds".into(), tex(7, 64));
        cache.store("b/x_alias.dds".into(), tex(7, 64));
        assert_eq!(cache.stats.decodes, 1);
        assert_eq!(cache.stats.alias_hits, 1);
        assert!(cache.stats.alias_bytes_saved > 0);
        assert_eq!(cache.key("a/x.dds"), cache.key("b/x_alias.dds"));
        assert!(cache.get("b/x_alias.dds").is_some());
        // Different content stays separate.
        cache.store("c/y.dds".into(), tex(9, 64));
        assert_eq!(cache.stats.decodes, 2);
        assert_ne!(cache.key("a/x.dds"), cache.key("c/y.dds"));
    }
    #[test]
    fn lru_eviction_keeps_residency_inside_the_byte_budget() {
        // Budget fits two 64x64 textures (~32 KiB each); load three across
        // three batches -> the oldest must be evicted.
        let one = 64 * 64 * 4 + 64 * 64 / 1024;
        let mut cache = TextureCache::new(2 * one + 10);
        cache.begin_batch();
        cache.store("t1".into(), tex(1, 64));
        cache.begin_batch();
        cache.store("t2".into(), tex(2, 64));
        cache.begin_batch();
        cache.store("t3".into(), tex(3, 64));
        assert_eq!(cache.stats.evictions, 1);
        assert!(cache.stats.resident_bytes <= cache.stats.budget_bytes);
        assert!(cache.get("t1").is_none(), "oldest must be evicted");
        assert!(cache.get("t2").is_some());
        assert!(cache.get("t3").is_some());
        // Alias names of an evicted entry disappear too.
        cache.begin_batch();
        cache.store("t1_alias".into(), tex(1, 64));
        assert!(cache.stats.evictions >= 2);
        assert!(cache.get("t2").is_none());
    }
    #[test]
    fn evicting_content_removes_every_alias_name_and_nothing_else() {
        let one = 64 * 64 * 4 + 64 * 64 / 1024;
        let mut cache = TextureCache::new(2 * one + 10);
        // The shared content is the oldest batch, so it is the one the budget
        // has to drop; its two names must both stop resolving, and the later
        // entries must keep theirs.
        cache.begin_batch();
        cache.store("a/shared.dds".into(), tex(1, 64));
        cache.store("b/shared_alias.dds".into(), tex(1, 64));
        assert_eq!(cache.stats.alias_hits, 1);
        cache.begin_batch();
        cache.store("c/other.dds".into(), tex(2, 64));
        cache.begin_batch();
        cache.store("d/new.dds".into(), tex(3, 64));
        assert_eq!(cache.stats.evictions, 1);
        assert!(cache.stats.resident_bytes <= cache.stats.budget_bytes);
        assert!(cache.get("a/shared.dds").is_none());
        assert!(cache.get("b/shared_alias.dds").is_none());
        assert!(!cache.contains("a/shared.dds"));
        assert!(!cache.contains("b/shared_alias.dds"));
        assert!(cache.get("c/other.dds").is_some());
        assert!(cache.get("d/new.dds").is_some());
        assert_ne!(cache.key("c/other.dds"), cache.key("d/new.dds"));
    }
    #[test]
    fn pinned_batch_entries_survive_over_budget() {
        // Everything is stored inside one batch: nothing may be evicted
        // mid-batch even though the budget is exceeded.
        let one = 64 * 64 * 4 + 64 * 64 / 1024;
        let mut cache = TextureCache::new(one);
        cache.begin_batch();
        cache.store("p1".into(), tex(1, 64));
        cache.store("p2".into(), tex(2, 64));
        cache.store("p3".into(), tex(3, 64));
        assert_eq!(cache.stats.evictions, 0);
        assert_eq!(cache.stats.pinned_over_budget, 1);
        assert!(cache.get("p1").is_some() && cache.get("p3").is_some());
    }
    #[test]
    fn touching_a_reference_pins_its_content_again() {
        let one = 64 * 64 * 4 + 64 * 64 / 1024;
        let mut cache = TextureCache::new(2 * one + 10);
        cache.begin_batch();
        cache.store("keep".into(), tex(1, 64));
        cache.begin_batch();
        cache.store("other".into(), tex(2, 64));
        cache.begin_batch();
        cache.touch("keep"); // used by this batch again
        cache.store("third".into(), tex(3, 64));
        assert!(cache.get("keep").is_some(), "touched entry must survive");
        assert!(cache.get("other").is_none(), "untouched LRU goes first");
    }
}

/// UV scale and scroll velocity authored by an Angelica `.sdr` shader script
/// (`A3DTextureMan.cpp:150-163`, `A3DShaderMan.cpp:139-178`, `A3DShader.cpp:528-598, 2081-2085`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SdrUvAnim {
    pub tc_scale: [f32; 2],
    pub tc_scroll: [f32; 2],
}

impl SdrUvAnim {
    pub const IDENTITY: Self = Self {
        tc_scale: [1.0, 1.0],
        tc_scroll: [0.0, 0.0],
    };

    /// Match the 7 animated `.sdr` scripts in 1.4.2 `shaders.pck` by `.sdr.` texture path.
    pub fn from_texture_path(tex_path: &str) -> Option<Self> {
        let lower = tex_path.to_ascii_lowercase();
        let idx = lower.find(".sdr.")?;
        let prefix = &tex_path[..idx];
        let stem = prefix.rsplit(['\\', '/']).next().unwrap_or(prefix).trim();
        let anim = match stem {
            "瀑布" | "瀑布细长" => Self {
                tc_scale: [1.0, 1.0],
                tc_scroll: [0.0, -0.5],
            },
            "青云山小瀑布" => Self {
                tc_scale: [0.6, 0.6],
                tc_scroll: [0.0, -0.5],
            },
            "池水" => Self {
                tc_scale: [0.7, 0.7],
                tc_scroll: [0.004, 0.004],
            },
            "岩浆" => Self {
                tc_scale: [1.0, 1.0],
                tc_scroll: [0.0, -0.15],
            },
            "水" => Self {
                tc_scale: [1.0, 1.0],
                tc_scroll: [0.0, 0.10],
            },
            "血池" => Self {
                tc_scale: [1.0, 1.0],
                tc_scroll: [0.001, 0.001],
            },
            _ => Self::IDENTITY,
        };
        Some(anim)
    }
}
