//! Extraction shared by viewer automatic setup and diagnostics.
use crate::{pck, scene};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}
pub fn extract(root: &Path, out: &Path) -> Result<(), String> {
    let element = if root.join("element").is_dir() {
        root.join("element")
    } else {
        root.to_path_buf()
    };
    extract_for_scene(&element, &element.join("maps/world/world.ecwld"), out, 59)
}

/// Extract the tree assets referenced by one selected map's ECWLD.
///
/// `source_count` may exceed the authored table: the legacy checked-in bridge
/// always loops 59 types, so instance maps with fewer types are padded with
/// repeated source files. `Trees::open` reads only the scene's authored types.
pub fn extract_for_scene(
    element: &Path,
    scene_path: &Path,
    out: &Path,
    source_count: usize,
) -> Result<(), String> {
    if out.exists() {
        return Err("output path already exists; choose a new directory".into());
    }
    if source_count > 10000 {
        return Err("tree source count exceeds the SpeedTree bridge limit".into());
    }
    let scene = scene::Scene::load(scene_path)?;
    if source_count > 0 && scene.tree_types.is_empty() {
        return Err(
            "scene declares no tree types but a non-zero bridge source count was requested".into(),
        );
    }
    let mut pkg = File::open(element.join("trees.pck")).map_err(|e| e.to_string())?;
    let entries: HashMap<_, _> = pck::directory(&mut pkg)?
        .into_iter()
        .map(|e| (e.name.to_lowercase(), e))
        .collect();
    // Check all paths before creating files; never derive an output path from a client-provided name.
    for t in &scene.tree_types {
        for name in [&t.tree, &t.composite] {
            if !entries.contains_key(&name.to_lowercase()) {
                return Err(format!("tree source asset missing: {name}"));
            }
        }
    }
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let mut manifest =
        BufWriter::new(File::create(out.join("tree-types.tsv")).map_err(|e| e.to_string())?);
    writeln!(
        manifest,
        "type_id\tspt_file\tcomposite_file\tclient_spt_path\tclient_composite_path"
    )
    .map_err(|e| e.to_string())?;
    for i in 0..source_count {
        let t = &scene.tree_types[i % scene.tree_types.len()];
        let spt = format!("tree-{i:03}.spt");
        let dds = format!("tree-{i:03}.dds");
        for (name, dst) in [(&t.tree, &spt), (&t.composite, &dds)] {
            let entry = &entries[&name.to_lowercase()];
            let data = pck::read_entry(&mut pkg, entry)?;
            fs::write(out.join(dst), data).map_err(|e| e.to_string())?;
        }
        writeln!(
            manifest,
            "{i}\t{spt}\t{dds}\t{}\t{}",
            escape(&t.tree),
            escape(&t.composite)
        )
        .map_err(|e| e.to_string())?;
    }
    let mut placements =
        BufWriter::new(File::create(out.join("tree-placements.tsv")).map_err(|e| e.to_string())?);
    writeln!(placements, "type_id\tx\ty\tz\tblock_row\tblock_col").map_err(|e| e.to_string())?;
    let mut count = 0;
    for block in &scene.blocks {
        for tree in &block.trees {
            writeln!(
                placements,
                "{}\t{}\t{}\t{}\t{}\t{}",
                tree.tree_type,
                tree.position[0],
                tree.position[1],
                tree.position[2],
                block.row,
                block.col
            )
            .map_err(|e| e.to_string())?;
            count += 1;
        }
    }
    manifest.flush().map_err(|e| e.to_string())?;
    placements.flush().map_err(|e| e.to_string())?;
    println!(
        "Extracted {source_count} SpeedTree sources and composite maps from {} authored types and {count} world placements",
        scene.tree_types.len()
    );
    Ok(())
}
