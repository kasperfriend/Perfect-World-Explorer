//! Validate real client grass masks and MOXB meshes without opening a window.
#[allow(dead_code)]
#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[allow(dead_code)]
#[path = "../dxt.rs"]
mod dxt;
#[path = "../grass.rs"]
mod grass;
#[path = "../model_dds.rs"]
mod model_dds;
#[allow(dead_code)]
#[path = "../pck.rs"]
mod pck;
#[allow(dead_code)]
#[path = "../scene.rs"]
mod scene;
#[allow(dead_code)]
#[path = "../vegetation_textures.rs"]
mod vegetation_textures;
#[path = "../water.rs"]
mod water;
fn main() {
    let Some(root) = std::env::args_os().nth(1) else {
        eprintln!("usage: grass-check <client root>");
        std::process::exit(2)
    };
    let element = std::path::Path::new(&root).join("element");
    let dir = element.join("maps/world");
    let r = scene::Scene::load(&dir.join("world.ecwld"))
        .and_then(|s| grass::Grass::load(&s, &dir, &element));
    match r {
        Ok(g) => println!(
            "{} masks; {} active cells; {} mesh types; {} decoded textures",
            g.areas.len(),
            g.areas.iter().map(|a| a.active.len()).sum::<usize>(),
            g.models.iter().filter(|m| m.is_some()).count(),
            g.textures.len()
        ),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}
