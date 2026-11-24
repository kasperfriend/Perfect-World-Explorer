//! Client Asset Explorer & Quick Bar pinning for Architect.
//!
//! Scans the client's `.pck` archives (`litmodels.pck`, `models.pck`,
//! `trees.pck`, `grasses.pck`, `gfx.pck`, `sfx.pck`, `textures.pck`,
//! `building.pck`, etc.) and loose directories on a background thread so
//! Architect opens instantaneously, presenting everything as a clean
//! expandable folder tree with right-click "Pin to quick bar" support.

use super::world::ObjectKind;
use crate::{angelica_pck, pck};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Seek,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
};

/// One node in the Client Asset Explorer folder tree.
#[derive(Clone, Debug)]
pub struct AssetNode {
    pub name: String,
    pub full_path: String,
    pub archive: Option<String>,
    pub is_dir: bool,
    pub kind: Option<ObjectKind>,
    pub children: Vec<AssetNode>,
}

impl AssetNode {
    pub fn folder(name: String, full_path: String, archive: Option<String>) -> Self {
        Self {
            name,
            full_path,
            archive,
            is_dir: true,
            kind: None,
            children: Vec::new(),
        }
    }

    pub fn file(
        name: String,
        full_path: String,
        archive: Option<String>,
        kind: ObjectKind,
    ) -> Self {
        Self {
            name,
            full_path,
            archive,
            is_dir: false,
            kind: Some(kind),
            children: Vec::new(),
        }
    }

    pub fn total_files(&self) -> usize {
        if !self.is_dir {
            1
        } else {
            self.children.iter().map(Self::total_files).sum()
        }
    }
}

/// An item (object or folder) pinned to the bottom Quick Bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickBarPin {
    pub label: String,
    pub full_path: String,
    pub archive: Option<String>,
    pub is_dir: bool,
    pub kind: Option<ObjectKind>,
}

/// Filter category in the Client Asset Explorer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetFilter {
    All,
    Buildings,
    Models,
    Trees,
    Effects,
    Audio,
    Primitives,
}

impl AssetFilter {
    pub const ALL: [Self; 7] = [
        Self::All,
        Self::Buildings,
        Self::Models,
        Self::Trees,
        Self::Effects,
        Self::Audio,
        Self::Primitives,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Buildings => "BMD",
            Self::Models => "ECM",
            Self::Trees => "Trees",
            Self::Effects => "GFX",
            Self::Audio => "SFX",
            Self::Primitives => "Built-in",
        }
    }

    pub fn matches(self, kind: ObjectKind) -> bool {
        match self {
            Self::All => true,
            Self::Buildings => kind == ObjectKind::BuildingBmd,
            Self::Models => kind == ObjectKind::ModelEcm,
            Self::Trees => kind == ObjectKind::Tree || kind == ObjectKind::Grass,
            Self::Effects => kind == ObjectKind::EffectGfx || kind == ObjectKind::Light,
            Self::Audio => kind == ObjectKind::SoundSfx,
            Self::Primitives => kind == ObjectKind::Primitive || kind == ObjectKind::Water,
        }
    }
}

/// Owns the scanned client asset tree, search/filter state, and Quick Bar pins.
pub struct ClientExplorer {
    pub roots: Vec<AssetNode>,
    pub scanning: bool,
    pub total_assets: usize,
    pub search: String,
    pub filter: AssetFilter,
    pub quick_bar: Vec<QuickBarPin>,
    pub focused_folder: Option<String>,
    scan_rx: Option<Receiver<Vec<AssetNode>>>,
    element: PathBuf,
}

impl ClientExplorer {
    pub fn new(element: PathBuf) -> Self {
        let mut explorer = Self {
            roots: vec![builtin_primitives_root()],
            scanning: false,
            total_assets: 0,
            search: String::new(),
            filter: AssetFilter::All,
            quick_bar: default_quick_bar_pins(),
            focused_folder: None,
            scan_rx: None,
            element: element.clone(),
        };
        explorer.load_pins();
        explorer.recount();
        explorer.start_scan();
        explorer
    }

    pub fn start_scan(&mut self) {
        if self.element.as_os_str().is_empty() || !self.element.is_dir() {
            return;
        }
        let element = self.element.clone();
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        self.scanning = true;
        std::thread::spawn(move || {
            let tree = scan_client_assets(&element);
            let _ = tx.send(tree);
        });
    }

    pub fn poll_scan(&mut self) {
        if let Some(rx) = &self.scan_rx {
            if let Ok(mut scanned) = rx.try_recv() {
                let mut roots = vec![builtin_primitives_root()];
                roots.append(&mut scanned);
                self.roots = roots;
                self.scanning = false;
                self.scan_rx = None;
                self.recount();
            }
        }
    }

    fn recount(&mut self) {
        self.total_assets = self.roots.iter().map(AssetNode::total_files).sum();
    }

    pub fn is_pinned(&self, full_path: &str) -> bool {
        self.quick_bar.iter().any(|p| p.full_path == full_path)
    }

    pub fn pin_node(&mut self, node: &AssetNode) {
        if self.is_pinned(&node.full_path) {
            return;
        }
        self.quick_bar.push(QuickBarPin {
            label: node.name.clone(),
            full_path: node.full_path.clone(),
            archive: node.archive.clone(),
            is_dir: node.is_dir,
            kind: node.kind,
        });
        self.save_pins();
    }

    pub fn unpin(&mut self, full_path: &str) {
        self.quick_bar.retain(|p| p.full_path != full_path);
        self.save_pins();
    }

    pub fn find_folder<'a>(&'a self, full_path: &str) -> Option<&'a AssetNode> {
        fn walk<'a>(nodes: &'a [AssetNode], target: &str) -> Option<&'a AssetNode> {
            for n in nodes {
                if n.is_dir {
                    if n.full_path == target {
                        return Some(n);
                    }
                    if let Some(found) = walk(&n.children, target) {
                        return Some(found);
                    }
                }
            }
            None
        }
        walk(&self.roots, full_path)
    }

    fn pins_file(&self) -> Option<PathBuf> {
        if self.element.as_os_str().is_empty() {
            None
        } else {
            Some(self.element.join("architect_quickbar.tsv"))
        }
    }

    fn save_pins(&self) {
        let Some(path) = self.pins_file() else {
            return;
        };
        let mut text = String::new();
        for pin in &self.quick_bar {
            let kind_str = pin.kind.map(|k| (k as u8).to_string()).unwrap_or_default();
            let arch = pin.archive.as_deref().unwrap_or("");
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                if pin.is_dir { "D" } else { "F" },
                pin.label.replace('\t', " "),
                pin.full_path.replace('\t', " "),
                arch.replace('\t', " "),
                kind_str
            ));
        }
        let _ = fs::write(path, text);
    }

    fn load_pins(&mut self) {
        let Some(path) = self.pins_file() else {
            return;
        };
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let mut loaded = Vec::new();
        for line in text.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 5 {
                continue;
            }
            let is_dir = parts[0] == "D";
            let label = parts[1].to_string();
            let full_path = parts[2].to_string();
            let archive = if parts[3].is_empty() {
                None
            } else {
                Some(parts[3].to_string())
            };
            let kind = parts[4].parse::<u8>().ok().map(ObjectKind::from_u8);
            loaded.push(QuickBarPin {
                label,
                full_path,
                archive,
                is_dir,
                kind,
            });
        }
        if !loaded.is_empty() {
            self.quick_bar = loaded;
        }
    }
}

pub fn builtin_primitives_root() -> AssetNode {
    let mut root = AssetNode::folder(
        "Built-in Primitives & Helpers".into(),
        "builtin://".into(),
        None,
    );
    let items = [
        ("Stone Block", "builtin://cube", ObjectKind::Primitive),
        ("Tall Pillar", "builtin://pillar", ObjectKind::Primitive),
        ("Wall Segment", "builtin://wall", ObjectKind::Primitive),
        ("Platform Slab", "builtin://platform", ObjectKind::Primitive),
        ("Ramp / Bridge", "builtin://ramp", ObjectKind::Primitive),
        ("Pine Tree Marker", "builtin://tree_pine", ObjectKind::Tree),
        ("Point Light", "builtin://light_point", ObjectKind::Light),
        ("Water Plane", "builtin://water_plane", ObjectKind::Water),
    ];
    for (name, path, kind) in items {
        root.children
            .push(AssetNode::file(name.into(), path.into(), None, kind));
    }
    root
}

fn default_quick_bar_pins() -> Vec<QuickBarPin> {
    vec![
        QuickBarPin {
            label: "Stone Block".into(),
            full_path: "builtin://cube".into(),
            archive: None,
            is_dir: false,
            kind: Some(ObjectKind::Primitive),
        },
        QuickBarPin {
            label: "Tall Pillar".into(),
            full_path: "builtin://pillar".into(),
            archive: None,
            is_dir: false,
            kind: Some(ObjectKind::Primitive),
        },
        QuickBarPin {
            label: "Pine Tree Marker".into(),
            full_path: "builtin://tree_pine".into(),
            archive: None,
            is_dir: false,
            kind: Some(ObjectKind::Tree),
        },
        QuickBarPin {
            label: "Point Light".into(),
            full_path: "builtin://light_point".into(),
            archive: None,
            is_dir: false,
            kind: Some(ObjectKind::Light),
        },
    ]
}

/// Trie-like builder to construct a sorted folder tree from flat archive paths.
#[derive(Default)]
struct FolderBuilder {
    dirs: BTreeMap<String, FolderBuilder>,
    files: BTreeMap<String, (String, ObjectKind)>,
}

impl FolderBuilder {
    fn insert(&mut self, segments: &[&str], full_path: String, kind: ObjectKind) {
        if let Some((first, rest)) = segments.split_first() {
            if rest.is_empty() {
                self.files
                    .entry((*first).to_string())
                    .or_insert((full_path, kind));
            } else {
                self.dirs
                    .entry((*first).to_string())
                    .or_default()
                    .insert(rest, full_path, kind);
            }
        }
    }

    fn into_children(self, prefix: &str, archive: &Option<String>) -> Vec<AssetNode> {
        let mut out = Vec::with_capacity(self.dirs.len() + self.files.len());
        for (name, sub) in self.dirs {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let mut node = AssetNode::folder(name, path.clone(), archive.clone());
            node.children = sub.into_children(&path, archive);
            out.push(node);
        }
        for (name, (full_path, kind)) in self.files {
            out.push(AssetNode::file(name, full_path, archive.clone(), kind));
        }
        out
    }
}

fn is_placeable_or_browsable(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".bmd")
        || lower.ends_with(".ecm")
        || lower.ends_with(".smd")
        || lower.ends_with(".spt")
        || lower.ends_with(".gfx")
        || lower.ends_with(".wav")
        || lower.ends_with(".mp3")
        || lower.ends_with(".ogg")
        || lower.ends_with(".grs")
}

/// Scan all `.pck` archives and loose client folders under `element`.
pub fn scan_client_assets(element: &Path) -> Vec<AssetNode> {
    let mut roots = Vec::new();
    let Ok(entries) = fs::read_dir(element) else {
        return roots;
    };

    let mut pck_files = Vec::new();
    let mut loose_dirs = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_file() && name.to_ascii_lowercase().ends_with(".pck") {
            pck_files.push((name, path));
        } else if path.is_dir()
            && !name.eq_ignore_ascii_case("maps")
            && !name.eq_ignore_ascii_case("architect_worlds")
        {
            loose_dirs.push((name, path));
        }
    }

    pck_files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    loose_dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

    for (pck_name, pck_path) in pck_files {
        let Ok(mut file) = File::open(&pck_path) else {
            continue;
        };
        let list = match pck::directory(&mut file) {
            Ok(list) => list,
            Err(_) => {
                let _ = file.seek(std::io::SeekFrom::Start(0));
                match angelica_pck::directory(&mut file) {
                    Ok(list) => list,
                    Err(_) => continue,
                }
            }
        };

        let mut builder = FolderBuilder::default();
        let mut count = 0usize;
        for entry in list {
            let norm = entry.name.replace('\\', "/");
            let norm = norm.trim_start_matches('/');
            if !is_placeable_or_browsable(norm) {
                continue;
            }
            let kind = ObjectKind::from_path(norm);
            let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
            if !segs.is_empty() {
                builder.insert(&segs, norm.to_string(), kind);
                count += 1;
            }
        }
        if count > 0 {
            let archive_opt = Some(pck_name.clone());
            let mut root =
                AssetNode::folder(pck_name.clone(), pck_name.clone(), archive_opt.clone());
            root.children = builder.into_children(&pck_name, &archive_opt);
            roots.push(root);
        }
    }

    for (dir_name, dir_path) in loose_dirs {
        let mut builder = FolderBuilder::default();
        let mut count = 0usize;
        scan_loose_dir(&dir_path, &dir_name, &mut builder, &mut count, 0);
        if count > 0 {
            let mut root = AssetNode::folder(format!("{dir_name}/"), dir_name.clone(), None);
            root.children = builder.into_children(&dir_name, &None);
            roots.push(root);
        }
    }

    roots
}

fn scan_loose_dir(
    dir: &Path,
    prefix: &str,
    builder: &mut FolderBuilder,
    count: &mut usize,
    depth: usize,
) {
    if depth > 6 || *count > 20_000 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = format!("{prefix}/{name}");
        if path.is_dir() {
            scan_loose_dir(&path, &rel, builder, count, depth + 1);
        } else if is_placeable_or_browsable(&rel) {
            let kind = ObjectKind::from_path(&rel);
            let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
            if !segs.is_empty() {
                builder.insert(&segs, rel.clone(), kind);
                *count += 1;
            }
        }
    }
}
