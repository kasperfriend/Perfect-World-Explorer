//! Validate real client grass masks and MOXB meshes without opening a window.
// `water.rs`, `far_trees_gpu.rs` and the grass loader report through
// `crate::diag!`; a `#[path]` module set is its own crate root, so the stub
// supplies it. See `support/diag_stub.rs`.
#[macro_use]
#[path = "support/diag_stub.rs"]
mod diag_stub;
#[allow(unused_imports)]
pub use diag_stub::diagnostics;

#[allow(dead_code)]
#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[allow(dead_code)]
#[path = "../dxt.rs"]
mod dxt;
#[path = "../grass.rs"]
mod grass;
#[path = "../load_status.rs"]
mod load_status;
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
