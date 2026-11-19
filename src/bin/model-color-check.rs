#![allow(dead_code)]
#[path = "../bmd.rs"]
mod bmd;
#[path = "../model_dds.rs"]
mod model_dds;
#[path = "../objects.rs"]
mod objects;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../texture_resolution.rs"]
mod texture_resolution;
fn main() {
    let e = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let s = scene::Scene::load(&e.join("maps/world/world.ecwld")).unwrap();
    let os = s.load_ornaments(&e.join("maps/world/world.ecbsd")).unwrap();
    let mut o = objects::Objects::open(&e, os).unwrap();
    let ids = o.update(-755., -1352., 450.).unwrap();
    let mut m = std::collections::HashMap::new();
    let mut count = 0;
    for id in ids {
        if let Some(b) = o.get(id) {
            for mesh in &b.meshes {
                for v in &mesh.vertices {
                    *m.entry(v.diffuse).or_insert(0usize) += 1;
                    count += 1;
                }
            }
        }
    }
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    println!(
        "{count} vertices, {} distinct diffuse colors: {:?}",
        v.len(),
        &v[..v.len().min(15)]
    );
}
