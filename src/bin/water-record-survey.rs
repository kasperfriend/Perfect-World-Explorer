//! Survey real ECWLD/ECBSD water and authored shoreline records.
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
    path::PathBuf,
};
fn main() -> Result<(), String> {
    let root = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: water-record-survey <directory containing world.ecwld, world.ecbsd and BSData .dat files>")?);
    let scene = scene::Scene::load(&root.join("world.ecwld"))?;
    let b = fs::read(root.join("world.ecbsd")).map_err(|e| e.to_string())?;
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
