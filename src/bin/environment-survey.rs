// `water.rs`, `far_trees_gpu.rs` and the grass loader report through
// `crate::diag!`; a `#[path]` module set is its own crate root, so the stub
// supplies it. See `support/diag_stub.rs`.
#[macro_use]
#[path = "support/diag_stub.rs"]
mod diag_stub;
#[allow(unused_imports)]
pub use diag_stub::diagnostics;

#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[path = "../client_weather.rs"]
mod client_weather;
#[allow(dead_code)]
#[path = "../environment.rs"]
mod environment;
#[allow(dead_code)]
#[path = "../load_status.rs"]
mod load_status;
#[path = "../pck.rs"]
mod pck;
#[allow(dead_code)]
#[path = "../scene.rs"]
mod scene;
#[path = "../sky_config.rs"]
mod sky_config;
#[allow(dead_code)]
#[path = "../water.rs"]
mod water;
use std::{env, path::Path};
fn main() {
    let root = env::args().nth(1).expect("element folder");
    let dir = Path::new(&root).join("maps/world");
    let s = scene::Scene::load(&dir.join("world.ecwld")).unwrap();
    let areas = water::load(&s, &dir).unwrap();
    let mut counts = [0usize; 256];
    for a in &areas {
        for b in &a.mask {
            counts[*b as usize] += 1;
        }
    }
    println!(
        "mask histogram {:?}",
        counts
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .collect::<Vec<_>>()
    );
    for a in areas.iter().take(3) {
        println!(
            "water {} center {:?} height {} grid {} {}x{}",
            a.export_id, a.center, a.height, a.grid_size, a.columns, a.rows
        );
    }
    for a in &areas {
        if let Some(i) = a.mask.iter().position(|m| *m != 0) {
            let x = a.center[0] - a.columns as f32 * a.grid_size * 0.5
                + (i % a.columns) as f32 * a.grid_size
                + a.grid_size * 0.5;
            let z = a.center[2] + a.rows as f32 * a.grid_size * 0.5
                - (i / a.columns) as f32 * a.grid_size
                - a.grid_size * 0.5;
            println!("active sample x={x} y={} z={z}", a.height - 4.);
            break;
        }
    }
    let env = environment::Environments::load(&s, &dir).unwrap();
    for p in [[-3584., 0., 5120.], [0., 0., 0.], [3584., 0., 5400.]] {
        println!(
            "{p:?} day {:?} night {:?}",
            env.underwater(p, 12.),
            env.underwater(p, 0.)
        );
    }
}
