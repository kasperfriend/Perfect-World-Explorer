//! Audit client-runtime sampled leaf animation across every cached type and LOD.
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
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("element path"));
    let cache = std::path::PathBuf::from(std::env::args().nth(2).expect("tree cache"));
    let scene = scene::Scene::load(&root.join("maps/world/world.ecwld")).unwrap();
    let trees = trees::Trees::open(&root, &scene, &cache).unwrap();
    let (mut total, mut animated, mut fixed, mut nonzero, mut max_delta) =
        (0usize, 0usize, 0usize, 0usize, 0f32);
    for tree in trees.types.iter().flatten() {
        for lod in &tree.lods {
            assert_eq!(lod.rocking.len(), lod.rocking_clusters * 4 * 64);
            for leaf in &lod.leaves {
                total += 1;
                if leaf.rho >= 1000. {
                    fixed += 1;
                }
                if lod.rocking.is_empty() {
                    continue;
                }
                animated += 1;
                let i = leaf.cluster as usize * 4 * 64;
                let d = glam::Vec3::from_array(lod.rocking[i])
                    - glam::Vec3::from_array(lod.rocking[i + 4]);
                let m = d.length();
                max_delta = max_delta.max(m);
                if m > 1e-5 {
                    nonzero += 1;
                }
            }
        }
    }
    println!("{total} LOD leaf records, {animated} animated-capable, {fixed} fixed-camera fallback, {nonzero} moving at t=0 to t=1, max corner delta {max_delta:.5}");
    assert!(animated > 0 && nonzero > 0);
}
