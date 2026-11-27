//! Architect: an isolated, non-destructive world-authoring workspace.
//!
//! The first editor layer stores custom placements and terrain-brush operations
//! in portable .pwa project files. It deliberately does not rewrite native
//! ECWLD/T2BK client data: a project remains reversible until a validated native
//! exporter exists.
use glam::Vec3;
use std::{
    collections::{HashMap, HashSet},
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
enum CursorMode {
    Select,
    MultiSelect,
    Place,
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

#[derive(Clone, Debug)]
struct PlacedObject {
    asset: String,
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
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
    project_name: String,
    selected_project: Option<usize>,
    selected_map: String,
    cursor: CursorMode,
    snap: SnapMode,
    brush: BrushMode,
    brush_radius: f32,
    brush_strength: f32,
    flatten_height: f32,
    selected_asset: Option<String>,
    selected_archive: Option<PathBuf>,
    archive_entries: Vec<String>,
    archive_error: Option<String>,
    selected_scene_object: Option<usize>,
    selected_scene_transform: Option<[f32; 3]>,
    selected_placement: Option<usize>,
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
        let client_root = element.parent().unwrap_or(&element).to_path_buf();
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
            project_name: "Untitled World".into(),
            selected_project: None,
            selected_map: "__empty__".into(),
            cursor: CursorMode::Select,
            snap: SnapMode::Smart,
            brush: BrushMode::Raise,
            brush_radius: 24.,
            brush_strength: 2.,
            flatten_height: 0.,
            selected_asset: None,
            selected_archive: None,
            archive_entries: Vec::new(),
            archive_error: None,
            selected_scene_object: None,
            selected_scene_transform: None,
            selected_placement: None,
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
        };
        editor.refresh_projects();
        editor
    }

    pub fn active(&self) -> bool {
        self.enabled
    }

    pub fn set_active(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            self.status = "Architect enabled. The original world remains unchanged.".into();
        }
    }

    fn refresh_projects(&mut self) {
        self.project_files.clear();
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
        let mut out = String::from("PWE_ARCHITECT\t1\n");
        out.push_str(&format!("name\t{}\n", Self::encode(&self.project.name)));
        out.push_str(&format!(
            "source_map\t{}\n",
            Self::encode(&self.project.source_map)
        ));
        for object in &self.project.objects {
            out.push_str(&format!(
                "object\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\n",
                Self::encode(&object.asset),
                object.position[0],
                object.position[1],
                object.position[2],
                object.rotation[0],
                object.rotation[1],
                object.rotation[2],
                object.scale
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
        let temp = path.with_extension("pwa.tmp");
        fs::write(&temp, out).map_err(|e| format!("{}: {e}", temp.display()))?;
        if path.exists() {
            fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        fs::rename(&temp, path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(())
    }

    fn read_project(path: &Path) -> Result<Project, String> {
        let metadata = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.len() > 64 * 1024 * 1024 {
            return Err("Architect project exceeds the 64 MiB safety limit".into());
        }
        let source = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut lines = source.lines();
        if lines.next() != Some("PWE_ARCHITECT\t1") {
            return Err("unsupported Architect project version".into());
        }
        let mut project = Project::default();
        for (line_no, line) in lines.enumerate() {
            let fields = line.split('\t').collect::<Vec<_>>();
            match fields.first().copied().unwrap_or("") {
                "name" if fields.len() == 2 => project.name = Self::decode(fields[1])?,
                "source_map" if fields.len() == 2 => project.source_map = Self::decode(fields[1])?,
                "object" if fields.len() == 9 => {
                    if project.objects.len() >= 100_000 {
                        return Err("Architect project has too many object placements".into());
                    }
                    let mut n = [0f32; 7];
                    for (slot, value) in n.iter_mut().zip(&fields[2..]) {
                        *slot = value.parse().map_err(|_| {
                            format!("invalid object number on line {}", line_no + 2)
                        })?;
                        if !slot.is_finite() {
                            return Err("non-finite object transform".into());
                        }
                    }
                    project.objects.push(PlacedObject {
                        asset: Self::decode(fields[1])?,
                        position: [n[0], n[1], n[2]],
                        rotation: [n[3], n[4], n[5]],
                        scale: n[6].clamp(0.01, 1000.),
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
                        radius: n[3].clamp(0.1, 10000.),
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
        let map = self.map.as_ref().ok_or("No terrain is loaded for sculpting")?;
        if !self.project.source_map.is_empty()
            && !self.current_map_id.is_empty()
            && !self.project.source_map.eq_ignore_ascii_case(&self.current_map_id)
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
            return Err("Terrain paint is not implemented yet; choose Raise, Lower, Flatten or Smooth.".into());
        }
        let radius = stroke.radius.clamp(0.1, 10000.);
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
                        let mut count = 0.;
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
                let left = if x > 0 { height_at(x - 1, y, &mut source) } else { center };
                let right = if x + 1 < map.width { height_at(x + 1, y, &mut source) } else { center };
                let north = if y > 0 { height_at(x, y - 1, &mut source) } else { center };
                let south = if y + 1 < map.height { height_at(x, y + 1, &mut source) } else { center };
                let dx = (right - left) / (if x == 0 || x + 1 == map.width { map.spacing } else { 2. * map.spacing });
                let dz = (north - south) / (if y == 0 || y + 1 == map.height { map.spacing } else { 2. * map.spacing });
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
        let map = self.map.as_ref().ok_or("No terrain is loaded for sculpting")?;
        if !self.project.source_map.is_empty()
            && !self.current_map_id.is_empty()
            && !self.project.source_map.eq_ignore_ascii_case(&self.current_map_id)
        {
            return Err(format!(
                "Project belongs to map {}, but the loaded terrain is {}.",
                self.project.source_map, self.current_map_id
            ));
        }
        map.heights.clear_edits()?;
        for stroke in &self.project.strokes {
            self.apply_stroke(stroke)?;
        }
        Ok(())
    }

    fn save_named(&mut self) -> bool {
        let name = self.project_name.trim();
        if name.is_empty() {
            self.status = "Enter a name before saving.".into();
            return false;
        }
        self.project.name = name.to_string();
        if self.project.source_map.is_empty() && self.selected_map != "__empty__" {
            self.project.source_map = self.selected_map.clone();
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
            Ok(project) => {
                self.project_name = project.name.clone();
                self.selected_map = if project.source_map.is_empty() {
                    "__empty__".into()
                } else {
                    project.source_map.clone()
                };
                self.project = project;
                self.redo_strokes.clear();
                let replay_error = self.rebuild_terrain().err();
                self.selected_placement = None;
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
                self.project = Project::default();
                self.redo_strokes.clear();
                if let Some(map) = &self.map {
                    let belongs_here = erased_source_map.is_empty()
                        || self.current_map_id.is_empty()
                        || erased_source_map.eq_ignore_ascii_case(&self.current_map_id);
                    if belongs_here {
                        if let Err(error) = map.heights.clear_edits() {
                            self.status = format!("Project erased, but terrain reset failed: {error}");
                        }
                    }
                }
                self.project_name = "Untitled World".into();
                self.selected_project = None;
                self.selected_placement = None;
                self.selected_scene_object = None;
                self.multi_selection.clear();
                self.multi_scene_items.clear();
                self.confirm_erase = false;
                self.refresh_projects();
                self.status = "Custom project erased. Client map files were not touched.".into();
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

    fn show_tree(&mut self, ui: &mut egui::Ui, path: &Path, depth: usize, camera: Vec3) {
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
                .take(500)
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
                    .show(ui, |ui| self.show_tree(ui, &child, depth + 1, camera));
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
                if response.clicked() {
                    if child
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("pck"))
                    {
                        self.open_archive(child.clone());
                    } else {
                        self.selected_asset = Some(child.to_string_lossy().into_owned());
                        if self.cursor == CursorMode::Place {
                            self.add_selected_asset_at_camera(camera);
                        }
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
        let path = if let Some((archive, entry)) = asset.split_once("::") {
            let archive_name = Path::new(archive)
                .file_name()
                .map(|name| name.to_string_lossy());
            if !archive_name.is_some_and(|name| name.eq_ignore_ascii_case("models.pck")) {
                return None;
            }
            entry
        } else {
            asset
        };
        let normalized = crate::ecmodel::normalize(path);
        // PCK entry names are untrusted archive data. Normalize canonicalizes
        // separators and case, but intentionally does not resolve dot segments;
        // reject traversal before handing a path to the archive-backed reader.
        let safe_segments = normalized
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
        (safe_segments
            && normalized.starts_with("models/")
            && normalized.ends_with(".ecm")
            && !normalized.contains(':'))
        .then_some(normalized)
    }

    /// Current project's supported ECM placements, for the native GPU model
    /// queue. Other archive types remain browsable/pinnable but are not guessed
    /// into a renderer format they do not implement.
    pub fn renderer_placements(&self) -> Vec<crate::world_models_gpu::EditorPlacement> {
        if !self.enabled {
            return Vec::new();
        }
        self.project
            .objects
            .iter()
            .filter_map(|object| {
                Some(crate::world_models_gpu::EditorPlacement {
                    path: Self::renderer_asset_path(&object.asset)?,
                    position: object.position,
                    rotation: object.rotation,
                    scale: object.scale,
                })
            })
            .collect()
    }

    fn add_selected_asset_at_camera(&mut self, camera: Vec3) {
        let Some(asset) = self.selected_asset.clone() else {
            return;
        };
        if Self::renderer_asset_path(&asset).is_none() {
            self.status = "Only ECM model entries from models.pck can currently be instantiated in the live viewport. Other client assets can still be pinned.".into();
            return;
        }
        let portable_asset = self.portable_asset_path(&asset);
        let position = self.placement_position(camera);
        self.project.objects.push(PlacedObject {
            asset: portable_asset,
            position,
            rotation: [0.; 3],
            scale: 1.,
        });
        self.selected_placement = Some(self.project.objects.len() - 1);
        self.status = "ECM placement added to the live Architect viewport.".into();
    }

    fn duplicate_selected_scene_item(&mut self) {
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
        for (_, asset, position) in items {
            if Self::renderer_asset_path(&asset).is_none() {
                self.status = "One or more selected items are not ECM models from models.pck; those items cannot be duplicated into the live model layer.".into();
                continue;
            }
            let portable_asset = self.portable_asset_path(&asset);
            self.project.objects.push(PlacedObject {
                asset: portable_asset,
                position,
                rotation: [0.; 3],
                scale: 1.,
            });
        }
        self.selected_placement = Some(self.project.objects.len() - 1);
        self.status = format!(
            "Copied {} selected client scene item(s) into the editable project layer.",
            self.multi_selection.len().max(1)
        );
    }

    fn scene_row(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        label: &str,
        asset: &str,
        position: [f32; 3],
        camera: Vec3,
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
            self.selected_asset = Some(asset.to_string());
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
            }
            if self.cursor == CursorMode::Place {
                self.add_selected_asset_at_camera(camera);
            }
        }
    }

    fn open_archive(&mut self, path: PathBuf) {
        self.selected_archive = Some(path.clone());
        self.archive_entries.clear();
        self.archive_error = None;
        if let Some(entries) = self.archive_cache.get(&path) {
            self.archive_entries = entries.clone();
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
                self.archive_entries = entries.into_iter().map(|entry| entry.name).collect();
                self.archive_cache
                    .insert(path.clone(), self.archive_entries.clone());
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
                    ui.selectable_value(&mut self.cursor, CursorMode::Place, "Place");
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
                            self.enabled = false;
                        }
                    });
                });
            });
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
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }
        if self.selected_map.is_empty() {
            self.selected_map = current_map.id.clone();
        }
        self.toolbar(ctx);
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
                    self.project = Project::default();
                    self.project_name = "Untitled World".into();
                    self.selected_map = "__empty__".into();
                    self.selected_project = None;
                    self.selected_placement = None;
                    self.selected_scene_object = None;
                    self.selected_scene_transform = None;
                    self.multi_selection.clear();
                    self.multi_scene_items.clear();
                    self.status = "New empty project. Choose a client map as its source when ready.".into();
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
                            self.project.objects.clear();
                            self.project.strokes.clear();
                            self.selected_placement = None;
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
        asset_panel.show(ctx, |ui| {
                ui.horizontal(|ui| { ui.heading("Client explorer"); Self::dock_picker(ui, &mut self.right_dock); });
                ui.label(egui::RichText::new("Loose client files are browsed lazily. Select a PCK archive to index its internal asset paths on demand.").small().weak());
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.tree_filter);
                    if ui.small_button("Refresh files").clicked() {
                        self.directory_cache.clear();
                        self.archive_cache.clear();
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
                        ui.label(format!("{} indexed entries", self.archive_entries.len()));
                        let archive_entries = self.archive_entries.clone();
                        egui::ScrollArea::vertical().max_height(180.).show(ui, |ui| {
                            for entry in archive_entries.iter().take(2000) {
                                if !self.tree_filter.is_empty()
                                    && !entry.to_lowercase().contains(&self.tree_filter.to_lowercase())
                                {
                                    continue;
                                }
                                let label = Path::new(entry).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| entry.clone());
                                let response = ui.selectable_label(false, format!("📦 {label}"));
                                if response.clicked() {
                                    self.selected_asset = Some(format!("{}::{}", archive.display(), entry));
                                    if self.cursor == CursorMode::Place { self.add_selected_asset_at_camera(camera); }
                                }
                                response.context_menu(|ui| {
                                    if ui.button("Pin archive entry to quick bar").clicked() {
                                        self.pin(format!("{}::{}", archive.display(), entry));
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
                    if ui.button("Record placement at camera").clicked() {
                        self.add_selected_asset_at_camera(camera);
                    }
                }
            });

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
                let mut delete_placement = None;
                let placement_rows = self
                    .project
                    .objects
                    .iter()
                    .enumerate()
                    .map(|(index, object)| {
                        let label = format!(
                            "{} · ({:.1}, {:.1}, {:.1})",
                            Path::new(&object.asset)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| object.asset.clone()),
                            object.position[0],
                            object.position[1],
                            object.position[2]
                        );
                        (index, label)
                    })
                    .collect::<Vec<_>>();
                egui::ScrollArea::vertical()
                    .max_height(100.)
                    .show(ui, |ui| {
                        for (index, label) in &placement_rows {
                            if ui
                                .selectable_label(self.selected_placement == Some(*index), label)
                                .clicked()
                            {
                                self.selected_placement = Some(*index);
                            }
                        }
                    });
                if let Some(index) = self.selected_placement {
                    if let Some(object) = self.project.objects.get_mut(index) {
                        ui.label(egui::RichText::new(&object.asset).small().weak());
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
                                .text("Scale"),
                        );
                        if ui.button("Delete custom placement").clicked() {
                            delete_placement = Some(index);
                        }
                    }
                }
                if let Some(index) = delete_placement {
                    self.project.objects.remove(index);
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
                        position: self.placement_position(camera),
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
                    if ui.button("Undo last placement").clicked() {
                        self.project.objects.pop();
                        self.selected_placement = None;
                    }
                });
                ui.label(format!("{} stored brush operations · {} stored placements", self.project.strokes.len(), self.project.objects.len()));
                ui.label(egui::RichText::new("Brush edits update the shared terrain, navigation and rendered mesh. Paint remains unavailable.").small().weak());
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
                            self.selected_asset = Some(item);
                        }
                    }
                });
            });
        map_request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_codec_round_trips_unicode_paths_and_editor_operations() {
        let project = Project {
            name: "我的世界".into(),
            source_map: "a01".into(),
            objects: vec![PlacedObject {
                asset: "models/场景/城堡.bmd".into(),
                position: [1., 2., 3.],
                rotation: [0.1, 0.2, 0.3],
                scale: 1.25,
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
            project_name: project.name.clone(),
            selected_project: None,
            selected_map: project.source_map.clone(),
            cursor: CursorMode::Select,
            snap: SnapMode::Smart,
            brush: BrushMode::Raise,
            brush_radius: 24.,
            brush_strength: 2.,
            flatten_height: 0.,
            selected_asset: None,
            selected_archive: None,
            archive_entries: Vec::new(),
            archive_error: None,
            selected_scene_object: None,
            selected_scene_transform: None,
            selected_placement: None,
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
        };
        editor.write_project(&path).unwrap();
        let loaded = Architect::read_project(&path).unwrap();
        assert_eq!(loaded.name, project.name);
        assert_eq!(loaded.source_map, project.source_map);
        assert_eq!(loaded.objects.len(), 1);
        assert_eq!(loaded.objects[0].asset, project.objects[0].asset);
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
    fn renderer_asset_paths_only_accept_models_pck_ecm_entries() {
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
    }

    #[test]
    fn project_names_cannot_escape_the_worlds_directory() {
        assert_eq!(Architect::safe_slug("../../Bad Name"), "bad-name");
        assert_eq!(Architect::safe_slug(""), "untitled-world");
    }
}
