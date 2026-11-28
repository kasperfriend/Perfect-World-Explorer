//! Whole-client diagnostic scanner triggered from the launcher toggle.
//!
//! Scans every discovered map in `element/maps/*` (ECWLD, EC-BSD ornaments,
//! TRN2 + terrain textures/T2MK, water, and every referenced `.bmd` inside
//! `litmodels.pck`) and writes a single human-readable report with plain-English
//! explanations for each failure category.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

pub struct Summary {
    pub report_path: PathBuf,
    pub maps_scanned: usize,
    pub maps_failed: usize,
    pub unique_bmds_scanned: usize,
    pub bmds_failed: usize,
    pub total_errors: usize,
}

fn normalize_pck_key(path: &str) -> String {
    path.trim()
        .trim_start_matches(['/', '\\'])
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn explain_error(category: &str, err: &str) -> &'static str {
    let lower = err.to_ascii_lowercase();
    if lower.contains("oversized ecwld string") {
        "ECWLD record layout mismatch: the parser expected or skipped a field (such as model scale) that differs in this ECWLD revision."
    } else if lower.contains("too many meshes") {
        "BMD header mismatch: an unexpected 32-bit sentinel or header word sits before nNumMeshes in this BMD version."
    } else if lower.contains("oversized bmd v300 mesh") {
        "BMD v300 mesh tail misalignment: the previous mesh ended at the wrong byte offset before the next mesh header."
    } else if lower.contains("invalid terrain dds header") {
        "Terrain texture format error: the file in textures.pck is neither a valid DDS nor a supported TGA/image format."
    } else if lower.contains("wrong version") || lower.contains("unsupported") {
        "Unsupported file revision: this asset uses a newer or unknown format version."
    } else if lower.contains("truncated") {
        "Truncated or short read: the file ended before the expected record size."
    } else if category == "Missing BMD" {
        "The map's .ecbsd ornament list references a .bmd path that was not found in litmodels.pck or on disk."
    } else {
        "Unhandled format or I/O error; inspect the exact message and offset above."
    }
}

fn report_output_path(element: &Path) -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("pw-explorer-diagnostics.txt"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("pw-explorer-diagnostics.txt"));
    }
    candidates.push(
        element
            .parent()
            .unwrap_or(element)
            .join("pw-explorer-diagnostics.txt"),
    );
    for path in candidates {
        if fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .is_ok()
        {
            return path;
        }
    }
    element.join("pw-explorer-diagnostics.txt")
}

fn check_terrain_and_materials(dir: &Path) -> Result<(), String> {
    let terrain_path = crate::world_catalog::data_file(dir, "trn2");
    let t = match fs::read(&terrain_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", terrain_path.display())),
    };
    if crate::u32at(&t, 0)? != 0x54524e32 || crate::u32at(&t, 4)? > 1 {
        return Err("unsupported TRN2".into());
    }
    let grid = crate::u32at(&t, 20)? as usize;
    let grid_spacing = crate::f32at(&t, 68)?;
    if !grid_spacing.is_finite() || grid_spacing <= 0.0 {
        return Err("invalid terrain grid spacing".into());
    }
    let rows = crate::u32at(&t, 28)? as usize;
    let cols = crate::u32at(&t, 32)? as usize;
    let count = crate::u32at(&t, 52)? as usize;
    let subgrid = crate::u32at(&t, 64)? as usize;
    let loader = crate::u32at(&t, 76)?;
    let loff = crate::u32at(&t, 80)? as usize;
    if loader != 1
        || grid == 0
        || grid > 256
        || subgrid == 0
        || subgrid % grid != 0
        || rows == 0
        || cols == 0
        || rows > 4096
        || cols > 4096
        || count > 10000
    {
        return Err("unsupported or invalid world terrain header".into());
    }
    let per = subgrid / grid;
    if rows % per != 0 || cols % per != 0 || rows / per * (cols / per) != count {
        return Err("inconsistent subterrain dimensions".into());
    }
    let w = cols.checked_mul(grid).ok_or("size overflow")?;
    let h = rows.checked_mul(grid).ok_or("size overflow")?;
    if w < 2 || h < 2 {
        return Err("map terrain dimensions are too small".into());
    }
    let mut pos = loff + 4;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        names.push(crate::read_string(&t, &mut pos)?);
    }
    crate::material::Materials::load(dir, &t, &names, w, h, grid_spacing)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn run_all_worlds(element: &Path) -> Result<Summary, String> {
    let catalog = crate::world_catalog::discover(element);
    let report_path = report_output_path(element);

    let mut lit_pkg = crate::pck::Package::open(&element.join("litmodels.pck")).ok();
    let mut lit_entries: HashMap<String, crate::pck::Entry> = HashMap::new();
    let mut lit_basenames: HashMap<String, String> = HashMap::new();
    if let Some(pkg) = lit_pkg.as_mut() {
        if let Ok(entries) = pkg.directory() {
            for entry in entries {
                let norm = normalize_pck_key(&entry.name);
                if let Some(base) = norm.rsplit('/').next() {
                    lit_basenames
                        .entry(base.to_string())
                        .or_insert_with(|| norm.clone());
                }
                lit_entries.insert(norm, entry);
            }
        }
    }

    let mut map_errors: Vec<(String, String, String)> = Vec::new();
    let mut bmd_refs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut maps_failed = 0usize;

    for map in &catalog.maps {
        let mut this_map_had_error = false;
        let ecwld_path = crate::world_catalog::data_file(&map.dir, "ecwld");
        let scene = match crate::scene::Scene::load(&ecwld_path) {
            Ok(s) => Some(s),
            Err(e) => {
                this_map_had_error = true;
                map_errors.push((map.id.clone(), "ECWLD".into(), e));
                None
            }
        };

        if let Some(scene) = scene.as_ref() {
            let ecbsd_path = crate::world_catalog::data_file(&map.dir, "ecbsd");
            if ecbsd_path.is_file() {
                match scene.load_ornaments(&ecbsd_path) {
                    Ok(ornaments) => {
                        for orn in ornaments {
                            let key = normalize_pck_key(&orn.path);
                            bmd_refs.entry(key).or_default().push(map.id.clone());
                        }
                    }
                    Err(e) => {
                        this_map_had_error = true;
                        map_errors.push((map.id.clone(), "ECBSD".into(), e));
                    }
                }
            }

            if let Err(e) = check_terrain_and_materials(&map.dir) {
                this_map_had_error = true;
                map_errors.push((map.id.clone(), "Terrain/Materials".into(), e));
            }

            if let Err(e) = crate::water::load(scene, &map.dir) {
                this_map_had_error = true;
                map_errors.push((map.id.clone(), "Water".into(), e));
            }
        }

        if this_map_had_error {
            maps_failed += 1;
        }
    }

    let mut bmd_errors: Vec<(String, String, Vec<String>)> = Vec::new();
    for (bmd_key, mut maps) in bmd_refs.clone() {
        maps.sort();
        maps.dedup();
        let disk_path = element.join(&bmd_key);
        let bytes: Result<Vec<u8>, String> = if disk_path.is_file() {
            fs::read(&disk_path).map_err(|e| e.to_string())
        } else if let Some(pkg) = lit_pkg.as_ref() {
            let resolved_key = if lit_entries.contains_key(&bmd_key) {
                Some(bmd_key.clone())
            } else {
                bmd_key
                    .rsplit('/')
                    .next()
                    .and_then(|base| lit_basenames.get(base))
                    .cloned()
            };
            match resolved_key.and_then(|k| lit_entries.get(&k).cloned()) {
                Some(entry) => pkg.read(&entry),
                None => Err("not found in litmodels.pck or on disk".into()),
            }
        } else {
            Err("litmodels.pck not opened and file not on disk".into())
        };

        match bytes {
            Ok(data) => {
                if let Err(e) = crate::bmd::Model::parse(&data) {
                    bmd_errors.push((bmd_key, e, maps));
                }
            }
            Err(e) => {
                bmd_errors.push((bmd_key, format!("Missing BMD: {e}"), maps));
            }
        }
    }

    let total_errors = map_errors.len() + bmd_errors.len();
    let mut lines: Vec<String> = Vec::new();
    lines.push("Perfect World Explorer — Full Client Diagnostics".into());
    lines.push(format!("Client element dir: {}", element.display()));
    lines.push(format!(
        "Maps scanned: {} ({} with errors)",
        catalog.maps.len(),
        maps_failed
    ));
    lines.push(format!(
        "Unique BMDs referenced: {} ({} failed)",
        bmd_refs.len(),
        bmd_errors.len()
    ));
    lines.push(format!("Total errors: {total_errors}"));
    lines.push(String::new());

    if map_errors.is_empty() {
        lines.push(format!(
            "[Map Errors] None — all {} maps loaded ECWLD/ECBSD/Terrain/Water cleanly.",
            catalog.maps.len()
        ));
    } else {
        lines.push(format!("[Map Errors] ({})", map_errors.len()));
        for (map_id, stage, err) in &map_errors {
            lines.push(format!("- map={map_id} stage={stage}: {err}"));
            lines.push(format!("  Explanation: {}", explain_error(stage, err)));
        }
    }
    lines.push(String::new());

    if bmd_errors.is_empty() {
        lines.push(format!(
            "[BMD Scenery Errors] None — all {} unique referenced BMDs parsed cleanly.",
            bmd_refs.len()
        ));
    } else {
        lines.push(format!("[BMD Scenery Errors] ({})", bmd_errors.len()));
        for (path, err, maps) in &bmd_errors {
            let cat = if err.starts_with("Missing BMD:") {
                "Missing BMD"
            } else {
                "BMD"
            };
            lines.push(format!("- {path} (used in {}): {err}", maps.join(", ")));
            lines.push(format!("  Explanation: {}", explain_error(cat, err)));
        }
    }

    let out = lines.join("\n") + "\n";
    fs::write(&report_path, &out)
        .map_err(|e| format!("Failed to write diagnostics report: {e}"))?;

    Ok(Summary {
        report_path,
        maps_scanned: catalog.maps.len(),
        maps_failed,
        unique_bmds_scanned: bmd_refs.len(),
        bmds_failed: bmd_errors.len(),
        total_errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_diagnostics_report_for_empty_or_broken_maps() {
        let dir = std::env::temp_dir().join(format!("pwe_diag_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let maps = dir.join("element").join("maps").join("a999");
        fs::create_dir_all(&maps).unwrap();
        fs::write(maps.join("a999.ecwld"), b"BAD!").unwrap();
        fs::write(maps.join("a999.trn2"), b"TRN2").unwrap();
        fs::write(maps.join("a999.ecbsd"), b"ECBSD").unwrap();

        let summary =
            run_all_worlds(&dir.join("element")).expect("diagnostics should write a report");
        assert_eq!(summary.maps_scanned, 1);
        assert_eq!(summary.maps_failed, 1);
        assert_eq!(summary.total_errors, 1);
        let text = fs::read_to_string(&summary.report_path).unwrap();
        assert!(text.contains("map=a999 stage=ECWLD"));
        assert!(text.contains("Explanation:"));
        assert!(
            !summary.report_path.starts_with(&dir),
            "report should be written in Explorer's root dir, not inside client folder: {}",
            summary.report_path.display()
        );
        let _ = fs::remove_file(&summary.report_path);
        let _ = fs::remove_dir_all(&dir);
    }
}
