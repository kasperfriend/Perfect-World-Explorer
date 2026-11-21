#![allow(dead_code)]
#[path = "../angelica_pck.rs"]
#[allow(dead_code)]
mod angelica_pck;
#[path = "../bmd.rs"]
mod bmd;
#[path = "../objects.rs"]
mod objects;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../texture_resolution.rs"]
mod texture_resolution;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};
fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().ok_or("need element dir")?);
    let x: f32 = args
        .next()
        .unwrap_or("0".into())
        .parse()
        .map_err(|_| "bad x")?;
    let z: f32 = args
        .next()
        .unwrap_or("0".into())
        .parse()
        .map_err(|_| "bad z")?;
    let radius: f32 = args
        .next()
        .unwrap_or("400".into())
        .parse()
        .map_err(|_| "bad radius")?;
    let dir = root.join("maps/world");
    let scene = scene::Scene::load(&dir.join("world.ecwld"))?;
    let o = scene.load_ornaments(&dir.join("world.ecbsd"))?;
    let mut objects = objects::Objects::open(&root, o)?;
    let ids = objects.update(x, z, radius)?;
    let mut seen = HashSet::new();
    let mut dims = HashMap::new();
    let mut files = Vec::new();
    for i in &ids {
        if let Some(m) = objects.get(*i) {
            for mesh in &m.meshes {
                if seen.insert(mesh.texture.to_lowercase()) {
                    if let Some(t) = objects.texture(&mesh.texture) {
                        *dims.entry((t.width, t.height)).or_insert(0usize) += 1;
                        files.push((t.width.max(t.height), mesh.texture.clone()));
                    }
                }
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    println!(
        "{} placements {} textures dims {:?}",
        ids.len(),
        files.len(),
        dims
    );
    for d in files.iter().take(30) {
        println!("{} {}", d.0, d.1)
    }
    Ok(())
}
