//! Inspect actual world BMD material records without changing rendering.
#![allow(dead_code)]
#[path = "../bmd.rs"]
mod bmd;
#[path = "../pck.rs"]
mod pck;
use std::{collections::BTreeMap, path::Path};
fn main() -> Result<(), String> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: bmd-material-survey litmodels.pck")?;
    let mut f = pck::PackageFile::open(Path::new(&path)).map_err(|e| e.to_string())?;
    let entries = pck::directory(&mut f)?;
    let mut ver = BTreeMap::new();
    let mut sided = [0, 0];
    let mut diffuse_alpha = BTreeMap::new();
    let mut samples = 0;
    let mut failures = 0;
    for e in entries.iter().filter(|e| {
        e.name.to_lowercase().starts_with("litmodels/world/")
            && e.name.to_lowercase().ends_with(".bmd")
    }) {
        let bytes = pck::read_entry(&mut f, e)?;
        let model = match bmd::Model::parse(&bytes) {
            Ok(m) => m,
            Err(err) => {
                failures += 1;
                if failures <= 8 {
                    eprintln!("{}: {}", e.name, err);
                }
                continue;
            }
        };
        for m in model.meshes {
            *ver.entry(m.version).or_insert(0usize) += 1;
            if let Some(mat) = m.material {
                sided[mat.two_sided as usize] += 1;
                *diffuse_alpha
                    .entry(mat.diffuse[3].to_bits())
                    .or_insert(0usize) += 1;
                if samples < 8 {
                    println!("{} mesh={} material={:?}", e.name, m.name, mat);
                    samples += 1;
                }
            }
        }
    }
    println!("mesh versions={ver:?}, one/two sided={sided:?}, parse errors={failures}");
    println!(
        "diffuse alpha bit values: {:?}",
        diffuse_alpha
            .into_iter()
            .map(|(v, n)| (f32::from_bits(v), n))
            .collect::<Vec<_>>()
    );
    Ok(())
}
