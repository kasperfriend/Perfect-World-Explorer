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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The staged fixtures: a real 1.8.4 `world.ecwld` and a `trees.pck` built
    /// from that client's own bytes.
    ///
    /// `trees.pck` is a real package: the client's own 12-byte prefix, the real
    /// stored payload bytes of every entry the world's tree table names, the
    /// real masked entry records, and the real 280-byte trailer with only
    /// `dwEntryOffset` patched to the shorter table. Nothing about the format is
    /// re-implemented here, so a change to the trailer layout breaks this test.
    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/trees-184")
            .join(name)
    }

    fn have_fixtures() -> bool {
        let p = fixture("trees.pck");
        let q = fixture("world.ecwld");
        if p.is_file() && q.is_file() {
            return true;
        }
        crate::pck::corpus_file_absent(&p);
        false
    }

    /// 1.1 — the reader already folds backslashes, so the old lookup missed.
    ///
    /// This is the check that decides whether the previous diagnosis was right.
    /// `pck::directory` decodes each record's `szFileName[260]` through GBK and
    /// then replaces `\` with `/` (`src/pck.rs`, the line that builds `Entry`),
    /// so every name it returns is already forward-slashed. The `.ecwld` tree
    /// table, however, stores **backslashes** — all 59 of the 1.8.4 world's
    /// names do. So the two sides agreed on nothing: the pack said
    /// `trees/精简树木/雪松/…` and the old lookup, which only lower-cased, asked
    /// for `trees\精简树木\雪松\…`.
    ///
    /// Both halves are asserted here against real bytes, not against a string
    /// written twice.
    #[test]
    fn the_pack_returns_forward_slashes_while_the_world_stores_backslashes() {
        if !have_fixtures() {
            return;
        }
        // The pack, through the real reader.
        let mut file = File::open(fixture("trees.pck")).unwrap();
        let entries = crate::pck::directory(&mut file).unwrap();
        assert_eq!(entries.len(), 53, "the fixture's entry count");
        assert!(
            entries.iter().all(|e| !e.name.contains('\\')),
            "pck::directory must return no backslash: {:?}",
            entries
                .iter()
                .find(|e| e.name.contains('\\'))
                .map(|e| &e.name)
        );
        let snowpine = entries
            .iter()
            .find(|e| e.name.ends_with("snowpinewithsnow1.spt"))
            .expect("the snow pine the world's first tree type names");
        assert_eq!(snowpine.name, "trees/精简树木/雪松/snowpinewithsnow1.spt");

        // The world, through the real reader.
        let scene = crate::scene::Scene::load(&fixture("world.ecwld")).unwrap();
        assert_eq!(scene.tree_types.len(), 59, "the 1.8.4 world's tree table");
        assert!(
            scene.tree_types.iter().all(|t| t.tree.contains('\\')),
            "the .ecwld stores backslashes, which is the other half of the bug"
        );

        // The bug, stated as the old code computed it: lower-case only leaves
        // the separator alone, so the two sides never met.
        let old_key = scene.tree_types[0].tree.to_lowercase();
        assert!(
            !entries.iter().any(|e| e.name.to_lowercase() == old_key),
            "the pre-fix lookup must miss {old_key:?}"
        );

        // And the fix's rule makes them meet.
        let wanted = scene::normalise_tree_name(&scene.tree_types[0].tree);
        assert!(
            entries.iter().any(|e| e.name.to_lowercase() == wanted),
            "after normalising, the first tree type resolves"
        );
    }

    /// 1.2 — end to end: every authored type resolves and files are written.
    ///
    /// This is the test the previous pass lacked. It calls the real
    /// `extract_for_scene` on a real world file and a real pack, and requires
    /// that all 59 tree types resolve to real bytes and that the manifest and
    /// the placement table come out non-empty. A failure here is the actual
    /// "no trees" failure, reproduced without a client install.
    #[test]
    fn every_authored_tree_type_resolves_and_is_written() {
        if !have_fixtures() {
            return;
        }
        // `extract_for_scene` expects `<element>/trees.pck` and the scene path.
        let element = std::env::temp_dir().join(format!(
            "pw-treesrc-e2e-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&element);
        fs::create_dir_all(&element).unwrap();
        fs::copy(fixture("trees.pck"), element.join("trees.pck")).unwrap();
        let out = element.join("out");

        let scene_path = fixture("world.ecwld");
        let result = extract_for_scene(&element, &scene_path, &out, 59);
        let cleanup = || {
            let _ = fs::remove_dir_all(&element);
        };
        if let Err(e) = result {
            cleanup();
            panic!("extract_for_scene failed on real 1.8.4 bytes: {e}");
        }

        // One `.spt` and one `.dds` per source, plus the two tables.
        for i in 0..59 {
            let spt = out.join(format!("tree-{i:03}.spt"));
            let dds = out.join(format!("tree-{i:03}.dds"));
            assert!(spt.is_file(), "missing {spt:?}");
            assert!(dds.is_file(), "missing {dds:?}");
            assert!(
                fs::metadata(&spt).unwrap().len() > 0,
                "{spt:?} was written empty"
            );
        }
        let manifest = fs::read_to_string(out.join("tree-types.tsv")).unwrap();
        assert_eq!(
            manifest.lines().count(),
            1 + 59,
            "a header plus one line per type"
        );
        let placements = fs::read_to_string(out.join("tree-placements.tsv")).unwrap();
        assert_eq!(
            placements.lines().count(),
            1 + 3_649,
            "the 1.8.4 world's authored tree placements"
        );
        cleanup();
    }
}
