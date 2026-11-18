#![allow(dead_code)]
#[allow(dead_code)]
#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[path = "../dxt.rs"]
mod dxt;
#[path = "../grass.rs"]
mod grass;
#[path = "../model_dds.rs"]
mod model_dds;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../vegetation_textures.rs"]
mod vegetation_textures;
#[path = "../water.rs"]
mod water;
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let s = scene::Scene::load(&root.join("maps/world/world.ecwld")).unwrap();
    let g = grass::Grass::load(&s, &root.join("maps/world"), &root).unwrap();
    let mut dims = std::collections::BTreeMap::new();
    for i in g.textures.values() {
        *dims.entry((i.width(), i.height())).or_insert(0) += 1;
    }
    println!("{dims:?}");
}
