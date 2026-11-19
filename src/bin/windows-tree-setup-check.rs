//! End-to-end check of the embedded PE32 client DLL bridge on Windows/Wine.
#![allow(dead_code)]
#[cfg(windows)]
#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[cfg(windows)]
#[path = "../client_detect.rs"]
mod client_detect;
#[cfg(windows)]
#[path = "../dxt.rs"]
mod dxt;
#[cfg(windows)]
#[path = "../launcher_ui.rs"]
mod launcher_ui;
#[cfg(windows)]
#[path = "../model_dds.rs"]
mod model_dds;
#[cfg(windows)]
#[path = "../pck.rs"]
mod pck;
#[cfg(windows)]
#[path = "../scene.rs"]
mod scene;
#[cfg(windows)]
#[path = "../tree_source.rs"]
mod tree_source;
// `trees.rs` compiles on every platform and names this module, so the module
// cannot be Windows-only even though the bridge around it is.
#[path = "../speedtree_version.rs"]
mod speedtree_version;
#[cfg(windows)]
#[path = "../trees.rs"]
mod trees;
#[cfg(windows)]
#[path = "../vegetation_textures.rs"]
mod vegetation_textures;
#[cfg(windows)]
#[path = "../windows_setup.rs"]
mod windows_setup;
// `windows_setup` draws its load-rate sparkline through
// `crate::loading_graph`.
#[path = "../loading_graph.rs"]
mod loading_graph;
#[cfg(windows)]
fn main() {
    let root = std::path::PathBuf::from(std::env::args_os().nth(1).expect("element path"));
    let cache = windows_setup::prepare_trees(&root).expect("first-run client bridge");
    let scene = scene::Scene::load(&root.join("maps/world/world.ecwld")).unwrap();
    let trees = trees::Trees::open(&root, &scene, &cache).unwrap();
    assert_eq!(trees.types.iter().flatten().count(), 59);
    assert!(trees
        .types
        .iter()
        .flatten()
        .any(|t| t.lods.iter().any(|l| !l.rocking.is_empty())));
    println!(
        "All 59 client trees loaded with runtime-sampled leaf rocking: {}",
        cache.display()
    );
    let again = windows_setup::prepare_trees(&root).unwrap();
    assert_eq!(cache, again);
    println!("Tree cache reused successfully");
}
#[cfg(not(windows))]
fn main() {
    println!("Windows-only setup check");
}
