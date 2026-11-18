//! Client-faithful texture reference resolution for BMD meshes.
//!
//! Native call chain being reproduced (elementclient.exe 1.4.2, md5
//! 5f02895688fa580506e869156340a664, image base 0x400000):
//!
//! 1. `A3DLitMesh::LoadTexture` (0x7b9880) — per-mesh entry point:
//!    - calls the texture manager with the 256-byte mesh texture name
//!      (offset +0x14) storing the handle at +0x114;
//!    - `strlen(name) <= 4` -> returns TRUE without a normal map (0x7b9948);
//!    - `strstr(name, "nrf_")` -> returns TRUE without a normal map
//!      (0x7b99c8);
//!    - otherwise derives the normal-map name
//!      `shaders\textures\nrf\<basename>` (GetFileName 0x76ee80, sprintf
//!      format 0x98ed38, buffer +0x118), and if `FileExists` (0x76f1e0)
//!      fails replaces it with `shaders\textures\nrf\rf_default.dds`
//!      (0x98ed14); the derived name is loaded through the texture manager
//!      into handle +0x218; failure -> FALSE -> mesh Create FALSE (log
//!      0x98edec).
//! 2. `A3DTextureMan::LoadTexture` (0x77c230) — cache and registry:
//!    - cache keyed by the lowercased name (0x77c160); hit -> refcount++,
//!      handle = record+4;
//!    - lowercased name containing `.sdr.` -> the name is truncated after
//!      `.sdr` (0x77c2f7) and looked up in the runtime shader-texture
//!      registry (0x785d30 via engine+0x164); a miss returns FALSE
//!      (0x77c320) — these textures are registered by shader/effect code at
//!      runtime, never read from a file under the truncated name;
//!    - empty name -> substituted with `Textures\MissingTex.bmp` (0x9881fc).
//! 3. `A3DTexture::Load` (0x78a440) — tolerant loader: file-open failure
//!    leaves tex+0xc = 0 and still returns TRUE (0x78a50c region); image
//!    header failure logs 0x98a500 and returns TRUE; only allocation or
//!    device failures are fatal.
//! 4. `AFileImage::Open` (0x76da20) -> `AFileImage::Init` (0x76d810) —
//!    archive-first, then disk:
//!    - VFS lookup (0x771970): mounted archives are scanned in mount order;
//!      an archive participates when its mount tag is a `strstr` substring
//!      of the lowercased query name, and the entry is then found by exact
//!      normalized name inside that archive's index;
//!    - on VFS miss: `fopen(name, "rb")` relative to the working directory
//!      (0x76d878) — no extra directory prefixes are appended;
//!    - final miss: if the raw name contains `Textures` (0x76d88d, string
//!      0x986f60) or `Tex_` (0x76d8a6, string 0x986f58) the failure is
//!      SILENT (return FALSE at 0x76d92d without logging); otherwise
//!      `AFileImage::Can not open file [%s] from disk!` is logged (0x986f28)
//!      before returning FALSE.
//!
//! Animated texture records: the BMD mesh texture reference is a single
//! 256-byte NUL-terminated name; the format carries no frame list or
//! animation record. The only animated/procedural texture class reachable
//! from a BMD reference is the `.sdr.` shader-registry family (step 2):
//! the registry (0x785d30) is populated at runtime by the effect system,
//! which is outside this lane, so a registry miss is terminal — the client
//! returns FALSE with NO fallback to the archive file (0x77c320) and the
//! mesh load aborts mid-stream (0x7ba844) without resynchronizing the file
//! position. In retail the lookups succeed because the effect system has
//! registered those textures; their source pixels are the same-named
//! archive members (e.g. `building/textures/d/瀑布.sdr.tga` exists in
//! building.pck), so `Objects` approximates the registry by loading that
//! file and records a register row for every such reference. The
//! whole-corpus census (Findings/Phase-3-Agent-A-bmd-corpus-census.md)
//! counts 194 `.sdr.` references (4 distinct names in the world subset).
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
/// Outcome of resolving one texture reference the way the client does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Name is 4 bytes or shorter, or contains `nrf_`: the client succeeds
    /// without loading anything for the main slot (0x7b9948/0x7b99c8).
    Trivial,
    /// Lowercased name contains `.sdr.`: shader-texture registry lookup
    /// (0x77c2e4 -> 0x785d30) under the name truncated after `.sdr`.
    /// The registry is runtime state; with only litmodels.pck/building.pck
    /// staged it can never resolve, so callers must register the row.
    ShaderRegistry { key: String },
    /// Found in a mounted archive (VFS-first, 0x771970).
    Archive { mount: usize, key: String },
    /// Found on the local disk via `fopen(name, "rb")` (0x76d878).
    Disk { path: PathBuf },
    /// Not found anywhere. `quiet` mirrors the client's log suppression for
    /// names containing `Textures` or `Tex_` (0x76d88d/0x76d8a6).
    Missing { key: String, quiet: bool },
}
/// One mounted archive: the mount tag (file stem, matched as a substring of
/// the query per 0x771970) and the normalized member-name index.
pub struct MountedArchive {
    pub tag: String,
    pub index: HashSet<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResolverStats {
    pub queries: usize,
    pub cache_hits: usize,
    pub trivial: usize,
    pub shader_registry: usize,
    pub archive_hits: usize,
    pub disk_hits: usize,
    pub missing: usize,
    pub missing_quiet: usize,
    pub nrf_derived: usize,
    pub nrf_derived_present: usize,
    pub nrf_default_fallback: usize,
    /// `.sdr.` registry references whose same-named file was found in an
    /// archive or on disk (documented approximation, see Objects).
    pub shader_registry_file_backed: usize,
}
pub struct TextureResolver {
    mounts: Vec<MountedArchive>,
    disk_root: PathBuf,
    cache: HashMap<String, Resolution>,
    /// Separate cache for constructed-path lookups (nrf derivations, .sdr.
    /// file-backed approximation): these must not read or pollute the
    /// registry-semantics cache, which maps the same full name to
    /// ShaderRegistry.
    path_cache: HashMap<String, Resolution>,
    pub stats: ResolverStats,
}
/// Normalizes a reference the way the client's lookups do: lowercase
/// (`_strlwr` before the VFS scan, 0x771970) and `\` folded to `/`.
pub fn normalize(name: &str) -> String {
    name.to_lowercase().replace('\\', "/")
}

/// Keep an EC-BSD-derived map ID safe to use as an archive namespace. A direct
/// map folder may have a name that is loadable but not safe as a process ID or
/// path component; those maps fall back to the legacy `litmodels/world/` lookup.
fn safe_map_namespace(id: &str) -> Option<String> {
    let id = normalize(id).trim_matches('/').to_string();
    (!id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    .then_some(id)
}

/// The world-relative part of a scenery model reference. EC-BSD records store
/// `1/litmodel_N.bmd`, while archives may use `litmodels/world/...` or the
/// retail `litmodels/<map-id>/...` partition. Traversal components are dropped
/// before any archive key is constructed.
fn litmodel_relative(path: &str) -> String {
    let normalized = normalize(path);
    let parts: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .collect();
    let relative = if parts.first() == Some(&"litmodels") {
        if parts.get(1) == Some(&"world") || parts.len() > 2 {
            parts[2.min(parts.len())..].join("/")
        } else {
            parts[1..].join("/")
        }
    } else if parts.first() == Some(&"world") {
        parts[1..].join("/")
    } else {
        parts.join("/")
    };
    relative
}

/// Canonical in-memory key for an EC-BSD ornament path. Cache identity stays
/// stable across the different archive layouts used by client packs.
pub fn litmodel_key(path: &str) -> String {
    format!("litmodels/world/{}", litmodel_relative(path))
}

fn litmodel_entry_candidates(canonical: &str) -> [String; 4] {
    let relative = litmodel_relative(canonical);
    [
        canonical.to_string(),
        format!("litmodels/{relative}"),
        format!("world/{relative}"),
        relative,
    ]
}

/// Basename index for the final fallback. A basename is only usable when it is
/// unique, so another map's same-named model is never selected by guess.
pub fn litmodel_basename_index(entry_names: &[String]) -> HashMap<String, Vec<String>> {
    let mut index: HashMap<String, Vec<String>> = HashMap::new();
    for name in entry_names {
        let basename = name.rsplit('/').next().unwrap_or(name).to_string();
        index.entry(basename).or_default().push(name.clone());
    }
    index
}

fn map_namespace(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("litmodels/")?;
    let id = rest.split('/').next()?;
    if id.is_empty()
        || id == "world"
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(id)
}

fn partition_for_another_map(entry_names: &[String], map_id: &str) -> bool {
    entry_names
        .iter()
        .any(|key| map_namespace(key).is_some_and(|id| id != map_id))
}

/// Resolve a scenery BMD reference against the normalized archive index.
/// A map-partitioned archive must resolve within the selected map namespace;
/// otherwise A01/A11 models with identical relative paths can be mixed or the
/// whole map can appear empty. For a non-partitioned archive the legacy
/// `litmodels/world/` layout and its older alternate roots remain available.
pub fn litmodel_entry_key(
    path: &str,
    map_id: &str,
    entry_names: &[String],
    by_basename: &HashMap<String, Vec<String>>,
) -> Result<String, String> {
    let has = |key: &str| entry_names.iter().any(|candidate| candidate == key);
    let archive_path = normalize(path);
    let canonical = litmodel_key(path);
    let relative = litmodel_relative(&canonical);
    let map_id = safe_map_namespace(map_id);

    if let Some(map_id) = map_id.as_deref() {
        if map_id != "world" {
            let map_prefix = format!("litmodels/{map_id}/");
            let scoped_candidate = format!("{map_prefix}{relative}");
            if has(&scoped_candidate) {
                return Ok(scoped_candidate);
            }
            if entry_names.iter().any(|key| key.starts_with(&map_prefix))
                || partition_for_another_map(entry_names, map_id)
            {
                return Err(format!(
                    "missing model {path} in map namespace {map_prefix}; refusing cross-map basename fallback"
                ));
            }
        }
    }

    if has(&archive_path) {
        return Ok(archive_path);
    }
    for candidate in litmodel_entry_candidates(&canonical) {
        if has(&candidate) {
            return Ok(candidate);
        }
    }
    let suffix = format!("/{relative}");
    let suffix_matches: Vec<&String> = entry_names
        .iter()
        .filter(|key| key.ends_with(&suffix))
        .collect();
    match suffix_matches.as_slice() {
        [only] => return Ok((*only).clone()),
        matches if !matches.is_empty() => {
            return Err(format!(
                "no exact archive path for {path}; world-relative suffix {relative} is ambiguous ({} matches)",
                matches.len()
            ));
        }
        _ => {}
    }
    let basename = canonical.rsplit('/').next().unwrap_or(path).to_string();
    match by_basename.get(&basename).map(Vec::as_slice) {
        Some([only]) => Ok(only.clone()),
        Some(matches) => Err(format!(
            "no exact archive path for {path}; basename {basename} is ambiguous ({} matches)",
            matches.len()
        )),
        None => Err(format!(
            "missing model {path}; litmodels.pck has no member named {basename}"
        )),
    }
}
impl TextureResolver {
    pub fn new(mounts: Vec<MountedArchive>, disk_root: PathBuf) -> Self {
        Self {
            mounts,
            disk_root,
            cache: HashMap::new(),
            path_cache: HashMap::new(),
            stats: ResolverStats::default(),
        }
    }
    /// Mesh-level skip test from A3DLitMesh::LoadTexture (0x7b9880): byte
    /// length <= 4 (0x7b9948) or the name contains `nrf_` (0x7b99c8).
    pub fn skips_main_load(raw: &str) -> bool {
        raw.len() <= 4 || raw.contains("nrf_")
    }
    /// Full resolution order for a main (diffuse) texture reference.
    /// Results are cached per normalized name, mirroring the client's
    /// texture-manager cache (0x77c160).
    pub fn resolve(&mut self, raw: &str) -> Resolution {
        self.stats.queries += 1;
        if Self::skips_main_load(raw) {
            self.stats.trivial += 1;
            return Resolution::Trivial;
        }
        let key = normalize(raw);
        if let Some(hit) = self.cache.get(&key) {
            self.stats.cache_hits += 1;
            return hit.clone();
        }
        let cache_key = key.clone();
        let resolution = if let Some(idx) = key.find(".sdr.") {
            self.stats.shader_registry += 1;
            Resolution::ShaderRegistry {
                key: key[..idx + 4].to_string(),
            }
        } else {
            // Archive-first: mount order, tag must be a substring of the
            // query (0x771970), then exact normalized member lookup.
            let mut found = None;
            for (mount, archive) in self.mounts.iter().enumerate() {
                if !key.contains(&archive.tag) {
                    continue;
                }
                if archive.index.contains(&key) {
                    found = Some(Resolution::Archive {
                        mount,
                        key: key.clone(),
                    });
                    break;
                }
            }
            match found {
                Some(hit) => {
                    self.stats.archive_hits += 1;
                    hit
                }
                None => {
                    // Disk fallback: fopen(name, "rb") relative to the game
                    // working directory (0x76d878).
                    let path = self
                        .disk_root
                        .join(key.replace('/', std::path::MAIN_SEPARATOR_STR));
                    if path.is_file() {
                        self.stats.disk_hits += 1;
                        Resolution::Disk { path }
                    } else {
                        let quiet = raw.contains("Textures") || raw.contains("Tex_");
                        self.stats.missing += 1;
                        if quiet {
                            self.stats.missing_quiet += 1;
                        }
                        Resolution::Missing { key, quiet }
                    }
                }
            }
        };
        self.cache.insert(cache_key, resolution.clone());
        resolution
    }
    /// Derived normal-map reference from A3DLitMesh::LoadTexture: basename
    /// of the raw name (GetFileName 0x76ee80) formatted into
    /// `shaders\textures\nrf\%s` (0x98ed38); when FileExists (0x76f1e0)
    /// fails the client substitutes `shaders\textures\nrf\rf_default.dds`
    /// (0x98ed14). Returns `None` when the mesh skips the second load.
    pub fn resolve_nrf(&mut self, raw: &str) -> Option<(Resolution, bool)> {
        if raw.len() <= 4 || raw.contains("nrf_") {
            return None;
        }
        self.stats.nrf_derived += 1;
        let basename = raw.rsplit(['\\', '/']).next().unwrap_or(raw);
        let derived = format!("shaders/textures/nrf/{}", basename);
        let resolution = self.resolve_raw_path(&derived);
        let present = matches!(
            resolution,
            Resolution::Archive { .. } | Resolution::Disk { .. }
        );
        if present {
            self.stats.nrf_derived_present += 1;
            return Some((resolution, false));
        }
        self.stats.nrf_default_fallback += 1;
        let fallback = "shaders/textures/nrf/rf_default.dds";
        Some((self.resolve_raw_path(fallback), true))
    }
    /// Archive-then-disk lookup for a constructed path, bypassing the
    /// mesh-level skip tests and `.sdr.` registry semantics. Used for
    /// derived nrf paths and for the documented file-backed approximation
    /// of `.sdr.` registry references (see Objects::load_texture_reference).
    pub fn resolve_file_path(&mut self, key: &str) -> Resolution {
        self.resolve_raw_path(key)
    }
    /// Archive-then-disk lookup without the mesh-level skip tests and
    /// without `.sdr.` handling (used for derived nrf paths, which are
    /// constructed by the client itself and never carry those markers).
    fn resolve_raw_path(&mut self, key: &str) -> Resolution {
        let key = normalize(key);
        if let Some(hit) = self.path_cache.get(&key) {
            return hit.clone();
        }
        let cache_key = key.clone();
        let mut found = None;
        for (mount, archive) in self.mounts.iter().enumerate() {
            if !key.contains(&archive.tag) {
                continue;
            }
            if archive.index.contains(&key) {
                found = Some(Resolution::Archive {
                    mount,
                    key: key.clone(),
                });
                break;
            }
        }
        let resolution = match found {
            Some(hit) => hit,
            None => {
                let path = self
                    .disk_root
                    .join(key.replace('/', std::path::MAIN_SEPARATOR_STR));
                if path.is_file() {
                    Resolution::Disk { path }
                } else {
                    let quiet = key.contains("textures") || key.contains("tex_");
                    Resolution::Missing { key, quiet }
                }
            }
        };
        self.path_cache.insert(cache_key, resolution.clone());
        resolution
    }
}
/// Verifies a derived nrf reference resolves on disk or in an archive the
/// same way FileExists (0x76f1e0) does — exposed for register rows.
pub fn file_exists(resolver_mounts: &[MountedArchive], disk_root: &Path, key: &str) -> bool {
    let key = normalize(key);
    if resolver_mounts
        .iter()
        .any(|a| key.contains(&a.tag) && a.index.contains(&key))
    {
        return true;
    }
    disk_root
        .join(key.replace('/', std::path::MAIN_SEPARATOR_STR))
        .is_file()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scenery_index(names: &[&str]) -> (Vec<String>, HashMap<String, Vec<String>>) {
        let entry_names = names.iter().map(|name| normalize(name)).collect::<Vec<_>>();
        let by_basename = litmodel_basename_index(&entry_names);
        (entry_names, by_basename)
    }
    #[test]
    fn map_scoped_litmodel_lookup_uses_the_selected_map_namespace() {
        let (entry_names, by_basename) = scenery_index(&[
            "litmodels/a01/1/litmodel_1399.bmd",
            "litmodels/a11/1/litmodel_1399.bmd",
        ]);
        assert_eq!(
            litmodel_entry_key(
                "litmodels/world/1/litmodel_1399.bmd",
                "a01",
                &entry_names,
                &by_basename
            )
            .unwrap(),
            "litmodels/a01/1/litmodel_1399.bmd"
        );
    }
    #[test]
    fn a_missing_instance_model_is_not_borrowed_from_another_map() {
        let (entry_names, by_basename) = scenery_index(&["litmodels/a11/1/litmodel_1399.bmd"]);
        let error = litmodel_entry_key(
            "litmodels/world/1/litmodel_1399.bmd",
            "a01",
            &entry_names,
            &by_basename,
        )
        .unwrap_err();
        assert!(error.contains("litmodels/a01/"));
        assert!(error.contains("refusing cross-map basename fallback"));
    }
    #[test]
    fn legacy_main_world_layout_still_resolves_exactly() {
        let (entry_names, by_basename) = scenery_index(&["litmodels/world/1/litmodel_1399.bmd"]);
        assert_eq!(
            litmodel_entry_key(
                "litmodels/world/1/litmodel_1399.bmd",
                "world",
                &entry_names,
                &by_basename
            )
            .unwrap(),
            "litmodels/world/1/litmodel_1399.bmd"
        );
    }
    #[test]
    fn ambiguous_world_relative_suffix_is_reported_instead_of_guessed() {
        let (entry_names, by_basename) = scenery_index(&[
            "litmodels/a01/1/litmodel_0.bmd",
            "litmodels/a02/1/litmodel_0.bmd",
        ]);
        let error = litmodel_entry_key(
            "litmodels/world/1/litmodel_0.bmd",
            "world",
            &entry_names,
            &by_basename,
        )
        .unwrap_err();
        assert!(error.contains("ambiguous"));
    }
    #[test]
    fn an_unsafe_map_namespace_uses_the_legacy_lookup() {
        let (entry_names, by_basename) = scenery_index(&["litmodels/world/1/litmodel_1399.bmd"]);
        assert_eq!(
            litmodel_entry_key(
                "litmodels/world/1/litmodel_1399.bmd",
                "../a01",
                &entry_names,
                &by_basename
            )
            .unwrap(),
            "litmodels/world/1/litmodel_1399.bmd"
        );
    }
    fn mount(tag: &str, names: &[&str]) -> MountedArchive {
        MountedArchive {
            tag: tag.to_string(),
            index: names.iter().map(|n| normalize(n)).collect(),
        }
    }
    fn resolver(root: &Path) -> TextureResolver {
        // Mount order mirrors Objects::open: litmodels.pck then building.pck.
        TextureResolver::new(
            vec![
                mount("litmodels", &["litmodels/world/house.bmd"]),
                mount(
                    "building",
                    &[
                        "building/textures/a/be.dds",
                        "building/textures/h/865.psd",
                        "building/textures/shared.dds",
                    ],
                ),
            ],
            root.to_path_buf(),
        )
    }
    #[test]
    fn archive_wins_over_disk_and_mount_order_decides_duplicates() {
        let dir = std::env::temp_dir().join("pw-res-test-dup");
        std::fs::create_dir_all(dir.join("building/textures/a")).unwrap();
        std::fs::write(dir.join("building/textures/a/be.dds"), b"disk").unwrap();
        let mut r = resolver(&dir);
        // Duplicate across disk and archive: archive-first (0x76d83e VFS
        // call precedes fopen at 0x76d878).
        assert_eq!(
            r.resolve("Building\\Textures\\A\\be.dds"),
            Resolution::Archive {
                mount: 1,
                key: "building/textures/a/be.dds".into()
            }
        );
        // Duplicate across two archives: first mount wins (0x771970 scans in
        // mount order). Note the tag-substring rule makes duplicates nearly
        // unreachable in practice — a query would have to contain BOTH mount
        // tags — and litmodels.pck holds no texture members at all, so the
        // order is unobservable in this corpus; the rule is implemented
        // anyway.
        r.mounts[0]
            .index
            .insert("litmodels+building/shared.dds".into());
        r.mounts[1]
            .index
            .insert("litmodels+building/shared.dds".into());
        r.cache.clear();
        assert_eq!(
            r.resolve("litmodels+building/shared.dds"),
            Resolution::Archive {
                mount: 0,
                key: "litmodels+building/shared.dds".into()
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn shared_references_resolve_identically_and_hit_cache() {
        let dir = std::env::temp_dir().join("pw-res-test-shared");
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = resolver(&dir);
        let first = r.resolve("building/textures/a/be.dds");
        let second = r.resolve("BUILDING/Textures/a/BE.DDS");
        assert_eq!(first, second);
        assert!(r.stats.cache_hits >= 1);
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn missing_references_report_and_quiet_flag_matches_client_strings() {
        let dir = std::env::temp_dir().join("pw-res-test-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = resolver(&dir);
        // Real corpus case: referenced by 2 meshes, absent from every staged
        // archive. Name contains "Textures" -> client fails SILENTLY
        // (0x76d88d jumps over the 0x986f28 log).
        match r.resolve("building/Textures/h/16e0.dds") {
            Resolution::Missing { key, quiet } => {
                assert_eq!(key, "building/textures/h/16e0.dds");
                assert!(quiet);
            }
            other => panic!("{other:?}"),
        }
        // A name without Textures/Tex_ would be logged by the client.
        match r.resolve("building/models/loud.dds") {
            Resolution::Missing { quiet, .. } => assert!(!quiet),
            other => panic!("{other:?}"),
        }
        assert_eq!(r.stats.missing, 2);
        assert_eq!(r.stats.missing_quiet, 1);
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn disk_fallback_uses_plain_fopen_semantics() {
        let dir = std::env::temp_dir().join("pw-res-test-disk");
        std::fs::create_dir_all(dir.join("loose")).unwrap();
        std::fs::write(dir.join("loose/extra.dds"), b"x").unwrap();
        let mut r = resolver(&dir);
        // No archive tag matches -> fopen(name,"rb") relative to the root.
        assert_eq!(
            r.resolve("loose/extra.dds"),
            Resolution::Disk {
                path: dir.join("loose/extra.dds")
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn shader_registry_file_backed_lookup_bypasses_registry_cache() {
        let dir = std::env::temp_dir().join("pw-res-test-sdr-file");
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = resolver(&dir);
        r.mounts[1]
            .index
            .insert("building/textures/d/lava.sdr.tga".into());
        // Registry semantics resolve first ...
        assert_eq!(
            r.resolve("building/textures/d/lava.sdr.tga"),
            Resolution::ShaderRegistry {
                key: "building/textures/d/lava.sdr".into()
            }
        );
        // ... and the file-backed approximation for the SAME full name must
        // still find the archive member (regression: shared cache used to
        // poison this lookup).
        assert_eq!(
            r.resolve_file_path("building/textures/d/lava.sdr.tga"),
            Resolution::Archive {
                mount: 1,
                key: "building/textures/d/lava.sdr.tga".into()
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn shader_registry_names_are_rejected_by_name() {
        let dir = std::env::temp_dir().join("pw-res-test-sdr");
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = resolver(&dir);
        // 0x77c2e4: truncate after ".sdr" (0x77c2f7 writes NUL at hit+4).
        assert_eq!(
            r.resolve("effects/water.sdr.tga"),
            Resolution::ShaderRegistry {
                key: "effects/water.sdr".into()
            }
        );
        assert_eq!(r.stats.shader_registry, 1);
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn trivial_names_never_reach_file_lookup() {
        let dir = std::env::temp_dir().join("pw-res-test-trivial");
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = resolver(&dir);
        assert_eq!(r.resolve(""), Resolution::Trivial);
        assert_eq!(r.resolve("a.dd"), Resolution::Trivial);
        assert_eq!(r.resolve("nrf_custom.dds"), Resolution::Trivial);
        assert_eq!(r.stats.trivial, 3);
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn nrf_derivation_and_default_fallback() {
        let dir = std::env::temp_dir().join("pw-res-test-nrf");
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = resolver(&dir);
        // GetFileName (0x76ee80) takes the basename after the last
        // separator, both flavours; sprintf builds the nrf path (0x98ed38).
        // Nothing under shaders/ is staged, so FileExists fails and the
        // client substitutes rf_default.dds (0x98ed14) — which is also
        // absent here, so the fallback resolution is Missing.
        let (resolution, used_default) = r.resolve_nrf("building\\textures\\a\\be.dds").unwrap();
        assert!(used_default);
        match resolution {
            Resolution::Missing { key, .. } => {
                assert_eq!(key, "shaders/textures/nrf/rf_default.dds")
            }
            other => panic!("{other:?}"),
        }
        // When the derived file exists it is used directly.
        std::fs::create_dir_all(dir.join("shaders/textures/nrf")).unwrap();
        std::fs::write(dir.join("shaders/textures/nrf/other.dds"), b"x").unwrap();
        let (resolution, used_default) = r.resolve_nrf("models/other.dds").unwrap();
        assert!(!used_default);
        assert!(matches!(resolution, Resolution::Disk { .. }));
        // nrf_-prefixed names skip the second load entirely (0x7b99c8).
        assert!(r.resolve_nrf("nrf_weird.dds").is_none());
        assert_eq!(r.stats.nrf_derived, 2);
        assert_eq!(r.stats.nrf_default_fallback, 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
