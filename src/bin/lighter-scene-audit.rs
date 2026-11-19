//! Read-only census of ECWLD placements and their direct GFX roots.
//! Not a playback census: no child/model-event traversal, texture binding,
//! activation, frustum culling or rendering. Verify retail input hashes first.
#![allow(dead_code)]
#[path = "../gfx_file.rs"]
mod gfx_file;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../water.rs"]
mod water;

use std::{collections::HashMap, fs::File, path::Path};

#[derive(Clone, Copy)]
enum Root {
    Readable {
        warp: usize,
        dest_src: usize,
        dest_dst: usize,
    },
    Unreadable,
    Rejected,
    Unsupported,
}

fn root(file: &mut File, entry: Option<&pck::Entry>) -> Root {
    let Some(bytes) = entry.and_then(|entry| pck::read_entry(file, entry).ok()) else {
        return Root::Unreadable;
    };
    let (text, bad) = gfx_file::decode(&bytes);
    if !text.starts_with("MOXT") {
        return Root::Unsupported;
    }
    if bad {
        return Root::Rejected;
    }
    let Ok(root) = gfx_file::parse(&text) else {
        return Root::Rejected;
    };
    let mut counts = [0; 3];
    for element in root.elements {
        let common = element.common;
        counts[0] += usize::from(common.warp);
        counts[1] += usize::from(matches!(common.source_blend, 9 | 10));
        counts[2] += usize::from(matches!(common.destination_blend, 9 | 10));
    }
    let [warp, dest_src, dest_dst] = counts;
    Root::Readable {
        warp,
        dest_src,
        dest_dst,
    }
}

fn audit(dir: &Path, file: &mut File, entries: &HashMap<String, pck::Entry>) -> Result<(), String> {
    let id = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid map directory")?;
    let scene = scene::Scene::load(&dir.join(format!("{id}.ecwld")))?;
    let ornaments = scene.load_ornaments(&dir.join(format!("{id}.ecbsd")))?;
    let water = water::load(&scene, dir)?;
    let grass_refs = scene
        .blocks
        .iter()
        .flat_map(|b| &b.grass)
        .map(|r| r.export_id)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let actors: usize = scene.blocks.iter().map(|b| b.models.len()).sum();
    let mut cache = HashMap::new();
    let [mut readable, mut unreadable, mut rejected, mut unsupported] = [0; 4];
    let [mut warp, mut dest_src, mut dest_dst] = [0; 3];
    for placement in scene.blocks.iter().flat_map(|b| &b.effects) {
        let key = placement.path.replace('\\', "/").to_lowercase();
        let key = if key.starts_with("gfx/") {
            key
        } else {
            format!("gfx/{key}")
        };
        match *cache
            .entry(key.clone())
            .or_insert_with(|| root(file, entries.get(&key)))
        {
            Root::Readable {
                warp: w,
                dest_src: s,
                dest_dst: d,
            } => {
                readable += 1;
                warp += w;
                dest_src += s;
                dest_dst += d;
            }
            Root::Unreadable => unreadable += 1,
            Root::Rejected => rejected += 1,
            Root::Unsupported => unsupported += 1,
        }
    }
    println!("{id}: ECWLD/EC-BSD 12/12, blocks={}, ornaments={}, grass_types={}, grass_refs={grass_refs}, actors={actors}, water_areas={}",
        scene.blocks.len(), ornaments.len(), scene.grass_types.len(), water.len());
    println!("  placed_effect_roots={} unique_direct_paths={} readable={readable} unreadable={unreadable} rejected={rejected} unsupported_container={unsupported}",
        readable + unreadable + rejected + unsupported, cache.len());
    println!("  authored_per_placement warp_elements={warp} dest_color_source_elements={dest_src} dest_color_destination_elements={dest_dst}");
    for area in water {
        let wet = area.mask.iter().filter(|&&cell| cell != 0).count();
        println!("  water {}: height={} center={:?} grid={}x{} pitch={} wet_cells={wet} simple={} shore_waves={} center_wet={}",
            area.export_id, area.height, area.center, area.columns, area.rows, area.grid_size,
            area.simple, area.shore_waves.len(), area.contains_xz(area.center[0], area.center[2]));
    }
    Ok(())
}

fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: lighter-scene-audit GFX.PCK MAP_DIRECTORY [MAP_DIRECTORY ...]";
    let pack = args.next().ok_or(usage)?;
    let dirs: Vec<_> = args.collect();
    if dirs.is_empty() {
        return Err(usage.into());
    }
    let mut file = File::open(&pack).map_err(|e| e.to_string())?;
    let entries: HashMap<_, _> = pck::directory(&mut file)?
        .into_iter()
        .map(|e| (e.name.replace('\\', "/").to_lowercase(), e))
        .collect();
    println!(
        "Read-only authored census: no render, root playback or child graph/material admission"
    );
    for dir in dirs {
        audit(Path::new(&dir), &mut file, &entries)?;
    }
    Ok(())
}
