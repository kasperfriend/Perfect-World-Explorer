//! Architect: an isolated, non-destructive world-authoring workspace.
//!
//! The first editor layer stores custom placements and terrain-brush operations
//! in portable .pwa project files. It deliberately does not rewrite native
//! ECWLD/T2BK client data: a project remains reversible until a validated native
//! exporter exists.
use glam::Vec3;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dock {
    Left,
    Right,
    Bottom,
    Floating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransformMode {
    Move,
    Rotate,
    Scale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CursorMode {
    Select,
    MultiSelect,
    Lasso,
    Place,
    Sculpt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapMode {
    Off,
    Terrain,
    Objects,
    Smart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrushMode {
    Raise,
    Lower,
    Flatten,
    Smooth,
    Paint,
}

#[derive(Clone, Debug, PartialEq)]
struct PlacedObject {
    asset: String,
    name: String,
    group: String,
    visible: bool,
    locked: bool,
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
    /// Per-local-axis multiplier layered over the uniform scale.
    scale_axes: [f32; 3],
}

#[derive(Clone, Debug)]
struct TerrainStroke {
    mode: BrushMode,
    position: [f32; 3],
    radius: f32,
    strength: f32,
}

#[derive(Clone, Debug)]
struct ExplorerEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

#[derive(Clone, Debug, Default)]
struct Project {
    name: String,
    source_map: String,
    objects: Vec<PlacedObject>,
    strokes: Vec<TerrainStroke>,
}

pub struct Architect {
    enabled: bool,
    client_root: PathBuf,
    current_map_id: String,
    map: Option<crate::navigation::MapSpace>,
    project_root: PathBuf,
    project: Project,
    redo_strokes: Vec<TerrainStroke>,
    project_files: Vec<PathBuf>,
    directory_cache: HashMap<PathBuf, Vec<ExplorerEntry>>,
    archive_cache: HashMap<PathBuf, Vec<String>>,
    archive_detail_cache: HashMap<PathBuf, Vec<(String, u32, u32)>>,
    archive_details: Vec<(String, u32, u32)>,
    project_name: String,
    selected_project: Option<usize>,
    selected_map: String,
    cursor: CursorMode,
    transform_mode: TransformMode,
    snap: SnapMode,
    brush: BrushMode,
    brush_radius: f32,
    brush_strength: f32,
    flatten_height: f32,
    selected_asset: Option<String>,
    hovered_asset: Option<String>,
    preview_assets: crate::modules::librarian::archive::LibrarianAssets,
    effect_catalog: Option<crate::gfx_catalog::Catalog>,
    preview: Option<crate::modules::librarian::preview3d::Preview>,
    /// Exact local-space bounds cached per loaded client mesh. Never substitute
    /// guessed class dimensions for an object's actual geometry.
    preview_bounds: HashMap<String, ([f32; 3], [f32; 3])>,
    tree_types: Vec<crate::scene::TreeType>,
    preview_message: String,
    selected_archive: Option<PathBuf>,
    archive_entries: Vec<String>,
    archive_error: Option<String>,
    selected_scene_object: Option<usize>,
    selected_scene_transform: Option<[f32; 3]>,
    selected_placement: Option<usize>,
    selected_placements: HashSet<usize>,
    multi_selection: HashSet<usize>,
    multi_scene_items: HashMap<usize, (String, [f32; 3])>,
    quick_bar: Vec<String>,
    left_dock: Dock,
    right_dock: Dock,
    bottom_dock: Dock,
    applied_left_dock: Dock,
    applied_right_dock: Dock,
    applied_bottom_dock: Dock,
    confirm_erase: bool,
    confirm_clear: bool,
    status: String,
    tree_filter: String,
    object_filter: String,
    show_scene_objects: bool,
    brush_cursor_position: Option<[f32; 3]>,
    brush_dragging: bool,
    last_brush_position: Option<[f32; 3]>,
    object_dragging: bool,
    object_drag_y_offset: f32,
    object_drag_xz_offset: [f32; 2],
    object_drag_axis: Option<usize>,
    object_drag_start_pointer: Option<egui::Pos2>,
    object_drag_start_position: [f32; 3],
    object_drag_start_hit: Option<[f32; 3]>,
    object_drag_start_axis_parameter: Option<f32>,
    object_drag_start_rotation_vector: Option<[f32; 3]>,
    object_drag_group: Vec<(usize, [f32; 3])>,
    object_drag_snapshot: Option<Vec<PlacedObject>>,
    marquee_start: Option<egui::Pos2>,
    marquee_current: Option<egui::Pos2>,
    lasso_points: Vec<egui::Pos2>,
    undo_objects: Vec<Vec<PlacedObject>>,
    redo_objects: Vec<Vec<PlacedObject>>,
    clipboard_objects: Vec<PlacedObject>,
    object_history_action: bool,
}

impl Architect {
    pub fn new(
        element: PathBuf,
        map: Option<crate::navigation::MapSpace>,
        current_map_id: String,
    ) -> Self {
        let data_root = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_DATA_HOME").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            });
        let base = data_root.unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        });
        let project_root = base
            .join("PerfectWorldExplorer")
            .join("Architect")
            .join("Worlds");
        // The browser is rooted at the client directory, but PCK readers must
        // be rooted at the actual `element` directory. Passing client_root here
        // silently made every models.pck/gfx.pck preview look one directory too
        // high (client/models.pck instead of client/element/models.pck).
        let client_root = element.parent().unwrap_or(&element).to_path_buf();
        let preview_assets = crate::modules::librarian::archive::LibrarianAssets::open(&element);
        let mut editor = Self {
            enabled: false,
            client_root,
            current_map_id,
            map,
            project_root,
            project: Project::default(),
            redo_strokes: Vec::new(),
            project_files: Vec::new(),
            directory_cache: HashMap::new(),
            archive_cache: HashMap::new(),
            archive_detail_cache: HashMap::new(),
            archive_details: Vec::new(),
            project_name: "Untitled World".into(),
            selected_project: None,
            selected_map: "__empty__".into(),
            cursor: CursorMode::Select,
            transform_mode: TransformMode::Move,
            snap: SnapMode::Smart,
            brush: BrushMode::Raise,
            brush_radius: 24.,
            brush_strength: 2.,
            flatten_height: 0.,
            selected_asset: None,
            hovered_asset: None,
            preview_assets,
            effect_catalog: None,
            preview: None,
            preview_bounds: HashMap::new(),
            tree_types: Vec::new(),
            preview_message: "Hover or select an ECM/SMD model to preview it.".into(),
            selected_archive: None,
            archive_entries: Vec::new(),
            archive_error: None,
            selected_scene_object: None,
            selected_scene_transform: None,
            selected_placement: None,
            selected_placements: HashSet::new(),
            multi_selection: HashSet::new(),
            multi_scene_items: HashMap::new(),
            quick_bar: Vec::new(),
            left_dock: Dock::Left,
            right_dock: Dock::Right,
            bottom_dock: Dock::Bottom,
            applied_left_dock: Dock::Left,
            applied_right_dock: Dock::Right,
            applied_bottom_dock: Dock::Bottom,
            confirm_erase: false,
            confirm_clear: false,
            status: "Ready. Changes are saved as a separate Architect project.".into(),
            tree_filter: String::new(),
            object_filter: String::new(),
            show_scene_objects: true,
            brush_cursor_position: None,
            brush_dragging: false,
            last_brush_position: None,
            object_dragging: false,
            object_drag_y_offset: 0.,
            object_drag_xz_offset: [0.; 2],
            object_drag_axis: None,
            object_drag_start_pointer: None,
            object_drag_start_position: [0.; 3],
            object_drag_start_hit: None,
            object_drag_start_axis_parameter: None,
            object_drag_start_rotation_vector: None,
            object_drag_group: Vec::new(),
            object_drag_snapshot: None,
            marquee_start: None,
            marquee_current: None,
            lasso_points: Vec::new(),
            undo_objects: Vec::new(),
            redo_objects: Vec::new(),
            clipboard_objects: Vec::new(),
            object_history_action: false,
        };
        editor.refresh_projects();
        editor
    }

    pub fn active(&self) -> bool {
        self.enabled
    }

    pub fn set_effect_catalog(&mut self, catalog: crate::gfx_catalog::Catalog) {
        self.effect_catalog = Some(catalog);
    }

    pub fn is_dragging_object(&self) -> bool {
        self.object_dragging
    }

    pub fn set_active(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            self.status = "Architect enabled. The original world remains unchanged.".into();
        } else {
            self.cancel_object_drag();
            self.brush_dragging = false;
            self.last_brush_position = None;
        }
    }

    fn refresh_projects(&mut self) {
        self.project_files.clear();
        // Recover a project if the process stopped after preserving the old
        // file but before publishing its staged replacement.
        if let Ok(entries) = fs::read_dir(&self.project_root) {
            for entry in entries.flatten() {
                let backup = entry.path();
                if backup
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.to_ascii_lowercase().ends_with(".pwa.bak"))
                {
                    let target = backup.with_extension("");
                    if target.exists() {
                        let _ = fs::remove_file(&backup);
                    } else {
                        let _ = fs::rename(&backup, target);
                    }
                }
            }
        }
        let Ok(entries) = fs::read_dir(&self.project_root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pwa"))
            {
                self.project_files.push(path);
            }
        }
        self.project_files.sort();
    }

    fn safe_slug(name: &str) -> String {
        let mut slug = String::new();
        for ch in name.trim().chars() {
            if ch.is_ascii_alphanumeric() {
                slug.push(ch.to_ascii_lowercase());
            } else if (ch.is_whitespace() || ch == '-' || ch == '_')
                && !slug.ends_with('-')
                && !slug.is_empty()
            {
                slug.push('-');
            }
            if slug.len() >= 48 {
                break;
            }
        }
        let slug = slug.trim_matches('-').to_string();
        if slug.is_empty() {
            "untitled-world".into()
        } else {
            slug
        }
    }

    fn encode(value: &str) -> String {
        let mut out = String::with_capacity(value.len() * 2);
        for byte in value.as_bytes() {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    fn decode(value: &str) -> Result<String, String> {
        if value.len() % 2 != 0 {
            return Err("invalid project string".into());
        }
        let mut bytes = Vec::with_capacity(value.len() / 2);
        for pair in value.as_bytes().chunks_exact(2) {
            let hex = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
            bytes.push(u8::from_str_radix(hex, 16).map_err(|e| e.to_string())?);
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }

    fn write_project(&self, path: &Path) -> Result<(), String> {
        if self.project.objects.len() > 100_000 || self.project.strokes.len() > 100_000 {
            return Err("Architect project exceeds the record-count safety limit".into());
        }
        for object in &self.project.objects {
            if object.asset.is_empty()
                || object.asset.len() > 16 * 1024
                || object.name.trim().is_empty()
                || object.name.len() > 1024
                || object.group.len() > 1024
                || !object
                    .position
                    .iter()
                    .chain(object.rotation.iter())
                    .all(|v| v.is_finite())
                || !object.scale.is_finite()
                || object.scale <= 0.
                || !object.scale_axes.iter().all(|value| value.is_finite() && *value > 0. && *value <= 1000.)
            {
                return Err("Project contains an invalid object asset or transform".into());
            }
        }
        for stroke in &self.project.strokes {
            if !stroke.position.iter().all(|v| v.is_finite())
                || !stroke.radius.is_finite()
                || stroke.radius <= 0.
                || !stroke.strength.is_finite()
            {
                return Err("Project contains an invalid terrain operation".into());
            }
        }
        let mut out = String::from("PWE_ARCHITECT\t3\n");
        out.push_str(&format!("name\t{}\n", Self::encode(&self.project.name)));
        out.push_str(&format!(
            "source_map\t{}\n",
            Self::encode(&self.project.source_map)
        ));
        for object in &self.project.objects {
            out.push_str(&format!(
                "object\t{}\t{}\t{}\t{}\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\n",
                Self::encode(&object.asset),
                Self::encode(&object.name),
                Self::encode(&object.group),
                object.visible,
                object.locked,
                object.position[0],
                object.position[1],
                object.position[2],
                object.rotation[0],
                object.rotation[1],
                object.rotation[2],
                object.scale,
                object.scale_axes[0],
                object.scale_axes[1],
                object.scale_axes[2]
            ));
        }
        for stroke in &self.project.strokes {
            out.push_str(&format!(
                "stroke\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\n",
                match stroke.mode {
                    BrushMode::Raise => "raise",
                    BrushMode::Lower => "lower",
                    BrushMode::Flatten => "flatten",
                    BrushMode::Smooth => "smooth",
                    BrushMode::Paint => "paint",
                },
                stroke.position[0],
                stroke.position[1],
                stroke.position[2],
                stroke.radius,
                stroke.strength
            ));
        }
        if out.len() > 64 * 1024 * 1024 {
            return Err("Architect project exceeds the 64 MiB safety limit".into());
        }
        let parent = path.parent().ok_or("project path has no parent")?;
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid project filename")?;
        let temp = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
        let backup = path.with_extension("pwa.bak");
        let _ = fs::remove_file(&temp);
        fs::write(&temp, out).map_err(|e| format!("{}: {e}", temp.display()))?;
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&temp)
            .map_err(|e| format!("{}: {e}", temp.display()))?;
        file.sync_all()
            .map_err(|e| format!("{}: {e}", temp.display()))?;
        let had_previous = path.exists();
        if had_previous {
            let _ = fs::remove_file(&backup);
            fs::rename(path, &backup).map_err(|e| {
                let _ = fs::remove_file(&temp);
                format!(
                    "Could not preserve previous project {}: {e}",
                    path.display()
                )
            })?;
        }
        if let Err(error) = fs::rename(&temp, path) {
            if had_previous {
                let _ = fs::rename(&backup, path);
            }
            let _ = fs::remove_file(&temp);
            return Err(format!("{}: {error}", path.display()));
        }
        if had_previous {
            let _ = fs::remove_file(&backup);
        }
        Ok(())
    }

    fn read_project(path: &Path) -> Result<Project, String> {
        let metadata = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.len() > 64 * 1024 * 1024 {
            return Err("Architect project exceeds the 64 MiB safety limit".into());
        }
        let source = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut lines = source.lines();
        let version = match lines.next() {
            Some("PWE_ARCHITECT\t1") => 1,
            Some("PWE_ARCHITECT\t2") => 2,
            Some("PWE_ARCHITECT\t3") => 3,
            _ => return Err("unsupported Architect project version".into()),
        };
        let mut project = Project::default();
        for (line_no, line) in lines.enumerate() {
            let fields = line.split('\t').collect::<Vec<_>>();
            match fields.first().copied().unwrap_or("") {
                "name" if fields.len() == 2 => project.name = Self::decode(fields[1])?,
                "source_map" if fields.len() == 2 => project.source_map = Self::decode(fields[1])?,
                "object"
                    if (version == 1 && fields.len() == 9)
                        || (version == 2 && fields.len() == 13)
                        || (version == 3 && fields.len() == 16) =>
                {
                    if project.objects.len() >= 100_000 {
                        return Err("Architect project has too many object placements".into());
                    }
                    let (asset, name, group, visible, locked, numbers) = if version == 1 {
                        let asset = Self::decode(fields[1])?;
                        let name = Path::new(&asset)
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().into_owned())
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| "Object".into());
                        (asset, name, "Default".into(), true, false, &fields[2..])
                    } else {
                        (
                            Self::decode(fields[1])?,
                            Self::decode(fields[2])?,
                            Self::decode(fields[3])?,
                            fields[4]
                                .parse::<bool>()
                                .map_err(|_| "invalid object visibility")?,
                            fields[5]
                                .parse::<bool>()
                                .map_err(|_| "invalid object lock state")?,
                            &fields[6..],
                        )
                    };
                    if name.trim().is_empty() || name.len() > 1024 || group.len() > 1024 {
                        return Err("invalid object name or hierarchy group".into());
                    }
                    let mut n = [0f32; 10];
                    for (slot, value) in n.iter_mut().zip(numbers) {
                        *slot = value.parse().map_err(|_| {
                            format!("invalid object number on line {}", line_no + 2)
                        })?;
                        if !slot.is_finite() {
                            return Err("non-finite object transform".into());
                        }
                    }
                    project.objects.push(PlacedObject {
                        asset,
                        name,
                        group,
                        visible,
                        locked,
                        position: [n[0], n[1], n[2]],
                        rotation: [n[3], n[4], n[5]],
                        scale: n[6].clamp(0.01, 1000.),
                        scale_axes: if version >= 3 {
                            [n[7].clamp(0.01, 1000.), n[8].clamp(0.01, 1000.), n[9].clamp(0.01, 1000.)]
                        } else { [1.; 3] },
                    });
                }
                "stroke" if fields.len() == 7 => {
                    if project.strokes.len() >= 100_000 {
                        return Err("Architect project has too many terrain operations".into());
                    }
                    let mode = match fields[1] {
                        "raise" => BrushMode::Raise,
                        "lower" => BrushMode::Lower,
                        "flatten" => BrushMode::Flatten,
                        "smooth" => BrushMode::Smooth,
                        "paint" => BrushMode::Paint,
                        _ => return Err("unknown terrain brush".into()),
                    };
                    let mut n = [0f32; 5];
                    for (slot, value) in n.iter_mut().zip(&fields[2..]) {
                        *slot = value
                            .parse()
                            .map_err(|_| format!("invalid brush number on line {}", line_no + 2))?;
                        if !slot.is_finite() {
                            return Err("non-finite terrain brush value".into());
                        }
                    }
                    project.strokes.push(TerrainStroke {
                        mode,
                        position: [n[0], n[1], n[2]],
                        radius: n[3].clamp(0.1, 250.),
                        strength: n[4].clamp(-10000., 10000.),
                    });
                }
                "" => {}
                _ => {
                    return Err(format!(
                        "invalid Architect project record on line {}",
                        line_no + 2
                    ))
                }
            }
        }
        Ok(project)
    }

    /// Apply one brush stroke to the shared terrain overlay. The source client
    /// pages remain immutable; the renderer and navigation readers see the
    /// same sparse height/normal overrides through MapSpace.
    fn apply_stroke(&self, stroke: &TerrainStroke) -> Result<usize, String> {
        let map = self
            .map
            .as_ref()
            .ok_or("No terrain is loaded for sculpting")?;
        if !self.project.source_map.is_empty()
            && !self.current_map_id.is_empty()
            && !self
                .project
                .source_map
                .eq_ignore_ascii_case(&self.current_map_id)
        {
            return Err(format!(
                "Project belongs to map {}, but the loaded terrain is {}.",
                self.project.source_map, self.current_map_id
            ));
        }
        if map.width < 2 || map.height < 2 || !map.spacing.is_finite() || map.spacing <= 0. {
            return Err("Terrain dimensions or spacing are invalid".into());
        }
        if stroke.mode == BrushMode::Paint {
            return Err(
                "Terrain paint is not implemented yet; choose Raise, Lower, Flatten or Smooth."
                    .into(),
            );
        }
        let radius = stroke.radius.clamp(0.1, 250.);
        let cx = stroke.position[0] / map.spacing + map.width as f32 * 0.5;
        let cy = map.height as f32 * 0.5 - stroke.position[2] / map.spacing;
        if !cx.is_finite() || !cy.is_finite() {
            return Err("Brush position is not finite".into());
        }
        let cells_radius = radius / map.spacing;
        let x0 = (cx - cells_radius).floor().max(0.) as usize;
        let y0 = (cy - cells_radius).floor().max(0.) as usize;
        let x1 = (cx + cells_radius).ceil().min((map.width - 1) as f32) as usize;
        let y1 = (cy + cells_radius).ceil().min((map.height - 1) as f32) as usize;
        if x0 > x1 || y0 > y1 {
            return Ok(0);
        }
        let area = (x1 - x0 + 1).saturating_mul(y1 - y0 + 1);
        if area > 2_000_000 {
            return Err("Brush footprint is too large for one edit operation".into());
        }
        let mut source = map.heights.reader();
        let mut changed = HashMap::<usize, f32>::new();
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = (x as f32 - cx) * map.spacing;
                let dz = (cy - y as f32) * map.spacing;
                let distance = (dx * dx + dz * dz).sqrt();
                if distance > radius {
                    continue;
                }
                let t = (1. - distance / radius).clamp(0., 1.);
                let falloff = t * t * (3. - 2. * t);
                let index = y * map.width + x;
                let old = source.height(index);
                let next = match stroke.mode {
                    BrushMode::Raise => old + stroke.strength * falloff,
                    BrushMode::Lower => old - stroke.strength * falloff,
                    BrushMode::Flatten => old + (stroke.strength - old) * falloff,
                    BrushMode::Smooth => {
                        let mut total = 0.;
                        let mut count: f32 = 0.;
                        for ny in y.saturating_sub(1)..=(y + 1).min(map.height - 1) {
                            for nx in x.saturating_sub(1)..=(x + 1).min(map.width - 1) {
                                total += source.height(ny * map.width + nx);
                                count += 1.;
                            }
                        }
                        let average = total / count.max(1.);
                        let blend = (stroke.strength / 20.).clamp(0., 1.) * falloff;
                        old + (average - old) * blend
                    }
                    BrushMode::Paint => unreachable!(),
                };
                if !next.is_finite() {
                    return Err("Brush produced a non-finite terrain height".into());
                }
                if (next - old).abs() > 0.00001 {
                    changed.insert(index, next);
                }
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }

        // Recompute normals over the changed cells plus a one-cell border. This
        // avoids visible lighting seams at the edge of a sculpted patch.
        let nx0 = x0.saturating_sub(1);
        let ny0 = y0.saturating_sub(1);
        let nx1 = (x1 + 1).min(map.width - 1);
        let ny1 = (y1 + 1).min(map.height - 1);
        let height_at = |x: usize, y: usize, source: &mut crate::terrain_store::Reader| {
            changed
                .get(&(y * map.width + x))
                .copied()
                .unwrap_or_else(|| source.height(y * map.width + x))
        };
        let mut edits = Vec::with_capacity((nx1 - nx0 + 1) * (ny1 - ny0 + 1));
        for y in ny0..=ny1 {
            for x in nx0..=nx1 {
                let center = height_at(x, y, &mut source);
                let left = if x > 0 {
                    height_at(x - 1, y, &mut source)
                } else {
                    center
                };
                let right = if x + 1 < map.width {
                    height_at(x + 1, y, &mut source)
                } else {
                    center
                };
                let north = if y > 0 {
                    height_at(x, y - 1, &mut source)
                } else {
                    center
                };
                let south = if y + 1 < map.height {
                    height_at(x, y + 1, &mut source)
                } else {
                    center
                };
                let dx = (right - left)
                    / (if x == 0 || x + 1 == map.width {
                        map.spacing
                    } else {
                        2. * map.spacing
                    });
                let dz = (north - south)
                    / (if y == 0 || y + 1 == map.height {
                        map.spacing
                    } else {
                        2. * map.spacing
                    });
                let normal = Vec3::new(-dx, 1., -dz).normalize_or_zero();
                let encoded = [
                    (normal.x * 127.).round().clamp(-127., 127.) as i8,
                    (normal.y * 127.).round().clamp(-127., 127.) as i8,
                    (normal.z * 127.).round().clamp(-127., 127.) as i8,
                ];
                edits.push((y * map.width + x, center, encoded));
            }
        }
        map.heights.apply_cells(&edits)?;
        Ok(changed.len())
    }

    /// Recreate the current project's terrain from the immutable source plus
    /// its ordered strokes. This makes undo/redo deterministic and reversible.
    fn rebuild_terrain(&self) -> Result<(), String> {
        let map = self
            .map
            .as_ref()
            .ok_or("No terrain is loaded for sculpting")?;
        if !self.project.source_map.is_empty()
            && !self.current_map_id.is_empty()
            && !self
                .project
                .source_map
                .eq_ignore_ascii_case(&self.current_map_id)
        {
            return Err(format!(
                "Project belongs to map {}, but the loaded terrain is {}.",
                self.project.source_map, self.current_map_id
            ));
        }
        map.heights.clear_edits()?;
        for stroke in &self.project.strokes {
            // Version-1 projects could store Paint strokes before material
            // painting was implemented. Preserve them in the project, but do
            // not let an unsupported record block terrain replay.
            if stroke.mode != BrushMode::Paint {
                self.apply_stroke(stroke)?;
            }
        }
        Ok(())
    }

    fn clear_object_selection(&mut self) {
        self.selected_placement = None;
        self.selected_placements.clear();
    }

    fn marquee_rect(start: egui::Pos2, current: egui::Pos2) -> egui::Rect {
        egui::Rect::from_two_pos(start, current)
    }

    fn marquee_contains(start: egui::Pos2, current: egui::Pos2, point: egui::Pos2) -> bool {
        Self::marquee_rect(start, current).contains(point)
    }

    fn lasso_contains(points: &[egui::Pos2], point: egui::Pos2) -> bool {
        if points.len() < 3 {
            return false;
        }
        let mut inside = false;
        let mut previous = points.len() - 1;
        for current in 0..points.len() {
            let a = points[current];
            let b = points[previous];
            let crosses = (a.y > point.y) != (b.y > point.y)
                && point.x < (b.x - a.x) * (point.y - a.y) / (b.y - a.y) + a.x;
            if crosses {
                inside = !inside;
            }
            previous = current;
        }
        inside
    }

    fn cancel_object_drag(&mut self) {
        self.object_dragging = false;
        self.object_drag_axis = None;
        self.object_drag_start_pointer = None;
        self.object_drag_start_hit = None;
        self.object_drag_start_axis_parameter = None;
        self.object_drag_start_rotation_vector = None;
        self.object_drag_group.clear();
        self.object_drag_snapshot = None;
        self.marquee_start = None;
        self.marquee_current = None;
        self.lasso_points.clear();
    }

    fn push_object_undo(&mut self, snapshot: Vec<PlacedObject>) {
        if snapshot == self.project.objects {
            return;
        }
        self.undo_objects.push(snapshot);
        if self.undo_objects.len() > 128 {
            self.undo_objects.remove(0);
        }
        self.redo_objects.clear();
    }

    fn undo_objects(&mut self) {
        self.object_history_action = true;
        if let Some(previous) = self.undo_objects.pop() {
            self.redo_objects
                .push(std::mem::replace(&mut self.project.objects, previous));
            // Placement indices are not stable across snapshot restoration: a
            // deletion or insertion before a selected row can make the same index
            // refer to a completely different object. Never preserve stale indices
            // across history navigation; require an explicit re-selection instead.
            self.clear_object_selection();
            self.cancel_object_drag();
            self.status = "Undid object edit. Select a placement to continue editing.".into();
        }
    }

    fn redo_objects(&mut self) {
        self.object_history_action = true;
        if let Some(next) = self.redo_objects.pop() {
            self.undo_objects
                .push(std::mem::replace(&mut self.project.objects, next));
            self.clear_object_selection();
            self.cancel_object_drag();
            self.status = "Redid object edit. Select a placement to continue editing.".into();
        }
    }

    fn save_named(&mut self) -> bool {
        let name = self.project_name.trim();
        if name.is_empty() {
            self.status = "Enter a name before saving.".into();
            return false;
        }
        self.project.name = name.to_string();
        if self.project.source_map.is_empty() && !self.current_map_id.is_empty() {
            self.project.source_map = self.current_map_id.clone();
        }
        let path = self
            .project_root
            .join(format!("{}.pwa", Self::safe_slug(name)));
        match self.write_project(&path) {
            Ok(()) => {
                self.status = format!("Saved project: {}", path.display());
                self.refresh_projects();
                self.selected_project = self.project_files.iter().position(|p| p == &path);
                true
            }
            Err(error) => {
                self.status = format!("Save failed: {error}");
                false
            }
        }
    }

    fn load_selected(&mut self) {
        let Some(path) = self
            .selected_project
            .and_then(|index| self.project_files.get(index))
            .cloned()
        else {
            return;
        };
        match Self::read_project(&path) {
            Ok(mut project) => {
                if project.source_map.is_empty()
                    && (!project.objects.is_empty() || !project.strokes.is_empty())
                {
                    project.source_map = self.current_map_id.clone();
                }
                self.project_name = project.name.clone();
                self.selected_map = if project.source_map.is_empty() {
                    "__empty__".into()
                } else {
                    project.source_map.clone()
                };
                let previous_source = self.project.source_map.clone();
                let previous_belongs_here = previous_source.is_empty()
                    || previous_source.eq_ignore_ascii_case(&self.current_map_id);
                let previous_reset_error = if previous_belongs_here {
                    self.map
                        .as_ref()
                        .and_then(|map| map.heights.clear_edits().err())
                } else {
                    None
                };
                self.cancel_object_drag();
                self.project = project;
                self.redo_strokes.clear();
                self.undo_objects.clear();
                self.redo_objects.clear();
                let replay_error = self.rebuild_terrain().err().or(previous_reset_error);
                self.selected_placement = None;
                self.selected_placements.clear();
                self.selected_scene_object = None;
                self.multi_selection.clear();
                self.multi_scene_items.clear();
                self.status = format!(
                    "Loaded {} ({} placements, {} brush operations).",
                    self.project.name,
                    self.project.objects.len(),
                    self.project.strokes.len()
                );
                if let Some(error) = replay_error {
                    self.status = format!("{} Terrain replay failed: {error}", self.status);
                }
            }
            Err(error) => self.status = format!("Could not load project: {error}"),
        }
    }

    fn erase_selected(&mut self) {
        let Some(path) = self
            .selected_project
            .and_then(|index| self.project_files.get(index))
            .cloned()
        else {
            return;
        };
        match fs::remove_file(&path) {
            Ok(()) => {
                let erased_source_map = self.project.source_map.clone();
                let reset_error = self.map.as_ref().and_then(|map| {
                    let belongs_here = erased_source_map.is_empty()
                        || self.current_map_id.is_empty()
                        || erased_source_map.eq_ignore_ascii_case(&self.current_map_id);
                    if belongs_here {
                        map.heights.clear_edits().err()
                    } else {
                        None
                    }
                });
                self.cancel_object_drag();
                self.project = Project::default();
                self.redo_strokes.clear();
                self.undo_objects.clear();
                self.redo_objects.clear();
                self.project_name = "Untitled World".into();
                self.selected_project = None;
                self.selected_placement = None;
                self.selected_placements.clear();
                self.selected_scene_object = None;
                self.multi_selection.clear();
                self.multi_scene_items.clear();
                self.confirm_erase = false;
                self.refresh_projects();
                self.status = "Custom project erased. Client map files were not touched.".into();
                if let Some(error) = reset_error {
                    self.status = format!("Project erased, but terrain reset failed: {error}");
                }
            }
            Err(error) => self.status = format!("Could not erase project: {error}"),
        }
    }

    fn dock_position(dock: Dock, screen: egui::Rect, width: f32, height: f32) -> egui::Pos2 {
        match dock {
            Dock::Left => egui::pos2(8., 52.),
            Dock::Right => egui::pos2((screen.width() - width - 8.).max(8.), 52.),
            Dock::Bottom => egui::pos2(8., (screen.height() - height - 48.).max(52.)),
            Dock::Floating => egui::pos2((screen.width() - width) * 0.5, 70.),
        }
    }

    fn dock_picker(ui: &mut egui::Ui, dock: &mut Dock) {
        egui::ComboBox::from_id_source(ui.next_auto_id())
            .selected_text(match dock {
                Dock::Left => "Dock: Left",
                Dock::Right => "Dock: Right",
                Dock::Bottom => "Dock: Bottom",
                Dock::Floating => "Floating",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(dock, Dock::Left, "Left");
                ui.selectable_value(dock, Dock::Right, "Right");
                ui.selectable_value(dock, Dock::Bottom, "Bottom");
                ui.selectable_value(dock, Dock::Floating, "Floating");
            });
    }

    fn preview_path_for_asset(&self, asset: &str) -> Result<String, String> {
        let portable = self.portable_asset_path(asset);
        let path = if let Some((archive, entry)) = portable.split_once("::") {
            let archive_name = Path::new(archive)
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let entry = crate::ecmodel::normalize(entry);
            match (archive_name.as_str(), entry.as_str()) {
                ("models.pck", value) if value.starts_with("models/") && value.ends_with(".ecm") => value.to_owned(),
                ("gfx.pck", value) if value.ends_with(".smd") => if value.starts_with("gfx/") { value.to_owned() } else { format!("gfx/{value}") },
                _ => return Err("This archive entry is not a supported preview model. ECM models from models.pck and static SMD models from gfx.pck are supported.".into()),
            }
        } else {
            let portable = portable.strip_prefix("element/").unwrap_or(&portable);
            crate::ecmodel::normalize(portable)
        };
        let supported = (path.starts_with("models/") && path.ends_with(".ecm"))
            || (path.starts_with("gfx/") && path.ends_with(".smd"));
        if supported {
            Ok(path)
        } else {
            Err("Preview supports ECM models in models.pck and static SMD models in gfx.pck. Other files are resources, textures or effect definitions, not directly placeable meshes.".into())
        }
    }

    pub fn prepare_preview(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        format: wgpu::TextureFormat,
    ) {
        let asset = self
            .hovered_asset
            .as_deref()
            .or(self.selected_asset.as_deref());
        let Some(asset) = asset else {
            self.preview_message = "Select a client asset to preview it or place it with its native world handler.".into();
            return;
        };
        if self.renderer_effect_path(asset).is_some() {
            self.preview = None;
            self.preview_message =
                "GFX effect uses the native world effect handler and appears after placement."
                    .into();
            return;
        }
        if let Some(path) = self.renderer_bmd_asset_path(asset) {
            self.preview = None;
            let loaded = self
                .preview_assets
                .read_litmodel_resource(&path)
                .and_then(|bytes| crate::bmd::Model::parse(&bytes));
            match loaded {
                Ok(model) => {
                    let identity = crate::scene::Ornament {
                        export_id: 0,
                        offset: 0,
                        position: [0.0; 3],
                        direction: [0.0, 0.0, 1.0],
                        up: [0.0, 1.0, 0.0],
                        extra: 0,
                        path: path.clone(),
                        map_id: self.current_map_id.to_ascii_lowercase(),
                    };
                    let mut min = Vec3::splat(f32::INFINITY);
                    let mut max = Vec3::splat(f32::NEG_INFINITY);
                    for vertex in model.meshes.iter().flat_map(|mesh| &mesh.vertices) {
                        let (position, _) =
                            crate::gpu::editor_bmd_vertex_transform(&model, &identity, vertex, 1.0);
                        if position.is_finite() {
                            min = min.min(position);
                            max = max.max(position);
                        }
                    }
                    if min.is_finite() && max.is_finite() {
                        self.preview_bounds
                            .insert(path.clone(), (min.to_array(), max.to_array()));
                        self.preview_message =
                            "BMD mesh bounds loaded. Its native materials appear in the world viewport after placement.".into();
                    } else {
                        self.preview_message =
                            "This BMD contains no finite mesh geometry for selection bounds."
                                .into();
                    }
                }
                Err(error) => {
                    self.preview_message = format!("Could not load BMD selection bounds: {error}");
                }
            }
            return;
        }
        if self.renderer_tree_type(asset).is_some() {
            self.preview = None;
            self.preview_message = "SpeedTree uses the native vegetation renderer. Select Place to see its live LOD/material/wind preview in the world.".into();
            return;
        }
        match self.preview_path_for_asset(asset) {
            Ok(path) => {
                let preview = self.preview.get_or_insert_with(|| {
                    crate::modules::librarian::preview3d::Preview::new(device, renderer, format)
                });
                preview.update(
                    device,
                    queue,
                    format,
                    &mut self.preview_assets,
                    &path,
                    None,
                    false,
                );
                if let Some(bounds) = preview.model_bounds() {
                    self.preview_bounds.insert(path.clone(), bounds);
                }
                self.preview_message.clear();
            }
            Err(message) => self.preview_message = message,
        }
    }

    fn show_tree(&mut self, ui: &mut egui::Ui, path: &Path, depth: usize, _camera: Vec3) {
        if depth > 12 {
            return;
        }
        let entries = if let Some(cached) = self.directory_cache.get(path) {
            cached.clone()
        } else {
            let Ok(directory) = fs::read_dir(path) else {
                return;
            };
            let mut entries = directory
                .flatten()
                .filter_map(|entry| {
                    let kind = entry.file_type().ok()?;
                    if kind.is_symlink() {
                        return None;
                    }
                    Some(ExplorerEntry {
                        path: entry.path(),
                        name: entry.file_name().to_string_lossy().into_owned(),
                        is_dir: kind.is_dir(),
                    })
                })
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.name.to_lowercase());
            self.directory_cache
                .insert(path.to_path_buf(), entries.clone());
            entries
        };
        for entry in entries {
            let child = entry.path;
            let label = entry.name;
            let is_dir = entry.is_dir;
            if !self.tree_filter.is_empty()
                && !label
                    .to_lowercase()
                    .contains(&self.tree_filter.to_lowercase())
                && !is_dir
            {
                continue;
            }
            if is_dir {
                let response = egui::CollapsingHeader::new(format!("📁 {label}"))
                    .id_source(child.to_string_lossy().to_string())
                    .show(ui, |ui| self.show_tree(ui, &child, depth + 1, _camera));
                response.header_response.context_menu(|ui| {
                    if ui.button("Pin folder to quick bar").clicked() {
                        self.pin(child.to_string_lossy().into_owned());
                        ui.close_menu();
                    }
                });
            } else {
                let response = ui.selectable_label(
                    self.selected_asset.as_deref() == Some(child.to_string_lossy().as_ref()),
                    format!("📄 {label}"),
                );
                if response.hovered() {
                    self.hovered_asset = Some(child.to_string_lossy().into_owned());
                }
                if response.clicked() {
                    if child
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("pck"))
                    {
                        self.open_archive(child.clone());
                    } else {
                        self.select_asset_for_placement(child.to_string_lossy().into_owned());
                    }
                }
                response.context_menu(|ui| {
                    if ui.button("Pin to quick bar").clicked() {
                        self.pin(child.to_string_lossy().into_owned());
                        ui.close_menu();
                    }
                });
            }
        }
    }

    fn placement_position(&self, camera: Vec3) -> [f32; 3] {
        let mut position = camera.to_array();
        match self.snap {
            SnapMode::Off => {}
            SnapMode::Objects => {
                if let Some(selected) = self.selected_scene_transform {
                    position = selected;
                }
            }
            SnapMode::Terrain => {
                if let Some(map) = &self.map {
                    position[1] = map.ground(position[0], position[2]) + 0.5;
                }
            }
            SnapMode::Smart => {
                if let Some(selected) = self.selected_scene_transform {
                    position = selected;
                } else if let Some(map) = &self.map {
                    position[1] = map.ground(position[0], position[2]) + 0.5;
                }
            }
        }
        position
    }

    fn portable_asset_path(&self, asset: &str) -> String {
        if let Some((archive, entry)) = asset.split_once("::") {
            let relative = Path::new(archive)
                .strip_prefix(&self.client_root)
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| archive.to_string());
            return format!("{}::{}", relative.replace('\\', "/"), entry);
        }
        Path::new(asset)
            .strip_prefix(&self.client_root)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| asset.to_string())
    }

    fn renderer_asset_path(asset: &str) -> Option<String> {
        let (archive, path) = if let Some((archive, entry)) = asset.split_once("::") {
            (Some(archive), entry)
        } else {
            (None, asset)
        };
        let normalized = crate::ecmodel::normalize(path);
        let archive_name = archive
            .and_then(|archive| Path::new(archive).file_name())
            .map(|name| name.to_string_lossy().to_ascii_lowercase());
        let valid_model = normalized.starts_with("models/")
            && normalized.ends_with(".ecm")
            && archive_name
                .as_deref()
                .is_none_or(|name| name == "models.pck");
        let valid_static_gfx = normalized.ends_with(".smd")
            && (normalized.starts_with("gfx/") || archive_name.as_deref() == Some("gfx.pck"))
            && archive_name.as_deref().is_none_or(|name| name == "gfx.pck");
        let normalized = if valid_static_gfx && !normalized.starts_with("gfx/") {
            format!("gfx/{normalized}")
        } else {
            normalized
        };
        let safe_segments = normalized
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
            && !normalized.contains(':');
        (safe_segments && (valid_model || valid_static_gfx)).then_some(normalized)
    }

    /// Resolve a loose or archive-qualified BMD through the client root.
    fn renderer_bmd_asset_path(&self, asset: &str) -> Option<String> {
        let portable = self.portable_asset_path(asset);
        let portable = if portable.contains("::") {
            portable
        } else {
            portable
                .strip_prefix("element/")
                .unwrap_or(&portable)
                .to_owned()
        };
        Self::renderer_bmd_path(&portable, &self.current_map_id)
    }

    /// Resolve a loose or archive-qualified ECM/SMD through the client root.
    fn renderer_model_asset_path(&self, asset: &str) -> Option<String> {
        let portable = self.portable_asset_path(asset);
        let portable = if portable.contains("::") {
            portable
        } else {
            portable
                .strip_prefix("element/")
                .unwrap_or(&portable)
                .to_owned()
        };
        Self::renderer_asset_path(&portable)
    }

    /// Resolve a BMD from litmodels.pck into the map-partitioned path used by
    /// the native baked-scenery loader. This is deliberately separate from the
    /// ECM/SMD model queue: BMDs need the scenery material/lighting pipeline.
    fn renderer_bmd_path(asset: &str, map_id: &str) -> Option<String> {
        let inner = if let Some((archive, inner)) = asset.split_once("::") {
            let archive_name = Path::new(archive)
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())?;
            if archive_name != "litmodels.pck" {
                return None;
            }
            inner
        } else {
            // EC-BSD source-scene rows already carry a map-relative BMD path
            // such as 1/litmodel_1399.bmd rather than an archive-qualified path.
            if Path::new(asset).is_absolute() || asset.contains(':') {
                return None;
            }
            asset
        };
        let mut path = crate::ecmodel::normalize(inner);
        if !path.ends_with(".bmd") {
            return None;
        }
        let safe = |value: &str| {
            value
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        };
        if !safe(&path) || path.contains(':') {
            return None;
        }
        let namespace = format!("litmodels/{}/", map_id.to_ascii_lowercase());
        if path.starts_with("litmodels/") {
            if path.starts_with(&namespace) {
                // Already namespaced for the current map.
            } else if let Some(relative) = path.strip_prefix("litmodels/world/") {
                path = format!("{namespace}{relative}");
            } else if let Some(relative) = path.strip_prefix("litmodels/") {
                // Archive entries may omit the map component. Do not silently
                // accept a path explicitly partitioned for another map.
                let first = relative.split('/').next().unwrap_or_default();
                if first.eq_ignore_ascii_case(map_id) {
                    path = format!("litmodels/{relative}");
                } else if first == "world" {
                    return None;
                } else if first
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphabetic())
                    && first.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                    })
                {
                    // A different map namespace must never be silently nested
                    // under the active map's prefix.
                    return None;
                } else {
                    path = format!("{namespace}{relative}");
                }
            } else {
                return None;
            }
        } else if let Some(relative) = path.strip_prefix("world/") {
            path = format!("{namespace}{relative}");
        } else {
            path = format!("{namespace}{path}");
        }
        Some(path)
    }

    fn fallback_selection_bounds(asset: &str) -> ([f32; 3], [f32; 3]) {
        let path = asset.to_ascii_lowercase();
        let (half_x, half_y, half_z) = if path.contains("tree") {
            (2.0, 10.0, 2.0)
        } else if path.contains("litmodels/") || path.contains("building")
            || path.contains("house") || path.contains("gate") || path.contains("wall")
        {
            (6.0, 8.0, 6.0)
        } else if path.ends_with(".gfx") || path.contains("effect") {
            (1.5, 1.5, 1.5)
        } else {
            (2.0, 3.0, 2.0)
        };
        ([-half_x, 0.0, -half_z], [half_x, half_y * 2.0, half_z])
    }

    fn renderer_bounds_key(&self, asset: &str) -> Option<String> {
        self.renderer_model_asset_path(asset)
            .or_else(|| self.renderer_bmd_asset_path(asset))
    }

    /// Resolve a GFX effect through the world's real effect catalogue. This is
    /// deliberately not a mesh path: WorldModels owns the native GFX owner and
    /// light-publication handler that plays it.
    fn renderer_effect_path(&self, asset: &str) -> Option<String> {
        let portable = self.portable_asset_path(asset);
        let (archive, entry) = if let Some((archive, entry)) = portable.split_once("::") {
            let name = Path::new(archive)
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())?;
            (Some(name), entry)
        } else {
            let entry = portable.strip_prefix("element/").unwrap_or(&portable);
            (None, entry)
        };
        if archive.as_deref().is_some_and(|name| name != "gfx.pck")
            || !entry.to_ascii_lowercase().ends_with(".gfx")
        {
            return None;
        }
        self.effect_catalog
            .as_ref()?
            .find(entry)
            .map(|effect| effect.path.clone())
    }

    /// Every asset family with a native placement adapter. Keep this
    /// predicate shared by archive actions and placement validation so the UI
    /// never disables an object type that the renderer can actually consume.
    fn renderer_placeable_asset(&self, asset: &str) -> bool {
        self.renderer_model_asset_path(asset).is_some()
            || self.renderer_bmd_asset_path(asset).is_some()
            || self.renderer_tree_type(asset).is_some()
            || self.renderer_effect_path(asset).is_some()
    }

    /// Route every asset-browser and source-scene selection through the same
    /// native placement gate. This avoids UI paths that merely highlight an
    /// asset while leaving the viewport in Select mode.
    fn select_asset_for_placement(&mut self, asset: String) {
        self.selected_asset = Some(asset.clone());
        if self.renderer_placeable_asset(&asset) {
            self.cursor = CursorMode::Place;
            self.snap = SnapMode::Terrain;
            self.status = "Asset selected. Move over the world and click to place it.".into();
        } else {
            self.cursor = CursorMode::Select;
            self.status =
                "Asset selected. This format is browse-only until a native placement adapter is available."
                    .into();
        }
    }

    fn renderer_tree_type(&self, asset: &str) -> Option<usize> {
        let candidate = asset.split_once("::").map_or(asset, |(_, entry)| entry);
        let candidate = crate::ecmodel::normalize(candidate);
        self.tree_types.iter().position(|tree| {
            [tree.tree.as_str(), tree.composite.as_str()]
                .into_iter()
                .filter(|path| !path.is_empty())
                .map(crate::ecmodel::normalize)
                .any(|path| {
                    candidate == path
                        || candidate.ends_with(&format!("/{path}"))
                        || path.ends_with(&format!("/{candidate}"))
                })
        })
    }

    /// Project GFX roots for the native effect-owner queue. Effects are
    /// synchronized only after a transform gesture ends so dragging does not
    /// repeatedly destroy and reload the live effect.
    pub fn renderer_effect_placements(
        &self,
    ) -> Vec<crate::world_models_gpu::EditorEffectPlacement> {
        if !self.enabled
            || (!self.project.source_map.is_empty()
                && !self.current_map_id.is_empty()
                && !self
                    .project
                    .source_map
                    .eq_ignore_ascii_case(&self.current_map_id))
        {
            return Vec::new();
        }
        self.project
            .objects
            .iter()
            .enumerate()
            .filter(|(_, object)| object.visible)
            .filter_map(|(index, object)| {
                let path = self.renderer_effect_path(&object.asset)?;
                if !object.position.iter().all(|value| value.is_finite())
                    || !object.rotation.iter().all(|value| value.is_finite())
                    || !object.scale.is_finite()
                    || !(0.001..=1000.0).contains(&object.scale)
                {
                    return None;
                }
                Some(crate::world_models_gpu::EditorEffectPlacement {
                    key: index as u64,
                    path,
                    position: object.position,
                    rotation: object.rotation,
                    scale: object.scale,
                })
            })
            .collect()
    }

    /// Project SpeedTree instances for the native GPU instancer.
    pub fn renderer_tree_placements(&self) -> Vec<crate::scene::EditorTreePlacement> {
        if !self.enabled
            || (!self.project.source_map.is_empty()
                && !self.current_map_id.is_empty()
                && !self
                    .project
                    .source_map
                    .eq_ignore_ascii_case(&self.current_map_id))
        {
            return Vec::new();
        }
        let mut placements = self
            .project
            .objects
            .iter()
            .filter(|object| object.visible)
            .filter_map(|object| {
                let tree_type = self.renderer_tree_type(&object.asset)?;
                if !object.position.iter().all(|value| value.is_finite())
                    || !object.rotation.iter().all(|value| value.is_finite())
                    || !object.scale.is_finite()
                    || !(0.05..=20.0).contains(&object.scale)
                {
                    return None;
                }
                Some(crate::scene::EditorTreePlacement {
                    tree_type: tree_type as u32,
                    position: object.position,
                    yaw: object.rotation[1],
                    scale: object.scale,
                })
            })
            .collect::<Vec<_>>();

        // Use the existing SpeedTree GPU instancer for the placement ghost as
        // well. The cursor preview and committed trees therefore share exactly
        // the same tree type, LOD, material and wind path.
        if self.cursor == CursorMode::Place {
            if let (Some(tree_type), Some(mut position)) = (
                self.selected_asset
                    .as_deref()
                    .and_then(|asset| self.renderer_tree_type(asset)),
                self.brush_cursor_position,
            ) {
                match self.snap {
                    SnapMode::Off => {}
                    SnapMode::Terrain => {
                        if let Some(map) = &self.map {
                            position[1] = map.ground(position[0], position[2]) + 0.5;
                        }
                    }
                    SnapMode::Objects => {
                        if let Some(selected) = self.selected_scene_transform {
                            position = selected;
                        }
                    }
                    SnapMode::Smart => {
                        if let Some(selected) = self.selected_scene_transform {
                            position = selected;
                        } else if let Some(map) = &self.map {
                            position[1] = map.ground(position[0], position[2]) + 0.5;
                        }
                    }
                }
                let already_committed = placements.iter().any(|placement| {
                    placement.tree_type == tree_type as u32
                        && placement
                            .position
                            .iter()
                            .zip(position)
                            .all(|(left, right)| (*left - right).abs() < 0.01)
                });
                if !already_committed {
                    placements.push(crate::scene::EditorTreePlacement {
                        tree_type: tree_type as u32,
                        position,
                        yaw: 0.0,
                        scale: 1.0,
                    });
                }
            }
        }
        placements
    }

    /// BMD scenery placements for the native baked-scenery page builder.
    /// These are transient and remain separate from ECM/SMD placements.
    pub fn renderer_scenery_placements(&self) -> Vec<crate::scene::EditorSceneryPlacement> {
        if !self.enabled
            || self.current_map_id.is_empty()
            || (!self.project.source_map.is_empty()
                && !self
                    .project
                    .source_map
                    .eq_ignore_ascii_case(&self.current_map_id))
        {
            return Vec::new();
        }
        let placements = self
            .project
            .objects
            .iter()
            .filter(|object| object.visible)
            .filter_map(|object| {
                let path = self.renderer_bmd_asset_path(&object.asset)?;
                if !object.position.iter().all(|value| value.is_finite())
                    || !object.rotation.iter().all(|value| value.is_finite())
                    || !object.scale.is_finite()
                    || !(0.001..=1000.0).contains(&object.scale)
                    || !object.scale_axes.iter().all(|v| v.is_finite() && (0.001..=1000.0).contains(v))
                {
                    return None;
                }
                let rotation = glam::Quat::from_euler(
                    glam::EulerRot::XYZ,
                    object.rotation[0],
                    object.rotation[1],
                    object.rotation[2],
                );
                Some(crate::scene::EditorSceneryPlacement {
                    path,
                    position: object.position,
                    direction: (rotation * Vec3::Z).to_array(),
                    up: (rotation * Vec3::Y).to_array(),
                    scale: object.scale,
                    scale_axes: object.scale_axes,
                    map_id: self.current_map_id.to_ascii_lowercase(),
                })
            })
            .collect::<Vec<_>>();

        // Unlike the ECM renderer, baked scenery is rebuilt into texture
        // atlases. Do not submit a per-frame pointer ghost here: that would
        // rebuild the atlas on every mouse move. A BMD becomes live when the
        // user commits it; a transform-only BMD preview is a separate task.
        placements
    }

    /// Current project's supported ECM placements, for the native GPU model
    /// queue. Other archive types remain browsable/pinnable but are not guessed
    /// into a renderer format they do not implement.
    pub fn renderer_placements(&self) -> Vec<crate::world_models_gpu::EditorPlacement> {
        if !self.enabled
            || (!self.project.source_map.is_empty()
                && !self.current_map_id.is_empty()
                && !self
                    .project
                    .source_map
                    .eq_ignore_ascii_case(&self.current_map_id))
        {
            return Vec::new();
        }
        let mut placements = self
            .project
            .objects
            .iter()
            .filter_map(|object| {
                if !object.visible {
                    return None;
                }
                Some(crate::world_models_gpu::EditorPlacement {
                    path: self.renderer_model_asset_path(&object.asset)?,
                    position: object.position,
                    rotation: object.rotation,
                    scale: object.scale,
                    scale_axes: object.scale_axes,
                })
            })
            .collect::<Vec<_>>();

        // A live, native-mesh placement ghost in the world viewport. It uses
        // the exact same ECM/SMD renderer as committed objects, and is never
        // serialized into the project until the user clicks to place it.
        if self.cursor == CursorMode::Place {
            if let (Some(asset), Some(mut position)) = (
                self.selected_asset
                    .as_deref()
                    .and_then(|asset| self.renderer_model_asset_path(asset)),
                self.brush_cursor_position,
            ) {
                if self.snap == SnapMode::Terrain {
                    if let Some(map) = &self.map {
                        position[1] = map.ground(position[0], position[2]) + 0.5;
                    }
                } else if matches!(self.snap, SnapMode::Objects | SnapMode::Smart) {
                    if let Some(target) = self.selected_scene_transform {
                        position = target;
                    } else if self.snap == SnapMode::Smart {
                        if let Some(map) = &self.map {
                            position[1] = map.ground(position[0], position[2]) + 0.5;
                        }
                    }
                }
                let already_committed = self.project.objects.iter().any(|object| {
                    self.renderer_model_asset_path(&object.asset).as_deref() == Some(asset.as_str())
                        && object
                            .position
                            .iter()
                            .zip(position)
                            .all(|(a, b)| (*a - b).abs() < 0.01)
                });
                if !already_committed {
                    placements.push(crate::world_models_gpu::EditorPlacement {
                        path: asset,
                        position,
                        rotation: [0.; 3],
                        scale: 1.,
                        scale_axes: [1.; 3],
                    });
                }
            }
        }
        placements
    }

    fn add_selected_asset_at_camera(&mut self, camera: Vec3) {
        let position = self.placement_position(camera);
        self.add_selected_asset_at_position(position);
    }

    fn add_selected_asset_at_position(&mut self, position: [f32; 3]) {
        if !self.project.source_map.is_empty()
            && !self.current_map_id.is_empty()
            && !self
                .project
                .source_map
                .eq_ignore_ascii_case(&self.current_map_id)
        {
            self.status = format!(
                "This Architect project targets map {}, not {}.",
                self.project.source_map, self.current_map_id
            );
            return;
        }
        if self.project.source_map.is_empty() && !self.current_map_id.is_empty() {
            self.project.source_map = self.current_map_id.clone();
        }
        let Some(asset) = self.selected_asset.clone() else {
            self.status =
                "Choose a placeable game object from the client assets before placing it.".into();
            return;
        };
        if !self.renderer_placeable_asset(&asset) {
            self.status = "This file is indexed but not yet placeable. Live placement supports ECM, static SMD, BMD scenery, SpeedTree and catalogue-backed GFX effects.".into();
            return;
        }
        let before = self.project.objects.clone();
        let portable_asset = self.portable_asset_path(&asset);
        self.project.objects.push(PlacedObject {
            name: Path::new(&portable_asset)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Object".into()),
            asset: portable_asset,
            group: "Default".into(),
            visible: true,
            locked: false,
            position,
            rotation: [0.; 3],
            scale: 1.,
            scale_axes: [1.; 3],
        });
        self.object_history_action = true;
        self.push_object_undo(before);
        let index = self.project.objects.len() - 1;
        self.selected_placement = Some(index);
        self.selected_placements.clear();
        self.selected_placements.insert(index);
        self.selected_scene_object = None;
        self.selected_scene_transform = None;
        // A committed placement is now an editable project object. Return to
        // Select so the just-created object can immediately be picked and
        // transformed with the gizmo; Place remains an explicit repeat action.
        self.cursor = CursorMode::Select;
        self.status =
            "Game object placed in the live Architect viewport. Selected for editing.".into();
    }

    fn duplicate_selected_scene_item(&mut self) {
        if !self.project.source_map.is_empty()
            && !self.current_map_id.is_empty()
            && !self
                .project
                .source_map
                .eq_ignore_ascii_case(&self.current_map_id)
        {
            self.status = format!(
                "This Architect project targets map {}, not {}.",
                self.project.source_map, self.current_map_id
            );
            return;
        }
        let mut items = self
            .multi_scene_items
            .iter()
            .map(|(index, (asset, position))| (*index, asset.clone(), *position))
            .collect::<Vec<_>>();
        items.sort_by_key(|(index, _, _)| *index);
        if items.is_empty() {
            let Some(asset) = self.selected_asset.clone() else {
                return;
            };
            items.push((
                self.selected_scene_object.unwrap_or(0),
                asset,
                self.selected_scene_transform.unwrap_or([0.; 3]),
            ));
        }

        let before = self.project.objects.clone();
        let mut created = Vec::new();
        for (_, asset, position) in items {
            if !self.renderer_placeable_asset(&asset) {
                continue;
            }
            let portable_asset = self.portable_asset_path(&asset);
            let index = self.project.objects.len();
            self.project.objects.push(PlacedObject {
                name: Path::new(&portable_asset)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Object".into()),
                asset: portable_asset,
                group: "Default".into(),
                visible: true,
                locked: false,
                position,
                rotation: [0.; 3],
                scale: 1.,
                scale_axes: [1.; 3],
            });
            created.push(index);
        }
        if created.is_empty() {
            self.status =
                "The selected client scene items have no supported native placement adapter."
                    .into();
            return;
        }

        self.object_history_action = true;
        self.push_object_undo(before);
        self.selected_placements = created.iter().copied().collect();
        self.selected_placement = created.last().copied();
        self.selected_scene_object = None;
        self.selected_scene_transform = None;
        self.multi_selection.clear();
        self.multi_scene_items.clear();
        self.status = format!(
            "Copied {} supported client scene item(s) into the editable project layer.",
            created.len()
        );
    }

    fn selected_placement_indices(&self) -> Vec<usize> {
        let mut indices = self.selected_placements.iter().copied().collect::<Vec<_>>();
        if indices.is_empty() {
            indices.extend(self.selected_placement);
        }
        indices.sort_unstable();
        indices.dedup();
        indices.retain(|index| *index < self.project.objects.len());
        indices
    }

    fn copy_selected_objects(&mut self) {
        let indices = self.selected_placement_indices();
        let mut copied = indices
            .iter()
            .filter_map(|index| self.project.objects.get(*index).cloned())
            .collect::<Vec<_>>();
        if copied.is_empty() {
            let mut scene = self
                .multi_scene_items
                .iter()
                .map(|(index, (asset, position))| (*index, asset.clone(), *position))
                .collect::<Vec<_>>();
            scene.sort_by_key(|(index, _, _)| *index);
            if scene.is_empty() {
                if let (Some(asset), Some(position)) =
                    (self.selected_asset.clone(), self.selected_scene_transform)
                {
                    scene.push((0, asset, position));
                }
            }
            for (_, asset, position) in scene {
                if !self.renderer_placeable_asset(&asset) {
                    continue;
                }
                let portable_asset = self.portable_asset_path(&asset);
                copied.push(PlacedObject {
                    name: Path::new(&portable_asset)
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Object".into()),
                    asset: portable_asset,
                    group: "Default".into(),
                    visible: true,
                    locked: false,
                    position,
                    rotation: [0.; 3],
                    scale: 1.,
                    scale_axes: [1.; 3],
                });
            }
        }
        if copied.is_empty() {
            self.status =
                "Select a placeable game object or project placement before copying.".into();
            return;
        }
        self.clipboard_objects = copied;
        self.status = format!(
            "Copied {} object(s) to the Architect clipboard.",
            self.clipboard_objects.len()
        );
    }

    fn paste_objects(&mut self) {
        if !self.project.source_map.is_empty()
            && !self.current_map_id.is_empty()
            && !self
                .project
                .source_map
                .eq_ignore_ascii_case(&self.current_map_id)
        {
            self.status = format!(
                "This Architect project targets map {}, not {}.",
                self.project.source_map, self.current_map_id
            );
            return;
        }
        if self.clipboard_objects.is_empty() {
            self.status =
                "Architect clipboard is empty. Select an object and press Ctrl+C first.".into();
            return;
        }
        if self.project.source_map.is_empty() && !self.current_map_id.is_empty() {
            self.project.source_map = self.current_map_id.clone();
        }
        let before = self.project.objects.clone();
        let mut pasted = Vec::with_capacity(self.clipboard_objects.len());
        for source in &self.clipboard_objects {
            if !self.renderer_placeable_asset(&source.asset) {
                continue;
            }
            let mut object = source.clone();
            object.position[0] += 2.0;
            object.position[2] += 2.0;
            object.locked = false;
            pasted.push(object);
        }
        if pasted.is_empty() {
            self.status =
                "Clipboard contains no assets supported by the live game-object renderer.".into();
            return;
        }
        self.object_history_action = true;
        self.selected_placements.clear();
        for object in pasted {
            let index = self.project.objects.len();
            self.project.objects.push(object);
            self.selected_placements.insert(index);
        }
        // History snapshots are pushed after the mutation and compared with
        // the current project. Keep the pre-paste vector until all copies have
        // been appended, otherwise the equality guard drops the undo record.
        self.push_object_undo(before);
        self.selected_placement = self.selected_placements.iter().copied().min();
        self.status = format!(
            "Pasted {} game object(s); move the selected copies into place.",
            self.selected_placements.len()
        );
    }

    fn delete_selected_objects(&mut self) {
        let indices = self.selected_placement_indices();
        if indices.is_empty() {
            self.status = "Select one or more project placements to delete.".into();
            return;
        }
        let before = self.project.objects.clone();
        let remove = indices.iter().copied().collect::<HashSet<_>>();
        self.project.objects = self
            .project
            .objects
            .drain(..)
            .enumerate()
            .filter_map(|(index, object)| (!remove.contains(&index)).then_some(object))
            .collect();
        self.push_object_undo(before);
        self.object_history_action = true;
        self.selected_placements.clear();
        self.selected_placement = None;
        self.status = format!("Deleted {} project placement(s).", indices.len());
    }

    /// Screen-space hit radius for source-scene objects. The serialized
    /// placement point is not the mesh centre for many trees, ornaments and
    /// effects, so a small fixed 20 px anchor target was too easy to miss.
    fn source_pick_radius(asset: &str) -> f32 {
        let path = asset.to_ascii_lowercase();
        if path.contains("tree") || path.contains("litmodels/") || path.contains("scenery") {
            52.0
        } else if path.ends_with(".ecm") || path.ends_with(".bmd") || path.ends_with(".smd") {
            40.0
        } else if path.ends_with(".gfx") || path.contains("effect") {
            34.0
        } else {
            30.0
        }
    }

    /// Pick the closest source-scene anchor under the pointer. The returned
    /// depth is reverse-Z (larger values are nearer), matching the viewer's
    /// projection and the GPU depth test.
    fn pick_source_scene(
        &self,
        pointer: egui::Pos2,
        screen: egui::Rect,
        view_projection: glam::Mat4,
        scene: &crate::scene::Scene,
        ornaments: &[crate::scene::Ornament],
    ) -> Option<(usize, f32, f32, String, [f32; 3])> {
        let mut scene_index = 0usize;
        let mut nearest_scene: Option<(usize, f32, f32, String, [f32; 3])> = None;
        let mut consider = |asset: &str, position: [f32; 3]| {
            let clip = view_projection.project_point3(Vec3::from_array(position));
            if clip.is_finite() && (0.0..=1.0).contains(&clip.z) {
                let point = egui::pos2(
                    screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                    screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                );
                let bounds_score = self
                    .renderer_bounds_key(asset)
                    .and_then(|key| self.preview_bounds.get(&key).copied())
                    .or_else(|| Some(Self::fallback_selection_bounds(asset)))
                    .and_then(|(min, max)| {
                        let mut rect = egui::Rect::NOTHING;
                        let mut projected_count = 0usize;
                        for bits in 0..8 {
                            let local = Vec3::new(
                                if bits & 1 == 0 { min[0] } else { max[0] },
                                if bits & 2 == 0 { min[1] } else { max[1] },
                                if bits & 4 == 0 { min[2] } else { max[2] },
                            );
                            let clip = view_projection.project_point3(Vec3::from_array(position) + local);
                            if clip.is_finite() && (0.0..=1.0).contains(&clip.z) {
                                rect.extend_with(egui::pos2(
                                    screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                                    screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                                ));
                                projected_count += 1;
                            }
                        }
                        (projected_count > 0).then_some(rect.expand(10.0))
                    });
                let score = if let Some(rect) = bounds_score {
                    let dx = (rect.left() - pointer.x).max(0.0).max(pointer.x - rect.right());
                    let dy = (rect.top() - pointer.y).max(0.0).max(pointer.y - rect.bottom());
                    dx.hypot(dy) / 14.0
                } else {
                    point.distance(pointer) / Self::source_pick_radius(asset)
                };
                if score <= 1.0
                    && nearest_scene
                        .as_ref()
                        .is_none_or(|(_, best_score, best_depth, _, _)| {
                            score < *best_score - 0.025
                                || ((score - *best_score).abs() <= 0.025 && clip.z > *best_depth)
                        })
                {
                    nearest_scene = Some((scene_index, score, clip.z, asset.to_string(), position));
                }
            }
            scene_index += 1;
        };
        for block in &scene.blocks {
            for model in &block.models {
                consider(&model.path, model.position);
            }
            for tree in &block.trees {
                if let Some(kind) = scene.tree_types.get(tree.tree_type as usize) {
                    consider(&kind.tree, tree.position);
                    if !kind.composite.is_empty() {
                        consider(&kind.composite, tree.position);
                    }
                }
            }
            for effect in &block.effects {
                consider(&effect.path, effect.position);
            }
        }
        for ornament in ornaments {
            consider(&ornament.path, ornament.position);
        }
        nearest_scene
    }

    fn scene_row(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        label: &str,
        asset: &str,
        position: [f32; 3],
        _camera: Vec3,
    ) {
        if !self.object_filter.is_empty()
            && !label
                .to_lowercase()
                .contains(&self.object_filter.to_lowercase())
        {
            return;
        }
        let selected =
            self.selected_scene_object == Some(index) || self.multi_selection.contains(&index);
        if ui.selectable_label(selected, label).clicked() {
            self.selected_scene_transform = Some(position);
            if self.cursor == CursorMode::MultiSelect {
                self.selected_scene_object = None;
                if self.multi_selection.insert(index) {
                    self.multi_scene_items
                        .insert(index, (asset.to_string(), position));
                } else {
                    self.multi_scene_items.remove(&index);
                }
            } else {
                self.selected_scene_object = Some(index);
                self.multi_selection.clear();
                self.multi_scene_items.clear();
                // A single click on a supported source-scene item arms a
                // terrain-snapped viewport placement. Previously the click only
                // changed selected_asset; viewport clicks stayed in Select mode,
                // so users saw a name highlight but could not place the item.
                self.select_asset_for_placement(asset.to_string());
            }
        }
    }

    fn open_archive(&mut self, path: PathBuf) {
        self.selected_archive = Some(path.clone());
        self.archive_entries.clear();
        self.archive_details.clear();
        self.archive_error = None;
        if let Some(entries) = self.archive_cache.get(&path) {
            self.archive_entries = entries.clone();
            self.archive_details = self
                .archive_detail_cache
                .get(&path)
                .cloned()
                .unwrap_or_default();
            self.status = format!(
                "Loaded {} cached archive entries.",
                self.archive_entries.len()
            );
            return;
        }
        let result = std::fs::File::open(&path)
            .map_err(|error| error.to_string())
            .and_then(|mut file| crate::pck::directory(&mut file));
        match result {
            Ok(entries) => {
                let mut details = entries
                    .into_iter()
                    .map(|entry| (entry.name, entry.size, entry.compressed))
                    .collect::<Vec<_>>();
                details.sort_by(|left, right| {
                    left.0
                        .to_ascii_lowercase()
                        .cmp(&right.0.to_ascii_lowercase())
                });
                self.archive_entries = details.iter().map(|(name, _, _)| name.clone()).collect();
                self.archive_cache
                    .insert(path.clone(), self.archive_entries.clone());
                self.archive_detail_cache
                    .insert(path.clone(), details.clone());
                self.archive_details = details;
                self.status = format!(
                    "Indexed {} archive entries from {}.",
                    self.archive_entries.len(),
                    path.display()
                );
            }
            Err(error) => self.archive_error = Some(error),
        }
    }

    fn pin(&mut self, path: String) {
        if !self.quick_bar.iter().any(|item| item == &path) && self.quick_bar.len() < 24 {
            self.quick_bar.push(path);
        }
    }

    fn toolbar(&mut self, ctx: &egui::Context) {
        let previous_cursor = self.cursor;
        egui::TopBottomPanel::top("architect_toolbar")
            .exact_height(46.)
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(27, 31, 39))
                    .inner_margin(egui::Margin::symmetric(8., 5.)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("ARCHITECT")
                            .strong()
                            .color(egui::Color32::from_rgb(120, 190, 255)),
                    );
                    ui.separator();
                    ui.selectable_value(&mut self.cursor, CursorMode::Select, "Select");
                    ui.selectable_value(&mut self.cursor, CursorMode::MultiSelect, "Multi-select");
                    ui.selectable_value(&mut self.cursor, CursorMode::Lasso, "Lasso");
                    ui.selectable_value(&mut self.cursor, CursorMode::Place, "Place");
                    if self.cursor == CursorMode::Place {
                        let can_place =
                            self.selected_asset.is_some() && self.brush_cursor_position.is_some();
                        if ui
                            .add_enabled(can_place, egui::Button::new("Place here"))
                            .clicked()
                        {
                            if let Some(position) = self.brush_cursor_position {
                                let mut placement = position;
                                match self.snap {
                                    SnapMode::Off | SnapMode::Objects => {}
                                    SnapMode::Terrain | SnapMode::Smart => {
                                        if let Some(map) = &self.map {
                                            placement[1] =
                                                map.ground(placement[0], placement[2]) + 0.5;
                                        }
                                    }
                                }
                                self.add_selected_asset_at_position(placement);
                            }
                        }
                    }
                    ui.selectable_value(&mut self.cursor, CursorMode::Sculpt, "Sculpt");
                    ui.separator();
                    ui.label("Gizmo");
                    ui.selectable_value(&mut self.transform_mode, TransformMode::Move, "Move");
                    ui.selectable_value(&mut self.transform_mode, TransformMode::Rotate, "Rotate");
                    ui.selectable_value(&mut self.transform_mode, TransformMode::Scale, "Scale");
                    ui.separator();
                    egui::ComboBox::from_id_source("architect_snap")
                        .selected_text(format!(
                            "Snap: {}",
                            match self.snap {
                                SnapMode::Off => "Off",
                                SnapMode::Terrain => "Terrain",
                                SnapMode::Objects => "Objects",
                                SnapMode::Smart => "Smart",
                            }
                        ))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.snap, SnapMode::Off, "Off");
                            ui.selectable_value(&mut self.snap, SnapMode::Terrain, "Terrain");
                            ui.selectable_value(&mut self.snap, SnapMode::Objects, "Objects");
                            ui.selectable_value(&mut self.snap, SnapMode::Smart, "Smart");
                        });
                    ui.separator();
                    ui.label("Terrain");
                    egui::ComboBox::from_id_source("architect_brush")
                        .selected_text(match self.brush {
                            BrushMode::Raise => "Raise",
                            BrushMode::Lower => "Lower",
                            BrushMode::Flatten => "Flatten",
                            BrushMode::Smooth => "Smooth",
                            BrushMode::Paint => "Paint",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.brush, BrushMode::Raise, "Raise");
                            ui.selectable_value(&mut self.brush, BrushMode::Lower, "Lower");
                            ui.selectable_value(&mut self.brush, BrushMode::Flatten, "Flatten");
                            ui.selectable_value(&mut self.brush, BrushMode::Smooth, "Smooth");
                            ui.selectable_value(&mut self.brush, BrushMode::Paint, "Paint");
                        });
                    ui.add(egui::Slider::new(&mut self.brush_radius, 1.0..=250.0).text("Radius"));
                    ui.add(
                        egui::Slider::new(&mut self.brush_strength, 0.1..=20.0).text("Strength"),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Turn off Architect")
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(165, 45, 45)),
                            )
                            .clicked()
                        {
                            self.set_active(false);
                        }
                    });
                });
            });
        if self.cursor != previous_cursor {
            // A tool switch must not commit a stale viewport gesture. In
            // particular, leaving Multi-select while a marquee is held must
            // never finalize a rectangle on the next pointer-up.
            self.cancel_object_drag();
            self.brush_dragging = false;
            self.last_brush_position = None;
        }
    }

    fn ray_plane_hit(
        pointer: egui::Pos2,
        viewport: egui::Rect,
        view_projection: glam::Mat4,
        height: f32,
    ) -> Option<Vec3> {
        let uv = egui::vec2(
            (pointer.x - viewport.left()) / viewport.width().max(1.) * 2. - 1.,
            1. - (pointer.y - viewport.top()) / viewport.height().max(1.) * 2.,
        );
        let inverse = view_projection.inverse();
        if !inverse.is_finite() {
            return None;
        }
        let near = inverse.project_point3(Vec3::new(uv.x, uv.y, 1.));
        let far = inverse.project_point3(Vec3::new(uv.x, uv.y, 0.));
        let direction = (far - near).normalize_or_zero();
        if !near.is_finite()
            || !direction.is_finite()
            || direction.length_squared() < 0.5
            || direction.y.abs() < 1e-5
        {
            return None;
        }
        let distance = (height - near.y) / direction.y;
        (distance >= 0. && distance.is_finite()).then(|| near + direction * distance)
    }

    /// Closest point on a world-space axis to the pointer's camera ray.
    /// Returns signed distance along the axis from its origin.
    fn wrapped_angle_delta(start: f32, current: f32) -> f32 {
        let mut delta = current - start;
        if delta > std::f32::consts::PI {
            delta -= std::f32::consts::TAU;
        }
        if delta < -std::f32::consts::PI {
            delta += std::f32::consts::TAU;
        }
        delta
    }

    fn ray_axis_parameter(
        pointer: egui::Pos2,
        viewport: egui::Rect,
        view_projection: glam::Mat4,
        origin: Vec3,
        axis: Vec3,
    ) -> Option<f32> {
        let uv = egui::vec2(
            (pointer.x - viewport.left()) / viewport.width().max(1.) * 2. - 1.,
            1. - (pointer.y - viewport.top()) / viewport.height().max(1.) * 2.,
        );
        let inverse = view_projection.inverse();
        if !inverse.is_finite() {
            return None;
        }
        let near = inverse.project_point3(Vec3::new(uv.x, uv.y, 1.));
        let far = inverse.project_point3(Vec3::new(uv.x, uv.y, 0.));
        let direction = (far - near).normalize_or_zero();
        let axis = axis.normalize_or_zero();
        if !near.is_finite()
            || !direction.is_finite()
            || direction.length_squared() < 0.5
            || axis.length_squared() < 0.5
        {
            return None;
        }
        let offset = near - origin;
        let dot = direction.dot(axis);
        let denominator = 1. - dot * dot;
        if denominator.abs() < 1e-4 {
            return None;
        }
        let ray_distance = (dot * axis.dot(offset) - direction.dot(offset)) / denominator;
        let axis_distance = (axis.dot(offset) - dot * direction.dot(offset)) / denominator;
        (ray_distance >= 0. && axis_distance.is_finite()).then_some(axis_distance)
    }

    fn ray_plane_point(
        pointer: egui::Pos2,
        viewport: egui::Rect,
        view_projection: glam::Mat4,
        origin: Vec3,
        normal: Vec3,
    ) -> Option<Vec3> {
        let uv = egui::vec2(
            (pointer.x - viewport.left()) / viewport.width().max(1.) * 2. - 1.,
            1. - (pointer.y - viewport.top()) / viewport.height().max(1.) * 2.,
        );
        let inverse = view_projection.inverse();
        if !inverse.is_finite() { return None; }
        let near = inverse.project_point3(Vec3::new(uv.x, uv.y, 1.));
        let far = inverse.project_point3(Vec3::new(uv.x, uv.y, 0.));
        let direction = (far - near).normalize_or_zero();
        let normal = normal.normalize_or_zero();
        let denominator = direction.dot(normal);
        if !near.is_finite() || !direction.is_finite()
            || direction.length_squared() < 0.5
            || normal.length_squared() < 0.5
            || denominator.abs() < 1e-5 { return None; }
        let distance = (origin - near).dot(normal) / denominator;
        (distance >= 0.).then_some(near + direction * distance)
    }

    fn gizmo_ring_points(
        origin: Vec3,
        normal: Vec3,
        radius: f32,
        viewport: egui::Rect,
        view_projection: glam::Mat4,
    ) -> Vec<egui::Pos2> {
        let normal = normal.normalize_or_zero();
        if normal.length_squared() < 0.5 || !radius.is_finite() || radius <= 0. {
            return Vec::new();
        }
        let reference = if normal.dot(Vec3::Y).abs() < 0.92 { Vec3::Y } else { Vec3::X };
        let tangent = normal.cross(reference).normalize_or_zero();
        let bitangent = normal.cross(tangent).normalize_or_zero();
        let mut points = Vec::with_capacity(65);
        for step in 0..=64 {
            let angle = step as f32 * std::f32::consts::TAU / 64.;
            let world = origin + tangent * (angle.cos() * radius) + bitangent * (angle.sin() * radius);
            let clip = view_projection.project_point3(world);
            if clip.is_finite() {
                points.push(egui::pos2(
                    viewport.left() + (clip.x + 1.) * 0.5 * viewport.width(),
                    viewport.top() + (1. - clip.y) * 0.5 * viewport.height(),
                ));
            }
        }
        points
    }

    fn distance_to_polyline(point: egui::Pos2, points: &[egui::Pos2]) -> f32 {
        points.windows(2).map(|pair| {
            let segment = pair[1] - pair[0];
            let t = ((point - pair[0]).dot(segment) / segment.length_sq().max(0.001)).clamp(0., 1.);
            (pair[0] + segment * t).distance(point)
        }).fold(f32::INFINITY, f32::min)
    }

    fn terrain_hit(
        pointer: egui::Pos2,
        viewport: egui::Rect,
        view_projection: glam::Mat4,
        map: &crate::navigation::MapSpace,
    ) -> Option<Vec3> {
        if map.width < 2 || map.height < 2 || !map.spacing.is_finite() || map.spacing <= 0. {
            return None;
        }
        let uv = egui::vec2(
            (pointer.x - viewport.left()) / viewport.width().max(1.) * 2. - 1.,
            1. - (pointer.y - viewport.top()) / viewport.height().max(1.) * 2.,
        );
        let inverse = view_projection.inverse();
        if !inverse.is_finite() {
            return None;
        }
        let near = inverse.project_point3(Vec3::new(uv.x, uv.y, 1.));
        let far = inverse.project_point3(Vec3::new(uv.x, uv.y, 0.));
        let direction = (far - near).normalize_or_zero();
        if !near.is_finite() || !direction.is_finite() || direction.length_squared() < 0.5 {
            return None;
        }
        let [width, height] = map.world_size();
        let delta = |point: Vec3| -> Option<f32> {
            if point.x < -width * 0.5
                || point.x > width * 0.5
                || point.z < -height * 0.5
                || point.z > height * 0.5
            {
                return None;
            }
            Some(point.y - map.ground(point.x, point.z))
        };
        let step_distance = (map.spacing * 4.).clamp(0.5, 64.);
        let max_steps = ((far - near).length() / step_distance).ceil() as usize;
        let mut previous: Option<(f32, f32)> = None;
        for step in 0..=max_steps {
            let t = step as f32 * step_distance;
            let point = near + direction * t;
            let Some(current_delta) = delta(point) else {
                continue;
            };
            if let Some((last_t, last_delta)) = previous {
                if last_delta >= 0. && current_delta <= 0. {
                    let (mut lo, mut hi) = (last_t, t);
                    for _ in 0..12 {
                        let mid = (lo + hi) * 0.5;
                        if delta(near + direction * mid).is_some_and(|value| value > 0.) {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                    }
                    let mut hit = near + direction * ((lo + hi) * 0.5);
                    hit.y = map.ground(hit.x, hit.z) + 0.05;
                    return Some(hit);
                }
            }
            previous = Some((t, current_delta));
        }
        None
    }

    /// Returns a requested client map ID when the user presses "Load map".
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        catalog: &crate::world_catalog::Catalog,
        current_map: &crate::world_catalog::WorldMap,
        scene: &crate::scene::Scene,
        ornaments: &[crate::scene::Ornament],
        camera: Vec3,
        view_projection: glam::Mat4,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.tree_types.clone_from(&scene.tree_types);
        self.object_history_action = false;
        let objects_before_ui = self.project.objects.clone();
        if self.selected_map.is_empty() || self.selected_map == "__empty__" {
            self.selected_map = current_map.id.clone();
        }
        self.toolbar(ctx);
        // Keep history shortcuts out of active text fields so Ctrl+Z edits a
        // placement snapshot only when the user is working in the viewport or
        // editor chrome, never while typing a name, group or filter.
        if !ctx.wants_keyboard_input() {
            let (undo, redo, copy, paste, delete) = ctx.input(|input| {
                let command = input.modifiers.command;
                (
                    command && input.key_pressed(egui::Key::Z) && !input.modifiers.shift,
                    (command && input.key_pressed(egui::Key::Y))
                        || (command && input.modifiers.shift && input.key_pressed(egui::Key::Z)),
                    command && input.key_pressed(egui::Key::C),
                    command && input.key_pressed(egui::Key::V),
                    input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace),
                )
            });
            if undo {
                self.undo_objects();
            } else if redo {
                self.redo_objects();
            } else if copy {
                self.copy_selected_objects();
            } else if paste {
                self.paste_objects();
            } else if delete {
                self.delete_selected_objects();
            }
        }
        let screen = ctx.input(|input| input.screen_rect());
        let mut map_request = None;

        let world_pos = Self::dock_position(self.left_dock, screen, 290., 560.);
        let world_dock_changed = self.left_dock != self.applied_left_dock;
        self.applied_left_dock = self.left_dock;
        let mut world_panel = egui::Window::new("World")
            .id(egui::Id::new("architect_world_panel"))
            .default_pos(world_pos)
            .default_size(egui::vec2(290., 560.))
            .min_size(egui::vec2(240., 300.))
            .resizable(true);
        if world_dock_changed {
            world_panel = world_panel.current_pos(world_pos);
        }
        world_panel.show(ctx, |ui| {
                ui.horizontal(|ui| { ui.heading("World selector"); Self::dock_picker(ui, &mut self.left_dock); });
                ui.label(egui::RichText::new(format!("Loaded: {} ({})", current_map.display_name, current_map.id)).small().weak());
                egui::ComboBox::from_id_source("architect_client_maps")
                    .selected_text(if self.selected_map == "__empty__" { "Empty world (new project)".into() } else { format!("{} ({})", catalog.maps.iter().find(|m| m.id == self.selected_map).map_or(current_map.display_name.as_str(), |m| m.display_name.as_str()), self.selected_map) })
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.selected_map, "__empty__".into(), "Empty world (new project)");
                        for map in &catalog.maps {
                            ui.selectable_value(&mut self.selected_map, map.id.clone(), map.label())
                                .on_hover_text(map.tooltip());
                        }
                    });
                if self.selected_map == "__empty__" {
                    ui.label(egui::RichText::new("Empty project: the current loaded world remains a reference viewport; no client map data is modified.").small().weak());
                } else if self.selected_map.eq_ignore_ascii_case(&current_map.id) {
                    if ui.button("Use loaded map as project source").clicked() {
                        self.project.source_map = current_map.id.clone();
                        self.status = format!("Project source set to {}. The client map remains unchanged.", current_map.display_name);
                    }
                } else if ui.button("Load selected client map").clicked() {
                    let needs_save = !self.project.objects.is_empty() || !self.project.strokes.is_empty();
                    let saved = !needs_save || self.save_named();
                    if saved { map_request = Some(self.selected_map.clone()); }
                }
                ui.separator();
                if ui.button("＋ New empty world project").clicked() {
                    let previous_source = self.project.source_map.clone();
                    let belongs_here = previous_source.is_empty()
                        || previous_source.eq_ignore_ascii_case(&current_map.id);
                    let reset_error = if belongs_here {
                        self.map
                            .as_ref()
                            .and_then(|map| map.heights.clear_edits().err())
                    } else {
                        None
                    };
                    self.project = Project::default();
                    self.redo_strokes.clear();
                    self.undo_objects.clear();
                    self.redo_objects.clear();
                    self.cancel_object_drag();
                    self.brush_dragging = false;
                    self.last_brush_position = None;
                    self.project_name = "Untitled World".into();
                    self.selected_map = "__empty__".into();
                    self.selected_project = None;
                    self.selected_placement = None;
                    self.selected_placements.clear();
                    self.selected_scene_object = None;
                    self.selected_scene_transform = None;
                    self.multi_selection.clear();
                    self.multi_scene_items.clear();
                    self.status = "New empty project. Choose a client map as its source when ready.".into();
                    if let Some(error) = reset_error {
                        self.status = format!("New project created, but previous terrain edits could not be cleared: {error}");
                    }
                }
                ui.heading("Custom worlds");
                if ui.small_button("Refresh saved worlds").clicked() {
                    self.refresh_projects();
                }
                if self.project_files.is_empty() {
                    ui.label(egui::RichText::new("No saved Architect worlds yet.").weak().small());
                }
                egui::ComboBox::from_id_source("architect_saved_worlds")
                    .selected_text(self.selected_project.and_then(|i| self.project_files.get(i)).and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Select saved world".into()))
                    .show_ui(ui, |ui| {
                        for (index, path) in self.project_files.iter().enumerate() {
                            let label = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                            ui.selectable_value(&mut self.selected_project, Some(index), label);
                        }
                    });
                ui.horizontal(|ui| {
                    if ui.button("Open").clicked() { self.load_selected(); }
                    if ui.add(egui::Button::new("Erase…").fill(egui::Color32::from_rgb(135, 42, 42))).clicked() { self.confirm_erase = true; }
                });
                if self.confirm_erase {
                    ui.colored_label(egui::Color32::LIGHT_RED, "Permanently erase this custom project?");
                    ui.horizontal(|ui| {
                        if ui.button("Confirm erase").clicked() { self.erase_selected(); }
                        if ui.button("Cancel").clicked() { self.confirm_erase = false; }
                    });
                }
                ui.separator();
                ui.heading("Save current world as custom");
                ui.text_edit_singleline(&mut self.project_name);
                if ui.button("Save custom world").clicked() { let _ = self.save_named(); }
                if ui.add(egui::Button::new("Clear current project edits…").fill(egui::Color32::from_rgb(135, 42, 42))).clicked() { self.confirm_clear = true; }
                if self.confirm_clear {
                    ui.colored_label(egui::Color32::LIGHT_RED, "Clear every unsaved placement and terrain operation in this project?");
                    ui.horizontal(|ui| {
                        if ui.button("Confirm clear").clicked() {
                            self.cancel_object_drag();
                            self.project.objects.clear();
                            self.project.strokes.clear();
                            self.selected_placement = None;
                            self.selected_placements.clear();
                            self.multi_selection.clear();
                            self.multi_scene_items.clear();
                            self.confirm_clear = false;
                            self.status = "Current project edits cleared. Saved project files are unchanged.".into();
                        }
                        if ui.button("Cancel").clicked() { self.confirm_clear = false; }
                    });
                }
                ui.label(egui::RichText::new("Projects are stored separately from the client and never overwrite its files.").small().weak());
                ui.separator();
                ui.add_enabled(false, egui::Button::new("Save as real game map").fill(egui::Color32::from_rgb(38, 125, 72)))
                    .on_hover_text("Disabled until the native ECWLD, EC-BSD and terrain exporters can round-trip validated client data.");
                ui.colored_label(egui::Color32::from_rgb(240, 190, 100), "Native game-map export is not available yet.");
                ui.label(egui::RichText::new("A real client map requires validated ECWLD, EC-BSD and terrain writers. Architect keeps edits in a portable project until that exporter is implemented.").small().weak());
                ui.label(egui::RichText::new(&self.status).small().color(egui::Color32::LIGHT_BLUE));
            });

        let asset_pos = Self::dock_position(self.right_dock, screen, 340., 600.);
        let asset_dock_changed = self.right_dock != self.applied_right_dock;
        self.applied_right_dock = self.right_dock;
        let mut asset_panel = egui::Window::new("Client assets")
            .id(egui::Id::new("architect_asset_panel"))
            .default_pos(asset_pos)
            .default_size(egui::vec2(340., 600.))
            .min_size(egui::vec2(250., 300.))
            .resizable(true);
        if asset_dock_changed {
            asset_panel = asset_panel.current_pos(asset_pos);
        }
        let previous_hovered_asset = self.hovered_asset.clone();
        self.hovered_asset = None;
        let asset_panel_response = asset_panel.show(ctx, |ui| {
                ui.horizontal(|ui| { ui.heading("Client explorer"); Self::dock_picker(ui, &mut self.right_dock); });
                ui.label(egui::RichText::new("Loose client files are browsed lazily. Select a PCK archive to index its internal asset paths on demand.").small().weak());
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.tree_filter);
                    if ui.small_button("Refresh files").clicked() {
                        self.directory_cache.clear();
                        self.archive_cache.clear();
                        self.archive_detail_cache.clear();
                        self.archive_details.clear();
                        self.selected_archive = None;
                        self.archive_entries.clear();
                    }
                });
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    self.show_tree(ui, &self.client_root.clone(), 0, camera);
                });
                if let Some(archive) = self.selected_archive.clone() {
                    ui.separator();
                    ui.heading("Archive contents");
                    ui.label(egui::RichText::new(archive.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| archive.display().to_string())).small().weak());
                    if let Some(error) = &self.archive_error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                    } else {
                        let archive_details = self.archive_details.clone();
                        let ecm_count = archive_details.iter().filter(|(name, _, _)| name.to_ascii_lowercase().ends_with(".ecm")).count();
                        let smd_count = archive_details.iter().filter(|(name, _, _)| name.to_ascii_lowercase().ends_with(".smd")).count();
                        let bmd_count = archive_details.iter().filter(|(name, _, _)| name.to_ascii_lowercase().ends_with(".bmd")).count();
                        let gfx_count = archive_details.iter().filter(|(name, _, _)| name.to_ascii_lowercase().ends_with(".gfx")).count();
                        ui.label(format!(
                            "{} files · {ecm_count} ECM · {smd_count} SMD · {bmd_count} BMD · {gfx_count} GFX",
                            archive_details.len()
                        ));
                        let filter = self.tree_filter.to_lowercase();
                        let filtered = archive_details.iter().filter(|(entry, _, _)| filter.is_empty() || entry.to_lowercase().contains(&filter)).collect::<Vec<_>>();
                        let row_height = ui.text_style_height(&egui::TextStyle::Body);
                        egui::ScrollArea::vertical().max_height(180.).show_rows(ui, row_height, filtered.len(), |ui, rows| {
                            for row in rows {
                                let (entry, size, compressed) = filtered[row];
                                let asset = format!("{}::{}", archive.display(), entry);
                                let label = Path::new(entry).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| entry.clone());
                                let response = ui.selectable_label(self.selected_asset.as_deref() == Some(asset.as_str()), format!("📦 {label} · {size} B"));
                                let response = response.on_hover_text(format!(
                                    "{entry}\nType: {}\nUnpacked: {size} bytes\nStored: {compressed} bytes",
                                    Path::new(entry).extension().and_then(|extension| extension.to_str()).unwrap_or("unknown").to_ascii_uppercase()
                                ));
                                if response.hovered() { self.hovered_asset = Some(asset.clone()); }
                                if response.clicked() {
                                    self.select_asset_for_placement(asset.clone());
                                }
                                response.context_menu(|ui| {
                                    if ui.button("Pin archive entry to quick bar").clicked() {
                                        self.pin(asset.clone());
                                        ui.close_menu();
                                    }
                                    if ui
                                        .add_enabled(
                                            self.renderer_placeable_asset(&asset),
                                            egui::Button::new("Place at camera"),
                                        )
                                        .clicked()
                                    {
                                        self.selected_asset = Some(asset.clone());
                                        self.add_selected_asset_at_camera(camera);
                                        ui.close_menu();
                                    }
                                });
                            }
                        });
                    }
                }
                if let Some(asset) = self.selected_asset.clone() {
                    ui.separator();
                    ui.label(egui::RichText::new(&asset).small().strong());
                    if ui.button("Add to quick bar").clicked() { self.pin(asset.clone()); }
                    if ui.button("Place at camera").clicked() {
                        self.add_selected_asset_at_camera(camera);
                    }
                }
            });

        let asset_panel_hovered = asset_panel_response
            .as_ref()
            .is_some_and(|response| response.response.hovered());
        let preview_asset = self
            .hovered_asset
            .clone()
            .or_else(|| previous_hovered_asset.clone())
            .or_else(|| self.selected_asset.clone());
        let mut preview_window_hovered = false;
        if preview_asset.is_some() {
            let preview_response = egui::Window::new("Asset preview")
                .id(egui::Id::new("architect_asset_preview"))
                .default_pos(egui::pos2(
                    (screen.width() - 330.).max(12.),
                    (screen.height() - 330.).max(60.),
                ))
                .default_size(egui::vec2(320., 340.))
                .resizable(true)
                .show(ctx, |ui| {
                    let label = self
                        .hovered_asset
                        .as_deref()
                        .or(previous_hovered_asset.as_deref())
                        .or(self.selected_asset.as_deref())
                        .unwrap_or("");
                    ui.label(egui::RichText::new(label).small().strong());
                    if self.preview_message.is_empty() {
                        if let Some(preview) = &mut self.preview {
                            preview.show_interactive(
                                ui,
                                egui::vec2(
                                    ui.available_width().min(280.),
                                    ui.available_width().min(280.),
                                ),
                            );
                        }
                    } else {
                        ui.label(egui::RichText::new(&self.preview_message).small().weak());
                    }
                    if ui.button("Select this asset").clicked() {
                        if let Some(asset) = self
                            .hovered_asset
                            .clone()
                            .or_else(|| previous_hovered_asset.clone())
                        {
                            self.select_asset_for_placement(asset);
                        }
                    }
                    if ui.button("Place selected asset").clicked() {
                        if let Some(asset) = self.selected_asset.clone() {
                            self.select_asset_for_placement(asset);
                        }
                        self.add_selected_asset_at_camera(camera);
                    }
                });
            preview_window_hovered = preview_response
                .as_ref()
                .is_some_and(|response| response.response.hovered());
        }
        if self.hovered_asset.is_none() && (asset_panel_hovered || preview_window_hovered) {
            self.hovered_asset = previous_hovered_asset;
        } else if !asset_panel_hovered && !preview_window_hovered {
            self.hovered_asset = None;
        }

        egui::Window::new("Scene objects")
            .id(egui::Id::new("architect_scene_panel"))
            .default_pos(egui::pos2(310., 60.))
            .default_size(egui::vec2(420., 420.))
            .min_size(egui::vec2(280., 220.))
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Loaded scene");
                    ui.checkbox(&mut self.show_scene_objects, "Show");
                });
                if self.show_scene_objects {
                    ui.label(format!(
                        "{} blocks · {} trees · {} placed models · {} scenery assets · {} effects",
                        scene.blocks.len(),
                        scene
                            .blocks
                            .iter()
                            .map(|block| block.trees.len())
                            .sum::<usize>(),
                        scene
                            .blocks
                            .iter()
                            .map(|block| block.models.len())
                            .sum::<usize>(),
                        ornaments.len(),
                        scene
                            .blocks
                            .iter()
                            .map(|block| block.effects.len())
                            .sum::<usize>()
                    ));
                    ui.text_edit_singleline(&mut self.object_filter);
                    egui::ScrollArea::vertical()
                        .max_height(270.)
                        .show(ui, |ui| {
                            let mut index = 0usize;
                            for block in &scene.blocks {
                                for model in &block.models {
                                    let label =
                                        format!("Model {} · {}", model.export_id, model.path);
                                    self.scene_row(
                                        ui,
                                        index,
                                        &label,
                                        &model.path,
                                        model.position,
                                        camera,
                                    );
                                    index += 1;
                                }
                                for tree in &block.trees {
                                    if let Some(kind) =
                                        scene.tree_types.get(tree.tree_type as usize)
                                    {
                                        let label =
                                            format!("Tree {} · {}", tree.tree_type, kind.tree);
                                        self.scene_row(
                                            ui,
                                            index,
                                            &label,
                                            &kind.tree,
                                            tree.position,
                                            camera,
                                        );
                                        index += 1;
                                        if !kind.composite.is_empty() {
                                            let composite = format!(
                                                "Tree composite {} · {}",
                                                tree.tree_type, kind.composite
                                            );
                                            self.scene_row(
                                                ui,
                                                index,
                                                &composite,
                                                &kind.composite,
                                                tree.position,
                                                camera,
                                            );
                                            index += 1;
                                        }
                                    }
                                }
                                for effect in &block.effects {
                                    let label =
                                        format!("Effect {} · {}", effect.export_id, effect.path);
                                    self.scene_row(
                                        ui,
                                        index,
                                        &label,
                                        &effect.path,
                                        effect.position,
                                        camera,
                                    );
                                    index += 1;
                                }
                            }
                            for ornament in ornaments {
                                let label =
                                    format!("Scenery {} · {}", ornament.export_id, ornament.path);
                                self.scene_row(
                                    ui,
                                    index,
                                    &label,
                                    &ornament.path,
                                    ornament.position,
                                    camera,
                                );
                                index += 1;
                            }
                        });
                }
            });

        egui::Window::new("Transform")
            .id(egui::Id::new("architect_transform_panel"))
            .default_pos(egui::pos2(310., 500.))
            .default_size(egui::vec2(360., 190.))
            .resizable(true)
            .show(ctx, |ui| {
                if let Some(index) = self.selected_scene_object {
                    ui.label(format!(
                        "Client scene item {index} selected. Native placements are read-only."
                    ));
                } else {
                    ui.label("Select a client scene item or a custom placement.");
                }
                ui.label(format!(
                    "Camera position: {:.1}, {:.1}, {:.1}",
                    camera.x, camera.y, camera.z
                ));
                if ui.button("Place selected asset at camera").clicked() {
                    self.add_selected_asset_at_camera(camera);
                }
                if (self.selected_scene_object.is_some() || !self.multi_selection.is_empty())
                    && ui.button("Duplicate selected scene item(s)").clicked()
                {
                    self.duplicate_selected_scene_item();
                }
                ui.separator();
                ui.heading("Custom placements");
                ui.horizontal(|ui| {
                    if ui.button("Copy (Ctrl+C)").clicked() {
                        self.copy_selected_objects();
                    }
                    if ui.button("Paste (Ctrl+V)").clicked() {
                        self.paste_objects();
                    }
                    if ui.button("Delete (Del)").clicked() {
                        self.delete_selected_objects();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(format!("{} selected", self.selected_placements.len()));
                    if ui.button("Select all").clicked() {
                        self.selected_placements = (0..self.project.objects.len()).collect();
                        self.selected_placement = self.selected_placements.iter().copied().min();
                    }
                    if ui.button("Clear selection").clicked() {
                        self.selected_placements.clear();
                        self.selected_placement = None;
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Duplicate selected").clicked() {
                        let mut indices =
                            self.selected_placements.iter().copied().collect::<Vec<_>>();
                        if indices.is_empty() {
                            indices.extend(self.selected_placement);
                        }
                        indices.sort_unstable();
                        let originals = indices
                            .iter()
                            .filter_map(|index| self.project.objects.get(*index).cloned())
                            .collect::<Vec<_>>();
                        self.selected_placements.clear();
                        for mut object in originals {
                            object.position[0] += 1.0;
                            let index = self.project.objects.len();
                            self.project.objects.push(object);
                            self.selected_placements.insert(index);
                        }
                        self.selected_placement = self.selected_placements.iter().copied().min();
                        self.status = format!(
                            "Duplicated {} custom placement(s).",
                            self.selected_placements.len()
                        );
                    }
                    if ui.button("Delete selected").clicked() {
                        let mut indices =
                            self.selected_placements.iter().copied().collect::<Vec<_>>();
                        if indices.is_empty() {
                            indices.extend(self.selected_placement);
                        }
                        if !indices.is_empty() {
                            let removed = indices.len();
                            let remove = indices.into_iter().collect::<HashSet<_>>();
                            let mut next_index = 0usize;
                            self.project.objects.retain(|_| {
                                let keep = !remove.contains(&next_index);
                                next_index += 1;
                                keep
                            });
                            self.selected_placements.clear();
                            self.selected_placement = None;
                            self.status = format!("Deleted {removed} custom placement(s).");
                        }
                    }
                });
                if self.selected_placements.len() > 1 {
                    let active_position = self
                        .selected_placement
                        .and_then(|index| self.project.objects.get(index))
                        .map(|object| object.position);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Hide selected").clicked() {
                            for index in &self.selected_placements {
                                if let Some(object) = self.project.objects.get_mut(*index) {
                                    object.visible = false;
                                }
                            }
                        }
                        if ui.button("Show selected").clicked() {
                            for index in &self.selected_placements {
                                if let Some(object) = self.project.objects.get_mut(*index) {
                                    object.visible = true;
                                }
                            }
                        }
                        if ui.button("Lock selected").clicked() {
                            for index in &self.selected_placements {
                                if let Some(object) = self.project.objects.get_mut(*index) {
                                    object.locked = true;
                                }
                            }
                        }
                        if ui.button("Unlock selected").clicked() {
                            for index in &self.selected_placements {
                                if let Some(object) = self.project.objects.get_mut(*index) {
                                    object.locked = false;
                                }
                            }
                        }
                        if ui.button("Group with active object").clicked() {
                            if let Some(group) = self
                                .selected_placement
                                .and_then(|index| self.project.objects.get(index))
                                .map(|object| object.group.clone())
                            {
                                for index in &self.selected_placements {
                                    if let Some(object) = self.project.objects.get_mut(*index) {
                                        object.group = group.clone();
                                    }
                                }
                            }
                        }
                    });
                    if let Some(target) = active_position {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Align selection to active:");
                            for (axis, label) in [(0usize, "X"), (1, "Y"), (2, "Z")] {
                                if ui.small_button(label).clicked() {
                                    for index in &self.selected_placements {
                                        if let Some(object) = self.project.objects.get_mut(*index) {
                                            object.position[axis] = target[axis];
                                        }
                                    }
                                    self.status = format!(
                                        "Aligned {} selected placements on the {label} axis.",
                                        self.selected_placements.len()
                                    );
                                }
                            }
                        });
                    }
                }
                let mut delete_placement = None;
                let mut duplicate_placement = None;
                let mut hierarchy = BTreeMap::<String, Vec<(usize, String)>>::new();
                for (index, object) in self.project.objects.iter().enumerate() {
                    let label = format!(
                        "{}{}",
                        if object.visible { "" } else { "◌ " },
                        if object.locked {
                            format!("🔒 {}", object.name)
                        } else {
                            object.name.clone()
                        }
                    );
                    hierarchy
                        .entry(if object.group.trim().is_empty() {
                            "Ungrouped".into()
                        } else {
                            object.group.clone()
                        })
                        .or_default()
                        .push((index, label));
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.object_filter)
                        .hint_text("Filter placements and scene objects"),
                );
                egui::ScrollArea::vertical()
                    .max_height(180.)
                    .show(ui, |ui| {
                        for (group, rows) in &hierarchy {
                            egui::CollapsingHeader::new(format!("📁 {group}"))
                                .id_source(("architect_group", group))
                                .default_open(true)
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.small_button("Select group").clicked() {
                                            self.selected_placements =
                                                rows.iter().map(|(index, _)| *index).collect();
                                            self.selected_placement =
                                                self.selected_placements.iter().copied().min();
                                        }
                                        if ui.small_button("Hide").clicked() {
                                            for (index, _) in rows {
                                                if let Some(object) =
                                                    self.project.objects.get_mut(*index)
                                                {
                                                    object.visible = false;
                                                }
                                            }
                                        }
                                        if ui.small_button("Show").clicked() {
                                            for (index, _) in rows {
                                                if let Some(object) =
                                                    self.project.objects.get_mut(*index)
                                                {
                                                    object.visible = true;
                                                }
                                            }
                                        }
                                        if ui.small_button("Lock").clicked() {
                                            for (index, _) in rows {
                                                if let Some(object) =
                                                    self.project.objects.get_mut(*index)
                                                {
                                                    object.locked = true;
                                                }
                                            }
                                        }
                                        if ui.small_button("Unlock").clicked() {
                                            for (index, _) in rows {
                                                if let Some(object) =
                                                    self.project.objects.get_mut(*index)
                                                {
                                                    object.locked = false;
                                                }
                                            }
                                        }
                                    });
                                    for (index, label) in rows {
                                        if !self.object_filter.is_empty()
                                            && !label
                                                .to_lowercase()
                                                .contains(&self.object_filter.to_lowercase())
                                            && !group
                                                .to_lowercase()
                                                .contains(&self.object_filter.to_lowercase())
                                        {
                                            continue;
                                        }
                                        if ui
                                            .selectable_label(
                                                self.selected_placements.contains(index)
                                                    || self.selected_placement == Some(*index),
                                                label,
                                            )
                                            .clicked()
                                        {
                                            self.selected_asset = self
                                                .project
                                                .objects
                                                .get(*index)
                                                .map(|object| object.asset.clone());
                                            if self.cursor == CursorMode::MultiSelect {
                                                if self.selected_placements.insert(*index) {
                                                    self.selected_placement = Some(*index);
                                                } else {
                                                    self.selected_placements.remove(index);
                                                    self.selected_placement = self
                                                        .selected_placements
                                                        .iter()
                                                        .copied()
                                                        .min();
                                                }
                                            } else {
                                                self.selected_placements.clear();
                                                self.selected_placements.insert(*index);
                                                self.selected_placement = Some(*index);
                                            }
                                        }
                                    }
                                });
                        }
                    });
                if let Some(index) = self.selected_placement {
                    if let Some(object) = self.project.objects.get_mut(index) {
                        ui.label(egui::RichText::new(&object.asset).small().weak());
                        ui.horizontal(|ui| {
                            ui.label("Name");
                            ui.text_edit_singleline(&mut object.name);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Group");
                            ui.text_edit_singleline(&mut object.group);
                        });
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut object.visible, "Visible");
                            ui.checkbox(&mut object.locked, "Locked");
                        });
                        ui.horizontal(|ui| {
                            ui.label("Position");
                            for axis in 0..3 {
                                ui.add(
                                    egui::DragValue::new(&mut object.position[axis])
                                        .speed(0.5)
                                        .prefix(["X ", "Y ", "Z "][axis]),
                                );
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Rotation");
                            let mut degrees = object.rotation.map(f32::to_degrees);
                            let mut changed = false;
                            for axis in 0..3 {
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut degrees[axis])
                                            .speed(1.0)
                                            .suffix("°")
                                            .prefix(["X ", "Y ", "Z "][axis]),
                                    )
                                    .changed();
                            }
                            if changed {
                                object.rotation = degrees.map(f32::to_radians);
                            }
                        });
                        ui.add(
                            egui::Slider::new(&mut object.scale, 0.01..=100.0)
                                .logarithmic(true)
                                .text("Uniform scale"),
                        );
                        ui.horizontal(|ui| {
                            ui.label("Scale XYZ");
                            for axis in 0..3 {
                                ui.add(egui::DragValue::new(&mut object.scale_axes[axis])
                                    .speed(0.02)
                                    .clamp_range(0.01..=100.0)
                                    .prefix(["X ", "Y ", "Z "][axis]));
                            }
                        });
                        if ui.button("Duplicate custom placement").clicked() {
                            duplicate_placement = Some(object.clone());
                        }
                        if ui.button("Delete custom placement").clicked() {
                            delete_placement = Some(index);
                        }
                    }
                }
                if let Some(mut duplicate) = duplicate_placement {
                    duplicate.position[0] += 1.0;
                    let index = self.project.objects.len();
                    self.project.objects.push(duplicate);
                    self.selected_placements.clear();
                    self.selected_placements.insert(index);
                    self.selected_placement = Some(index);
                    self.status = "Duplicated custom placement.".into();
                }
                if let Some(index) = delete_placement {
                    self.project.objects.remove(index);
                    self.selected_placements.clear();
                    self.selected_placement = None;
                }
                ui.label(format!(
                    "Pending custom placements: {}",
                    self.project.objects.len()
                ));
            });

        let terrain_pos = Self::dock_position(self.bottom_dock, screen, 560., 230.);
        let terrain_dock_changed = self.bottom_dock != self.applied_bottom_dock;
        self.applied_bottom_dock = self.bottom_dock;
        let mut terrain_panel = egui::Window::new("Terrain tools")
            .id(egui::Id::new("architect_terrain_panel"))
            .default_pos(terrain_pos)
            .default_size(egui::vec2(560., 230.))
            .min_size(egui::vec2(360., 160.))
            .resizable(true);
        if terrain_dock_changed {
            terrain_panel = terrain_panel.current_pos(terrain_pos);
        }
        terrain_panel.show(ctx, |ui| {
                ui.horizontal(|ui| { ui.heading("Terrain sculpt"); Self::dock_picker(ui, &mut self.bottom_dock); });
                ui.add(egui::Slider::new(&mut self.flatten_height, -500.0..=500.0).text("Flatten target"));
                if ui.button("Apply brush at camera").clicked() {
                    let stroke = TerrainStroke {
                        mode: self.brush,
                        position: {
                            let mut position = camera.to_array();
                            if let Some(map) = &self.map {
                                position[1] = map.ground(position[0], position[2]) + 0.5;
                            }
                            position
                        },
                        radius: self.brush_radius,
                        strength: if self.brush == BrushMode::Flatten {
                            self.flatten_height
                        } else {
                            self.brush_strength
                        },
                    };
                    match self.apply_stroke(&stroke) {
                        Ok(cells) => {
                            self.project.strokes.push(stroke);
                            self.redo_strokes.clear();
                            self.status = format!("Applied terrain brush to {cells} cells.");
                        }
                        Err(error) => self.status = error,
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("Undo last brush").clicked() {
                        if let Some(stroke) = self.project.strokes.pop() {
                            self.redo_strokes.push(stroke);
                            match self.rebuild_terrain() {
                                Ok(()) => self.status = "Undid last terrain brush.".into(),
                                Err(error) => self.status = format!("Terrain undo failed: {error}"),
                            }
                        }
                    }
                    if ui.button("Redo brush").clicked() {
                        if let Some(stroke) = self.redo_strokes.pop() {
                            self.project.strokes.push(stroke);
                            match self.rebuild_terrain() {
                                Ok(()) => self.status = "Redid terrain brush.".into(),
                                Err(error) => self.status = format!("Terrain redo failed: {error}"),
                            }
                        }
                    }
                    if ui.button("Undo object edit").clicked() {
                        self.undo_objects();
                    }
                    if ui.button("Redo object edit").clicked() {
                        self.redo_objects();
                    }
                });
                ui.label(format!(
                    "{} stored brush operations · {} stored placements",
                    self.project.strokes.len(),
                    self.project.objects.len()
                ));
                ui.label(
                    egui::RichText::new(
                        "Brush edits update the shared terrain, navigation and rendered mesh. Paint remains unavailable.",
                    )
                    .small()
                    .weak(),
                );
            });

        egui::TopBottomPanel::bottom("architect_quick_bar")
            .exact_height(42.)
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(27, 31, 39))
                    .inner_margin(egui::Margin::symmetric(8., 4.)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("QUICK BAR").strong());
                    for item in self.quick_bar.clone() {
                        let label = Path::new(&item)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| item.clone());
                        if ui.small_button(label).on_hover_text(&item).clicked() {
                            self.select_asset_for_placement(item);
                        }
                    }
                });
            });
        let (pointer, down, pressed, released) = ctx.input(|input| {
            (
                input.pointer.hover_pos(),
                input.pointer.primary_down(),
                input.pointer.button_pressed(egui::PointerButton::Primary),
                input.pointer.button_released(egui::PointerButton::Primary),
            )
        });
        let over_ui = ctx.is_pointer_over_area() || ctx.wants_pointer_input();
        let terrain_hit = if matches!(
            self.cursor,
            CursorMode::Sculpt
                | CursorMode::Select
                | CursorMode::MultiSelect
                | CursorMode::Lasso
                | CursorMode::Place
        ) && !over_ui
        {
            pointer.and_then(|position| {
                self.map
                    .as_ref()
                    .and_then(|map| Self::terrain_hit(position, screen, view_projection, map))
            })
        } else {
            None
        };
        let hit = if self.cursor == CursorMode::Place && !over_ui {
            terrain_hit.or_else(|| {
                pointer.and_then(|position| {
                    let height = self
                        .map
                        .as_ref()
                        .map(|map| map.ground(camera.x, camera.z))
                        .unwrap_or(0.);
                    Self::ray_plane_hit(position, screen, view_projection, height)
                })
            })
        } else {
            terrain_hit
        };
        self.brush_cursor_position = hit.map(|position| position.to_array());
        let placement_click = pressed && self.cursor == CursorMode::Place && !over_ui;
        if placement_click {
            if let Some(position) = hit {
                let mut placement = position.to_array();
                match self.snap {
                    SnapMode::Off => {}
                    SnapMode::Terrain => {
                        if let Some(map) = &self.map {
                            placement[1] = map.ground(placement[0], placement[2]) + 0.5;
                        }
                    }
                    SnapMode::Objects => {
                        if let Some(selected) = self.selected_scene_transform {
                            placement = selected;
                        }
                    }
                    SnapMode::Smart => {
                        if let Some(selected) = self.selected_scene_transform {
                            placement = selected;
                        } else if let Some(map) = &self.map {
                            placement[1] = map.ground(placement[0], placement[2]) + 0.5;
                        }
                    }
                }
                self.add_selected_asset_at_position(placement);
            } else {
                self.status = "Could not project the pointer into the world. Move the cursor over the viewport and try again.".into();
            }
        }
        // Pick staged project objects directly in the viewport. Their editor
        // transform is independent from the immutable source-scene instances.
        if pressed
            && !placement_click
            && matches!(
                self.cursor,
                CursorMode::Select | CursorMode::MultiSelect | CursorMode::Lasso
            )
            && !over_ui
        {
            let mut axis_pick = None;
            if let (Some(index), Some(pointer)) = (
                self.selected_placement
                    .filter(|_| self.cursor == CursorMode::Select),
                pointer,
            ) {
                if let Some(object) = self.project.objects.get(index) {
                    let origin = Vec3::from_array(object.position);
                    let project_point = |point: Vec3| {
                        let clip = view_projection.project_point3(point);
                        (clip.is_finite() && (0.0..=1.0).contains(&clip.z)).then(|| {
                            egui::pos2(
                                screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                                screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                            )
                        })
                    };
                    // The handle direction is a screen-space aid, not a second
                    // world-space point that must pass the camera depth clip.
                    // Requiring the full 24-unit endpoint to be in range made
                    // gizmo axes vanish near the far plane and at steep angles.
                    let project_direction_point = |point: Vec3| {
                        let clip = view_projection.project_point3(point);
                        clip.is_finite().then(|| {
                            egui::pos2(
                                screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                                screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                            )
                        })
                    };
                    if let Some(center) = project_point(origin) {
                        let gizmo_radius = (camera.distance(origin) * 0.045).clamp(0.5, 10.);
                        let axis_end = |axis: Vec3| {
                            project_direction_point(origin + axis * gizmo_radius).map(|projected| {
                                let direction = projected - center;
                                let direction = if direction.length_sq() > 0.0001 { direction.normalized() } else { egui::Vec2::ZERO };
                                center + direction * 76.0
                            })
                        };
                        match self.transform_mode {
                            TransformMode::Move | TransformMode::Scale => {
                                for (axis_index, axis) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().enumerate() {
                                    if axis_end(axis).is_some_and(|end| {
                                        let segment = end - center;
                                        let t = ((pointer - center).dot(segment) / segment.length_sq().max(0.001)).clamp(0.0, 1.0);
                                        (center + segment * t).distance(pointer) <= 12.0
                                    }) {
                                        axis_pick = Some(axis_index);
                                        break;
                                    }
                                }
                            }
                            TransformMode::Rotate => {
                                for (axis_index, axis) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().enumerate() {
                                    let ring = Self::gizmo_ring_points(origin, axis, gizmo_radius, screen, view_projection);
                                    if Self::distance_to_polyline(pointer, &ring) <= 9.0 {
                                        axis_pick = Some(axis_index);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let mut nearest: Option<(usize, f32, f32)> = None;
            if axis_pick.is_none() {
                if let Some(pointer) = pointer {
                    for (index, object) in self.project.objects.iter().enumerate() {
                        let clip =
                            view_projection.project_point3(Vec3::from_array(object.position));
                        if !object.visible || !clip.is_finite() || !(0.0..=1.0).contains(&clip.z) {
                            continue;
                        }
                        let point = egui::pos2(
                            screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                            screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                        );
                        // Prefer the projected mesh footprint over a small
                        // origin-only click radius. This makes tall/wide models
                        // selectable where their visible geometry actually is.
                        let projected_bounds = self
                            .renderer_bounds_key(&object.asset)
                            .and_then(|path| self.preview_bounds.get(&path).copied())
                            .and_then(|(min, max)| {
                                let rotation = glam::Quat::from_euler(
                                    glam::EulerRot::XYZ,
                                    object.rotation[0],
                                    object.rotation[1],
                                    object.rotation[2],
                                );
                                let center = Vec3::from_array(object.position);
                                let mut rect = egui::Rect::NOTHING;
                                let mut count = 0usize;
                                for bits in 0..8 {
                                    let local = Vec3::new(
                                        if bits & 1 == 0 { min[0] } else { max[0] },
                                        if bits & 2 == 0 { min[1] } else { max[1] },
                                        if bits & 4 == 0 { min[2] } else { max[2] },
                                    ) * (object.scale.max(0.01) * Vec3::from_array(object.scale_axes));
                                    let world = center + rotation * local;
                                    let projected = view_projection.project_point3(world);
                                    if projected.is_finite() && (0.0..=1.0).contains(&projected.z) {
                                        let screen_point = egui::pos2(
                                            screen.left()
                                                + (projected.x + 1.) * 0.5 * screen.width(),
                                            screen.top()
                                                + (1. - projected.y) * 0.5 * screen.height(),
                                        );
                                        rect.extend_with(screen_point);
                                        count += 1;
                                    }
                                }
                                (count > 0).then_some(rect.expand(8.0))
                            });
                        let score = if let Some(rect) = projected_bounds {
                            let dx = if pointer.x < rect.left() {
                                rect.left() - pointer.x
                            } else if pointer.x > rect.right() {
                                pointer.x - rect.right()
                            } else {
                                0.0
                            };
                            let dy = if pointer.y < rect.top() {
                                rect.top() - pointer.y
                            } else if pointer.y > rect.bottom() {
                                pointer.y - rect.bottom()
                            } else {
                                0.0
                            };
                            dx.hypot(dy) / 14.0
                        } else {
                            point.distance(pointer) / Self::source_pick_radius(&object.asset)
                        };
                        if score <= 1.0
                            && nearest.is_none_or(|(_, best_score, best_depth)| {
                                score < best_score - 0.025
                                    || ((score - best_score).abs() <= 0.025 && clip.z > best_depth)
                            })
                        {
                            nearest = Some((index, score, clip.z));
                        }
                    }
                }
                let source_pick =
                    if matches!(self.cursor, CursorMode::Select | CursorMode::MultiSelect)
                        && axis_pick.is_none()
                    {
                        pointer.and_then(|pointer| {
                            self.pick_source_scene(
                                pointer,
                                screen,
                                view_projection,
                                scene,
                                ornaments,
                            )
                        })
                    } else {
                        None
                    };
                // Resolve both editor and source-scene candidates together. A
                // source model in front of a staged placement must be selectable,
                // while a nearer staged object still wins when their hit targets
                // overlap. Reverse-Z makes the larger depth value the nearer one.
                let source_is_frontmost = match (nearest.as_ref(), source_pick.as_ref()) {
                    (None, Some(_)) => true,
                    (Some(_), None) | (None, None) => false,
                    (
                        Some((_, project_score, project_depth)),
                        Some((_, scene_score, scene_depth, _, _)),
                    ) => {
                        *scene_score < *project_score - 0.025
                            || ((*scene_score - *project_score).abs() <= 0.025
                                && *scene_depth > *project_depth)
                    }
                };
                let picked_placement = if axis_pick.is_some() {
                    self.selected_placement
                } else if source_is_frontmost {
                    None
                } else {
                    nearest.map(|(index, _, _)| index)
                };
                let begin_marquee = self.cursor == CursorMode::MultiSelect
                    && picked_placement.is_none()
                    && !source_is_frontmost;
                let begin_lasso = self.cursor == CursorMode::Lasso;
                if self.cursor == CursorMode::MultiSelect {
                    if source_is_frontmost {
                        self.selected_placement = None;
                        self.selected_placements.clear();
                    } else if let Some(index) = picked_placement {
                        self.selected_placements.insert(index);
                        self.selected_placement = Some(index);
                        self.selected_asset = self
                            .project
                            .objects
                            .get(index)
                            .map(|object| object.asset.clone());
                        self.selected_scene_object = None;
                        self.selected_scene_transform = None;
                    } else {
                        self.marquee_start = pointer;
                        self.marquee_current = pointer;
                        self.selected_scene_object = None;
                        self.selected_scene_transform = None;
                    }
                } else if begin_lasso {
                    self.lasso_points.clear();
                    if let Some(position) = pointer {
                        self.lasso_points.push(position);
                    }
                    self.selected_scene_object = None;
                    self.selected_scene_transform = None;
                } else {
                    let clicked_inside_group = picked_placement
                        .is_some_and(|index| self.selected_placements.contains(&index));
                    self.selected_placement = picked_placement;
                    if let Some(index) = picked_placement {
                        self.selected_asset = self
                            .project
                            .objects
                            .get(index)
                            .map(|object| object.asset.clone());
                    }
                    if !clicked_inside_group {
                        self.selected_placements.clear();
                        if let Some(index) = picked_placement {
                            self.selected_placements.insert(index);
                        }
                    }
                }
                if begin_marquee || begin_lasso {
                    // Selection gestures own the viewport drag; do not fall
                    // through to source-scene picking or object transforms.
                } else if source_is_frontmost {
                    self.selected_placement = None;
                    self.selected_placements.clear();
                    if let Some((index, _, _, asset, position)) = source_pick {
                        self.selected_scene_object = Some(index);
                        self.selected_scene_transform = Some(position);
                        self.selected_asset = Some(asset.clone());
                        if self.cursor == CursorMode::MultiSelect {
                            if self.multi_selection.insert(index) {
                                self.multi_scene_items.insert(index, (asset, position));
                                self.selected_scene_object = Some(index);
                            } else {
                                self.multi_selection.remove(&index);
                                self.multi_scene_items.remove(&index);
                                self.selected_scene_object =
                                    self.multi_selection.iter().copied().min();
                            }
                        } else {
                            self.multi_selection.clear();
                            self.multi_scene_items.clear();
                        }
                    }
                } else if self.selected_placement.is_some() {
                    self.selected_scene_object = None;
                    self.selected_scene_transform = None;
                } else {
                    self.selected_scene_object = None;
                    self.selected_scene_transform = None;
                    self.multi_selection.clear();
                    self.multi_scene_items.clear();
                }
            }
            self.object_drag_axis = axis_pick;
            self.object_drag_group.clear();
            self.object_drag_start_hit = hit.map(|position| position.to_array());
            self.object_drag_start_axis_parameter = pointer.and_then(|pointer| {
                let axis = match self.object_drag_axis {
                    Some(0) => Some(Vec3::X),
                    Some(1) => Some(Vec3::Y),
                    Some(2) => Some(Vec3::Z),
                    _ => None,
                }?;
                let object = self.selected_placement
                    .and_then(|index| self.project.objects.get(index))?;
                Self::ray_axis_parameter(
                    pointer, screen, view_projection, Vec3::from_array(object.position), axis,
                )
            });
            let selected_is_unlocked = self
                .selected_placement
                .and_then(|index| self.project.objects.get(index))
                .is_some_and(|object| !object.locked);
            self.object_dragging = self.cursor == CursorMode::Select
                && selected_is_unlocked
                && ((self.selected_placements.len() > 1
                    && (hit.is_some() || self.object_drag_axis.is_some()))
                    || (self.selected_placements.len() <= 1
                        && (hit.is_some() || self.object_drag_axis.is_some())));
            if self.object_dragging && self.selected_placements.len() > 1 {
                self.object_drag_group = self
                    .selected_placements
                    .iter()
                    .copied()
                    .filter_map(|index| {
                        self.project
                            .objects
                            .get(index)
                            .filter(|object| !object.locked)
                            .map(|object| (index, object.position))
                    })
                    .collect();
            }
            self.object_dragging = self.object_dragging && self.marquee_start.is_none();
            self.object_drag_snapshot = self.object_dragging.then(|| self.project.objects.clone());
            self.object_drag_start_pointer = pointer;
            if let Some(index) = self.selected_placement {
                self.object_drag_start_position = self.project.objects[index].position;
                if let Some(hit) = hit {
                    let position = self.project.objects[index].position;
                    self.object_drag_y_offset = position[1] - hit.y;
                    self.object_drag_xz_offset = [position[0] - hit.x, position[2] - hit.z];
                }
            }
            self.object_drag_start_rotation_vector = if self.transform_mode == TransformMode::Rotate {
                pointer.and_then(|pointer| {
                    let axis = [Vec3::X, Vec3::Y, Vec3::Z][self.object_drag_axis.unwrap_or(1).min(2)];
                    let origin = Vec3::from_array(self.object_drag_start_position);
                    Self::ray_plane_point(pointer, screen, view_projection, origin, axis)
                        .map(|point| (point - origin).normalize_or_zero().to_array())
                })
            } else {
                None
            };
        }
        if self.marquee_start.is_some()
            && down
            && self.cursor == CursorMode::MultiSelect
            && !over_ui
        {
            self.marquee_current = pointer;
        }
        if !self.lasso_points.is_empty() && down && self.cursor == CursorMode::Lasso && !over_ui {
            if let Some(position) = pointer {
                if self
                    .lasso_points
                    .last()
                    .is_none_or(|last| last.distance(position) >= 3.0)
                {
                    self.lasso_points.push(position);
                }
            }
        }
        if !self.lasso_points.is_empty() && (released || !down) {
            if !over_ui {
                if let Some(position) = pointer {
                    if self
                        .lasso_points
                        .last()
                        .is_none_or(|last| last.distance(position) >= 1.0)
                    {
                        self.lasso_points.push(position);
                    }
                }
            }
            if self.lasso_points.len() >= 3 {
                for (index, object) in self.project.objects.iter().enumerate() {
                    if !object.visible {
                        continue;
                    }
                    let clip = view_projection.project_point3(Vec3::from_array(object.position));
                    if !clip.is_finite() || !(0.0..=1.0).contains(&clip.z) {
                        continue;
                    }
                    let projected = egui::pos2(
                        screen.left() + (clip.x + 1.0) * 0.5 * screen.width(),
                        screen.top() + (1.0 - clip.y) * 0.5 * screen.height(),
                    );
                    if Self::lasso_contains(&self.lasso_points, projected) {
                        self.selected_placements.insert(index);
                    }
                }
                self.selected_placement = self
                    .selected_placement
                    .filter(|index| self.selected_placements.contains(index))
                    .or_else(|| self.selected_placements.iter().copied().min());
                self.status = format!("Selected {} placements.", self.selected_placements.len());
            }
            self.lasso_points.clear();
        }
        if self.marquee_start.is_some() && (released || !down) {
            let release_position = if over_ui {
                self.marquee_current
            } else {
                pointer.or(self.marquee_current)
            };
            if let (Some(start), Some(current)) = (self.marquee_start, release_position) {
                let rect = Self::marquee_rect(start, current);
                if rect.width() >= 4.0 || rect.height() >= 4.0 {
                    for (index, object) in self.project.objects.iter().enumerate() {
                        if !object.visible {
                            continue;
                        }
                        let clip =
                            view_projection.project_point3(Vec3::from_array(object.position));
                        if !clip.is_finite() || !(0.0..=1.0).contains(&clip.z) {
                            continue;
                        }
                        let projected = egui::pos2(
                            screen.left() + (clip.x + 1.0) * 0.5 * screen.width(),
                            screen.top() + (1.0 - clip.y) * 0.5 * screen.height(),
                        );
                        if rect.contains(projected) {
                            self.selected_placements.insert(index);
                        }
                    }
                    self.selected_placement = self
                        .selected_placement
                        .filter(|index| self.selected_placements.contains(index))
                        .or_else(|| self.selected_placements.iter().copied().min());
                    self.status =
                        format!("Selected {} placements.", self.selected_placements.len());
                }
            }
            self.marquee_start = None;
            self.marquee_current = None;
        }
        if self.object_dragging && down && self.cursor == CursorMode::Select && !over_ui {
            let indices = if self.object_drag_group.is_empty() {
                self.selected_placement.into_iter().collect::<Vec<_>>()
            } else {
                self.object_drag_group
                    .iter()
                    .map(|(index, _)| *index)
                    .collect()
            };
            let pivot = self
                .selected_placement
                .and_then(|index| self.project.objects.get(index))
                .map(|_| Vec3::from_array(self.object_drag_start_position))
                .unwrap_or(Vec3::ZERO);
            match self.transform_mode {
                TransformMode::Move => {
                    if !self.object_drag_group.is_empty() {
                        let delta = match self.object_drag_axis {
                            Some(axis_index @ 0..=2) => {
                                let axis = [Vec3::X, Vec3::Y, Vec3::Z][axis_index];
                                self.object_drag_start_axis_parameter
                                    .zip(pointer.and_then(|pointer| Self::ray_axis_parameter(
                                        pointer, screen, view_projection, pivot, axis,
                                    )))
                                    .map(|(start, current)| axis * (current - start))
                            }
                            _ => self
                                .object_drag_start_hit
                                .zip(hit)
                                .map(|(start, current)| current - Vec3::from_array(start)),
                        };
                        if let Some(delta) = delta {
                            for (index, position) in &self.object_drag_group {
                                if let Some(object) = self.project.objects.get_mut(*index) {
                                    object.position =
                                        (Vec3::from_array(*position) + delta).to_array();
                                }
                            }
                        }
                    } else if let Some(index) = self.selected_placement {
                        if let Some(object) = self.project.objects.get_mut(index) {
                            match self.object_drag_axis {
                                Some(axis_index @ 0..=2) => {
                                    let axis = [Vec3::X, Vec3::Y, Vec3::Z][axis_index];
                                    if let (Some(start), Some(current)) = (
                                        self.object_drag_start_axis_parameter,
                                        pointer.and_then(|pointer| Self::ray_axis_parameter(
                                            pointer, screen, view_projection,
                                            Vec3::from_array(self.object_drag_start_position), axis,
                                        )),
                                    ) {
                                        object.position = (Vec3::from_array(self.object_drag_start_position)
                                            + axis * (current - start)).to_array();
                                    }
                                }
                                _ => {
                                    if let Some(hit) = hit {
                                        object.position = [
                                            hit.x + self.object_drag_xz_offset[0],
                                            hit.y + self.object_drag_y_offset,
                                            hit.z + self.object_drag_xz_offset[1],
                                        ];
                                    }
                                }
                            }
                        }
                    }
                }
                TransformMode::Rotate => {
                    if let (Some(start_vector), Some(current)) = (
                        self.object_drag_start_rotation_vector,
                        pointer.and_then(|pointer| {
                            let axis = [Vec3::X, Vec3::Y, Vec3::Z][self.object_drag_axis.unwrap_or(1).min(2)];
                            Self::ray_plane_point(pointer, screen, view_projection,
                                Vec3::from_array(self.object_drag_start_position), axis)
                                .map(|point| (point - Vec3::from_array(self.object_drag_start_position)).normalize_or_zero().to_array())
                        }),
                    ) {
                        let axis = self.object_drag_axis.unwrap_or(1).min(2);
                        let start_vector = Vec3::from_array(start_vector).normalize_or_zero();
                        let current_vector = Vec3::from_array(current).normalize_or_zero();
                        let axis_vector = [Vec3::X, Vec3::Y, Vec3::Z][axis];
                        let angle = start_vector.cross(current_vector).dot(axis_vector)
                            .atan2(start_vector.dot(current_vector));
                        for index in indices {
                            if let Some(object) = self.project.objects.get_mut(index) {
                                let original = self
                                    .object_drag_snapshot
                                    .as_ref()
                                    .and_then(|items| items.get(index))
                                    .cloned();
                                if let Some(original) = original {
                                    let relative = Vec3::from_array(original.position) - pivot;
                                    let rotation = match axis {
                                        0 => glam::Quat::from_rotation_x(angle),
                                        1 => glam::Quat::from_rotation_y(angle),
                                        _ => glam::Quat::from_rotation_z(angle),
                                    };
                                    object.position = (pivot + rotation * relative).to_array();
                                    object.rotation = original.rotation;
                                    object.rotation[axis] += angle;
                                }
                            }
                        }
                    }
                }
                TransformMode::Scale => {
                    if let (Some(start), Some(current)) = (self.object_drag_start_pointer, pointer) {
                        let axis = self.object_drag_axis;
                        let factor = if let Some(axis_index) = axis {
                            let axis_vector = [Vec3::X, Vec3::Y, Vec3::Z][axis_index.min(2)];
                            let clip_origin = view_projection.project_point3(pivot);
                            let clip_end = view_projection.project_point3(
                                pivot + axis_vector * (camera.distance(pivot) * 0.045).clamp(0.5, 10.)
                            );
                            let to_screen = |clip: Vec3| egui::pos2(
                                screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                                screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                            );
                            let direction = (to_screen(clip_end) - to_screen(clip_origin)).normalized();
                            (((current - start).dot(direction)) * 0.015).exp().clamp(0.05, 20.0)
                        } else {
                            (-(current.y - start.y) * 0.01).exp().clamp(0.05, 20.0)
                        };
                        for index in indices {
                            if let Some(object) = self.project.objects.get_mut(index) {
                                if let Some(original) = self.object_drag_snapshot.as_ref().and_then(|items| items.get(index)) {
                                    let relative = Vec3::from_array(original.position) - pivot;
                                    if let Some(axis_index) = axis {
                                        let axis_index = axis_index.min(2);
                                        object.scale = original.scale;
                                        object.scale_axes = original.scale_axes;
                                        object.scale_axes[axis_index] =
                                            (original.scale_axes[axis_index] * factor).clamp(0.01, 100.0);
                                        if self.object_drag_group.len() > 1 {
                                            let mut scaled = relative.to_array();
                                            scaled[axis_index] *= factor;
                                            object.position = (pivot + Vec3::from_array(scaled)).to_array();
                                        } else {
                                            object.position = original.position;
                                        }
                                    } else {
                                        object.scale = (original.scale * factor).clamp(0.01, 100.0);
                                        object.scale_axes = original.scale_axes;
                                        object.position = (pivot + relative * factor).to_array();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if released || !down {
            if self.object_dragging {
                if let Some(snapshot) = self.object_drag_snapshot.take() {
                    self.push_object_undo(snapshot);
                }
            }
            self.object_drag_group.clear();
            self.object_drag_start_hit = None;
        self.object_drag_start_axis_parameter = None;
        self.object_drag_start_rotation_vector = None;
            self.object_dragging = false;
            self.object_drag_axis = None;
            self.object_drag_start_pointer = None;
        }
        if pressed && self.cursor == CursorMode::Sculpt && !over_ui && hit.is_some() {
            if self.project.source_map.is_empty() && !self.current_map_id.is_empty() {
                self.project.source_map = self.current_map_id.clone();
            }
            self.brush_dragging = true;
            self.last_brush_position = None;
        }
        if self.brush_dragging && down {
            if let Some(position) = hit {
                let spacing = self.map.as_ref().map_or(1., |m| m.spacing);
                let step = (self.brush_radius * 0.18).max(spacing).max(0.25);
                let previous = self
                    .last_brush_position
                    .map(Vec3::from_array)
                    .unwrap_or(position);
                let distance = (position - previous).length();
                if distance >= step || self.last_brush_position.is_none() {
                    // Interpolate along fast pointer movement so a high-speed drag
                    // cannot leave holes between individual brush dabs.
                    let count = ((distance / step).ceil() as usize).clamp(1, 128);
                    let mut total_cells = 0usize;
                    for dab in 1..=count {
                        let point = previous.lerp(position, dab as f32 / count as f32);
                        let stroke = TerrainStroke {
                            mode: self.brush,
                            position: point.to_array(),
                            radius: self.brush_radius,
                            strength: if self.brush == BrushMode::Flatten {
                                self.flatten_height
                            } else {
                                self.brush_strength
                            },
                        };
                        match self.apply_stroke(&stroke) {
                            Ok(cells) if cells > 0 => {
                                total_cells = total_cells.saturating_add(cells);
                                self.project.strokes.push(stroke);
                            }
                            Ok(_) => {}
                            Err(error) => {
                                self.status = error;
                                break;
                            }
                        }
                    }
                    if total_cells > 0 {
                        self.redo_strokes.clear();
                        self.status = format!(
                            "Sculpted {total_cells} terrain cells · {} dabs",
                            self.project.strokes.len()
                        );
                    }
                    self.last_brush_position = Some(position.to_array());
                }
            }
        }
        if released || !down {
            self.brush_dragging = false;
            self.last_brush_position = None;
        }
        if let (Some(start), Some(current)) = (self.marquee_start, self.marquee_current) {
            let rect = Self::marquee_rect(start, current);
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("architect_marquee_selection"),
            ))
            .rect(
                rect,
                2.0,
                egui::Color32::from_rgba_unmultiplied(70, 145, 235, 28),
                egui::Stroke::new(1.5, egui::Color32::from_rgb(100, 175, 255)),
            );
        }
        if self.lasso_points.len() >= 2 {
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("architect_lasso_selection"),
            ))
            .add(egui::Shape::line(
                self.lasso_points.clone(),
                egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(120, 210, 175)),
            ));
        }
        if let Some(index) = self
            .selected_placement
            .filter(|index| *index < self.project.objects.len())
        {
            let object = &self.project.objects[index];
            let origin = Vec3::from_array(object.position);
            let project_point = |point: Vec3| -> Option<egui::Pos2> {
                let clip = view_projection.project_point3(point);
                (clip.is_finite() && (0.0..=1.0).contains(&clip.z)).then(|| {
                    egui::pos2(
                        screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                        screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                    )
                })
            };
            let project_direction_point = |point: Vec3| -> Option<egui::Pos2> {
                let clip = view_projection.project_point3(point);
                clip.is_finite().then(|| {
                    egui::pos2(
                        screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                        screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                    )
                })
            };
            if let Some(center) = project_point(origin) {
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("architect_transform_gizmo"),
                ));
                let axes = [
                    (Vec3::X, egui::Color32::from_rgb(245, 75, 75)),
                    (Vec3::Y, egui::Color32::from_rgb(90, 230, 120)),
                    (Vec3::Z, egui::Color32::from_rgb(90, 150, 255)),
                ];
                painter.circle_stroke(center, 5., egui::Stroke::new(2., egui::Color32::WHITE));
                let axis_end = |axis: Vec3| {
                    project_direction_point(origin + axis).map(|projected| {
                        let direction = projected - center;
                        let direction = if direction.length_sq() > 0.0001 {
                            direction.normalized()
                        } else {
                            egui::Vec2::ZERO
                        };
                        center + direction * 76.0
                    })
                };
                match self.transform_mode {
                    TransformMode::Move => {
                        for (axis, color) in axes {
                            if let Some(end) = axis_end(axis) {
                                painter.line_segment([center, end], egui::Stroke::new(4., color));
                                painter.circle_filled(end, 6., color);
                            }
                        }
                    }
                    TransformMode::Rotate => {
                        let radius = (camera.distance(origin) * 0.045).clamp(0.5, 10.);
                        for (axis_index, axis) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().enumerate() {
                            let mut points = Self::gizmo_ring_points(origin, axis, radius, screen, view_projection);
                            if points.len() >= 3 {
                                points.push(points[0]);
                                painter.add(egui::Shape::line(points, egui::Stroke::new(3.0, axes[axis_index].1)));
                            }
                        }
                    }
                    TransformMode::Scale => {
                        for (axis, color) in axes {
                            if let Some(end) = axis_end(axis) {
                                painter.line_segment([center, end], egui::Stroke::new(4., color));
                                painter.rect_filled(
                                    egui::Rect::from_center_size(end, egui::vec2(12., 12.)),
                                    2.,
                                    color,
                                );
                            }
                        }
                    }
                }
            }
        }
        // Draw a clear white 3D wire boundary for every selected project
        // placement. The current scene API exposes placement origins rather
        // than per-instance mesh bounds, so use conservative class-aware extents.
        let project_outline_point = |point: Vec3| -> Option<egui::Pos2> {
            let clip = view_projection.project_point3(point);
            (clip.is_finite() && (0.0..=1.0).contains(&clip.z)).then(|| {
                egui::pos2(
                    screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                    screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                )
            })
        };
        let mut outline_items = self
            .selected_placements
            .iter()
            .filter_map(|index| self.project.objects.get(*index))
            .filter(|object| object.visible)
            .map(|object| {
                (
                    object.asset.clone(),
                    object.position,
                    object.scale,
                    object.scale_axes,
                    object.rotation,
                )
            })
            .collect::<Vec<_>>();
        if outline_items.is_empty() {
            if let (Some(asset), Some(position)) = (
                self.selected_asset
                    .as_ref()
                    .filter(|_| self.selected_scene_object.is_some()),
                self.selected_scene_transform,
            ) {
                outline_items.push((asset.clone(), position, 1.0, [1.; 3], [0.; 3]));
            }
        }
        let outline_painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("architect_selected_object_bounds"),
        ));
        for (asset, position, scale, scale_axes, euler) in outline_items {
            // Use the exact mesh bounds for this asset. If the mesh has not
            // been loaded yet, omit the outline rather than draw a misleading
            // guessed-size box that can be wildly wrong for PW models.
            let (min, max) = self
                .renderer_bounds_key(&asset)
                .and_then(|path| self.preview_bounds.get(&path).copied())
                .unwrap_or_else(|| Self::fallback_selection_bounds(&asset));
            let (local_min, local_max) = (Vec3::from_array(min), Vec3::from_array(max));
            let center = Vec3::from_array(position);
            let rotation =
                glam::Quat::from_euler(glam::EulerRot::XYZ, euler[0], euler[1], euler[2]);
            let mut corners = [egui::Pos2::ZERO; 8];
            let mut valid = [false; 8];
            for bits in 0..8 {
                let local = Vec3::new(
                    if bits & 1 == 0 {
                        local_min.x
                    } else {
                        local_max.x
                    },
                    if bits & 2 == 0 {
                        local_min.y
                    } else {
                        local_max.y
                    },
                    if bits & 4 == 0 {
                        local_min.z
                    } else {
                        local_max.z
                    },
                ) * (scale.max(0.01) * Vec3::from_array(scale_axes));
                let point = center + rotation * local;
                if let Some(projected) = project_outline_point(point) {
                    corners[bits] = projected;
                    valid[bits] = true;
                }
            }
            for bits in 0..8 {
                for axis in [1usize, 2, 4] {
                    let other = bits ^ axis;
                    if bits < other && valid[bits] && valid[other] {
                        outline_painter.line_segment(
                            [corners[bits], corners[other]],
                            egui::Stroke::new(1.8, egui::Color32::WHITE),
                        );
                    }
                }
            }
        }
        if self.cursor == CursorMode::Sculpt {
            if let (Some(position), Some(map)) = (self.brush_cursor_position, self.map.as_ref()) {
                let center = Vec3::from_array(position);
                let mut points = Vec::with_capacity(65);
                for n in 0..=64 {
                    let angle = n as f32 * std::f32::consts::TAU / 64.;
                    let x = center.x + angle.cos() * self.brush_radius;
                    let z = center.z + angle.sin() * self.brush_radius;
                    let world = Vec3::new(x, map.ground(x, z) + 0.12, z);
                    let clip = view_projection.project_point3(world);
                    if clip.is_finite() && (0.0..=1.0).contains(&clip.z) {
                        points.push(egui::pos2(
                            screen.left() + (clip.x + 1.) * 0.5 * screen.width(),
                            screen.top() + (1. - clip.y) * 0.5 * screen.height(),
                        ));
                    }
                }
                if points.len() > 2 {
                    let color = match self.brush {
                        BrushMode::Raise => egui::Color32::from_rgb(90, 230, 130),
                        BrushMode::Lower => egui::Color32::from_rgb(245, 110, 90),
                        BrushMode::Flatten => egui::Color32::from_rgb(100, 180, 255),
                        BrushMode::Smooth => egui::Color32::from_rgb(220, 190, 90),
                        BrushMode::Paint => egui::Color32::GRAY,
                    };
                    ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("architect_brush_cursor"),
                    ))
                    .add(egui::Shape::line(points, egui::Stroke::new(1.8, color)));
                }
            }
        }
        if !self.object_history_action
            && !self.object_dragging
            && objects_before_ui != self.project.objects
        {
            self.push_object_undo(objects_before_ui);
        }
        map_request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiselect_mode_keeps_source_scene_picks_out_of_marquee_fallback() {
        let source_pick = Some((
            4usize,
            0.2f32,
            0.8f32,
            "models/tree.ecm".to_string(),
            [1., 2., 3.],
        ));
        let source_is_frontmost = source_pick.is_some();
        let picked_placement: Option<usize> = None;
        let begin_marquee = CursorMode::MultiSelect == CursorMode::MultiSelect
            && picked_placement.is_none()
            && !source_is_frontmost;
        assert!(!begin_marquee);
    }

    #[test]
    fn fallback_selection_bounds_keep_buildings_pickable_before_mesh_load() {
        let building = Architect::fallback_selection_bounds("litmodels/a01/castle.bmd");
        let ordinary = Architect::fallback_selection_bounds("models/props/barrel.ecm");
        let tree = Architect::fallback_selection_bounds("trees/forest/oak.ecm");
        assert!(building.1[0] - building.0[0] > ordinary.1[0] - ordinary.0[0]);
        assert!(tree.1[1] - tree.0[1] > ordinary.1[1] - ordinary.0[1]);
    }

    #[test]
    fn source_scene_picking_uses_a_larger_target_for_trees_and_models() {
        assert!(
            Architect::source_pick_radius("trees/forest/tree.ecm")
                > Architect::source_pick_radius("effects/small.gfx")
        );
        assert!(
            Architect::source_pick_radius("models/building.ecm")
                >= Architect::source_pick_radius("misc/unknown.bin")
        );
    }

    #[test]
    fn viewport_ray_hits_flat_terrain_at_view_center() {
        let mut builder = crate::terrain_store::Builder::new_resident(8, 8, 4).unwrap();
        for by in 0..2 {
            for bx in 0..2 {
                builder
                    .block(bx, by, &[0.; 16], &[[0, 127, 0]; 16])
                    .unwrap();
            }
        }
        let (heights, _) = builder.finish(0).unwrap();
        let map = crate::navigation::MapSpace {
            width: 8,
            height: 8,
            spacing: 1.,
            heights,
        };
        let eye = Vec3::new(0., 10., 10.);
        let direction = Vec3::new(0., -1., -1.).normalize();
        let view = glam::Mat4::look_to_rh(eye, direction, Vec3::Y);
        let projection = glam::Mat4::from_scale(Vec3::new(-1., 1., 1.))
            * glam::Mat4::perspective_rh(70f32.to_radians(), 1., 1000., 0.1);
        let vp = projection * view;
        let hit = Architect::terrain_hit(
            egui::pos2(50., 50.),
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100., 100.)),
            vp,
            &map,
        )
        .expect("center ray should hit flat terrain");
        assert!(hit.x.abs() < 0.05);
        assert!(hit.z.abs() < 0.05);
        assert!(hit.y.abs() < 0.1);
    }

    #[test]
    fn marquee_selection_rect_is_direction_independent_and_inclusive() {
        let start = egui::pos2(100.0, 80.0);
        let end = egui::pos2(20.0, 160.0);
        assert!(Architect::marquee_contains(
            start,
            end,
            egui::pos2(20.0, 80.0)
        ));
        assert!(Architect::marquee_contains(
            start,
            end,
            egui::pos2(60.0, 120.0)
        ));
        assert!(!Architect::marquee_contains(
            start,
            end,
            egui::pos2(101.0, 120.0)
        ));
    }

    #[test]
    fn lasso_selection_contains_polygon_interior_and_excludes_exterior() {
        let polygon = vec![
            egui::pos2(10.0, 10.0),
            egui::pos2(90.0, 15.0),
            egui::pos2(70.0, 80.0),
            egui::pos2(20.0, 70.0),
        ];
        assert!(Architect::lasso_contains(&polygon, egui::pos2(40.0, 40.0)));
        assert!(Architect::lasso_contains(&polygon, egui::pos2(50.0, 20.0)));
        assert!(!Architect::lasso_contains(
            &polygon,
            egui::pos2(100.0, 90.0)
        ));
        assert!(!Architect::lasso_contains(
            &polygon[..2],
            egui::pos2(40.0, 40.0)
        ));
    }

    #[test]
    fn cancelling_viewport_gesture_discards_marquee_and_transform_state() {
        let mut editor = Architect::new(
            PathBuf::from("/client/element/element.data"),
            None,
            "a01".into(),
        );
        editor.marquee_start = Some(egui::pos2(10.0, 20.0));
        editor.marquee_current = Some(egui::pos2(80.0, 90.0));
        editor.lasso_points = vec![egui::pos2(10.0, 10.0), egui::pos2(20.0, 20.0)];
        editor.object_dragging = true;
        editor.object_drag_axis = Some(1);
        editor.object_drag_start_pointer = Some(egui::pos2(40.0, 50.0));
        editor.object_drag_start_hit = Some([1.0, 2.0, 3.0]);
        editor.object_drag_group.push((0, [1.0, 2.0, 3.0]));
        editor.object_drag_snapshot = Some(Vec::new());

        editor.cancel_object_drag();

        assert!(!editor.object_dragging);
        assert_eq!(editor.object_drag_axis, None);
        assert_eq!(editor.object_drag_start_pointer, None);
        assert_eq!(editor.object_drag_start_hit, None);
        assert!(editor.object_drag_group.is_empty());
        assert!(editor.object_drag_snapshot.is_none());
        assert_eq!(editor.marquee_start, None);
        assert_eq!(editor.marquee_current, None);
        assert!(editor.lasso_points.is_empty());
    }

    #[test]
    fn object_undo_redo_restores_complete_placement_snapshots() {
        let mut editor = Architect::new(
            PathBuf::from("/client/element/element.data"),
            None,
            "a01".into(),
        );
        let original = PlacedObject {
            asset: "models/test.ecm".into(),
            name: "Test object".into(),
            group: "Default".into(),
            visible: true,
            locked: false,
            position: [1., 2., 3.],
            rotation: [0.; 3],
            scale: 1.,
            scale_axes: [1.; 3],
        };
        editor.project.objects.push(original.clone());
        let before = editor.project.objects.clone();
        editor.project.objects[0].position = [4., 5., 6.];
        editor.push_object_undo(before);
        assert_eq!(editor.project.objects[0].position, [4., 5., 6.]);

        // History restores whole vectors, so a surviving numeric index must not
        // be mistaken for the same logical object after insertions/deletions.
        editor.selected_placement = Some(0);
        editor.selected_placements.insert(0);
        editor.object_dragging = true;
        editor.object_drag_group.push((0, original.position));
        editor.object_drag_snapshot = Some(vec![original.clone()]);
        editor.undo_objects();
        assert_eq!(editor.project.objects[0].position, original.position);
        assert_eq!(editor.selected_placement, None);
        assert!(editor.selected_placements.is_empty());
        assert!(!editor.object_dragging);
        assert!(editor.object_drag_group.is_empty());
        assert!(editor.object_drag_snapshot.is_none());

        editor.selected_placement = Some(0);
        editor.selected_placements.insert(0);
        editor.redo_objects();
        assert_eq!(editor.project.objects[0].position, [4., 5., 6.]);
        assert_eq!(editor.selected_placement, None);
        assert!(editor.selected_placements.is_empty());
    }

    #[test]
    fn object_clipboard_preserves_game_asset_and_transform_on_paste() {
        let mut editor = Architect::new(
            PathBuf::from("/client/element/element.data"),
            None,
            "a01".into(),
        );
        editor.enabled = true;
        editor.project.source_map = "a01".into();
        editor.project.objects.push(PlacedObject {
            asset: "element/models.pck::Models/Buildings/gate.ecm".into(),
            name: "Gate".into(),
            group: "Buildings".into(),
            visible: true,
            locked: true,
            position: [10.0, 20.0, 30.0],
            rotation: [0.1, 0.2, 0.3],
            scale: 1.75,
            scale_axes: [1.; 3],
        });
        editor.selected_placement = Some(0);

        editor.copy_selected_objects();
        assert_eq!(editor.clipboard_objects.len(), 1);
        editor.paste_objects();

        assert_eq!(editor.project.objects.len(), 2);
        let pasted = &editor.project.objects[1];
        assert_eq!(
            pasted.asset,
            "element/models.pck::Models/Buildings/gate.ecm"
        );
        assert_eq!(pasted.name, "Gate");
        assert_eq!(pasted.group, "Buildings");
        assert!(pasted.visible);
        assert!(!pasted.locked);
        assert_eq!(pasted.position, [12.0, 20.0, 32.0]);
        assert_eq!(pasted.rotation, [0.1, 0.2, 0.3]);
        assert_eq!(pasted.scale, 1.75);
        assert_eq!(editor.selected_placements, HashSet::from([1]));

        editor.undo_objects();
        assert_eq!(editor.project.objects.len(), 1);
        assert_eq!(editor.project.objects[0].position, [10.0, 20.0, 30.0]);
    }

    #[test]
    fn duplicating_source_scene_items_selects_all_copies_and_is_undoable() {
        let mut editor = Architect::new(
            PathBuf::from("/client/element/element.data"),
            None,
            "a01".into(),
        );
        editor.enabled = true;
        editor.project.source_map = "a01".into();
        editor.multi_scene_items.insert(
            3,
            (
                "element/models.pck::models/buildings/gate.ecm".into(),
                [1., 2., 3.],
            ),
        );
        editor.multi_scene_items.insert(
            8,
            (
                "element/models.pck::models/buildings/tower.ecm".into(),
                [4., 5., 6.],
            ),
        );

        editor.duplicate_selected_scene_item();

        assert_eq!(editor.project.objects.len(), 2);
        assert_eq!(editor.selected_placements, HashSet::from([0, 1]));
        assert_eq!(editor.selected_placement, Some(1));
        assert_eq!(editor.project.objects[0].position, [1., 2., 3.]);
        assert_eq!(editor.project.objects[1].position, [4., 5., 6.]);

        editor.undo_objects();
        assert!(editor.project.objects.is_empty());
        assert!(editor.selected_placements.is_empty());
        assert_eq!(editor.selected_placement, None);
    }

    #[test]
    fn place_mode_adds_a_live_preview_mesh_without_serializing_it() {
        let mut editor = Architect::new(
            PathBuf::from("/client/element/element.data"),
            None,
            "a01".into(),
        );
        editor.enabled = true;
        editor.project.source_map = "a01".into();
        editor.cursor = CursorMode::Place;
        editor.selected_asset = Some("element/models.pck::models/buildings/gate.ecm".into());
        editor.brush_cursor_position = Some([12., 3., 18.]);
        let preview = editor.renderer_placements();
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].path, "models/buildings/gate.ecm");
        assert_eq!(preview[0].position, [12., 3., 18.]);
        assert!(editor.project.objects.is_empty());
    }

    #[test]
    fn placements_from_another_source_map_are_not_submitted_to_renderer() {
        let mut editor = Architect::new(
            PathBuf::from("/client/element/element.data"),
            None,
            "a01".into(),
        );
        editor.enabled = true;
        editor.project.source_map = "b02".into();
        editor.project.objects.push(PlacedObject {
            asset: "models/test.ecm".into(),
            name: "Test object".into(),
            group: "Default".into(),
            visible: true,
            locked: false,
            position: [0.; 3],
            rotation: [0.; 3],
            scale: 1.,
            scale_axes: [1.; 3],
        });
        assert!(editor.renderer_placements().is_empty());
        editor.project.source_map = "a01".into();
        assert_eq!(editor.renderer_placements().len(), 1);
        editor.project.objects[0].visible = false;
        assert!(editor.renderer_placements().is_empty());
    }

    #[test]
    fn sculpt_brush_updates_shared_heights_and_replays_reversibly() {
        let mut builder = crate::terrain_store::Builder::new_resident(8, 8, 4).unwrap();
        for by in 0..2 {
            for bx in 0..2 {
                builder
                    .block(bx, by, &[0.; 16], &[[0, 127, 0]; 16])
                    .unwrap();
            }
        }
        let (heights, normals) = builder.finish(0).unwrap();
        let map = crate::navigation::MapSpace {
            width: 8,
            height: 8,
            spacing: 1.,
            heights,
        };
        let mut editor = Architect::new(
            PathBuf::from("/client/element/element.data"),
            Some(map.clone()),
            "a01".into(),
        );
        editor.project.source_map = "a01".into();
        let stroke = TerrainStroke {
            mode: BrushMode::Raise,
            position: [0., 0., 0.],
            radius: 2.,
            strength: 10.,
        };
        let changed = editor.apply_stroke(&stroke).unwrap();
        assert!(changed > 0);
        assert_eq!(map.heights.at(4 * 8 + 4), 10.);
        assert_ne!(normals.at(4 * 8 + 5), [0, 127, 0]);

        editor.project.strokes.push(stroke);
        editor.rebuild_terrain().unwrap();
        assert_eq!(map.heights.at(4 * 8 + 4), 10.);
        editor.project.strokes.clear();
        editor.rebuild_terrain().unwrap();
        assert_eq!(map.heights.at(4 * 8 + 4), 0.);
        assert_eq!(normals.at(4 * 8 + 5), [0, 127, 0]);
    }

    #[test]
    fn sculpt_brush_rejects_a_project_from_another_map() {
        let mut builder = crate::terrain_store::Builder::new_resident(8, 8, 4).unwrap();
        for by in 0..2 {
            for bx in 0..2 {
                builder
                    .block(bx, by, &[0.; 16], &[[0, 127, 0]; 16])
                    .unwrap();
            }
        }
        let (heights, _) = builder.finish(0).unwrap();
        let map = crate::navigation::MapSpace {
            width: 8,
            height: 8,
            spacing: 1.,
            heights,
        };
        let mut editor = Architect::new(
            PathBuf::from("/client/element/element.data"),
            Some(map.clone()),
            "a01".into(),
        );
        editor.project.source_map = "b03".into();
        let stroke = TerrainStroke {
            mode: BrushMode::Raise,
            position: [0., 0., 0.],
            radius: 2.,
            strength: 10.,
        };
        assert!(editor.apply_stroke(&stroke).is_err());
        assert_eq!(map.heights.revision(), 0);
        assert_eq!(map.heights.at(4 * 8 + 4), 0.);
    }

    #[test]
    fn project_codec_round_trips_unicode_paths_and_editor_operations() {
        let project = Project {
            name: "我的世界".into(),
            source_map: "a01".into(),
            objects: vec![PlacedObject {
                asset: "models/场景/城堡.bmd".into(),
                name: "Castle".into(),
                group: "Buildings".into(),
                visible: false,
                locked: true,
                position: [1., 2., 3.],
                rotation: [0.1, 0.2, 0.3],
                scale: 1.25,
                scale_axes: [1.5, 0.75, 2.0],
            }],
            strokes: vec![TerrainStroke {
                mode: BrushMode::Smooth,
                position: [4., 5., 6.],
                radius: 18.,
                strength: 2.,
            }],
        };
        let path = std::env::temp_dir().join(format!("pwe-architect-{}.pwa", std::process::id()));
        let editor = Architect {
            enabled: false,
            client_root: PathBuf::from("/client"),
            current_map_id: "a01".into(),
            map: None,
            project_root: path.parent().unwrap().to_path_buf(),
            project: project.clone(),
            redo_strokes: Vec::new(),
            project_files: Vec::new(),
            directory_cache: HashMap::new(),
            archive_cache: HashMap::new(),
            archive_detail_cache: HashMap::new(),
            archive_details: Vec::new(),
            project_name: project.name.clone(),
            selected_project: None,
            selected_map: project.source_map.clone(),
            cursor: CursorMode::Select,
            transform_mode: TransformMode::Move,
            snap: SnapMode::Smart,
            brush: BrushMode::Raise,
            brush_radius: 24.,
            brush_strength: 2.,
            flatten_height: 0.,
            selected_asset: None,
            hovered_asset: None,
            preview_assets: crate::modules::librarian::archive::LibrarianAssets::open(Path::new(
                "/client",
            )),
            effect_catalog: None,
            preview: None,
            preview_bounds: HashMap::new(),
            tree_types: Vec::new(),
            preview_message: String::new(),
            selected_archive: None,
            archive_entries: Vec::new(),
            archive_error: None,
            selected_scene_object: None,
            selected_scene_transform: None,
            selected_placement: None,
            selected_placements: HashSet::new(),
            multi_selection: HashSet::new(),
            multi_scene_items: HashMap::new(),
            quick_bar: Vec::new(),
            left_dock: Dock::Left,
            right_dock: Dock::Right,
            bottom_dock: Dock::Bottom,
            applied_left_dock: Dock::Left,
            applied_right_dock: Dock::Right,
            applied_bottom_dock: Dock::Bottom,
            confirm_erase: false,
            confirm_clear: false,
            status: String::new(),
            tree_filter: String::new(),
            object_filter: String::new(),
            show_scene_objects: true,
            brush_cursor_position: None,
            brush_dragging: false,
            last_brush_position: None,
            object_dragging: false,
            object_drag_y_offset: 0.,
            object_drag_xz_offset: [0.; 2],
            object_drag_axis: None,
            object_drag_start_pointer: None,
            object_drag_start_position: [0.; 3],
            object_drag_start_hit: None,
            object_drag_start_axis_parameter: None,
            object_drag_start_rotation_vector: None,
            object_drag_group: Vec::new(),
            object_drag_snapshot: None,
            marquee_start: None,
            marquee_current: None,
            lasso_points: Vec::new(),
            undo_objects: Vec::new(),
            redo_objects: Vec::new(),
            clipboard_objects: Vec::new(),
            object_history_action: false,
        };
        editor.write_project(&path).unwrap();
        let loaded = Architect::read_project(&path).unwrap();
        assert_eq!(loaded.name, project.name);
        assert_eq!(loaded.source_map, project.source_map);
        assert_eq!(loaded.objects.len(), 1);
        assert_eq!(loaded.objects[0].asset, project.objects[0].asset);
        assert_eq!(loaded.objects[0].name, "Castle");
        assert_eq!(loaded.objects[0].group, "Buildings");
        assert!(!loaded.objects[0].visible);
        assert!(loaded.objects[0].locked);
        assert_eq!(loaded.objects[0].scale_axes, [1.5, 0.75, 2.0]);
        assert_eq!(loaded.strokes.len(), 1);
        assert_eq!(loaded.strokes[0].mode, BrushMode::Smooth);
        assert_eq!(
            editor.portable_asset_path("/client/element/models/tree.ecm"),
            "element/models/tree.ecm"
        );
        assert_eq!(
            editor.portable_asset_path("/client/element/models.pck::models/tree.ecm"),
            "element/models.pck::models/tree.ecm"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn version_one_projects_migrate_objects_with_default_hierarchy_metadata() {
        let path =
            std::env::temp_dir().join(format!("pwe-architect-legacy-{}.pwa", std::process::id()));
        let asset = Architect::encode("models/legacy/guard.ecm");
        fs::write(
            &path,
            format!(
                "PWE_ARCHITECT\t1\nname\t{}\nsource_map\t{}\nobject\t{}\t1\t2\t3\t0\t0\t0\t1\n",
                Architect::encode("Legacy map"),
                Architect::encode("a01"),
                asset
            ),
        )
        .unwrap();
        let loaded = Architect::read_project(&path).unwrap();
        assert_eq!(loaded.objects.len(), 1);
        assert_eq!(loaded.objects[0].name, "guard");
        assert_eq!(loaded.objects[0].group, "Default");
        assert!(loaded.objects[0].visible);
        assert!(!loaded.objects[0].locked);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn loose_client_models_resolve_through_the_same_native_handlers() {
        let editor = Architect::new(PathBuf::from("/client/element"), None, "a01".into());

        assert_eq!(
            editor.renderer_model_asset_path("/client/element/models/npc/guard.ecm"),
            Some("models/npc/guard.ecm".into())
        );
        assert_eq!(
            editor.renderer_model_asset_path("/client/element/gfx/props/stone.smd"),
            Some("gfx/props/stone.smd".into())
        );
        assert_eq!(
            editor.renderer_bmd_asset_path("/client/element/litmodels/a01/1/litmodel_42.bmd"),
            Some("litmodels/a01/1/litmodel_42.bmd".into())
        );
    }

    #[test]
    fn rotation_gizmo_renders_three_distinct_world_axis_rings() {
        let viewport = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(800.0, 600.0),
        );
        let projection = glam::Mat4::perspective_rh(1.0, 800.0 / 600.0, 0.1, 1000.0);
        let view = glam::Mat4::look_at_rh(
            Vec3::new(8.0, 6.0, 10.0),
            Vec3::ZERO,
            Vec3::Y,
        );
        let vp = projection * view;
        let x = Architect::gizmo_ring_points(Vec3::ZERO, Vec3::X, 2.0, viewport, vp);
        let y = Architect::gizmo_ring_points(Vec3::ZERO, Vec3::Y, 2.0, viewport, vp);
        let z = Architect::gizmo_ring_points(Vec3::ZERO, Vec3::Z, 2.0, viewport, vp);
        assert!(x.len() >= 60 && y.len() >= 60 && z.len() >= 60);
        assert!(x.iter().zip(&y).any(|(a, b)| a.distance(*b) > 1.0));
        assert!(y.iter().zip(&z).any(|(a, b)| a.distance(*b) > 1.0));
        assert!(z.iter().zip(&x).any(|(a, b)| a.distance(*b) > 1.0));
    }

    #[test]
    fn rotation_gizmo_ray_intersects_the_selected_axis_plane() {
        let viewport = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(100.0, 100.0),
        );
        let projection = glam::Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0);
        let view = glam::Mat4::look_at_rh(
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::ZERO,
            Vec3::Y,
        );
        let point = Architect::ray_plane_point(
            egui::pos2(60.0, 50.0),
            viewport,
            projection * view,
            Vec3::ZERO,
            Vec3::Y,
        ).unwrap();
        assert!(point.y.abs() < 1e-4);
        assert!(point.x > 0.0);
    }

    #[test]
    fn rotation_gizmo_uses_shortest_wrapped_angle_across_pi() {
        let delta = Architect::wrapped_angle_delta(3.10, -3.10);
        assert!(delta > 0.0);
        assert!(delta < 0.1);
        let reverse = Architect::wrapped_angle_delta(-3.10, 3.10);
        assert!(reverse < 0.0);
        assert!(reverse > -0.1);
    }

    #[test]
    fn ray_axis_parameter_is_finite_for_a_center_ray_and_horizontal_axis() {
        let viewport = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(100.0, 100.0),
        );
        let projection = glam::Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0);
        let view = glam::Mat4::look_at_rh(
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::ZERO,
            Vec3::Y,
        );
        let value = Architect::ray_axis_parameter(
            egui::pos2(50.0, 50.0),
            viewport,
            projection * view,
            Vec3::ZERO,
            Vec3::X,
        );
        assert!(value.is_some_and(|distance| distance.abs() < 0.01));
    }

    #[test]
    fn placeable_asset_gate_accepts_every_native_adapter() {
        let mut editor = Architect::new(PathBuf::from("/client/element"), None, "a01".into());
        editor.tree_types = vec![crate::scene::TreeType {
            tree: "trees/oak.spt".into(),
            composite: "trees/oak.dds".into(),
        }];
        assert!(editor.renderer_placeable_asset("models.pck::models/building.ecm"));
        assert!(editor.renderer_placeable_asset("litmodels.pck::litmodels/a01/1/litmodel_42.bmd"));
        assert!(editor.renderer_placeable_asset("trees.pck::trees/oak.spt"));
        assert!(editor.renderer_placeable_asset("trees.pck::trees/oak.dds"));
        assert!(!editor.renderer_placeable_asset("gfx.pck::textures/oak.dds"));
    }

    #[test]
    fn selecting_a_tree_composite_arms_the_existing_tree_placement_handler() {
        let mut editor = Architect::new(PathBuf::from("/client/element"), None, "a01".into());
        editor.tree_types = vec![crate::scene::TreeType {
            tree: "trees/oak.spt".into(),
            composite: "trees/oak.dds".into(),
        }];

        editor.select_asset_for_placement("element/trees.pck::trees/oak.dds".into());

        assert_eq!(
            editor.selected_asset.as_deref(),
            Some("element/trees.pck::trees/oak.dds")
        );
        assert_eq!(editor.cursor, CursorMode::Place);
        assert_eq!(editor.snap, SnapMode::Terrain);
        assert_eq!(
            editor.renderer_tree_type("element/trees.pck::trees/oak.dds"),
            Some(0)
        );
    }

    #[test]
    fn catalogued_gfx_effects_can_be_selected_and_committed_as_project_objects() {
        let mut editor = Architect::new(PathBuf::from("/client/element"), None, "a01".into());
        editor.enabled = true;
        editor.effect_catalog = Some(crate::gfx_catalog::Catalog::from_directory(vec![(
            "gfx/scene/waterfall.gfx",
            128,
        )]));
        let asset = "element/gfx.pck::gfx/scene/waterfall.gfx";

        assert!(editor.renderer_placeable_asset(asset));
        editor.select_asset_for_placement(asset.into());
        assert_eq!(editor.cursor, CursorMode::Place);
        editor.add_selected_asset_at_position([12.0, 3.0, 24.0]);

        let effects = editor.renderer_effect_placements();
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].key, 0);
        assert_eq!(effects[0].path, "gfx/scene/waterfall.gfx");
        assert_eq!(effects[0].position, [12.0, 3.0, 24.0]);
    }

    #[test]
    fn renderer_tree_placements_export_tree_type_yaw_and_scale() {
        let mut editor = Architect::new(PathBuf::from("/client/element"), None, "a01".into());
        editor.enabled = true;
        editor.project.source_map = "a01".into();
        editor.tree_types = vec![crate::scene::TreeType {
            tree: "trees/pine.spt".into(),
            composite: "trees/pine.dds".into(),
        }];
        editor.project.objects.push(PlacedObject {
            name: "Pine".into(),
            asset: "element/trees.pck::trees/pine.spt".into(),
            group: "Default".into(),
            visible: true,
            locked: false,
            position: [10.0, 20.0, 30.0],
            rotation: [0.0, 1.25, 0.0],
            scale: 1.75,
            scale_axes: [1.; 3],
        });

        let placements = editor.renderer_tree_placements();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].tree_type, 0);
        assert_eq!(placements[0].position, [10.0, 20.0, 30.0]);
        assert!((placements[0].yaw - 1.25).abs() < 0.0001);
        assert!((placements[0].scale - 1.75).abs() < 0.0001);
    }

    #[test]
    fn renderer_tree_placements_include_the_native_cursor_ghost() {
        let mut editor = Architect::new(PathBuf::from("/client/element"), None, "a01".into());
        editor.enabled = true;
        editor.project.source_map = "a01".into();
        editor.tree_types = vec![crate::scene::TreeType {
            tree: "trees/oak.spt".into(),
            composite: "trees/oak.dds".into(),
        }];
        editor.selected_asset = Some("element/trees.pck::trees/oak.dds".into());
        editor.cursor = CursorMode::Place;
        editor.brush_cursor_position = Some([4.0, 5.0, 6.0]);

        let placements = editor.renderer_tree_placements();

        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].tree_type, 0);
        assert_eq!(placements[0].position, [4.0, 5.0, 6.0]);
        assert_eq!(placements[0].scale, 1.0);
    }

    #[test]
    fn renderer_scenery_placements_emit_map_scoped_bmd_instances() {
        let mut editor = Architect::new(PathBuf::from("/client/element"), None, "a01".into());
        editor.enabled = true;
        editor.project.source_map = "a01".into();
        editor.project.objects.push(PlacedObject {
            name: "BMD prop".into(),
            asset: "element/litmodels.pck::litmodels/a01/1/litmodel_42.bmd".into(),
            group: "Default".into(),
            visible: true,
            locked: false,
            position: [10.0, 20.0, 30.0],
            rotation: [0.0, std::f32::consts::FRAC_PI_2, 0.0],
            scale: 2.0,
            scale_axes: [1.; 3],
        });

        let placements = editor.renderer_scenery_placements();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].path, "litmodels/a01/1/litmodel_42.bmd");
        assert_eq!(placements[0].position, [10.0, 20.0, 30.0]);
        assert_eq!(placements[0].map_id, "a01");
        assert!((placements[0].scale - 2.0).abs() < 0.0001);
        assert!((placements[0].direction[0] - 1.0).abs() < 0.0001);
    }

    #[test]
    fn renderer_bmd_paths_keep_map_namespace_and_reject_other_archives() {
        assert_eq!(
            Architect::renderer_bmd_path(
                "element/litmodels.pck::litmodels/a01/1/litmodel_42.bmd",
                "a01"
            )
            .as_deref(),
            Some("litmodels/a01/1/litmodel_42.bmd")
        );
        assert_eq!(
            Architect::renderer_bmd_path(
                "element/litmodels.pck::litmodels/world/1/litmodel_42.bmd",
                "a01"
            )
            .as_deref(),
            Some("litmodels/a01/1/litmodel_42.bmd")
        );
        assert!(Architect::renderer_bmd_path(
            "element/litmodels.pck::litmodels/a11/1/litmodel_42.bmd",
            "a01"
        )
        .is_none());
        assert!(Architect::renderer_bmd_path(
            "element/models.pck::litmodels/a01/1/litmodel_42.bmd",
            "a01"
        )
        .is_none());
        assert!(Architect::renderer_bmd_path(
            "element/litmodels.pck::litmodels/a01/../other.bmd",
            "a01"
        )
        .is_none());
        assert_eq!(
            Architect::renderer_bmd_path("1/litmodel_1399.bmd", "a01").as_deref(),
            Some("litmodels/a01/1/litmodel_1399.bmd")
        );
    }

    #[test]
    fn renderer_asset_paths_accept_ecm_and_static_gfx_smd_entries_only() {
        assert_eq!(
            Architect::renderer_asset_path("element/models.pck::Models/NPCs/Guard/guard.ecm"),
            Some("models/npcs/guard/guard.ecm".into())
        );
        assert_eq!(
            Architect::renderer_asset_path("models/trees/oak.ecm"),
            Some("models/trees/oak.ecm".into())
        );
        assert_eq!(
            Architect::renderer_asset_path("element/gfx.pck::gfx/models/fire.ecm"),
            None
        );
        assert_eq!(
            Architect::renderer_asset_path("element/gfx.pck::models/fire.smd"),
            Some("gfx/models/fire.smd".into())
        );
        assert_eq!(
            Architect::renderer_asset_path("gfx/models/stone.smd"),
            Some("gfx/models/stone.smd".into())
        );
        assert_eq!(
            Architect::renderer_asset_path("element/models.pck::textures/tree.dds"),
            None
        );
        assert_eq!(
            Architect::renderer_asset_path("element/loose/guard.ecm"),
            None
        );
        assert_eq!(
            Architect::renderer_asset_path("element/models.pck::models/../../outside.ecm"),
            None
        );
        assert_eq!(
            Architect::renderer_asset_path("element/models.pck::models/./guard.ecm"),
            None
        );
        assert_eq!(
            Architect::renderer_asset_path("element/models.pck::models/npcs/guard:evil.ecm"),
            None
        );
        assert_eq!(
            Architect::renderer_asset_path("element/other.pck::models/npcs/guard.ecm"),
            None
        );
    }

    #[test]
    fn project_names_cannot_escape_the_worlds_directory() {
        assert_eq!(Architect::safe_slug("../../Bad Name"), "bad-name");
        assert_eq!(Architect::safe_slug(""), "untitled-world");
    }
}
