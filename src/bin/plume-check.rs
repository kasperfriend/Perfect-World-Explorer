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
    let d = e.join("maps/world");
    let s = scene::Scene::load(&d.join("world.ecwld")).unwrap();
    let os = s.load_ornaments(&d.join("world.ecbsd")).unwrap();
    let mut o = objects::Objects::open(&e, os).unwrap();
    let ids = o.update(-755., -1352., 450.).unwrap();
    let mut all = Vec::new();
    for id in ids {
        let m = o.get(id).unwrap();
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for mesh in &m.meshes {
            for v in &mesh.vertices {
                for k in 0..3 {
                    min[k] = min[k].min(v.position[k]);
                    max[k] = max[k].max(v.position[k]);
                }
            }
        }
        let size = (max[0] - min[0]).max(max[1] - min[1]).max(max[2] - min[2]);
        if size > 30. {
            all.push((
                size,
                id,
                m.position,
                m.meshes
                    .iter()
                    .map(|m| m.texture.clone())
                    .collect::<Vec<_>>(),
            ));
        }
    }
    all.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (size, id, pos, tex) in all.iter().take(50) {
        println!(
            "{size:.1} id={id} {:?} {} {:?}",
            pos, o.ornaments[*id].path, tex
        );
    }
}
