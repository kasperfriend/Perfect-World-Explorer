#![allow(dead_code)]
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
fn main() {
    let path = std::env::args().nth(1).expect("world.ecwld path");
    let s = scene::Scene::load(std::path::Path::new(&path)).unwrap();
    let mut modes = std::collections::BTreeMap::new();
    for g in &s.grass_types {
        *modes
            .entry((g.alpha_blend & 255, g.alpha_ref, g.sight_range as i32))
            .or_insert(0) += 1;
    }
    println!("grass types {}: {:?}", s.grass_types.len(), modes);
    for (i, g) in s.grass_types.iter().enumerate() {
        println!(
            "{i} {} blend={} ref={} range={} size={} var={} softness={}",
            g.path,
            g.alpha_blend & 255,
            g.alpha_ref,
            g.sight_range,
            g.size,
            g.size_variation,
            g.softness
        );
    }
}
