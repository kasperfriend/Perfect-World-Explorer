//! Architect: an isolated, non-destructive world-authoring workspace.
//!
//! The first editor layer stores custom placements and terrain-brush operations
//! in portable .pwa project files. It deliberately does not rewrite native
//! ECWLD/T2BK client data: a project remains reversible until a validated native
//! exporter exists.
use std::{
    collections::HashSet,
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

#[derive(Clone, Debug, Default)]
struct Project {
    name: String,
    source_map: String,
    objects: Vec<PlacedObject>,
    strokes: Vec<TerrainStroke>,
}

pub struct Architect {
    enabled: bool,
    element: PathBuf,
    project_root: PathBuf,
    project: Project,
    project_files: Vec<PathBuf>,
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
    selected_scene_object: Option<usize>,
    multi_selection: HashSet<usize>,
    quick_bar: Vec<String>,
    left_dock: Dock,
    right_dock: Dock,
    bottom_dock: Dock,
    confirm_erase: bool,
    status: String,
    tree_filter: String,
    object_filter: String,
    show_scene_objects: bool,
}

impl Architect {
    pub fn new(element: PathBuf) -> Self {
        let base = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let project_root = base.join("Architect").join("Worlds");
        let mut editor = Self {
            enabled: false,
            element,
            project_root,
            project: Project::default(),
            project_files: Vec::new(),
            project_name: "Untitled World".into(),
            selected_project: None,
            selected_map: "world".into(),
            cursor: CursorMode::Select,
            snap: SnapMode::Smart,
            brush: BrushMode::Raise,
            brush_radius: 24.,
            brush_strength: 2.,
            flatten_height: 0.,
            selected_asset: None,
            selected_scene_object: None,
            multi_selection: HashSet::new(),
            quick_bar: Vec::new(),
            left_dock: Dock::Left,
            right_dock: Dock::Right,
            bottom_dock: Dock::Bottom,
            confirm_erase: false,
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
        let Ok(entries) = fs::read_dir(&self.project_root) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pwa")) {
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
            if slug.len() >= 48 { break; }
        }
        let slug = slug.trim_matches('-').to_string();
        if slug.is_empty() { "untitled-world".into() } else { slug }
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
        if value.len() % 2 != 0 { return Err("invalid project string".into()); }
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
        out.push_str(&format!("source_map\t{}\n", Self::encode(&self.project.source_map)));
        for object in &self.project.objects {
            out.push_str(&format!(
                "object\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\n",
                Self::encode(&object.asset),
                object.position[0], object.position[1], object.position[2],
                object.rotation[0], object.rotation[1], object.rotation[2], object.scale
            ));
        }
        for stroke in &self.project.strokes {
            out.push_str(&format!(
                "stroke\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\n",
                match stroke.mode { BrushMode::Raise => "raise", BrushMode::Lower => "lower", BrushMode::Flatten => "flatten", BrushMode::Smooth => "smooth", BrushMode::Paint => "paint" },
                stroke.position[0], stroke.position[1], stroke.position[2],
                stroke.radius, stroke.strength
            ));
        }
        let parent = path.parent().ok_or("project path has no parent")?;
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        let temp = path.with_extension("pwa.tmp");
        fs::write(&temp, out).map_err(|e| format!("{}: {e}", temp.display()))?;
        fs::rename(&temp, path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(())
    }

    fn read_project(path: &Path) -> Result<Project, String> {
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
                    let mut n = [0f32; 7];
                    for (slot, value) in n.iter_mut().zip(&fields[2..]) {
                        *slot = value.parse().map_err(|_| format!("invalid object number on line {}", line_no + 2))?;
                        if !slot.is_finite() { return Err("non-finite object transform".into()); }
                    }
                    project.objects.push(PlacedObject {
                        asset: Self::decode(fields[1])?,
                        position: [n[0], n[1], n[2]],
                        rotation: [n[3], n[4], n[5]],
                        scale: n[6].clamp(0.01, 1000.),
                    });
                }
                "stroke" if fields.len() == 7 => {
                    let mode = match fields[1] {
                        "raise" => BrushMode::Raise, "lower" => BrushMode::Lower,
                        "flatten" => BrushMode::Flatten, "smooth" => BrushMode::Smooth,
                        "paint" => BrushMode::Paint, _ => return Err("unknown terrain brush".into()),
                    };
                    let mut n = [0f32; 5];
                    for (slot, value) in n.iter_mut().zip(&fields[2..]) {
                        *slot = value.parse().map_err(|_| format!("invalid brush number on line {}", line_no + 2))?;
                        if !slot.is_finite() { return Err("non-finite terrain brush value".into()); }
                    }
                    project.strokes.push(TerrainStroke {
                        mode, position: [n[0], n[1], n[2]],
                        radius: n[3].clamp(0.1, 10000.), strength: n[4].clamp(-10000., 10000.),
                    });
                }
                "" => {}
                _ => return Err(format!("invalid Architect project record on line {}", line_no + 2)),
            }
        }
        Ok(project)
    }

    fn save_named(&mut self) {
        let name = self.project_name.trim();
        if name.is_empty() {
            self.status = "Enter a name before saving.".into();
            return;
        }
        self.project.name = name.to_string();
        if self.project.source_map.is_empty() { self.project.source_map = self.selected_map.clone(); }
        let path = self.project_root.join(format!("{}.pwa", Self::safe_slug(name)));
        match self.write_project(&path) {
            Ok(()) => {
                self.status = format!("Saved project: {}", path.display());
                self.refresh_projects();
                self.selected_project = self.project_files.iter().position(|p| p == &path);
            }
            Err(error) => self.status = format!("Save failed: {error}"),
        }
    }

    fn load_selected(&mut self) {
        let Some(path) = self.selected_project.and_then(|index| self.project_files.get(index)).cloned() else { return };
        match Self::read_project(&path) {
            Ok(project) => {
                self.project_name = project.name.clone();
                self.selected_map = if project.source_map.is_empty() { "world".into() } else { project.source_map.clone() };
                self.project = project;
                self.status = format!("Loaded {} ({} placements, {} brush operations).", self.project.name, self.project.objects.len(), self.project.strokes.len());
            }
            Err(error) => self.status = format!("Could not load project: {error}"),
        }
    }

    fn erase_selected(&mut self) {
        let Some(path) = self.selected_project.and_then(|index| self.project_files.get(index)).cloned() else { return };
        match fs::remove_file(&path) {
            Ok(()) => {
                self.project = Project::default();
                self.project_name = "Untitled World".into();
                self.selected_project = None;
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
            .selected_text(match dock { Dock::Left => "Dock: Left", Dock::Right => "Dock: Right", Dock::Bottom => "Dock: Bottom", Dock::Floating => "Floating" })
            .show_ui(ui, |ui| {
                ui.selectable_value(dock, Dock::Left, "Left");
                ui.selectable_value(dock, Dock::Right, "Right");
                ui.selectable_value(dock, Dock::Bottom, "Bottom");
                ui.selectable_value(dock, Dock::Floating, "Floating");
            });
    }

    fn show_tree(&mut self, ui: &mut egui::Ui, path: &Path, depth: usize) {
        if depth > 12 { return; }
        let Ok(entries) = fs::read_dir(path) else { return };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
        for entry in entries.into_iter().take(500) {
            let child = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            let label = entry.file_name().to_string_lossy().into_owned();
            if !self.tree_filter.is_empty() && !label.to_lowercase().contains(&self.tree_filter.to_lowercase()) && !kind.is_dir() {
                continue;
            }
            if kind.is_dir() {
                let response = egui::CollapsingHeader::new(format!("📁 {label}"))
                    .id_source(child.to_string_lossy().to_string())
                    .show(ui, |ui| self.show_tree(ui, &child, depth + 1));
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
                if response.clicked() { self.selected_asset = Some(child.to_string_lossy().into_owned()); }
                response.context_menu(|ui| {
                    if ui.button("Pin to quick bar").clicked() {
                        self.pin(child.to_string_lossy().into_owned());
                        ui.close_menu();
                    }
                });
            }
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
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(27, 31, 39)).inner_margin(egui::Margin::symmetric(8., 5.)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("ARCHITECT").strong().color(egui::Color32::from_rgb(120, 190, 255)));
                    ui.separator();
                    ui.selectable_value(&mut self.cursor, CursorMode::Select, "Select");
                    ui.selectable_value(&mut self.cursor, CursorMode::MultiSelect, "Multi-select");
                    ui.selectable_value(&mut self.cursor, CursorMode::Place, "Place");
                    ui.separator();
                    egui::ComboBox::from_id_source("architect_snap")
                        .selected_text(format!("Snap: {}", match self.snap { SnapMode::Off => "Off", SnapMode::Terrain => "Terrain", SnapMode::Objects => "Objects", SnapMode::Smart => "Smart" }))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.snap, SnapMode::Off, "Off");
                            ui.selectable_value(&mut self.snap, SnapMode::Terrain, "Terrain");
                            ui.selectable_value(&mut self.snap, SnapMode::Objects, "Objects");
                            ui.selectable_value(&mut self.snap, SnapMode::Smart, "Smart");
                        });
                    ui.separator();
                    ui.label("Terrain");
                    egui::ComboBox::from_id_source("architect_brush")
                        .selected_text(match self.brush { BrushMode::Raise => "Raise", BrushMode::Lower => "Lower", BrushMode::Flatten => "Flatten", BrushMode::Smooth => "Smooth", BrushMode::Paint => "Paint" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.brush, BrushMode::Raise, "Raise");
                            ui.selectable_value(&mut self.brush, BrushMode::Lower, "Lower");
                            ui.selectable_value(&mut self.brush, BrushMode::Flatten, "Flatten");
                            ui.selectable_value(&mut self.brush, BrushMode::Smooth, "Smooth");
                            ui.selectable_value(&mut self.brush, BrushMode::Paint, "Paint");
                        });
                    ui.add(egui::Slider::new(&mut self.brush_radius, 1.0..=250.0).text("Radius"));
                    ui.add(egui::Slider::new(&mut self.brush_strength, 0.1..=20.0).text("Strength"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new(egui::RichText::new("Turn off Architect").strong().color(egui::Color32::WHITE)).fill(egui::Color32::from_rgb(165, 45, 45))).clicked() {
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
        if !self.enabled { return None; }
        self.selected_map = current_map.id.clone();
        self.toolbar(ctx);
        let screen = ctx.input(|input| input.screen_rect());
        let mut map_request = None;

        egui::Window::new("World")
            .id(egui::Id::new("architect_world_panel"))
            .default_pos(Self::dock_position(self.left_dock, screen, 290., 560.))
            .default_size(egui::vec2(290., 560.))
            .min_size(egui::vec2(240., 300.))
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| { ui.heading("World selector"); Self::dock_picker(ui, &mut self.left_dock); });
                ui.label(egui::RichText::new(format!("Loaded: {} ({})", current_map.display_name, current_map.id)).small().weak());
                egui::ComboBox::from_id_source("architect_client_maps")
                    .selected_text(format!("{} ({})", catalog.maps.iter().find(|m| m.id == self.selected_map).map_or(current_map.display_name.as_str(), |m| m.display_name.as_str()), self.selected_map))
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for map in &catalog.maps {
                            ui.selectable_value(&mut self.selected_map, map.id.clone(), map.label())
                                .on_hover_text(map.tooltip());
                        }
                    });
                if ui.button("Load selected client map").clicked() && !self.selected_map.eq_ignore_ascii_case(&current_map.id) {
                    map_request = Some(self.selected_map.clone());
                }
                ui.separator();
                ui.heading("Custom worlds");
                self.refresh_projects();
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
                    if ui.button("Erase…").clicked() { self.confirm_erase = true; }
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
                if ui.button("Save custom world").clicked() { self.save_named(); }
                ui.label(egui::RichText::new("Projects are stored separately from the client and never overwrite its files.").small().weak());
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(240, 190, 100), "Native game-map export is not available yet.");
                ui.label(egui::RichText::new("A real client map requires validated ECWLD, EC-BSD and terrain writers. Architect keeps edits in a portable project until that exporter is implemented.").small().weak());
                ui.label(egui::RichText::new(&self.status).small().color(egui::Color32::LIGHT_BLUE));
            });

        egui::Window::new("Client assets")
            .id(egui::Id::new("architect_asset_panel"))
            .default_pos(Self::dock_position(self.right_dock, screen, 340., 600.))
            .default_size(egui::vec2(340., 600.))
            .min_size(egui::vec2(250., 300.))
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| { ui.heading("Client explorer"); Self::dock_picker(ui, &mut self.right_dock); });
                ui.label(egui::RichText::new("Loose files and folders under the client element directory. PCK archives are listed as files; their internal entries are not expanded here.").small().weak());
                ui.text_edit_singleline(&mut self.tree_filter);
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    self.show_tree(ui, &self.element.clone(), 0);
                });
                if let Some(asset) = &self.selected_asset {
                    ui.separator();
                    ui.label(egui::RichText::new(asset).small().strong());
                    if ui.button("Add to quick bar").clicked() { self.pin(asset.clone()); }
                    if ui.button("Record placement at camera").clicked() {
                        self.project.objects.push(PlacedObject {
                            asset: asset.clone(),
                            position: camera.to_array(),
                            rotation: [0., 0., 0.],
                            scale: 1.,
                        });
                        self.status = "Placement recorded in the project. Native scene rendering/export is not wired yet.".into();
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
                    ui.label(format!("{} blocks · {} trees · {} placed models · {} scenery assets · {} effects",
                        scene.blocks.len(),
                        scene.blocks.iter().map(|b| b.trees.len()).sum::<usize>(),
                        scene.blocks.iter().map(|b| b.models.len()).sum::<usize>(),
                        ornaments.len(),
                        scene.blocks.iter().map(|b| b.effects.len()).sum::<usize>()));
                    ui.text_edit_singleline(&mut self.object_filter);
                    egui::ScrollArea::vertical().max_height(270.).show(ui, |ui| {
                        let mut index = 0usize;
                        for block in &scene.blocks {
                            for model in &block.models {
                                let label = format!("Model {} · {}", model.export_id, model.path);
                                if self.object_filter.is_empty() || label.to_lowercase().contains(&self.object_filter.to_lowercase()) {
                                    let selected = self.selected_scene_object == Some(index) || self.multi_selection.contains(&index);
                                    if ui.selectable_label(selected, label).clicked() {
                                        if self.cursor == CursorMode::MultiSelect {
                                            if !self.multi_selection.insert(index) { self.multi_selection.remove(&index); }
                                        } else {
                                            self.selected_scene_object = Some(index);
                                            self.multi_selection.clear();
                                        }
                                    }
                                }
                                index += 1;
                            }
                        }
                        for ornament in ornaments {
                            let label = format!("Scenery {} · {}", ornament.export_id, ornament.path);
                            if self.object_filter.is_empty() || label.to_lowercase().contains(&self.object_filter.to_lowercase()) {
                                if ui.selectable_label(false, label).clicked() {
                                    self.selected_asset = Some(ornament.path.clone());
                                }
                            }
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
                    ui.label(format!("Scene object index {index} selected. Existing client placements are read-only."));
                } else {
                    ui.label("Select an object in the scene list or choose an asset in the client explorer.");
                }
                ui.label(format!("Pending custom placements: {}", self.project.objects.len()));
                ui.label(format!("Camera position: {:.1}, {:.1}, {:.1}", camera.x, camera.y, camera.z));
                if ui.button("Place selected asset at camera").clicked() {
                    if let Some(asset) = self.selected_asset.clone() {
                        self.project.objects.push(PlacedObject { asset, position: camera.to_array(), rotation: [0.; 3], scale: 1. });
                        self.status = "Custom placement recorded.".into();
                    }
                }
            });

        egui::Window::new("Terrain tools")
            .id(egui::Id::new("architect_terrain_panel"))
            .default_pos(Self::dock_position(self.bottom_dock, screen, 560., 230.))
            .default_size(egui::vec2(560., 230.))
            .min_size(egui::vec2(360., 160.))
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| { ui.heading("Terrain sculpt"); Self::dock_picker(ui, &mut self.bottom_dock); });
                ui.add(egui::Slider::new(&mut self.flatten_height, -500.0..=500.0).text("Flatten target"));
                if ui.button("Record brush operation at camera").clicked() {
                    self.project.strokes.push(TerrainStroke {
                        mode: self.brush,
                        position: camera.to_array(),
                        radius: self.brush_radius,
                        strength: if self.brush == BrushMode::Flatten { self.flatten_height } else { self.brush_strength },
                    });
                    self.status = "Brush operation recorded in the project. Terrain mesh mutation is not wired yet.".into();
                }
                ui.label(format!("{} stored brush operations · {} stored placements", self.project.strokes.len(), self.project.objects.len()));
                ui.label(egui::RichText::new("Brush modes and snapping are project metadata at this stage; they do not alter the live terrain mesh.").small().weak());
            });

        egui::TopBottomPanel::bottom("architect_quick_bar")
            .exact_height(42.)
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(27, 31, 39)).inner_margin(egui::Margin::symmetric(8., 4.)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("QUICK BAR").strong());
                    for item in self.quick_bar.clone() {
                        let label = Path::new(&item).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| item.clone());
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
            element: PathBuf::new(),
            project_root: path.parent().unwrap().to_path_buf(),
            project: project.clone(),
            project_files: Vec::new(),
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
            selected_scene_object: None,
            multi_selection: HashSet::new(),
            quick_bar: Vec::new(),
            left_dock: Dock::Left,
            right_dock: Dock::Right,
            bottom_dock: Dock::Bottom,
            confirm_erase: false,
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
        let _ = fs::remove_file(path);
    }

    #[test]
    fn project_names_cannot_escape_the_worlds_directory() {
        assert_eq!(Architect::safe_slug("../../Bad Name"), "bad-name");
        assert_eq!(Architect::safe_slug(""), "untitled-world");
    }
}
