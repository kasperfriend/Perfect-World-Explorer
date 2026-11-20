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
/// Upper bound on the tree source count handed to the SpeedTree bridge.
///
/// See the call site: the measured maximum across staged clients is 59 tree
/// types, and this is a guard against a mistyped count.
const TREE_SOURCE_LIMIT: usize = 10_000;

pub fn extract_for_scene(
    element: &Path,
    scene_path: &Path,
    out: &Path,
    source_count: usize,
) -> Result<(), String> {
    if out.exists() {
        return Err("output path already exists; choose a new directory".into());
    }
    // The bridge's own bound, not a measured one: the largest tree table in any
    // staged client is 59 types (`windows_setup::LEGACY_BRIDGE_TREE_TYPES` says
    // why 59 is the padded size), so a caller asking for more than 10,000 is
    // asking the DLL to bake a table no client authors. The number is two
    // orders of magnitude above the measured maximum and exists to reject a
    // mistyped count, not to police a newer client.
    if source_count > TREE_SOURCE_LIMIT {
        return Err(format!(
            "tree source count {source_count} exceeds the SpeedTree bridge limit \
             {TREE_SOURCE_LIMIT}"
        ));
    }
    let scene = scene::Scene::load(scene_path)?;
    if source_count > 0 && scene.tree_types.is_empty() {
        return Err(
            "scene declares no tree types but a non-zero bridge source count was requested".into(),
        );
    }
    let mut pkg = File::open(element.join("trees.pck")).map_err(|e| e.to_string())?;
    // Both sides go through `scene::normalise_tree_name`, which folds `\\` to
    // `/` as well as lower-casing. Lower-casing alone was a real defect: every
    // shipped `trees.pck` stores backslash paths (`trees\\精简树木\\雪松\\x.spt`)
    // while every `.ecwld` writes forward slashes (`Trees/精简树木/雪松/x.spt`),
    // so `contains_key` was false for every asset and preparation aborted with
    // "tree source asset missing" -- no cache, therefore no trees, in every
    // world and every client that stores backslash names.
    let entries: HashMap<_, _> = pck::directory(&mut pkg)?
        .into_iter()
        .map(|e| (scene::normalise_tree_name(&e.name), e))
        .collect();
    // Check all paths before creating files; never derive an output path from a client-provided name.
    for t in &scene.tree_types {
        for name in [&t.tree, &t.composite] {
            if !entries.contains_key(&scene::normalise_tree_name(name)) {
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
            let entry = &entries[&scene::normalise_tree_name(name)];
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

