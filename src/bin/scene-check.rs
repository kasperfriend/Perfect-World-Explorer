#[allow(dead_code)]
#[path = "../pck.rs"]
mod pck;
#[allow(dead_code)]
#[path = "../scene.rs"]
mod scene;
use std::{collections::HashSet, env, fs::File, path::Path};
fn run() -> Result<(), String> {
    let a: Vec<_> = env::args().collect();
    if a.len() != 2 {
        return Err("usage: scene-check <client-root>".into());
    }
    let root = Path::new(&a[1]);
    let element = if root.join("element").is_dir() {
        root.join("element")
    } else {
        root.to_path_buf()
    };
    let scene = scene::Scene::load(&element.join("maps/world/world.ecwld"))?;
    let mut n = [0usize; 10];
    for b in &scene.blocks {
        for (i, c) in [
            b.trees.len(),
            b.water.len(),
            b.grass.len(),
            b.ornaments.len(),
            b.boxes.len(),
            b.effects.len(),
            b.models.len(),
            b.critters.len(),
            b.beziers.len(),
            b.sounds.len(),
        ]
        .iter()
        .enumerate()
        {
            n[i] += c
        }
    }
    println!(
        "ECWLD v12: {} x {} blocks, {} tree types, {} grass types",
        scene.rows,
        scene.cols,
        scene.tree_types.len(),
        scene.grass_types.len()
    );
    println!("Trees {} water refs {} grass refs {} ornaments {} boxes {} effects {} models {} critters {} bezier refs {} sounds {}",n[0],n[1],n[2],n[3],n[4],n[5],n[6],n[7],n[8],n[9]);
    let ornaments = scene.load_ornaments(&element.join("maps/world/world.ecbsd"))?;
    let mut lit =
        pck::PackageFile::open(&element.join("litmodels.pck")).map_err(|e| e.to_string())?;
    let litset: HashSet<String> = pck::directory(&mut lit)?
        .into_iter()
        .map(|e| e.name.to_lowercase())
        .collect();
    let found = ornaments
        .iter()
        .filter(|o| litset.contains(&format!("litmodels/world/{}", o.path.to_lowercase())))
        .count();
    println!(
        "Unique EC-BSD placed ornaments: {}; resolved in litmodels.pck: {found}/{}",
        ornaments.len(),
        ornaments.len()
    );
    for o in ornaments
        .iter()
        .filter(|o| !litset.contains(&format!("litmodels/world/{}", o.path.to_lowercase())))
        .take(5)
    {
        eprintln!("missing BMD {}", o.path);
    }
    for (archive, paths) in [
        (
            "trees.pck",
            scene
                .tree_types
                .iter()
                .flat_map(|t| [t.tree.as_str(), t.composite.as_str()])
                .collect::<Vec<_>>(),
        ),
        (
            "grasses.pck",
            scene.grass_types.iter().map(|g| g.path.as_str()).collect(),
        ),
        (
            "models.pck",
            scene
                .blocks
                .iter()
                .flat_map(|b| b.models.iter().map(|m| m.path.as_str()))
                .collect(),
        ),
        (
            "gfx.pck",
            scene
                .blocks
                .iter()
                .flat_map(|b| b.effects.iter().map(|m| m.path.as_str()))
                .collect(),
        ),
    ] {
        let mut f = File::open(element.join(archive)).map_err(|e| e.to_string())?;
        let entries = pck::directory(&mut f)?;
        let set: HashSet<String> = entries.into_iter().map(|x| x.name.to_lowercase()).collect();
        let resolves = |x: &&str| {
            let name = x.to_lowercase();
            set.contains(&name) || (archive == "gfx.pck" && set.contains(&format!("gfx/{name}")))
        };
        let matched = paths.iter().filter(|x| resolves(x)).count();
        println!(
            "{archive}: {matched}/{} scene path references resolved",
            paths.len()
        );
        for path in paths.iter().filter(|x| !resolves(x)).take(5) {
            eprintln!("not resolved in {archive}: {path}");
        }
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
