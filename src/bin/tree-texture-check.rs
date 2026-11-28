#![allow(dead_code)]
#[path = "../dxt.rs"]
mod dxt;
#[path = "../model_dds.rs"]
mod model_dds;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../speedtree_version.rs"]
mod speedtree_version;
#[path = "../trees.rs"]
mod trees;
#[path = "../vegetation_textures.rs"]
mod vegetation_textures;
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let cache = std::path::PathBuf::from(std::env::args().nth(2).unwrap());
    let s = scene::Scene::load(&root.join("maps/world/world.ecwld")).unwrap();
    let t = trees::Trees::open(&root, &s, &cache).unwrap();
    let mut dims = std::collections::BTreeMap::new();
    for x in t.types.iter().flatten() {
        for img in [&x.bark, &x.composite] {
            *dims.entry((img.width(), img.height())).or_insert(0) += 1;
        }
    }
    println!("{dims:?}");
    let grid = 64usize;
    let mut used = vec![false; grid * grid];
    let mut count = 0;
    for (id, tree) in t.types.iter().enumerate() {
        let Some(tree) = tree else { continue };
        if !t.placements.iter().any(|p| p.tree_type as usize == id) {
            continue;
        }
        for img in [&tree.bark, &tree.composite] {
            let w = (img.width() as usize).next_power_of_two().max(128) / 128;
            let h = (img.height() as usize).next_power_of_two().max(128) / 128;
            let mut found = None;
            'scan: for y in 0..=grid - h {
                for x in 0..=grid - w {
                    if (y..y + h).all(|yy| (x..x + w).all(|xx| !used[yy * grid + xx])) {
                        found = Some((x, y));
                        break 'scan;
                    }
                }
            }
            let (x, y) = found.expect("tree native atlas exhausted");
            for yy in y..y + h {
                for xx in x..x + w {
                    used[yy * grid + xx] = true;
                }
            }
            count += 1;
        }
    }
    println!("Packed {count} real client textures without reduction in 8192x8192 atlas");
}
