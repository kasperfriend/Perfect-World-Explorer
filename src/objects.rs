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
    texture_resolution::{normalize, MountedArchive, Resolution, TextureResolver},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::Path,
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
    let mut hasher = Sha256::new();
    hasher.update((tex.width as u64).to_le_bytes());
    hasher.update((tex.height as u64).to_le_bytes());
    hasher.update(&tex.rgba);
    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().unwrap())
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
    pub ornaments: Vec<Ornament>,
    models: HashMap<String, bmd::Model>,
    cache: TextureCache,
    resolver: TextureResolver,
    litfile: File,
    texfile: File,
    litentries: HashMap<String, pck::Entry>,
    texentries: HashMap<String, pck::Entry>,
    unresolved: Vec<UnresolvedTexture>,
    unresolved_seen: HashSet<String>,
}
/// RAM-first decoded-texture residency budget. Re-entry may re-decode evicted
/// sources; source resolution and all visible textures remain unchanged.
/// The whole corpus of
/// building.pck textures is ~445 MiB compressed on disk and far larger
/// decoded, so the full world never fits under this budget by design.
pub const DEFAULT_TEX_BUDGET_MIB: usize = 32;
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
impl Objects {
    pub fn open(element: &Path, ornaments: Vec<Ornament>) -> Result<Self, String> {
        let mut litfile = File::open(element.join("litmodels.pck")).map_err(|e| e.to_string())?;
        let mut texfile = File::open(element.join("building.pck")).map_err(|e| e.to_string())?;
        let litentries = pck::directory(&mut litfile)?
            .into_iter()
            .map(|e| (e.name.to_lowercase(), e))
            .collect::<HashMap<_, _>>();
        let texentries = pck::directory(&mut texfile)?
            .into_iter()
            .map(|e| (e.name.to_lowercase(), e))
            .collect::<HashMap<_, _>>();
        // Mount order matches the open order above. The retail mount order
        // of the two packages is not recoverable from the executable alone,
        // but it is also unobservable in this corpus: the census found 0
        // texture names present in both archives and litmodels.pck contains
        // no texture members at all (28858/28858 are .bmd).
        let resolver = TextureResolver::new(
            vec![
                MountedArchive {
                    tag: "litmodels".into(),
                    index: litentries.keys().cloned().collect(),
                },
                MountedArchive {
                    tag: "building".into(),
                    index: texentries.keys().cloned().collect(),
                },
            ],
            element.to_path_buf(),
        );
        Ok(Self {
            ornaments,
            models: HashMap::new(),
            cache: TextureCache::new(tex_budget_bytes()),
            resolver,
            litfile,
            texfile,
            litentries,
            texentries,
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
        let active: HashSet<String> = ids
            .iter()
            .map(|&i| format!("litmodels/world/{}", self.ornaments[i].path.to_lowercase()))
            .collect();
        self.models.retain(|name, _| active.contains(name));
        self.cache.begin_batch();
        for &i in ids {
            let name = format!("litmodels/world/{}", self.ornaments[i].path.to_lowercase());
            if !self.models.contains_key(&name) {
                let entry = self
                    .litentries
                    .get(&name)
                    .ok_or_else(|| format!("missing model {name}"))?;
                let data = pck::read_entry(&mut self.litfile, entry)?;
                let model = bmd::Model::parse(&data).map_err(|e| format!("{name}: {e}"))?;
                self.models.insert(name.clone(), model);
            }
            let references: Vec<String> = self.models[&name]
                .meshes
                .iter()
                .map(|m| m.texture.clone())
                .collect();
            for raw in references {
                self.load_texture_reference(&raw)?;
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
        }
        Ok(())
    }
    fn load_texture_reference(&mut self, raw: &str) -> Result<(), String> {
        let resolution = self.resolver.resolve(raw);
        let key = normalize(raw);
        match resolution {
            Resolution::Trivial => Ok(()),
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
                            return Ok(());
                        }
                        self.load_from_archive(mount, &fkey)
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
                            return Ok(());
                        }
                        let data = std::fs::read(&path)
                            .map_err(|e| format!("disk texture {}: {e}", path.display()))?;
                        self.decode_and_store(&key, &data)
                    }
                    _ => {
                        self.register(
                            raw,
                            "shader-registry",
                            &format!(
                                "name truncates to registry key {registry_key:?} (0x77c2e4 -> 0x785d30); the registry is runtime state and no same-named file exists in any staged archive or on disk"
                            ),
                        );
                        Ok(())
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
                Ok(())
            }
            Resolution::Archive { mount, key } => {
                if self.cache.contains(&key) {
                    self.cache.touch(&key);
                    return Ok(());
                }
                self.load_from_archive(mount, &key)
            }
            Resolution::Disk { path } => {
                if self.cache.contains(&key) {
                    self.cache.touch(&key);
                    return Ok(());
                }
                let data = std::fs::read(&path)
                    .map_err(|e| format!("disk texture {}: {e}", path.display()))?;
                self.decode_and_store(&key, &data)
            }
        }
    }
    fn load_from_archive(&mut self, mount: usize, key: &str) -> Result<(), String> {
        let data = if mount == 0 {
            let entry = self
                .litentries
                .get(key)
                .ok_or_else(|| format!("resolver/index mismatch for {key}"))?;
            pck::read_entry(&mut self.litfile, entry)?
        } else {
            let entry = self
                .texentries
                .get(key)
                .ok_or_else(|| format!("resolver/index mismatch for {key}"))?;
            pck::read_entry(&mut self.texfile, entry)?
        };
        self.decode_and_store(key, &data)
    }
    fn decode_and_store(&mut self, key: &str, data: &[u8]) -> Result<(), String> {
        // TGA has no magic header, so generic format sniffing rejects
        // valid client TGAs (including .sdr.tga files).
        let decoded = if key.ends_with(".tga") {
            image::load_from_memory_with_format(data, image::ImageFormat::Tga)
        } else {
            image::load_from_memory(data)
        };
        match decoded {
            Ok(img) => {
                let img = img.to_rgba8();
                let (w, h) = img.dimensions();
                self.cache.store(
                    key.to_string(),
                    Texture {
                        width: w as usize,
                        height: h as usize,
                        rgba: img.into_raw(),
                    },
                );
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
        self.models.get(&format!(
            "litmodels/world/{}",
            self.ornaments.get(idx)?.path.to_lowercase()
        ))
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
}
#[cfg(test)]
mod tests {
    use super::*;
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
