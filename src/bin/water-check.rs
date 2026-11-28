//! Inventory the real world water elevations and grid extents.
// `water.rs`, `far_trees_gpu.rs` and the grass loader report through
// `crate::diag!`; a `#[path]` module set is its own crate root, so the stub
// supplies it. See `support/diag_stub.rs`.
#[macro_use]
#[path = "support/diag_stub.rs"]
mod diag_stub;
#[allow(unused_imports)]
pub use diag_stub::diagnostics;

#[allow(dead_code)]
#[path = "../load_status.rs"]
mod load_status;
#[path = "../pck.rs"]
mod pck;
#[allow(dead_code)]
#[path = "../scene.rs"]
mod scene;
#[allow(dead_code)]
#[path = "../water.rs"]
mod water;
use std::{env, path::Path};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 2 {
        return Err("usage: water-check <client-root-or-element>".into());
    }
    let root = Path::new(&args[1]);
    let element = if root.join("element").is_dir() {
        root.join("element")
    } else {
        root.to_path_buf()
    };
    let world = element.join("maps/world");
    let scene = scene::Scene::load(&world.join("world.ecwld"))?;
    let areas = water::load(&scene, &world)?;
    let mut heights = std::collections::BTreeMap::<i32, usize>::new();
    for a in &areas {
        if let Some(i) = a.mask.iter().position(|m| m & 1 != 0) {
            let x = a.center[0] - a.columns as f32 * a.grid_size * 0.5
                + (i % a.columns) as f32 * a.grid_size
                + 0.5 * a.grid_size;
            let z = a.center[2] + a.rows as f32 * a.grid_size * 0.5
                - (i / a.columns) as f32 * a.grid_size
                - 0.5 * a.grid_size;
            println!(" sample: {x:.1} {:.1} {z:.1}", a.height - 5.);
        }
        let count = a.mask.iter().filter(|c| **c & 1 != 0).count();
        *heights
            .entry((a.height * 1000.).round() as i32)
            .or_default() += 1;
        println!(
            "id={} height={:.3} center=({:.1},{:.1}) size={}x{} grid={:.1} active={}",
            a.export_id, a.height, a.center[0], a.center[2], a.columns, a.rows, a.grid_size, count
        )
    }
    eprintln!(
        "{} water areas; {} approximately distinct elevations",
        areas.len(),
        heights.len()
    );
    Ok(())
}
