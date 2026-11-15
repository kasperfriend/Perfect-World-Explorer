#[allow(dead_code)]
#[path = "../bmd.rs"]
mod bmd;
#[allow(dead_code)]
#[path = "../pck.rs"]
mod pck;
use std::{collections::BTreeMap, env, path::Path};
fn run() -> Result<(), String> {
    let a: Vec<_> = env::args().collect();
    if a.len() != 2 {
        return Err("usage: bmd-check client-root".into());
    }
    let r = Path::new(&a[1]);
    let root = if r.join("element").is_dir() {
        r.join("element")
    } else {
        r.to_path_buf()
    };
    let mut f = pck::PackageFile::open(&root.join("litmodels.pck"))
        .map_err(|e| e.to_string())?;
    let entries = pck::directory(&mut f)?;
    let mut ok = 0;
    let mut fail = 0;
    let mut examples = Vec::new();
    let mut vertex_count = 0usize;
    let mut tex = BTreeMap::new();
    for e in entries.iter().filter(|e| {
        e.name.to_lowercase().starts_with("litmodels/world/")
            && e.name.to_lowercase().ends_with(".bmd")
    }) {
        let data = pck::read_entry(&mut f, e)?;
        match bmd::Model::parse(&data) {
            Ok(m) => {
                ok += 1;
                for mesh in m.meshes {
                    vertex_count += mesh.vertices.len();
                    *tex.entry(mesh.texture).or_insert(0usize) += 1;
                }
            }
            Err(err) => {
                fail += 1;
                if examples.len() < 10 {
                    examples.push(format!("{}: {err}", e.name))
                }
            }
        }
    }
    println!(
        "BMD decoded {ok}, failed {fail}, vertices {vertex_count}, unique texture paths {}",
        tex.len()
    );
    let mut texture_file = pck::PackageFile::open(&root.join("building.pck"))
        .map_err(|e| e.to_string())?;
    let paths: std::collections::HashSet<String> = pck::directory(&mut texture_file)?
        .into_iter()
        .map(|e| e.name.to_lowercase())
        .collect();
    let found = tex
        .keys()
        .filter(|p| p.is_empty() || paths.contains(&p.to_lowercase()))
        .count();
    println!(
        "building.pck matches {found}/{} unique texture names (including empty)",
        tex.len()
    );
    for name in tex
        .keys()
        .filter(|p| !p.is_empty() && !paths.contains(&p.to_lowercase()))
        .take(10)
    {
        eprintln!("missing texture {name}");
    }
    for e in examples {
        eprintln!("{e}")
    }
    if fail > 0 {
        return Err("some models could not be decoded".into());
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
