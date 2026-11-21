#![allow(dead_code)]
#[path = "../navigation.rs"]
mod navigation;
#[path = "../pck.rs"]
mod pck;
#[path = "../terrain_store.rs"]
mod terrain_store;
#[path = "../world_place_names.rs"]
#[allow(dead_code)]
mod world_place_names;
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("element folder"));
    let d = navigation::destinations(&root.join("maps/world/world_targets.txt")).unwrap();
    println!(
        "{} main-world targets, {} distinct names",
        d.len(),
        d.iter()
            .map(|p| &p.name)
            .collect::<std::collections::HashSet<_>>()
            .len()
    );
    let m = navigation::MapSpace {
        width: 4096,
        height: 5632,
        spacing: 2.,
        heights: vec![0.; 4].into(),
    };
    for name in [
        "City of a Thousand Streams",
        "Archosaur, North District",
        "City of Raging Tides",
    ] {
        let p = d.iter().find(|p| p.name == name).unwrap();
        let [u, v] = m.to_uv(p.position[0], p.position[2]);
        println!(
            "{name} {:?} -> tile row {} col {}, local {:.3},{:.3}",
            p.position,
            (v * 11.) as u32,
            (u * 8.) as u32,
            (u * 8.).fract(),
            (v * 11.).fract()
        );
    }
    let image = navigation::original_map(&root).unwrap();
    println!(
        "Original map atlas {}x{} from 88 client DDS tiles",
        image.width(),
        image.height()
    );
}
