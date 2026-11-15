#[allow(dead_code)]
#[path = "../pck.rs"]
mod pck;
use std::{collections::BTreeMap, env, path::Path};
fn u(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4).ok_or("short BMD")?.try_into().unwrap(),
    ))
}
fn run() -> Result<(), String> {
    let a: Vec<_> = env::args().collect();
    if a.len() != 2 {
        return Err("usage: bmd-survey client-root".into());
    }
    let root = Path::new(&a[1]);
    let element = if root.join("element").is_dir() {
        root.join("element")
    } else {
        root.to_path_buf()
    };
    let mut f = pck::PackageFile::open(&element.join("litmodels.pck"))
        .map_err(|e| e.to_string())?;
    let entries = pck::directory(&mut f)?;
    let mut model_versions = BTreeMap::new();
    let mut mesh_versions = BTreeMap::new();
    let mut files = 0;
    for e in entries.iter().filter(|e| {
        e.name.to_lowercase().starts_with("litmodels/world/")
            && e.name.to_lowercase().ends_with(".bmd")
    }) {
        let data = pck::read_entry(&mut f, e)?;
        if u(&data, 0)? != 0x42584f4d {
            return Err(format!("{}: bad afile header", e.name));
        }
        let mut base = 4;
        let mut version = u(&data, base)?;
        if version == 0x80000001 {
            base += 5;
            version = u(&data, base)?;
        }
        *model_versions.entry(version).or_insert(0usize) += 1;
        let nv = u(&data, base + 52)?;
        if nv > 0 && nv < 10000 {
            *mesh_versions.entry(u(&data, base + 56)?).or_insert(0usize) += 1;
        }
        files += 1;
    }
    println!("{files} world BMD files");
    for (v, n) in model_versions {
        println!("model version {v:#010x}: {n}")
    }
    for (v, n) in mesh_versions {
        println!("first mesh version {v:#010x}: {n}")
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
