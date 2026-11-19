//! Read-only census of ECWLD placements and the authored GFX graphs they name.
//! Not playback: no model-event roots, runtime pruning, texture binding,
//! activation, frustum culling or rendering. Verify retail input hashes first.
#![allow(dead_code)]
#[path = "../gfx_file.rs"]
mod gfx_file;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../water.rs"]
mod water;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::File,
    path::Path,
};

#[derive(Clone)]
enum Root {
    Readable {
        warp: usize,
        dest_src: usize,
        dest_dst: usize,
        children: Vec<String>,
    },
    Unreadable,
    Rejected,
    Unsupported,
}

fn root(file: &mut File, entry: Option<&pck::Entry>) -> Root {
    let Some(bytes) = entry.and_then(|entry| pck::read_entry(file, entry).ok()) else {
        return Root::Unreadable;
    };
    let (text, bad) = gfx_file::decode(&bytes);
    if !text.starts_with("MOXT") {
        return Root::Unsupported;
    }
    if bad {
        return Root::Rejected;
    }
    let Ok(root) = gfx_file::parse(&text) else {
        return Root::Rejected;
    };
    let mut counts = [0; 3];
    let mut children = Vec::new();
    for element in root.elements {
        if let gfx_file::Spec::Child(child) = element.spec {
            children.push(child.path);
        }
        let common = element.common;
        counts[0] += usize::from(common.warp);
        counts[1] += usize::from(matches!(common.source_blend, 9 | 10));
        counts[2] += usize::from(matches!(common.destination_blend, 9 | 10));
    }
    let [warp, dest_src, dest_dst] = counts;
    Root::Readable {
        warp,
        dest_src,
        dest_dst,
        children,
    }
}

#[derive(Default)]
struct Counts {
    outcomes: BTreeMap<&'static str, usize>,
    elements: [usize; 3],
}

impl Counts {
    fn record(&mut self, outcome: &'static str, copies: usize) {
        *self.outcomes.entry(outcome).or_default() += copies;
    }

    fn root(&mut self, root: &Root, copies: usize) {
        let outcome = match root {
            Root::Readable {
                warp,
                dest_src,
                dest_dst,
                ..
            } => {
                for (total, count) in self.elements.iter_mut().zip([warp, dest_src, dest_dst]) {
                    *total += count * copies;
                }
                "readable"
            }
            Root::Unreadable => "unreadable",
            Root::Rejected => "rejected",
            Root::Unsupported => "unsupported_container",
        };
        self.record(outcome, copies);
    }

    fn get(&self, outcome: &str) -> usize {
        self.outcomes.get(outcome).copied().unwrap_or_default()
    }
}

struct Graph<F> {
    fetch: F,
    cache: HashMap<String, Root>,
    paths: BTreeSet<String>,
    direct: Counts,
    descendants: Counts,
    remaining_edges: usize,
}

impl<F: FnMut(&str) -> Root> Graph<F> {
    fn new(fetch: F) -> Self {
        Self {
            fetch,
            cache: HashMap::new(),
            paths: BTreeSet::new(),
            direct: Counts::default(),
            descendants: Counts::default(),
            remaining_edges: 100_000,
        }
    }

    fn lookup(&mut self, path: &str) -> Root {
        self.cache
            .entry(path.into())
            .or_insert_with(|| (self.fetch)(path))
            .clone()
    }

    fn root(&mut self, name: &str, copies: usize) -> Result<(), String> {
        let root = self.lookup(name);
        self.direct.root(&root, copies);
        self.walk(root, copies, &mut vec![name.into()])
    }

    fn walk(&mut self, root: Root, copies: usize, stack: &mut Vec<String>) -> Result<(), String> {
        let Root::Readable { children, .. } = root else {
            return Ok(());
        };
        for child in children {
            self.remaining_edges = self
                .remaining_edges
                .checked_sub(1)
                .ok_or("authored GFX graph audit edge limit exceeded")?;
            if stack.len() > gfx_file::MAX_CHILD_DEPTH {
                self.descendants.record("depth_limit", copies);
                continue;
            }
            let Ok(path) = gfx_file::child_member_path(&child) else {
                self.descendants.record("invalid_path", copies);
                continue;
            };
            if stack.contains(&path) {
                self.descendants.record("cycle", copies);
                continue;
            }
            let nested = self.lookup(&path);
            self.paths.insert(path.clone());
            self.descendants.root(&nested, copies);
            stack.push(path);
            self.walk(nested, copies, stack)?;
            stack.pop();
        }
        Ok(())
    }
}

fn audit(dir: &Path, file: &mut File, entries: &HashMap<String, pck::Entry>) -> Result<(), String> {
    let id = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid map directory")?;
    let scene = scene::Scene::load(&dir.join(format!("{id}.ecwld")))?;
    let ornaments = scene.load_ornaments(&dir.join(format!("{id}.ecbsd")))?;
    let water = water::load(&scene, dir)?;
    let grass_refs = scene
        .blocks
        .iter()
        .flat_map(|b| &b.grass)
        .map(|r| r.export_id)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let actors: usize = scene.blocks.iter().map(|b| b.models.len()).sum();
    let mut placements = BTreeMap::new();
    for placement in scene.blocks.iter().flat_map(|b| &b.effects) {
        let key = placement.path.replace('\\', "/").to_lowercase();
        let key = if key.starts_with("gfx/") {
            key
        } else {
            format!("gfx/{key}")
        };
        *placements.entry(key).or_default() += 1;
    }
    let mut graph = Graph::new(|path| root(file, entries.get(path)));
    for (key, &count) in &placements {
        graph.root(key, count)?;
    }
    let [warp, dest_src, dest_dst] = graph.direct.elements;
    let [child_warp, child_src, child_dst] = graph.descendants.elements;
    println!("{id}: ECWLD/EC-BSD 12/12, blocks={}, ornaments={}, grass_types={}, grass_refs={grass_refs}, actors={actors}, water_areas={}",
        scene.blocks.len(), ornaments.len(), scene.grass_types.len(), water.len());
    println!("  placed_effect_roots={} unique_direct_paths={} readable={} unreadable={} rejected={} unsupported_container={}",
        placements.values().sum::<usize>(), placements.len(), graph.direct.get("readable"), graph.direct.get("unreadable"),
        graph.direct.get("rejected"), graph.direct.get("unsupported_container"));
    println!("  authored_per_placement warp_elements={warp} dest_color_source_elements={dest_src} dest_color_destination_elements={dest_dst}");
    println!(
        "  authored_child_refs_per_placement={} unique_descendant_paths={} outcomes={:?}",
        graph.descendants.outcomes.values().sum::<usize>(),
        graph.paths.len(),
        graph.descendants.outcomes
    );
    println!("  authored_child_elements_per_placement warp={child_warp} dest_color_source={child_src} dest_color_destination={child_dst}");
    for area in water {
        let wet = area.mask.iter().filter(|&&cell| cell != 0).count();
        println!("  water {}: height={} center={:?} grid={}x{} pitch={} wet_cells={wet} simple={} shore_waves={} center_wet={}",
            area.export_id, area.height, area.center, area.columns, area.rows, area.grid_size,
            area.simple, area.shore_waves.len(), area.contains_xz(area.center[0], area.center[2]));
    }
    Ok(())
}

fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: lighter-scene-audit GFX.PCK MAP_DIRECTORY [MAP_DIRECTORY ...]";
    let pack = args.next().ok_or(usage)?;
    let dirs: Vec<_> = args.collect();
    if dirs.is_empty() {
        return Err(usage.into());
    }
    let mut file = File::open(&pack).map_err(|e| e.to_string())?;
    let entries: HashMap<_, _> = pck::directory(&mut file)?
        .into_iter()
        .map(|e| (e.name.replace('\\', "/").to_lowercase(), e))
        .collect();
    println!(
        "Read-only authored census: placed text roots and child references, no playback, material admission or visibility"
    );
    for dir in dirs {
        audit(Path::new(&dir), &mut file, &entries)?;
    }
    Ok(())
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn weighted_graph_counts_children_and_stops_cycles_and_bad_paths() {
        let text = |warp, children: &[&str]| Root::Readable {
            warp,
            dest_src: 0,
            dest_dst: 0,
            children: children.iter().map(|s| s.to_string()).collect(),
        };
        let mut graph = Graph::new(|path: &str| match path {
            "gfx/root.gfx" => text(
                1,
                &["Child.GFX", "gfx/child.gfx", "missing.gfx", "../unsafe.gfx"],
            ),
            "gfx/child.gfx" => text(2, &["root.gfx", "label.gfx"]),
            "gfx/label.gfx" => Root::Unsupported,
            _ => Root::Unreadable,
        });
        graph.root("gfx/root.gfx", 3).unwrap();
        assert_eq!(graph.direct.elements, [3, 0, 0]);
        assert_eq!(graph.descendants.elements, [12, 0, 0]);
        assert_eq!(graph.descendants.get("readable"), 6);
        assert_eq!(graph.descendants.get("cycle"), 6);
        assert_eq!(graph.descendants.get("unsupported_container"), 6);
        assert_eq!(graph.descendants.get("unreadable"), 3);
        assert_eq!(graph.descendants.get("invalid_path"), 3);
        assert_eq!(graph.paths.len(), 3);
    }
}
