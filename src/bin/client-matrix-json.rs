//! Emit one JSON file per client version describing what the readers can do.
//!
//! The job log is not readable from the agent's side, so a printed number is a
//! number nobody can check. This binary writes `ci-results/<client>.json`
//! instead: every cell the compatibility matrix needs, with the status word the
//! matrix uses (`ok`, `named-refusal`, `silent`, `absent`, `blocked`) and the
//! reason string when it is not `ok`.
//!
//! `silent` is the status that must never appear. It means a reader returned
//! nothing without saying why, which is the failure class this whole exercise
//! exists to eliminate: a map that draws no trees and prints no reason. It is
//! produced from what the reader actually returned -- a world that declares tree
//! types but yields no placements is `silent` even though the parse succeeded --
//! so the rule has teeth rather than being a status no code can set.
//!
//! Usage:
//!   client-matrix-json <out-dir> <client-name>=<client-root> [more...]
//!
//! A client root is a directory holding `element/` (or one that IS `element`)
//! plus whatever the client ships beside it.
// `water.rs` reports through `crate::diag!`, and a `#[path]` module set is its
// own crate root, so the stub supplies it. See `support/diag_stub.rs`.
#![allow(dead_code)]
#[macro_use]
#[path = "support/diag_stub.rs"]
mod diag_stub;
#[allow(unused_imports)]
pub use diag_stub::diagnostics;

#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[path = "../client_detect.rs"]
mod client_detect;
#[path = "../model_dds.rs"]
mod model_dds;
#[path = "../music.rs"]
mod music;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../terrain_block.rs"]
mod terrain_block;
#[path = "../terrain_lightmaps.rs"]
mod terrain_lightmaps;
#[path = "../water.rs"]
mod water;

use std::fs::File;
use std::path::{Path, PathBuf};

/// One row of the matrix: what a category did for this client.
struct Cell {
    category: &'static str,
    status: &'static str,
    detail: String,
}

impl Cell {
    fn ok(category: &'static str, detail: impl Into<String>) -> Self {
        Self {
            category,
            status: "ok",
            detail: detail.into(),
        }
    }
    fn refused(category: &'static str, detail: impl Into<String>) -> Self {
        Self {
            category,
            status: "named-refusal",
            detail: detail.into(),
        }
    }
    /// The file is not in this fixture. Deliberately *not* "this client ships
    /// none": a staged corpus is a subset of a client, so claiming a client
    /// lacks a file on the strength of a fixture would be the same
    /// overclaiming the matrix exists to prevent.
    fn absent(category: &'static str) -> Self {
        Self {
            category,
            status: "absent",
            detail: "not in this fixture (the fixture is a subset of the client)".into(),
        }
    }
    fn silent(category: &'static str, detail: impl Into<String>) -> Self {
        Self {
            category,
            status: "silent",
            detail: detail.into(),
        }
    }
}

/// A JSON string with the characters that would break the document escaped.
///
/// Written by hand rather than pulling in `serde`: this binary is compiled by a
/// job that has no network, and one escaping function is cheaper than a
/// dependency. Control characters become `\u00XX`, which is what serde does.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Every file under `dir`, recursively.
fn walk_all(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_all(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn walk(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, name, out);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
        {
            out.push(p);
        }
    }
}

/// The directory that holds `elementclient.exe`'s data: `<root>/element` when
/// the root is a client folder, or the root itself when it is already one.
fn element_of(root: &Path) -> PathBuf {
    if root.join("element").is_dir() {
        root.join("element")
    } else {
        root.to_path_buf()
    }
}

/// `world.ecwld`, wherever this client keeps it.
fn world_file(element: &Path) -> Option<PathBuf> {
    let mut all = Vec::new();
    walk(&element.join("maps"), "world.ecwld", &mut all);
    all.sort();
    // Prefer the shallowest path: `maps/world/world.ecwld`.
    all.into_iter().next()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.into_iter();
    let Some(out_dir) = it.next().map(PathBuf::from) else {
        eprintln!("usage: client-matrix-json <out-dir> <name>=<root> [...]");
        std::process::exit(2);
    };
    let clients: Vec<(String, PathBuf)> = it
        .filter_map(|a| {
            a.split_once('=')
                .map(|(n, p)| (n.to_string(), PathBuf::from(p)))
        })
        .collect();
    if clients.is_empty() {
        eprintln!("no clients given");
        std::process::exit(2);
    }
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let mut any_silent = false;
    for (name, root) in &clients {
        // One client must not be able to destroy the whole run. A panic inside a
        // reader would otherwise leave ci-results empty with no explanation,
        // which is exactly what happened to the first attempt at this step.
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| measure_client(name, root)));
        let (cells, facts) = match outcome {
            Ok(pair) => pair,
            Err(payload) => {
                let detail = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic with no message".into());
                eprintln!("{name}: PANIC while measuring: {detail}");
                // Node: recorded as a refusal, so the file still exists and the
                // failure is visible in the JSON rather than in a missing file.
                let cells = vec![Cell::refused(
                    "measurement",
                    format!("the tool panicked while reading this client: {detail}"),
                )];
                (cells, Vec::new())
            }
        };
        any_silent |= cells.iter().any(|c| c.status == "silent");
        write_client(&out_dir, name, root, &cells, &facts);
        let silent = cells.iter().filter(|c| c.status == "silent").count();
        println!(
            "{}: {} cells, {} ok, {} refused, {} absent, {} SILENT",
            name,
            cells.len(),
            cells.iter().filter(|c| c.status == "ok").count(),
            cells.iter().filter(|c| c.status == "named-refusal").count(),
            cells.iter().filter(|c| c.status == "absent").count(),
            silent
        );
    }
    if any_silent {
        eprintln!("SILENT cells present: a reader returned nothing without a reason");
        std::process::exit(1);
    }
}

/// Measure one client and return its cells and facts.
fn measure_client(name: &str, root: &Path) -> (Vec<Cell>, Vec<(&'static str, String)>) {
    let _ = name;
    {
        let element = element_of(root);
        let mut cells: Vec<Cell> = Vec::new();
        let mut facts: Vec<(&'static str, String)> = Vec::new();

        // --- the client's declared data version
        let elements_data = element.join("elements.data");
        if elements_data.is_file() {
            let bytes = std::fs::read(&elements_data).unwrap_or_default();
            match client_detect::ElementsHeader::parse(&bytes) {
                Some(h) => {
                    facts.push((
                        "elementsVersion",
                        format!("{}", h.version()),
                    ));
                    facts.push(("elementsWord", format!("{:#010x}", h.canonical())));
                    facts.push(("elementsFamily", format!("{:#06x}", h.family())));
                    cells.push(Cell::ok(
                        "elements data",
                        format!(
                            "v{} (family {:#06x}), timestamp {:#010x}",
                            h.version(),
                            h.family(),
                            h.timestamp
                        ),
                    ));
                }
                None => cells.push(Cell::refused(
                    "elements data",
                    "shorter than its 8-byte header",
                )),
            }
        } else {
            cells.push(Cell::absent("elements data"));
        }
        let tasks_data = element.join("tasks.data");
        if tasks_data.is_file() {
            let bytes = std::fs::read(&tasks_data).unwrap_or_default();
            let v = bytes
                .get(0..4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()));
            match v {
                Some(w) => cells.push(Cell::ok("tasks data", format!("version word {w}"))),
                None => cells.push(Cell::refused("tasks data", "shorter than 4 bytes")),
            }
        } else {
            cells.push(Cell::absent("tasks data"));
        }

        // --- the world file, which is where trees, terrain, water and refs live
        let world = world_file(&element);
        let mut scene_parsed = None;
        match &world {
            Some(path) => match scene::Scene::load(path) {
                Ok(s) => {
                    let trees: usize = s.blocks.iter().map(|b| b.trees.len()).sum();
                    let grass: usize = s.blocks.iter().map(|b| b.grass.len()).sum();
                    let water_refs: usize = s.blocks.iter().map(|b| b.water.len()).sum();
                    let models: usize = s.blocks.iter().map(|b| b.models.len()).sum();
                    let effects: usize = s.blocks.iter().map(|b| b.effects.len()).sum();
                    let sounds: usize = s.blocks.iter().map(|b| b.sounds.len()).sum();
                    facts.push(("ecwldBlocks", format!("{}", s.blocks.len())));
                    facts.push(("ecwldTreeTypes", format!("{}", s.tree_types.len())));
                    facts.push(("ecwldTreePlacements", format!("{trees}")));
                    facts.push(("ecwldGrassTypes", format!("{}", s.grass_types.len())));
                    facts.push(("ecwldGrassPlacements", format!("{grass}")));
                    facts.push(("ecwldWaterRefs", format!("{water_refs}")));
                    facts.push(("ecwldModels", format!("{models}")));
                    facts.push(("ecwldEffects", format!("{effects}")));
                    facts.push(("ecwldSounds", format!("{sounds}")));
                    cells.push(Cell::ok(
                        "world ECWLD",
                        format!(
                            "{} blocks, {}x{} grid, tree types {}",
                            s.blocks.len(),
                            s.rows,
                            s.cols,
                            s.tree_types.len()
                        ),
                    ));
                    // A zero tree count is exactly the symptom this matrix
                    // exists to find, so it may not be reported as `ok` on its
                    // own. It is honest only when the world declares no tree
                    // types at all (a64 and a91 do this): then there was nothing
                    // to place and nothing could have been lost.
                    if trees == 0 && !s.tree_types.is_empty() {
                        cells.push(Cell::silent(
                            "trees",
                            format!(
                                "the world declares {} tree types but carries 0 \
                                 placements",
                                s.tree_types.len()
                            ),
                        ));
                    } else {
                        cells.push(Cell::ok(
                            "trees",
                            format!(
                                "{} types, {trees} placements{}",
                                s.tree_types.len(),
                                if trees == 0 {
                                    " (the world declares zero tree types)"
                                } else {
                                    ""
                                }
                            ),
                        ));
                    }
                    cells.push(Cell::ok(
                        "grass",
                        format!("{} types, {grass} placements", s.grass_types.len()),
                    ));
                    scene_parsed = Some(s);
                }
                Err(e) => {
                    cells.push(Cell::refused("world ECWLD", e.clone()));
                    cells.push(Cell::refused("trees", "the world file did not parse"));
                    cells.push(Cell::refused("grass", "the world file did not parse"));
                }
            },
            None => {
                cells.push(Cell::absent("world ECWLD"));
                cells.push(Cell::absent("trees"));
                cells.push(Cell::absent("grass"));
            }
        }

        // --- water areas, which are BSData beside the maps
        match &scene_parsed {
            Some(s) => match water::load(s, &element) {
                Ok(areas) => {
                    cells.push(Cell::ok("water (BSData)", format!("{} areas", areas.len())))
                }
                Err(e) => cells.push(Cell::refused("water (BSData)", e)),
            },
            None => cells.push(Cell::refused(
                "water (BSData)",
                "cannot be looked for: the world file did not parse, and water \
                 is loaded relative to it",
            )),
        }

        // --- every package this client ships: the trailers and the counts
        let mut packs: Vec<PathBuf> = Vec::new();
        if element.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&element) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension()
                        .and_then(|x| x.to_str())
                        .is_some_and(|x| x.eq_ignore_ascii_case("pck"))
                    {
                        packs.push(p);
                    }
                }
            }
        }
        packs.sort();
        let mut trailing = Vec::new();
        let mut angelica = Vec::new();
        for pack in &packs {
            let short = pack
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            // The retail container first; its guard pair decides.
            match File::open(pack) {
                Ok(mut f) => match pck::directory(&mut f) {
                    Ok(entries) => {
                        trailing.push(format!("{}:retail:{}", esc(&short), entries.len()))
                    }
                    Err(retail_err) => {
                        // The second container, which shares the prefix but not
                        // the directory scheme.
                        match angelica_pck::directory(&mut f) {
                            Ok(entries) => {
                                angelica.push(format!("{}:angelica:{}", esc(&short), entries.len()))
                            }
                            Err(angelica_err) => trailing.push(format!(
                                "{}:refused:{}",
                                esc(&short),
                                esc(&format!("{retail_err}; also {angelica_err}"))
                            )),
                        }
                    }
                },
                Err(e) => trailing.push(format!(
                    "{}:open-failed:{}",
                    esc(&short),
                    esc(&e.to_string())
                )),
            }
        }
        if packs.is_empty() {
            cells.push(Cell::absent("package trailers (pck)"));
        } else {
            let refusals = trailing
                .iter()
                .filter(|s| s.contains(":refused:") || s.contains(":open-failed:"))
                .count();
            if refusals == 0 {
                cells.push(Cell::ok(
                    "package trailers (pck)",
                    format!("{} packs, all directories read", trailing.len()),
                ));
            } else {
                cells.push(Cell::refused(
                    "package trailers (pck)",
                    format!("{refusals} of {} packs refused", trailing.len()),
                ));
            }
            facts.push(("packs", trailing.join(" ")));
            if !angelica.is_empty() {
                facts.push(("angelicaPacks", angelica.join(" ")));
                cells.push(Cell::ok(
                    "package trailers (angelica)",
                    format!("{} packs read as the second container", angelica.len()),
                ));
            } else {
                cells.push(Cell::absent("package trailers (angelica)"));
            }
        }

        // --- GFX effects inside gfx.pck
        let gfx = element.join("gfx.pck");
        if gfx.is_file() {
            match File::open(&gfx) {
                Ok(mut f) => match pck::directory(&mut f) {
                    Ok(entries) => {
                        let roots = entries
                            .iter()
                            .filter(|e| e.name.to_lowercase().ends_with(".gfx"))
                            .count();
                        facts.push(("gfxEntries", format!("{}", entries.len())));
                        facts.push(("gfxRoots", format!("{roots}")));
                        cells.push(Cell::ok(
                            "GFX effects",
                            format!("{} entries, {roots} .gfx roots", entries.len()),
                        ));
                    }
                    Err(e) => cells.push(Cell::refused("GFX effects", e)),
                },
                Err(e) => cells.push(Cell::refused("GFX effects", e.to_string())),
            }
        } else {
            cells.push(Cell::absent("GFX effects"));
        }

        // --- DDS textures inside models.pck / litmodels.pck
        let mut dds_pack = None;
        for stem in ["models.pck", "litmodels.pck"] {
            let p = element.join(stem);
            if p.is_file() {
                dds_pack = Some(p);
                break;
            }
        }
        match dds_pack {
            Some(p) => match File::open(&p) {
                Ok(mut f) => match pck::directory(&mut f) {
                    Ok(entries) => {
                        let dds: Vec<_> = entries
                            .iter()
                            .filter(|e| e.name.to_lowercase().ends_with(".dds"))
                            .collect();
                        facts.push(("ddsEntries", format!("{}", dds.len())));
                        // Sample the formats: reading every texture would take
                        // minutes, and the cell only needs to know which formats
                        // are present and which the parser accepts.
                        let mut seen = std::collections::BTreeMap::new();
                        for e in dds.iter().take(400) {
                            match pck::read_entry(&mut f, e) {
                                Ok(bytes) => match model_dds::Dds::parse(&bytes) {
                                    Ok(d) => {
                                        *seen.entry(format!("{:?}", d.format)).or_insert(0) += 1
                                    }
                                    Err(err) => {
                                        let first = err.lines().next().unwrap_or("");
                                        *seen.entry(format!("refused: {first}")).or_insert(0) += 1
                                    }
                                },
                                Err(err) => {
                                    *seen.entry(format!("read-failed: {err}")).or_insert(0) += 1
                                }
                            }
                        }
                        let summary = seen
                            .iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join(",");
                        facts.push(("ddsFormats", esc(&summary)));
                        let refused = seen.keys().filter(|k| k.starts_with("refused:")).count();
                        if refused == 0 {
                            cells.push(Cell::ok(
                                "DDS textures",
                                format!("{} DDS entries, formats {summary}", dds.len()),
                            ));
                        } else {
                            cells.push(Cell::refused(
                                "DDS textures",
                                format!("{refused} format(s) refused: {summary}"),
                            ));
                        }
                    }
                    Err(e) => cells.push(Cell::refused("DDS textures", e)),
                },
                Err(e) => cells.push(Cell::refused("DDS textures", e.to_string())),
            },
            None => cells.push(Cell::absent("DDS textures")),
        }

        // --- music precinct
        let mut precincts = Vec::new();
        walk(&element.join("maps"), "precinct.clt", &mut precincts);
        precincts.sort();
        if precincts.is_empty() {
            cells.push(Cell::absent("music precinct"));
        } else {
            let mut ok = 0;
            let mut refused = Vec::new();
            let mut zone_total = 0usize;
            for p in &precincts {
                // `Zones::open` is the reader the application uses: it decodes
                // the UTF-16 file the client ships and then parses it.
                match music::Zones::open(p) {
                    Ok(v) => {
                        ok += 1;
                        zone_total += v.zones.len();
                    }
                    Err(e) => refused.push(format!("{}: {e}", p.display())),
                }
            }
            if facts.iter().all(|(k, _)| *k != "precinctZones") {
                facts.push(("precinctZones", format!("{zone_total}")));
            }
            if refused.is_empty() {
                cells.push(Cell::ok(
                    "music precinct",
                    format!("{ok} precinct.clt files"),
                ));
            } else {
                cells.push(Cell::refused("music precinct", refused.join("; ")));
            }
        }

        // --- sounds and zone names ride on the parsed scene
        match &scene_parsed {
            Some(s) => {
                let sounds: usize = s.blocks.iter().map(|b| b.sounds.len()).sum();
                cells.push(Cell::ok("sounds", format!("{sounds} authored sound refs")));
                cells.push(Cell::ok(
                    "zone names",
                    "the ECWLD carries no zone table; zone names are the precinct's \
                     (see the music precinct row)",
                ));
            }
            None => {
                cells.push(Cell::refused("sounds", "the world file did not parse"));
                cells.push(Cell::refused("zone names", "the world file did not parse"));
            }
        }

        // --- instances: every `.ecwld` under `maps/` except the world itself
        let mut ecwlds = Vec::new();
        walk_all(&element.join("maps"), &mut ecwlds);
        ecwlds.retain(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("ecwld"))
        });
        // The world map is reported on its own row; an instance row that
        // repeated it would double-count and hide a genuinely empty set.
        if let Some(w) = &world {
            ecwlds.retain(|p| p != w);
        }
        ecwlds.sort();
        ecwlds.dedup();
        let mut inst_ok = 0;
        let mut inst_trees = Vec::new();
        for p in &ecwlds {
            let stem = p
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            match scene::Scene::load(p) {
                Ok(s) => {
                    inst_ok += 1;
                    let trees: usize = s.blocks.iter().map(|b| b.trees.len()).sum();
                    inst_trees.push(format!(
                        "{stem}:{}types/{trees}trees",
                        s.tree_types.len()
                    ));
                }
                Err(e) => inst_trees.push(format!("{stem}:refused:{}", esc(&e))),
            }
        }
        if ecwlds.is_empty() {
            cells.push(Cell::absent("instances"));
        } else {
            let detail = inst_trees.join(" ");
            facts.push(("instances", detail.clone()));
            if inst_ok == ecwlds.len() {
                cells.push(Cell::ok("instances", detail));
            } else {
                cells.push(Cell::refused(
                    "instances",
                    format!(
                        "{} of {} instance maps refused: {detail}",
                        ecwlds.len() - inst_ok,
                        ecwlds.len()
                    ),
                ));
            }
        }

        // --- terrain T2BK and lightmaps T2LM
        //
        // These used to report `ok` from a file count alone, which says nothing:
        // a staged T2BK that the reader refuses and a staged T2BK that decodes
        // were the same cell. Read them, and let the reader decide.
        let mut t2bk = Vec::new();
        walk(&element.join("maps"), "t2bk", &mut t2bk);
        t2bk.sort();
        cells.push(if t2bk.is_empty() {
            Cell::absent("terrain T2BK")
        } else {
            let mut decoded = 0;
            let mut versions = std::collections::BTreeSet::new();
            let mut refusals = Vec::new();
            for path in &t2bk {
                match std::fs::read(path) {
                    Ok(bytes) => match terrain_block::trn2_header(&bytes) {
                        Ok(header) => {
                            decoded += 1;
                            versions.insert(format!("v{}", header.version));
                        }
                        Err(e) => refusals.push(format!("{}: {e}", path.display())),
                    },
                    Err(e) => refusals.push(format!("{}: {e}", path.display())),
                }
            }
            let seen: Vec<String> = versions.into_iter().collect();
            facts.push((
                "terrainVersions",
                if seen.is_empty() {
                    "none".to_string()
                } else {
                    seen.join(",")
                },
            ));
            facts.push(("terrainBlocks", format!("{decoded}")));
            if refusals.is_empty() {
                Cell::ok(
                    "terrain T2BK",
                    format!("{decoded} blocks decoded, versions {}", seen.join(",")),
                )
            } else {
                Cell::refused(
                    "terrain T2BK",
                    format!(
                        "{} of {} blocks refused: {}",
                        refusals.len(),
                        t2bk.len(),
                        refusals.join("; ")
                    ),
                )
            }
        });

        let mut t2lm = Vec::new();
        walk(&element.join("maps"), "t2lm", &mut t2lm);
        t2lm.sort();
        cells.push(if t2lm.is_empty() {
            Cell::absent("lightmaps T2LM")
        } else {
            let mut decoded = 0;
            let mut sides = std::collections::BTreeSet::new();
            let mut refusals = Vec::new();
            for path in &t2lm {
                match terrain_lightmaps::side_of(path) {
                    Ok(Some(side)) => {
                        decoded += 1;
                        sides.insert(side.to_string());
                    }
                    Ok(None) => refusals.push(format!("{}: no tile grid", path.display())),
                    Err(e) => refusals.push(format!("{}: {e}", path.display())),
                }
            }
            let seen: Vec<String> = sides.into_iter().collect();
            facts.push((
                "lightmapSides",
                if seen.is_empty() {
                    "none".to_string()
                } else {
                    seen.join(",")
                },
            ));
            facts.push(("lightmapTiles", format!("{decoded}")));
            if refusals.is_empty() {
                Cell::ok(
                    "lightmaps T2LM",
                    format!(
                        "{decoded} lightmaps decoded, tile sides {}",
                        seen.join(",")
                    ),
                )
            } else {
                Cell::refused(
                    "lightmaps T2LM",
                    format!(
                        "{} of {} lightmaps refused: {}",
                        refusals.len(),
                        t2lm.len(),
                        refusals.join("; ")
                    ),
                )
            }
        });

        (cells, facts)
    }
}

/// Write one client's JSON, and say what it holds.
fn write_client(
    out_dir: &Path,
    name: &str,
    root: &Path,
    cells: &[Cell],
    facts: &[(&'static str, String)],
) {
    let mut doc = String::new();
    doc.push_str("{\n");
    doc.push_str(&format!("  \"client\": \"{}\",\n", esc(name)));
    doc.push_str(&format!(
        "  \"root\": \"{}\",\n",
        esc(&root.display().to_string())
    ));
    doc.push_str("  \"facts\": {");
    for (i, (k, v)) in facts.iter().enumerate() {
        if i > 0 {
            doc.push(',');
        }
        let value = if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '-') {
            v.clone()
        } else {
            format!("\"{}\"", esc(v))
        };
        doc.push_str(&format!("\n    \"{k}\": {value}"));
    }
    doc.push_str("\n  },\n  \"cells\": [\n");
    for (i, c) in cells.iter().enumerate() {
        doc.push_str(&format!(
            "    {{\"category\": \"{}\", \"status\": \"{}\", \"detail\": \"{}\"}}{}\n",
            esc(c.category),
            c.status,
            esc(&c.detail),
            if i + 1 < cells.len() { "," } else { "" }
        ));
    }
    doc.push_str("  ]\n}\n");
    let path = out_dir.join(format!("{name}.json"));
    std::fs::write(&path, &doc).expect("write json");
    println!("  wrote {}", path.display());
}
