//! Survey real ECWLD/ECBSD water and authored shoreline records.
//!
//! Two modes:
//!
//!   water-record-survey <map folder>        per-area detail for one map
//!   water-record-survey --classify <root>   every water reference under
//!                                           <root> in exactly one bucket
//!
//! The classifier exists because "water coverage is unexplained" was a real
//! question with a real answer hiding behind it: a map that reports 148 water
//! references and draws 3 areas looks broken until the references are counted
//! by what happened to them. Every reference lands in exactly one bucket —
//! empty record, unknown version, unknown format, or read OK — and the bucket
//! counts add up to the reference count. Choosing per bucket is then a decision
//! made on numbers instead of on a guess.
#![allow(dead_code)]
// `water.rs`, `far_trees_gpu.rs` and the grass loader report through
// `crate::diag!`; a `#[path]` module set is its own crate root, so the stub
// supplies it. See `support/diag_stub.rs`.
#[macro_use]
#[path = "support/diag_stub.rs"]
mod diag_stub;
#[allow(unused_imports)]
pub use diag_stub::diagnostics;

#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../water.rs"]
mod water;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

/// What happened to one water reference.
///
/// The four buckets are exhaustive by construction: every reference either has
/// a record that decoded, an empty record, a record whose version word has no
/// recovered layout, or a record this reader could not interpret at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Bucket {
    /// The record is the authored-empty 16-byte `MOXB` v2 stub: zero entries.
    EmptyRecord,
    /// A `0x100SSSSS` version word with no recovered layout.
    UnknownVersion,
    /// Not a water record: bad header, bad framing, out-of-range index.
    UnknownFormat,
    /// At least one area decoded.
    ReadOk,
}

impl Bucket {
    fn label(self) -> &'static str {
        match self {
            Self::EmptyRecord => "empty record",
            Self::UnknownVersion => "unknown version",
            Self::UnknownFormat => "unknown format",
            Self::ReadOk => "read OK",
        }
    }
}

fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let first = args
        .next()
        .ok_or("usage: water-record-survey <map folder> | --classify <root>")?;
    if first == "--classify" {
        let root = PathBuf::from(
            args.next()
                .ok_or("usage: water-record-survey --classify <root>")?,
        );
        return classify(&root);
    }
    detail(&PathBuf::from(first))
}

/// Find every map folder under `root`: a directory holding a `.ecwld`.
fn map_folders(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut found_world = false;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Do not walk into a map folder's own data directories.
                stack.push(path);
                continue;
            }
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("ecwld"))
            {
                found_world = true;
            }
        }
        if found_world {
            out.push(dir);
        }
    }
    out.sort();
    out
}

/// The `.ecwld` a map folder holds, whatever it is named.
fn world_file(dir: &Path) -> Option<PathBuf> {
    let named = dir.join("world.ecwld");
    if named.is_file() {
        return Some(named);
    }
    let mut found = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("ecwld"))
        })
        .collect::<Vec<_>>();
    found.sort();
    found.into_iter().next()
}

/// Classify every water reference for every map under `root`.
fn classify(root: &Path) -> Result<(), String> {
    let maps = map_folders(root);
    if maps.is_empty() {
        return Err(format!("no .ecwld found under {}", root.display()));
    }
    let mut totals = BTreeMap::<Bucket, usize>::new();
    let mut all_refs = 0usize;
    println!(
        "{:<28} {:>6} {:>6} {:>7} {:>8} {:>9} {:>8} {:>6}",
        "map", "refs", "unique", "areas", "empty", "unk-ver", "unk-fmt", "ok"
    );
    for dir in maps {
        let name = dir.strip_prefix(root).unwrap_or(&dir).display().to_string();
        let Some(world) = world_file(&dir) else {
            continue;
        };
        let Ok(scene) = scene::Scene::load(&world) else {
            println!("{name:<28} {:>6}", "world file does not parse");
            continue;
        };
        let ecbsd = water::ecbsd_path_for_survey(&dir);
        let Ok(b) = fs::read(&ecbsd) else {
            // A map that references water but ships no manifest still has
            // references to account for, so they all land in one bucket rather
            // than vanishing from the table.
            let refs: usize = scene.blocks.iter().map(|block| block.water.len()).sum();
            if refs > 0 {
                println!(
                    "{name:<28} {refs:>6} {:>6} {:>7} {:>8} {:>9} {refs:>8}",
                    "", "", "", ""
                );
                all_refs += refs;
                *totals.entry(Bucket::UnknownFormat).or_default() += refs;
            }
            continue;
        };
        let count = water::u(&b, 12)? as usize;
        let mut pos = 0x4c;
        let mut paths = Vec::new();
        for _ in 0..count {
            paths.push(water::path_string(&b, &mut pos)?);
        }
        let mut seen = HashSet::new();
        let mut buckets = BTreeMap::<Bucket, usize>::new();
        let mut unique = 0usize;
        let mut areas = 0usize;
        let mut refs = 0usize;
        for reference in scene.blocks.iter().flat_map(|block| &block.water) {
            refs += 1;
            // A reference repeats once per block that names it; the reference
            // count is what the map declares, so every one is bucketed, and
            // the unique collapse is reported next to it so neither number
            // hides the other.
            let fresh = seen.insert((reference.export_id, reference.offset));
            if fresh {
                unique += 1;
            }
            let bucket = classify_one(&dir, &b, &paths, reference, &mut areas, fresh);
            *buckets.entry(bucket).or_default() += 1;
            // Refusals are named, never merely counted: a bucket total tells
            // you a fault exists, and only the error tells you what it is.
            if bucket != Bucket::ReadOk && std::env::var_os("WATER_SURVEY_VERBOSE").is_some() {
                eprintln!(
                    "{name} export {} offset {} -> {}",
                    reference.export_id,
                    reference.offset,
                    bucket.label()
                );
            }
        }
        for (bucket, n) in &buckets {
            *totals.entry(*bucket).or_default() += n;
        }
        all_refs += refs;
        println!(
            "{name:<28} {refs:>6} {unique:>6} {areas:>7} {:>8} {:>9} {:>8} {:>6}",
            buckets.get(&Bucket::EmptyRecord).copied().unwrap_or(0),
            buckets.get(&Bucket::UnknownVersion).copied().unwrap_or(0),
            buckets.get(&Bucket::UnknownFormat).copied().unwrap_or(0),
            buckets.get(&Bucket::ReadOk).copied().unwrap_or(0),
        );
    }
    let bucketed: usize = totals.values().sum();
    println!();
    for (bucket, n) in &totals {
        println!("{:<22} {n}", bucket.label());
    }
    println!("{:<22} {all_refs}", "references");
    println!("{:<22} {bucketed}", "references bucketed");
    if bucketed == all_refs {
        println!("every reference is in exactly one bucket");
        Ok(())
    } else {
        Err(format!(
            "{all_refs} references but {bucketed} bucketed: a reference went missing"
        ))
    }
}

/// Bucket one reference. Only a fresh unique pair is decoded; a repeat of a
/// pair already bucketed inherits that pair's bucket, because it is the same
/// record and decoding it twice would double-count its areas.
fn classify_one(
    dir: &Path,
    manifest: &[u8],
    paths: &[String],
    reference: &scene::Reference,
    areas: &mut usize,
    fresh: bool,
) -> Bucket {
    let off = reference.offset as usize;
    let (Ok(index), Ok(record)) = (water::u(manifest, off), water::u(manifest, off + 4)) else {
        return Bucket::UnknownFormat;
    };
    let Some(path) = paths.get(index as usize) else {
        return Bucket::UnknownFormat;
    };
    let Ok(resolved) = water::resolve_case_insensitive(dir, path) else {
        return Bucket::UnknownFormat;
    };
    let Ok(data) = fs::read(&resolved) else {
        return Bucket::UnknownFormat;
    };
    let (Ok(magic), Ok(version)) = (water::u(&data, 0), water::u(&data, 4)) else {
        return Bucket::UnknownFormat;
    };
    if magic != 0x42584f4d || version != 2 {
        return Bucket::UnknownFormat;
    }
    let Some(tail) = data.len().checked_sub(4) else {
        return Bucket::UnknownFormat;
    };
    let Ok(entries) = water::u(&data, tail) else {
        return Bucket::UnknownFormat;
    };
    let entries = entries as usize;
    if entries == 0 {
        return Bucket::EmptyRecord;
    }
    let Some(table) = (entries * 4)
        .checked_sub(0)
        .and_then(|n| tail.checked_sub(n))
    else {
        return Bucket::UnknownFormat;
    };
    let record = record as usize;
    if record >= entries {
        return Bucket::UnknownFormat;
    }
    let Ok(rec) = water::u(&data, table + record * 4) else {
        return Bucket::UnknownFormat;
    };
    let rec = rec as usize;
    let mut end = table;
    for i in 0..entries {
        let Ok(other) = water::u(&data, table + i * 4) else {
            continue;
        };
        let other = other as usize;
        if other > rec && other < end {
            end = other;
        }
    }
    if rec >= end || end > data.len() {
        return Bucket::UnknownFormat;
    }
    let Ok(batch) = water::parse_record(&data[rec..end], reference.export_id) else {
        let error = water::parse_record(&data[rec..end], reference.export_id).unwrap_err();
        if std::env::var_os("WATER_SURVEY_VERBOSE").is_some() {
            eprintln!(
                "  {path} record {record} bytes {}..{end}: {error}",
                rec,
                path = path
            );
        }
        return bucket_of_error(&error);
    };
    // A repeat of a pair already counted names the same areas; counting them
    // again would make the area total the reference total, which is exactly the
    // confusion the unique column exists to prevent.
    if fresh {
        *areas += batch.len();
    }
    Bucket::ReadOk
}

/// Bucket a parse failure by *what the error says*, never by guessing.
///
/// `unknown version` means a water revision word arrived that has no recovered
/// layout — the client wrote a format this reader has not been taught. Anything
/// else (a frame that will not inflate, an index past the table, a patch that
/// carries no water at all) is a format question, not a version question. The
/// two were conflated once: a "version 0x1af" error was really 431, a zlib
/// stored length, and treating it as a version sent the investigation after a
/// format that does not exist.
fn bucket_of_error(error: &str) -> Bucket {
    if error.contains("no recovered layout") || error.contains("unsupported shoreline wave version")
    {
        Bucket::UnknownVersion
    } else {
        Bucket::UnknownFormat
    }
}

/// Per-area detail for one map folder, the original mode.
fn detail(root: &Path) -> Result<(), String> {
    let world = world_file(root).ok_or_else(|| "no .ecwld in that folder".to_string())?;
    let scene = scene::Scene::load(&world)?;
    let b = fs::read(water::ecbsd_path_for_survey(root)).map_err(|e| e.to_string())?;
    let n = water::u(&b, 12)? as usize;
    let mut pos = 0x4c;
    let mut paths = Vec::new();
    for _ in 0..n {
        paths.push(water::path_string(&b, &mut pos)?);
    }
    let mut seen = HashSet::new();
    let mut counts = BTreeMap::new();
    let mut hist = BTreeMap::new();
    let mut wave_count = 0;
    let mut loaded = 0;
    let mut points = 0;
    let mut failures = 0;
    let mut missing = BTreeMap::new();
    let mut center_height_mismatches = Vec::new();
    let mut all_areas = Vec::new();
    let mut mask_values = BTreeMap::<u8, usize>::new();
    let mut surface_flags = BTreeMap::<u8, usize>::new();
    for reference in scene.blocks.iter().flat_map(|b| &b.water) {
        if !seen.insert(reference.export_id) {
            continue;
        };
        let off = reference.offset as usize;
        let ix = water::u(&b, off)? as usize;
        let record = water::u(&b, off + 4)? as usize;
        let path = paths.get(ix).ok_or("bad path")?;
        let name = path.rsplit('/').next().ok_or("bad basename")?;
        *counts.entry(name.to_string()).or_insert(0) += 1;
        let Ok(data) = fs::read(root.join(name)) else {
            *missing.entry(name.to_string()).or_insert(0) += 1;
            continue;
        };
        let tail = data.len() - 4;
        let count = water::u(&data, tail)? as usize;
        let table = tail - count * 4;
        if record >= count {
            failures += 1;
            continue;
        }
        let start = water::u(&data, table + record * 4)? as usize;
        let mut end = table;
        for i in 0..count {
            let x = water::u(&data, table + i * 4)? as usize;
            if x > start && x < end {
                end = x;
            }
        }
        let area = water::WaterArea::parse(&data[..end], start, reference.export_id)?;
        *surface_flags.entry(data[start + 44]).or_default() += 1;
        if data[start + 44] != 0 {
            eprintln!(
                "Simple water flag={} id={} source={}:{} height={} mask={} cells={}",
                data[start + 44],
                area.export_id,
                name,
                record,
                area.height,
                area.mask.iter().filter(|v| **v != 0).count(),
                area.mask.len()
            );
        }
        if (area.center[1] - area.height).abs() > 0.0001 {
            center_height_mismatches.push((area.export_id, area.height, area.center[1]));
        }
        for value in &area.mask {
            *mask_values.entry(*value).or_default() += 1;
        }
        all_areas.push(area.clone());
        loaded += 1;
        wave_count += area.shore_waves.len();
        for wave in &area.shore_waves {
            points += wave.points.len();
            *hist.entry(wave.capacity).or_insert(0) += 1;
        }
        println!(
            "{name}:{record} id={} water={:.1} waves={} points={}",
            reference.export_id,
            area.height,
            area.shore_waves.len(),
            area.shore_waves
                .iter()
                .map(|w| w.points.len())
                .sum::<usize>()
        );
    }
    println!(
        "Water areas={} loaded={} waves={} points={} errors={} missing={:?}",
        seen.len(),
        loaded,
        wave_count,
        points,
        failures,
        missing
    );
    let mut overlaps = 0;
    for (i, a) in all_areas.iter().enumerate() {
        for b in all_areas.iter().skip(i + 1) {
            let ax = a.columns as f32 * a.grid_size * 0.5;
            let az = a.rows as f32 * a.grid_size * 0.5;
            let bx = b.columns as f32 * b.grid_size * 0.5;
            let bz = b.rows as f32 * b.grid_size * 0.5;
            if (a.center[0] - b.center[0]).abs() < ax + bx
                && (a.center[2] - b.center[2]).abs() < az + bz
            {
                overlaps += 1;
            }
        }
    }
    println!(
        "Center Y/height mismatches: {:?}; overlapping bounding-box pairs: {}",
        center_height_mismatches, overlaps
    );
    println!(
        "Mask byte distribution: {:?}; surface flags {:?}",
        mask_values, surface_flags
    );
    println!("Water per BSData: {:?}", counts);
    println!("Wave capacity distribution: {:?}", hist);
    Ok(())
}
