//! Survey real BMD baked-light arrays without modifying the game archive.
#![allow(dead_code)]
#[path = "../bmd.rs"]
mod bmd;
#[path = "../pck.rs"]
mod pck;
use std::{env, path::Path};
fn main() {
    let path = env::args()
        .nth(1)
        .expect("usage: bmd-light-survey path-to-litmodels.pck");
    let mut file = pck::PackageFile::open(Path::new(&path)).unwrap();
    let entries = pck::directory(&mut file).unwrap();
    let mut models = 0u64;
    let mut meshes = 0u64;
    let mut vertices = 0u64;
    let mut first = 0u64;
    let mut second = 0u64;
    let mut equal = 0u64;
    let mut sum = [0u64; 3];
    let mut failed = 0;
    let mut examples = Vec::new();
    let mut day_blend = 0u64;
    let mut night_blend = 0u64;
    let mut rgbfirst = 0u64;
    let mut rgbsecond = 0u64;
    let mut samples = Vec::new();
    for e in entries.iter().filter(|e| {
        e.name.to_lowercase().starts_with("litmodels/world/")
            && e.name.to_lowercase().ends_with(".bmd")
    }) {
        match pck::read_entry(&mut file, e).and_then(|b| bmd::Model::parse(&b)) {
            Ok(m) => {
                models += 1;
                for mesh in m.meshes {
                    meshes += 1;
                    let mut ns = [0u64; 3];
                    for v in mesh.vertices {
                        vertices += 1;
                        let endpoint = |c: u32| -> u32 {
                            0xff000000
                                | ((((c >> 16) & 255) * 255 / 256) << 16)
                                | ((((c >> 8) & 255) * 255 / 256) << 8)
                                | ((c & 255) * 255 / 256)
                        };
                        day_blend += (v.diffuse == endpoint(v.day_diffuse)) as u64;
                        night_blend += (v.diffuse == endpoint(v.night_diffuse)) as u64;
                        rgbfirst += ((v.diffuse & 0xffffff) == (v.day_diffuse & 0xffffff)) as u64;
                        rgbsecond +=
                            ((v.diffuse & 0xffffff) == (v.night_diffuse & 0xffffff)) as u64;
                        if samples.len() < 8 {
                            samples.push([v.diffuse, v.day_diffuse, v.night_diffuse]);
                        }
                        first += (v.diffuse == v.day_diffuse) as u64;
                        second += (v.diffuse == v.night_diffuse) as u64;
                        equal += (v.day_diffuse == v.night_diffuse) as u64;
                        for (i, c) in [v.diffuse, v.day_diffuse, v.night_diffuse]
                            .iter()
                            .enumerate()
                        {
                            let s = ((c >> 16) & 255) as u64
                                + ((c >> 8) & 255) as u64
                                + (c & 255) as u64;
                            sum[i] += s;
                            ns[i] += s;
                        }
                    }
                    if examples.len() < 10 && ns[1] != ns[2] {
                        examples.push(format!("{} {} RGB sums {:?}", e.name, mesh.texture, ns));
                    }
                }
            }
            Err(err) => {
                failed += 1;
                if failed <= 10 {
                    eprintln!("{}: {err}", e.name);
                }
            }
        }
    }
    println!("models={models} meshes={meshes} vertices={vertices} failures={failed}; base=first {first}, base=second {second}, first=second {equal}");
    if vertices > 0 {
        println!(
            "mean per-channel encoded RGB: base={:.2}, array0={:.2}, array1={:.2}",
            sum[0] as f64 / (3 * vertices) as f64,
            sum[1] as f64 / (3 * vertices) as f64,
            sum[2] as f64 / (3 * vertices) as f64
        );
    }
    println!(
        "serialized vertex matches client blend endpoint: first {day_blend}, second {night_blend}"
    );
    println!(
        "base RGB=first {rgbfirst}, base RGB=second {rgbsecond}; first samples {samples:08x?}"
    );
    for e in examples {
        println!("{e}");
    }
    if failed != 0 {
        std::process::exit(1)
    }
}
