//! Architect — Independent Blender-style World Editor module.
//!
//! When enabled, Architect takes over the application window, removing the core
//! viewer UI and world render and replacing them with a full 3D world-building
//! workspace. Clicking the red "Turn off Architect" button in the upper bar
//! cleanly restores the original viewer UI and renderer.

pub mod dock;
pub mod explorer;
pub mod export;
pub mod viewport;
pub mod world;

use crate::world_catalog;
use dock::{DockLayout, DockSide, EditorTab};
use explorer::{AssetFilter, AssetNode, ClientExplorer};
use glam::Vec3;
use std::{collections::HashSet, path::PathBuf};
use viewport::{
    pick_gizmo_axis, selection_pivot, ArchitectViewport, EditorCamera, GizmoAxis, GizmoMode,
    ShadingMode, ViewportFrame,
};
use winit::keyboard::KeyCode;
use world::{
    delete_custom_world, list_custom_worlds, load_custom_world, save_custom_world, snap_position,
    ObjectKind, PaintLayer, SavedCustomWorld, SnappingMode, TerrainBrush, TerrainBrushMode,
    WorldDocument,
};

/// Active tool / cursor mode in the upper toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorMode {
    Select,
    BoxSelect,
    Place,
    Move,
    Rotate,
    Scale,
    Terrain,
}

impl CursorMode {
    pub const ALL: [Self; 7] = [
        Self::Select,
        Self::BoxSelect,
        Self::Place,
        Self::Move,
        Self::Rotate,
        Self::Scale,
        Self::Terrain,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select [Q]",
            Self::BoxSelect => "Box [B]",
            Self::Place => "Place [P]",
            Self::Move => "Move [G]",
            Self::Rotate => "Rotate [R]",
            Self::Scale => "Scale [S]",
            Self::Terrain => "Terrain [T]",
        }
    }

    pub fn gizmo_mode(self) -> GizmoMode {
        match self {
            Self::Move => GizmoMode::Move,
            Self::Rotate => GizmoMode::Rotate,
            Self::Scale => GizmoMode::Scale,
            _ => GizmoMode::None,
        }
    }
}

/// Optional host action returned by Architect to `gpu.rs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArchitectAction {
    /// Refresh the host's world catalog (e.g., after exporting a new game map).
    RefreshCatalog,
    /// Turn off Architect and travel into the exported game map ID in the viewer.
    TravelToMap(String),
}

/// Complete independent state of the Architect module.
pub struct Architect {
    active: bool,
    element: PathBuf,
    status: String,
    notice: Option<(String, bool)>, // (message, is_error)
    world: WorldDocument,
    camera: EditorCamera,
    viewport: ArchitectViewport,
    explorer: ClientExplorer,
    dock: DockLayout,
    client_maps: Vec<world_catalog::WorldMap>,
    custom_worlds: Vec<SavedCustomWorld>,
    world_search: String,
    outliner_search: String,
    save_name: String,
    last_exported_map_id: Option<String>,
    confirm_erase_open: bool,

    // Editing state
    cursor_mode: CursorMode,
    snapping: SnappingMode,
    grid_step: f32,
    brush: TerrainBrush,
    shading: ShadingMode,
    show_grid: bool,
    sun_angle: f32,
    armed_asset: Option<String>,
    selected: HashSet<u64>,
    primary_selected: Option<u64>,

    // Interactive viewport drag state
    hovered_ground: Option<Vec3>,
    hovered_gizmo: Option<GizmoAxis>,
    active_gizmo_axis: Option<GizmoAxis>,
    gizmo_undo_pushed: bool,
    sculpt_undo_pushed: bool,
    box_select_start: Option<egui::Pos2>,
    box_select_end: Option<egui::Pos2>,
    fly_active: bool,
    pending_action: Option<ArchitectAction>,
    saved_visuals: Option<egui::Visuals>,
    quick_folder_popup: Option<String>,
}

impl Default for Architect {
    fn default() -> Self {
        Self::new(PathBuf::new())
    }
}

impl Architect {
    pub fn new(element: PathBuf) -> Self {
        let client_maps = if element.as_os_str().is_empty() {
            Vec::new()
        } else {
            world_catalog::discover(&element).maps
        };
        let custom_worlds = list_custom_worlds(&element);
        let world = WorldDocument::new_empty();
        let save_name = world.name.clone();
        Self {
            active: false,
            element: element.clone(),
            status: "Disabled".into(),
            notice: None,
            world,
            camera: EditorCamera::default(),
            viewport: ArchitectViewport::new(element.clone()),
            explorer: ClientExplorer::new(element),
            dock: DockLayout::default(),
            client_maps,
            custom_worlds,
            world_search: String::new(),
            outliner_search: String::new(),
            save_name,
            last_exported_map_id: None,
            confirm_erase_open: false,
            cursor_mode: CursorMode::Select,
            snapping: SnappingMode::Smart,
            grid_step: 1.0,
            brush: TerrainBrush::default(),
            shading: ShadingMode::Solid,
            show_grid: true,
            sun_angle: 0.75,
            armed_asset: Some("builtin://cube".into()),
            selected: HashSet::new(),
            primary_selected: None,
            hovered_ground: None,
            hovered_gizmo: None,
            active_gizmo_axis: None,
            gizmo_undo_pushed: false,
            sculpt_undo_pushed: false,
            box_select_start: None,
            box_select_end: None,
            fly_active: false,
            pending_action: None,
            saved_visuals: None,
            quick_folder_popup: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn set_active(&mut self, enabled: bool) {
        if self.active == enabled {
            return;
        }
        self.active = enabled;
        if enabled {
            self.status = format!("Active — editing '{}'", self.world.name);
            self.refresh_world_lists();
        } else {
            self.status = "Disabled".into();
            self.confirm_erase_open = false;
            self.active_gizmo_axis = None;
            self.box_select_start = None;
            self.box_select_end = None;
            self.fly_active = false;
        }
    }

    pub fn take_action(&mut self) -> Option<ArchitectAction> {
        self.pending_action.take()
    }

    pub fn refresh_world_lists(&mut self) {
        if !self.element.as_os_str().is_empty() {
            self.client_maps = world_catalog::discover(&self.element).maps;
        }
        self.custom_worlds = list_custom_worlds(&self.element);
    }

    /// Update fly-camera movement and background explorer scan.
    pub fn update(&mut self, dt: f32, keys: &HashSet<KeyCode>) {
        if !self.active {
            return;
        }
        self.explorer.poll_scan();

        // Free-fly movement while holding right mouse button over viewport.
        if self.fly_active {
            let speed = if keys.contains(&KeyCode::ShiftLeft) || keys.contains(&KeyCode::ShiftRight)
            {
                self.camera.fly_speed * 2.5
            } else {
                self.camera.fly_speed
            };
            let forward = self.camera.forward();
            let right = self.camera.right();
            let mut delta = Vec3::ZERO;
            if keys.contains(&KeyCode::KeyW) {
                delta += forward;
            }
            if keys.contains(&KeyCode::KeyS) {
                delta -= forward;
            }
            if keys.contains(&KeyCode::KeyD) {
                delta += right;
            }
            if keys.contains(&KeyCode::KeyA) {
                delta -= right;
            }
            if keys.contains(&KeyCode::KeyE) || keys.contains(&KeyCode::Space) {
                delta += Vec3::Y;
            }
            if keys.contains(&KeyCode::KeyQ) || keys.contains(&KeyCode::ControlLeft) {
                delta -= Vec3::Y;
            }
            if delta.length_squared() > 0.001 {
                self.camera.pivot += delta.normalize() * speed * dt;
            }
        }
    }

    /// Render the offscreen 3D Blender-style viewport texture before egui draws.
    pub fn prepare_viewport(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        egui_renderer: &mut egui_wgpu::Renderer,
    ) {
        if !self.active {
            return;
        }
        let brush_preview = (self.cursor_mode == CursorMode::Terrain)
            .then(|| self.hovered_ground.map(|p| (p, self.brush.radius)))
            .flatten();
        let placement_preview = (self.cursor_mode == CursorMode::Place)
            .then(|| {
                let path = self.armed_asset.as_deref()?;
                let raw = self.hovered_ground?;
                let snapped = snap_position(
                    raw,
                    self.snapping,
                    self.grid_step,
                    &self.world.terrain,
                    &self.world.objects,
                    &HashSet::new(),
                );
                Some((snapped, ObjectKind::from_path(path)))
            })
            .flatten();

        let frame = ViewportFrame {
            camera: &self.camera,
            terrain: &self.world.terrain,
            objects: &self.world.objects,
            selected: &self.selected,
            primary_selected: self.primary_selected,
            shading: self.shading,
            show_grid: self.show_grid,
            gizmo_mode: self.cursor_mode.gizmo_mode(),
            gizmo_hover: self.active_gizmo_axis.or(self.hovered_gizmo),
            brush_preview,
            placement_preview,
            sun_angle: self.sun_angle,
        };
        self.viewport.render(device, queue, egui_renderer, &frame);
    }

    /// Render the complete Blender-style Architect UI (replacing all old viewer UI).
    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.active {
            if let Some(prev) = self.saved_visuals.take() {
                ctx.set_visuals(prev);
            }
            return;
        }

        if self.saved_visuals.is_none() {
            self.saved_visuals = Some(ctx.style().visuals.clone());
            ctx.set_visuals(blender_visuals());
        }

        self.handle_shortcuts(ctx);
        self.top_toolbar_ui(ctx);
        self.docked_panels_ui(ctx);
        self.central_viewport_ui(ctx);
        self.floating_windows_ui(ctx);
        self.confirm_erase_modal_ui(ctx);

        if !self.active {
            if let Some(prev) = self.saved_visuals.take() {
                ctx.set_visuals(prev);
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() || self.confirm_erase_open || self.fly_active {
            return;
        }
        ctx.input(|i| {
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            let shift = i.modifiers.shift;
            if ctrl && i.key_pressed(egui::Key::Z) {
                if shift {
                    self.world.redo();
                } else {
                    self.world.undo();
                }
                return;
            }
            if ctrl && i.key_pressed(egui::Key::Y) {
                self.world.redo();
                return;
            }
            if ctrl && i.key_pressed(egui::Key::D) {
                let new_ids = self.world.duplicate_objects(&self.selected);
                if !new_ids.is_empty() {
                    self.selected = new_ids.iter().copied().collect();
                    self.primary_selected = new_ids.last().copied();
                }
                return;
            }
            if i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace) {
                if self.world.delete_objects(&self.selected) > 0 {
                    self.selected.clear();
                    self.primary_selected = None;
                }
                return;
            }
            if i.key_pressed(egui::Key::Escape) {
                self.box_select_start = None;
                self.box_select_end = None;
                self.active_gizmo_axis = None;
                if self.cursor_mode == CursorMode::Place {
                    self.cursor_mode = CursorMode::Select;
                } else {
                    self.selected.clear();
                    self.primary_selected = None;
                }
                return;
            }
            if i.key_pressed(egui::Key::Q) {
                self.cursor_mode = CursorMode::Select;
            } else if i.key_pressed(egui::Key::B) {
                self.cursor_mode = CursorMode::BoxSelect;
            } else if i.key_pressed(egui::Key::P) {
                self.cursor_mode = CursorMode::Place;
            } else if i.key_pressed(egui::Key::G) {
                self.cursor_mode = CursorMode::Move;
            } else if i.key_pressed(egui::Key::R) {
                self.cursor_mode = CursorMode::Rotate;
            } else if i.key_pressed(egui::Key::S) {
                self.cursor_mode = CursorMode::Scale;
            } else if i.key_pressed(egui::Key::T) {
                self.cursor_mode = CursorMode::Terrain;
            } else if i.key_pressed(egui::Key::F) {
                self.focus_selection();
            } else if i.key_pressed(egui::Key::Home) {
                self.camera = EditorCamera::default();
            } else if i.key_pressed(egui::Key::OpenBracket) {
                self.brush.radius = (self.brush.radius - 2.0).max(1.0);
            } else if i.key_pressed(egui::Key::CloseBracket) {
                self.brush.radius = (self.brush.radius + 2.0).min(80.0);
            }
        });
    }

    fn focus_selection(&mut self) {
        if let Some(pivot) = selection_pivot(&self.world.objects, &self.selected) {
            self.camera.focus(pivot, 8.0);
        } else {
            self.camera.focus(Vec3::ZERO, 24.0);
        }
    }

    // -----------------------------------------------------------------------
    // Upper Bar (Tools, Snapping, Terrain Modes, Red "Turn off Architect")
    // -----------------------------------------------------------------------

    fn top_toolbar_ui(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("architect_top_bar")
            .exact_height(if self.cursor_mode == CursorMode::Terrain {
                72.0
            } else {
                40.0
            })
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(30, 32, 36))
                    .inner_margin(egui::Margin::symmetric(10.0, 5.0))
                    .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(52, 56, 64))),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Branding + world name
                    ui.label(
                        egui::RichText::new("ARCHITECT")
                            .strong()
                            .color(egui::Color32::from_rgb(245, 162, 54)),
                    );
                    let dirty_mark = if self.world.dirty { "*" } else { "" };
                    ui.label(
                        egui::RichText::new(format!("{}{}", self.world.name, dirty_mark))
                            .weak()
                            .small(),
                    );
                    ui.separator();

                    // Undo / Redo
                    if ui
                        .add_enabled(self.world.can_undo(), egui::Button::new("Undo"))
                        .on_hover_text("Undo (Ctrl+Z)")
                        .clicked()
                    {
                        self.world.undo();
                    }
                    if ui
                        .add_enabled(self.world.can_redo(), egui::Button::new("Redo"))
                        .on_hover_text("Redo (Ctrl+Y)")
                        .clicked()
                    {
                        self.world.redo();
                    }
                    ui.separator();

                    // Cursor / Selection / Transform / Terrain modes
                    for mode in CursorMode::ALL {
                        let active = self.cursor_mode == mode;
                        let btn = egui::Button::new(mode.label()).fill(if active {
                            egui::Color32::from_rgb(52, 108, 184)
                        } else {
                            egui::Color32::from_rgb(44, 47, 54)
                        });
                        if ui.add(btn).clicked() {
                            self.cursor_mode = mode;
                        }
                    }

                    ui.separator();

                    // Snapping selector
                    ui.label(egui::RichText::new("Snap:").small().weak());
                    egui::ComboBox::from_id_source("architect_snap_mode")
                        .width(92.0)
                        .selected_text(self.snapping.label())
                        .show_ui(ui, |ui| {
                            for s in SnappingMode::ALL {
                                ui.selectable_value(&mut self.snapping, s, s.label());
                            }
                        });
                    if matches!(self.snapping, SnappingMode::Grid | SnappingMode::Smart) {
                        ui.add(
                            egui::DragValue::new(&mut self.grid_step)
                                .speed(0.25)
                                .clamp_range(0.25..=32.0)
                                .suffix("u"),
                        )
                        .on_hover_text("Grid snap step size");
                    }

                    ui.separator();

                    // Shading & Grid
                    egui::ComboBox::from_id_source("architect_shading")
                        .width(86.0)
                        .selected_text(self.shading.label())
                        .show_ui(ui, |ui| {
                            for sh in ShadingMode::ALL {
                                ui.selectable_value(&mut self.shading, sh, sh.label());
                            }
                        });
                    ui.toggle_value(&mut self.show_grid, "Grid");

                    if ui
                        .button("Reset Layout")
                        .on_hover_text("Restore default Left/Right/Bottom tab docking")
                        .clicked()
                    {
                        self.dock.reset();
                    }

                    // Far right: Red "Turn off Architect" button
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let exit_btn = egui::Button::new(
                            egui::RichText::new("Turn off Architect")
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(196, 44, 44));
                        if ui
                            .add_sized([148.0, 26.0], exit_btn)
                            .on_hover_text("Exit Architect editor and return to the original viewer UI and renderer")
                            .clicked()
                        {
                            self.set_active(false);
                        }
                    });
                });

                // Second compact strip for Terrain Sculpt / Paint controls when Terrain mode is active.
                if self.cursor_mode == CursorMode::Terrain {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Terrain Brush:")
                                .small()
                                .strong()
                                .color(egui::Color32::from_rgb(110, 205, 245)),
                        );
                        for bm in TerrainBrushMode::ALL {
                            let active = self.brush.mode == bm;
                            let btn = egui::Button::new(bm.label()).fill(if active {
                                egui::Color32::from_rgb(44, 130, 168)
                            } else {
                                egui::Color32::from_rgb(44, 47, 54)
                            });
                            if ui.add(btn).clicked() {
                                self.brush.mode = bm;
                            }
                        }
                        ui.separator();
                        ui.label(egui::RichText::new("Radius").small());
                        ui.add(
                            egui::Slider::new(&mut self.brush.radius, 1.0..=80.0)
                                .max_decimals(1)
                                .suffix("u"),
                        );
                        ui.label(egui::RichText::new("Strength").small());
                        ui.add(
                            egui::Slider::new(&mut self.brush.strength, 1.0..=60.0)
                                .max_decimals(1),
                        );
                        if matches!(
                            self.brush.mode,
                            TerrainBrushMode::Flatten | TerrainBrushMode::Level
                        ) {
                            ui.label(egui::RichText::new("Target Y").small());
                            ui.add(
                                egui::DragValue::new(&mut self.brush.target_height)
                                    .speed(0.5)
                                    .clamp_range(-200.0..=500.0),
                            );
                        }
                        if self.brush.mode == TerrainBrushMode::Paint {
                            ui.separator();
                            egui::ComboBox::from_id_source("architect_paint_layer")
                                .width(96.0)
                                .selected_text(self.brush.paint_layer.label())
                                .show_ui(ui, |ui| {
                                    for layer in PaintLayer::ALL {
                                        ui.selectable_value(
                                            &mut self.brush.paint_layer,
                                            layer,
                                            layer.label(),
                                        );
                                    }
                                });
                        }
                    });
                }
            });
    }

    // -----------------------------------------------------------------------
    // Movable & Pinnable Dock Regions (Left, Right, Bottom, Floating)
    // -----------------------------------------------------------------------

    fn docked_panels_ui(&mut self, ctx: &egui::Context) {
        let panel_frame = egui::Frame::none()
            .fill(egui::Color32::from_rgb(35, 37, 42))
            .inner_margin(egui::Margin::same(8.0))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(52, 56, 64),
            ));

        // Left dock
        if !self.dock.left_tabs.is_empty() {
            egui::SidePanel::left("architect_dock_left")
                .resizable(true)
                .default_width(self.dock.left_width)
                .width_range(210.0..=480.0)
                .frame(panel_frame)
                .show(ctx, |ui| {
                    self.dock_tab_bar(ui, DockSide::Left);
                    ui.separator();
                    if let Some(tab) = self.dock.active_on(DockSide::Left) {
                        self.render_tab_contents(ui, tab);
                    }
                });
        }

        // Right dock
        if !self.dock.right_tabs.is_empty() {
            egui::SidePanel::right("architect_dock_right")
                .resizable(true)
                .default_width(self.dock.right_width)
                .width_range(230.0..=520.0)
                .frame(panel_frame)
                .show(ctx, |ui| {
                    self.dock_tab_bar(ui, DockSide::Right);
                    ui.separator();
                    if let Some(tab) = self.dock.active_on(DockSide::Right) {
                        self.render_tab_contents(ui, tab);
                    }
                });
        }

        // Bottom dock
        if !self.dock.bottom_tabs.is_empty() {
            egui::TopBottomPanel::bottom("architect_dock_bottom")
                .resizable(true)
                .default_height(self.dock.bottom_height)
                .height_range(76.0..=280.0)
                .frame(panel_frame)
                .show(ctx, |ui| {
                    self.dock_tab_bar(ui, DockSide::Bottom);
                    ui.separator();
                    if let Some(tab) = self.dock.active_on(DockSide::Bottom) {
                        self.render_tab_contents(ui, tab);
                    }
                });
        }
    }

    fn floating_windows_ui(&mut self, ctx: &egui::Context) {
        let floating = self.dock.floating_tabs.clone();
        for tab in floating {
            egui::Window::new(tab.title())
                .id(egui::Id::new(("architect_float", tab as u8)))
                .default_size(egui::vec2(320.0, 420.0))
                .resizable(true)
                .collapsible(true)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Pin to:").small().weak());
                        for side in [DockSide::Left, DockSide::Right, DockSide::Bottom] {
                            if ui.small_button(side.short_label()).clicked() {
                                self.dock.move_tab(tab, side);
                            }
                        }
                    });
                    ui.separator();
                    self.render_tab_contents(ui, tab);
                });
        }
    }

    fn dock_tab_bar(&mut self, ui: &mut egui::Ui, side: DockSide) {
        let tabs: Vec<EditorTab> = self.dock.tabs_on(side).to_vec();
        let active = self.dock.active_on(side);
        let mut move_req: Option<(EditorTab, DockSide)> = None;

        ui.horizontal_wrapped(|ui| {
            for tab in tabs {
                let is_active = active == Some(tab);
                let resp =
                    ui.selectable_label(is_active, egui::RichText::new(tab.title()).strong());
                if resp.clicked() {
                    self.dock.set_active(side, tab);
                }
                resp.context_menu(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Pin '{}' to:", tab.title()))
                            .small()
                            .weak(),
                    );
                    for target in DockSide::ALL {
                        if target != side && ui.button(target.short_label()).clicked() {
                            move_req = Some((tab, target));
                            ui.close_menu();
                        }
                    }
                });
            }
            if let Some(active_tab) = active {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("Pin ▾", |ui| {
                        for target in DockSide::ALL {
                            if target != side
                                && ui
                                    .button(format!("Move to {}", target.short_label()))
                                    .clicked()
                            {
                                move_req = Some((active_tab, target));
                                ui.close_menu();
                            }
                        }
                    });
                });
            }
        });

        if let Some((tab, target)) = move_req {
            self.dock.move_tab(tab, target);
        }
    }

    fn render_tab_contents(&mut self, ui: &mut egui::Ui, tab: EditorTab) {
        match tab {
            EditorTab::WorldSelector => self.world_selector_tab_ui(ui),
            EditorTab::SceneOutliner => self.scene_outliner_tab_ui(ui),
            EditorTab::ClientExplorer => self.client_explorer_tab_ui(ui),
            EditorTab::Inspector => self.inspector_tab_ui(ui),
            EditorTab::QuickBar => self.quick_bar_tab_ui(ui),
        }
    }

    // -----------------------------------------------------------------------
    // Tab 1: World Selector (Default Left)
    // -----------------------------------------------------------------------

    fn world_selector_tab_ui(&mut self, ui: &mut egui::Ui) {
        // Reserve bottom action area (Save Custom, Export Game Map, Erase Everything)
        // so it stays pinned right under the world list!
        let avail_h = ui.available_height();
        let list_h = (avail_h - 195.0).max(120.0);

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.world_search)
                    .hint_text("Filter worlds...")
                    .desired_width(ui.available_width() - 56.0),
            );
            if ui.small_button("Refresh").clicked() {
                self.refresh_world_lists();
            }
        });
        ui.add_space(4.0);

        let filter = self.world_search.trim().to_ascii_lowercase();
        egui::ScrollArea::vertical()
            .id_source("architect_world_list")
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // 1. Default Empty World
                let is_empty_world = self.world.source_map_id.is_none()
                    && self.world.objects.is_empty()
                    && self.world.name == "Untitled World";
                if ui
                    .selectable_label(
                        is_empty_world,
                        egui::RichText::new("✦ Empty World (Default)")
                            .strong()
                            .color(egui::Color32::from_rgb(160, 215, 255)),
                    )
                    .clicked()
                {
                    self.world = WorldDocument::new_empty();
                    self.save_name = self.world.name.clone();
                    self.selected.clear();
                    self.primary_selected = None;
                    self.camera = EditorCamera::default();
                    self.notice = Some(("Loaded blank empty world.".into(), false));
                }

                ui.add_space(6.0);

                // 2. Saved Custom Worlds
                egui::CollapsingHeader::new(format!(
                    "My Custom Worlds ({})",
                    self.custom_worlds.len()
                ))
                .default_open(true)
                .show(ui, |ui| {
                    if self.custom_worlds.is_empty() {
                        ui.label(
                            egui::RichText::new("No saved custom worlds yet.")
                                .small()
                                .weak(),
                        );
                    }
                    let mut load_path: Option<PathBuf> = None;
                    let mut del_path: Option<PathBuf> = None;
                    for cw in &self.custom_worlds {
                        if !filter.is_empty()
                            && !cw.display_name.to_ascii_lowercase().contains(&filter)
                        {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            let label = format!("{} ({} obj)", cw.display_name, cw.object_count);
                            if ui
                                .selectable_label(self.world.name == cw.display_name, label)
                                .clicked()
                            {
                                load_path = Some(cw.path.clone());
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .small_button("×")
                                        .on_hover_text("Delete custom world file")
                                        .clicked()
                                    {
                                        del_path = Some(cw.path.clone());
                                    }
                                },
                            );
                        });
                    }
                    if let Some(path) = load_path {
                        match load_custom_world(&path) {
                            Ok(doc) => {
                                self.save_name = doc.name.clone();
                                self.world = doc;
                                self.selected.clear();
                                self.primary_selected = None;
                                self.notice = Some((
                                    format!("Loaded custom world '{}'.", self.world.name),
                                    false,
                                ));
                            }
                            Err(e) => {
                                self.notice = Some((format!("Load failed: {e}"), true));
                            }
                        }
                    }
                    if let Some(path) = del_path {
                        if let Err(e) = delete_custom_world(&path) {
                            self.notice = Some((format!("Delete failed: {e}"), true));
                        } else {
                            self.refresh_world_lists();
                        }
                    }
                });

                ui.add_space(4.0);

                // 3. Client Maps
                egui::CollapsingHeader::new(format!("Client Maps ({})", self.client_maps.len()))
                    .default_open(true)
                    .show(ui, |ui| {
                        if self.client_maps.is_empty() {
                            ui.label(
                                egui::RichText::new("No client maps discovered in element/maps/.")
                                    .small()
                                    .weak(),
                            );
                        }
                        let mut load_map: Option<world_catalog::WorldMap> = None;
                        for map in &self.client_maps {
                            let label = map.label();
                            if !filter.is_empty() && !label.to_ascii_lowercase().contains(&filter) {
                                continue;
                            }
                            let selected = self
                                .world
                                .source_map_id
                                .as_deref()
                                .is_some_and(|id| id.eq_ignore_ascii_case(&map.id));
                            if ui
                                .selectable_label(selected, label)
                                .on_hover_text(map.tooltip())
                                .clicked()
                            {
                                load_map = Some(map.clone());
                            }
                        }
                        if let Some(map) = load_map {
                            match export::import_game_map(&map) {
                                Ok(doc) => {
                                    self.save_name = format!("{}_edited", map.id);
                                    self.world = doc;
                                    self.selected.clear();
                                    self.primary_selected = None;
                                    self.camera = EditorCamera::default();
                                    self.notice = Some((
                                        format!(
                                            "Loaded client map '{}' ({} objects).",
                                            map.display_name,
                                            self.world.objects.len()
                                        ),
                                        false,
                                    ));
                                }
                                Err(e) => {
                                    self.notice =
                                        Some((format!("Could not import {}: {e}", map.id), true));
                                }
                            }
                        }
                    });
            });

        ui.separator();
        ui.label(egui::RichText::new("World Actions").strong().small());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Name:").small());
            ui.add(
                egui::TextEdit::singleline(&mut self.save_name)
                    .hint_text("my_world")
                    .desired_width(ui.available_width()),
            );
        });
        ui.add_space(4.0);

        let full_w = ui.available_width();
        if ui
            .add_sized([full_w, 26.0], egui::Button::new("Save as Custom World"))
            .on_hover_text("Save this editable world as a custom Architect project (.pwarch)")
            .clicked()
        {
            match save_custom_world(&self.element, &mut self.world, &self.save_name) {
                Ok(path) => {
                    self.refresh_world_lists();
                    self.notice = Some((
                        format!(
                            "Saved custom world to {}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ),
                        false,
                    ));
                }
                Err(e) => {
                    self.notice = Some((format!("Save failed: {e}"), true));
                }
            }
        }
        ui.add_space(4.0);

        // Green button: Save as real playable game world map
        let export_btn = egui::Button::new(
            egui::RichText::new("Save as Game World Map")
                .strong()
                .color(egui::Color32::WHITE),
        )
        .fill(egui::Color32::from_rgb(38, 134, 72));
        if ui
            .add_sized([full_w, 28.0], export_btn)
            .on_hover_text(
                "Export a real Perfect World map (TRN2 + T2BK + T2MK + ECWLD + ECBSD) under element/maps/<name> so the viewer can load and travel inside it",
            )
            .clicked()
        {
            match export::export_as_game_map(&self.element, &self.world, &self.save_name) {
                Ok(exp) => {
                    self.last_exported_map_id = Some(exp.map_id.clone());
                    self.pending_action = Some(ArchitectAction::RefreshCatalog);
                    self.refresh_world_lists();
                    self.notice = Some((
                        format!(
                            "Exported playable map '{}' ({} BMD, {} ECM, {} trees, {} FX, {} SFX).",
                            exp.map_id, exp.ornaments, exp.models, exp.trees, exp.effects, exp.sounds
                        ),
                        false,
                    ));
                }
                Err(e) => {
                    self.notice = Some((format!("Map export failed: {e}"), true));
                }
            }
        }

        if let Some(map_id) = self.last_exported_map_id.clone() {
            ui.add_space(3.0);
            if ui
                .add_sized(
                    [full_w, 24.0],
                    egui::Button::new(format!("Travel in Viewer -> '{map_id}'")),
                )
                .clicked()
            {
                self.pending_action = Some(ArchitectAction::TravelToMap(map_id));
                self.set_active(false);
            }
        }

        ui.add_space(4.0);
        // Red button: Erase Everything (opens confirmation dialog)
        let erase_btn = egui::Button::new(
            egui::RichText::new("Erase Everything")
                .strong()
                .color(egui::Color32::WHITE),
        )
        .fill(egui::Color32::from_rgb(184, 46, 46));
        if ui
            .add_sized([full_w, 26.0], erase_btn)
            .on_hover_text("Flatten terrain and delete all placed objects (with confirmation)")
            .clicked()
        {
            self.confirm_erase_open = true;
        }

        if let Some((msg, is_err)) = &self.notice {
            ui.add_space(4.0);
            let col = if *is_err {
                egui::Color32::LIGHT_RED
            } else {
                egui::Color32::LIGHT_GREEN
            };
            ui.label(egui::RichText::new(msg).small().color(col));
        }
    }

    fn confirm_erase_modal_ui(&mut self, ctx: &egui::Context) {
        if !self.confirm_erase_open {
            return;
        }
        egui::Window::new("Confirm Erase Everything")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("This will flatten all terrain sculpting and remove all placed objects in the current world.");
                ui.label(
                    egui::RichText::new("(You can still undo this action with Ctrl+Z.)")
                        .small()
                        .weak(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let confirm = egui::Button::new(
                        egui::RichText::new("Yes, Erase Everything")
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(196, 44, 44));
                    if ui.add(confirm).clicked() {
                        self.world.erase_everything();
                        self.selected.clear();
                        self.primary_selected = None;
                        self.confirm_erase_open = false;
                        self.notice = Some(("World erased to flat empty canvas.".into(), false));
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_erase_open = false;
                    }
                });
            });
    }

    // -----------------------------------------------------------------------
    // Tab 2: Scene Outliner (Co-docked Left by default)
    // -----------------------------------------------------------------------

    fn scene_outliner_tab_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.outliner_search)
                    .hint_text("Search placed objects...")
                    .desired_width(ui.available_width() - 62.0),
            );
            ui.label(
                egui::RichText::new(format!("{} obj", self.world.objects.len()))
                    .small()
                    .weak(),
            );
        });
        ui.horizontal(|ui| {
            if ui.small_button("Select All").clicked() {
                self.selected = self.world.objects.iter().map(|o| o.id).collect();
                self.primary_selected = self.world.objects.last().map(|o| o.id);
            }
            if ui.small_button("Clear").clicked() {
                self.selected.clear();
                self.primary_selected = None;
            }
            if ui
                .add_enabled(
                    !self.selected.is_empty(),
                    egui::Button::new("Focus").small(),
                )
                .clicked()
            {
                self.focus_selection();
            }
            if ui
                .add_enabled(!self.selected.is_empty(), egui::Button::new("Dup").small())
                .clicked()
            {
                let ids = self.world.duplicate_objects(&self.selected);
                self.selected = ids.iter().copied().collect();
                self.primary_selected = ids.last().copied();
            }
        });
        ui.separator();

        let filter = self.outliner_search.trim().to_ascii_lowercase();
        let shift = ui.input(|i| i.modifiers.shift);
        let mut toggle_vis: Option<u64> = None;
        let mut toggle_lock: Option<u64> = None;
        let mut clicked_id: Option<u64> = None;

        egui::ScrollArea::vertical()
            .id_source("architect_outliner_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.world.objects.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No objects placed yet. Pick an asset in Client Explorer or Quick Bar.",
                        )
                        .small()
                        .weak(),
                    );
                }
                for obj in &self.world.objects {
                    if !filter.is_empty()
                        && !obj.name.to_ascii_lowercase().contains(&filter)
                        && !obj.asset_path.to_ascii_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    let is_sel = self.selected.contains(&obj.id);
                    ui.horizontal(|ui| {
                        if ui
                            .small_button(if obj.visible { "👁" } else { "–" })
                            .on_hover_text("Toggle visibility")
                            .clicked()
                        {
                            toggle_vis = Some(obj.id);
                        }
                        if ui
                            .small_button(if obj.locked { "🔒" } else { "·" })
                            .on_hover_text("Toggle lock")
                            .clicked()
                        {
                            toggle_lock = Some(obj.id);
                        }
                        let text = format!("[{}] {}", obj.kind.badge(), obj.name);
                        if ui.selectable_label(is_sel, text).clicked() {
                            clicked_id = Some(obj.id);
                        }
                    });
                }
            });

        if let Some(id) = toggle_vis {
            if let Some(o) = self.world.objects.iter_mut().find(|o| o.id == id) {
                o.visible = !o.visible;
            }
        }
        if let Some(id) = toggle_lock {
            if let Some(o) = self.world.objects.iter_mut().find(|o| o.id == id) {
                o.locked = !o.locked;
            }
        }
        if let Some(id) = clicked_id {
            if shift {
                if !self.selected.remove(&id) {
                    self.selected.insert(id);
                    self.primary_selected = Some(id);
                }
            } else {
                self.selected.clear();
                self.selected.insert(id);
                self.primary_selected = Some(id);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Tab 3: Client Asset Explorer (Default Right)
    // -----------------------------------------------------------------------

    fn client_explorer_tab_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.explorer.search)
                    .hint_text("Search client assets...")
                    .desired_width(ui.available_width() - 56.0),
            );
            if self.explorer.scanning {
                ui.spinner();
            } else {
                ui.label(
                    egui::RichText::new(format!("{}", self.explorer.total_assets))
                        .small()
                        .weak(),
                );
            }
        });
        ui.horizontal_wrapped(|ui| {
            for f in AssetFilter::ALL {
                ui.selectable_value(&mut self.explorer.filter, f, f.label());
            }
        });
        if let Some(armed) = &self.armed_asset {
            let short = armed.rsplit(['/', '\\']).next().unwrap_or(armed);
            ui.label(
                egui::RichText::new(format!("Armed: {short}"))
                    .small()
                    .color(egui::Color32::from_rgb(125, 220, 150)),
            );
        }
        ui.label(
            egui::RichText::new("Right-click any folder or object to Pin to Quick Bar.")
                .small()
                .weak(),
        );
        ui.separator();

        let search = self.explorer.search.trim().to_ascii_lowercase();
        let filter = self.explorer.filter;
        let roots = self.explorer.roots.clone();
        let mut pin_req: Option<AssetNode> = None;
        let mut unpin_req: Option<String> = None;
        let mut arm_req: Option<String> = None;
        let mut place_now_req: Option<String> = None;

        egui::ScrollArea::vertical()
            .id_source("architect_explorer_tree")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut rendered = 0usize;
                for root in &roots {
                    self.render_asset_node(
                        ui,
                        root,
                        &search,
                        filter,
                        0,
                        &mut rendered,
                        &mut pin_req,
                        &mut unpin_req,
                        &mut arm_req,
                        &mut place_now_req,
                    );
                }
            });

        if let Some(node) = pin_req {
            self.explorer.pin_node(&node);
        }
        if let Some(path) = unpin_req {
            self.explorer.unpin(&path);
        }
        if let Some(path) = arm_req {
            self.armed_asset = Some(path);
            self.cursor_mode = CursorMode::Place;
        }
        if let Some(path) = place_now_req {
            self.spawn_object_at_pivot(path);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_asset_node(
        &self,
        ui: &mut egui::Ui,
        node: &AssetNode,
        search: &str,
        filter: AssetFilter,
        depth: usize,
        rendered: &mut usize,
        pin_req: &mut Option<AssetNode>,
        unpin_req: &mut Option<String>,
        arm_req: &mut Option<String>,
        place_now_req: &mut Option<String>,
    ) {
        if *rendered > 600 {
            return;
        }
        let is_pinned = self.explorer.is_pinned(&node.full_path);

        if node.is_dir {
            // When searching or filtering, skip folders that have no matching descendants.
            if (!search.is_empty() || filter != AssetFilter::All)
                && !node_matches_filter(node, search, filter)
            {
                return;
            }
            let force_open = !search.is_empty()
                || self.explorer.focused_folder.as_deref() == Some(node.full_path.as_str())
                || (depth == 0 && node.full_path == "builtin://");
            let id = ui.make_persistent_id(("arch_dir", &node.full_path));
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                force_open,
            );
            if force_open && !search.is_empty() {
                state.set_open(true);
            }
            let header = state.show_header(ui, |ui| {
                let label = if is_pinned {
                    format!("📁 {} ★", node.name)
                } else {
                    format!("📁 {}", node.name)
                };
                let resp = ui.add(
                    egui::Label::new(egui::RichText::new(label).strong())
                        .sense(egui::Sense::click()),
                );
                resp.context_menu(|ui| {
                    if is_pinned {
                        if ui.button("Unpin from quick bar").clicked() {
                            *unpin_req = Some(node.full_path.clone());
                            ui.close_menu();
                        }
                    } else if ui.button("Pin to quick bar").clicked() {
                        *pin_req = Some(node.clone());
                        ui.close_menu();
                    }
                });
            });
            header.body(|ui| {
                for child in &node.children {
                    self.render_asset_node(
                        ui,
                        child,
                        search,
                        filter,
                        depth + 1,
                        rendered,
                        pin_req,
                        unpin_req,
                        arm_req,
                        place_now_req,
                    );
                }
            });
        } else {
            let kind = node.kind.unwrap_or(ObjectKind::Primitive);
            if !filter.matches(kind) {
                return;
            }
            if !search.is_empty()
                && !node.name.to_ascii_lowercase().contains(search)
                && !node.full_path.to_ascii_lowercase().contains(search)
            {
                return;
            }
            *rendered += 1;
            let is_armed = self.armed_asset.as_deref() == Some(node.full_path.as_str());
            let label = format!("[{}] {}", kind.badge(), node.name);
            let resp = ui.selectable_label(is_armed, label);
            if resp.clicked() {
                *arm_req = Some(node.full_path.clone());
            }
            if resp.double_clicked() {
                *place_now_req = Some(node.full_path.clone());
            }
            resp.context_menu(|ui| {
                if is_pinned {
                    if ui.button("Unpin from quick bar").clicked() {
                        *unpin_req = Some(node.full_path.clone());
                        ui.close_menu();
                    }
                } else if ui.button("Pin to quick bar").clicked() {
                    *pin_req = Some(node.clone());
                    ui.close_menu();
                }
                if ui.button("Place at viewport center").clicked() {
                    *place_now_req = Some(node.full_path.clone());
                    ui.close_menu();
                }
                if ui.button("Arm for click-to-place").clicked() {
                    *arm_req = Some(node.full_path.clone());
                    ui.close_menu();
                }
            });
        }
    }

    fn spawn_object_at_pivot(&mut self, asset_path: String) {
        let raw = Vec3::new(
            self.camera.pivot.x,
            self.world
                .terrain
                .sample_height(self.camera.pivot.x, self.camera.pivot.z),
            self.camera.pivot.z,
        );
        let pos = snap_position(
            raw,
            self.snapping,
            self.grid_step,
            &self.world.terrain,
            &self.world.objects,
            &HashSet::new(),
        );
        let id = self.world.add_object(asset_path.clone(), pos);
        self.selected.clear();
        self.selected.insert(id);
        self.primary_selected = Some(id);
        self.armed_asset = Some(asset_path);
    }

    // -----------------------------------------------------------------------
    // Tab 4: Inspector (Co-docked Right by default)
    // -----------------------------------------------------------------------

    fn inspector_tab_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_source("architect_inspector_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(id) = self.primary_selected {
                    if let Some(idx) = self.world.objects.iter().position(|o| o.id == id) {
                        ui.heading("Object Transform");
                        ui.label(
                            egui::RichText::new(format!(
                                "{}  •  {} selected",
                                self.world.objects[idx].kind.label(),
                                self.selected.len()
                            ))
                            .small()
                            .weak(),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("Name:");
                            ui.text_edit_singleline(&mut self.world.objects[idx].name);
                        });
                        ui.label(
                            egui::RichText::new(&self.world.objects[idx].asset_path)
                                .small()
                                .weak(),
                        );
                        ui.separator();

                        let mut changed = false;
                        egui::Grid::new("architect_transform_grid")
                            .num_columns(4)
                            .spacing([6.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("Position");
                                for axis in 0..3 {
                                    changed |= ui
                                        .add(
                                            egui::DragValue::new(
                                                &mut self.world.objects[idx].position[axis],
                                            )
                                            .speed(0.25),
                                        )
                                        .changed();
                                }
                                ui.end_row();

                                ui.label("Rotation°");
                                for axis in 0..3 {
                                    changed |= ui
                                        .add(
                                            egui::DragValue::new(
                                                &mut self.world.objects[idx].rotation_deg[axis],
                                            )
                                            .speed(1.0),
                                        )
                                        .changed();
                                }
                                ui.end_row();

                                ui.label("Scale");
                                for axis in 0..3 {
                                    changed |= ui
                                        .add(
                                            egui::DragValue::new(
                                                &mut self.world.objects[idx].scale[axis],
                                            )
                                            .speed(0.05)
                                            .clamp_range(0.05..=50.0),
                                        )
                                        .changed();
                                }
                                ui.end_row();
                            });
                        if changed {
                            self.world.dirty = true;
                        }

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("Tint:");
                            if ui
                                .color_edit_button_rgb(&mut self.world.objects[idx].color_tint)
                                .changed()
                            {
                                self.world.dirty = true;
                            }
                        });

                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Snap to Terrain").clicked() {
                                self.world.push_undo();
                                for obj in &mut self.world.objects {
                                    if self.selected.contains(&obj.id) && !obj.locked {
                                        obj.position[1] = self
                                            .world
                                            .terrain
                                            .sample_height(obj.position[0], obj.position[2]);
                                    }
                                }
                            }
                            if ui.button("Reset Rotation").clicked() {
                                self.world.push_undo();
                                self.world.objects[idx].rotation_deg = [0.0; 3];
                            }
                            if ui.button("Duplicate").clicked() {
                                let ids = self.world.duplicate_objects(&self.selected);
                                self.selected = ids.iter().copied().collect();
                                self.primary_selected = ids.last().copied();
                            }
                            if ui
                                .add(
                                    egui::Button::new("Delete")
                                        .fill(egui::Color32::from_rgb(160, 45, 45)),
                                )
                                .clicked()
                            {
                                self.world.delete_objects(&self.selected);
                                self.selected.clear();
                                self.primary_selected = None;
                            }
                        });
                        ui.separator();
                    }
                } else {
                    ui.label(
                        egui::RichText::new(
                            "Select an object in the viewport or Outliner to edit its transform.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.separator();
                }

                // World Environment Settings
                ui.heading("World Environment");
                let mut water_enabled = self.world.terrain.water_level.is_some();
                if ui
                    .checkbox(&mut water_enabled, "Global Water Plane")
                    .changed()
                {
                    self.world.push_undo();
                    self.world.terrain.water_level = water_enabled.then_some(-1.5);
                }
                if let Some(wl) = self.world.terrain.water_level.as_mut() {
                    ui.add(egui::Slider::new(wl, -50.0..=100.0).text("Water Y"));
                }
                ui.add(
                    egui::Slider::new(&mut self.sun_angle, 0.0..=std::f32::consts::TAU)
                        .text("Sun Azimuth"),
                );
                ui.add(
                    egui::Slider::new(&mut self.camera.fly_speed, 10.0..=200.0).text("Fly Speed"),
                );
            });
    }

    // -----------------------------------------------------------------------
    // Tab 5: Quick Bar (Default Bottom)
    // -----------------------------------------------------------------------

    fn quick_bar_tab_ui(&mut self, ui: &mut egui::Ui) {
        let pins = self.explorer.quick_bar.clone();
        let mut unpin_req: Option<String> = None;
        let mut arm_req: Option<String> = None;
        let mut place_req: Option<String> = None;
        let mut open_folder_req: Option<String> = None;

        egui::ScrollArea::horizontal()
            .id_source("architect_quick_bar_scroll")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if pins.is_empty() {
                        ui.label(
                            egui::RichText::new("Right-click any folder or object in Client Explorer -> 'Pin to quick bar'.")
                                .small()
                                .weak(),
                        );
                    }
                    for pin in &pins {
                        let is_armed =
                            !pin.is_dir && self.armed_asset.as_deref() == Some(pin.full_path.as_str());
                        let prefix = if pin.is_dir {
                            "📁"
                        } else {
                            pin.kind.map(ObjectKind::badge).unwrap_or("OBJ")
                        };
                        let label = format!("{prefix}  {}", pin.label);
                        let fill = if is_armed {
                            egui::Color32::from_rgb(52, 118, 86)
                        } else if pin.is_dir {
                            egui::Color32::from_rgb(56, 62, 76)
                        } else {
                            egui::Color32::from_rgb(46, 50, 58)
                        };
                        let resp = ui
                            .add_sized([128.0, 38.0], egui::Button::new(label).fill(fill))
                            .on_hover_text(&pin.full_path);

                        if resp.clicked() {
                            if pin.is_dir {
                                open_folder_req = Some(pin.full_path.clone());
                            } else {
                                arm_req = Some(pin.full_path.clone());
                            }
                        }
                        if resp.double_clicked() && !pin.is_dir {
                            place_req = Some(pin.full_path.clone());
                        }
                        resp.context_menu(|ui| {
                            if !pin.is_dir && ui.button("Place at viewport center").clicked() {
                                place_req = Some(pin.full_path.clone());
                                ui.close_menu();
                            }
                            if ui.button("Unpin from quick bar").clicked() {
                                unpin_req = Some(pin.full_path.clone());
                                ui.close_menu();
                            }
                        });
                    }
                });
            });

        // If a pinned folder was clicked, display a quick horizontal sub-strip of its items.
        if let Some(folder_path) = self.quick_folder_popup.clone() {
            if let Some(folder) = self.explorer.find_folder(&folder_path).cloned() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("📁 {}:", folder.name))
                            .small()
                            .strong(),
                    );
                    egui::ScrollArea::horizontal()
                        .id_source("architect_quick_folder_items")
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for child in folder.children.iter().take(40) {
                                    if !child.is_dir && ui.small_button(&child.name).clicked() {
                                        arm_req = Some(child.full_path.clone());
                                    }
                                }
                            });
                        });
                    if ui.small_button("×").clicked() {
                        self.quick_folder_popup = None;
                    }
                });
            }
        }

        if let Some(path) = open_folder_req {
            self.quick_folder_popup = Some(path.clone());
            self.explorer.focused_folder = Some(path);
        }
        if let Some(path) = unpin_req {
            if self.quick_folder_popup.as_deref() == Some(path.as_str()) {
                self.quick_folder_popup = None;
            }
            self.explorer.unpin(&path);
        }
        if let Some(path) = arm_req {
            self.armed_asset = Some(path);
            self.cursor_mode = CursorMode::Place;
        }
        if let Some(path) = place_req {
            self.spawn_object_at_pivot(path);
        }
    }

    // -----------------------------------------------------------------------
    // Central 3D Blender-Style Viewport & Interaction
    // -----------------------------------------------------------------------

    fn central_viewport_ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(28, 30, 35)))
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                let ppp = ctx.pixels_per_point();
                let pw = (rect.width() * ppp).round().max(64.0) as u32;
                let ph = (rect.height() * ppp).round().max(64.0) as u32;
                self.viewport.set_desired_size(pw, ph);

                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

                // Draw the rendered 3D wgpu viewport texture.
                if let Some(tex_id) = self.viewport.texture_id() {
                    ui.painter().image(
                        tex_id,
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }

                // Handle viewport interaction (orbit, pan, zoom, fly, pick, box select, gizmo, place, sculpt).
                self.handle_viewport_input(ctx, rect, &response);

                // Draw box-select marquee overlay if active.
                if let (Some(a), Some(b)) = (self.box_select_start, self.box_select_end) {
                    let sel_rect = egui::Rect::from_two_pos(a, b);
                    ui.painter().rect(
                        sel_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(64, 148, 255, 42),
                        egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(100, 180, 255)),
                    );
                }

                // Compact Blender-style top-left viewport HUD & camera view presets.
                self.viewport_hud_overlay(ui, rect);
            });
    }

    fn handle_viewport_input(
        &mut self,
        ctx: &egui::Context,
        rect: egui::Rect,
        response: &egui::Response,
    ) {
        let (
            pointer_pos,
            scroll_y,
            modifiers,
            primary_down,
            middle_down,
            secondary_down,
            drag_delta,
            dt,
        ) = ctx.input(|i| {
            (
                i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()),
                i.smooth_scroll_delta.y,
                i.modifiers,
                i.pointer.primary_down(),
                i.pointer.middle_down(),
                i.pointer.secondary_down(),
                i.pointer.delta(),
                i.stable_dt.min(0.1),
            )
        });

        // Update hovered ground point & gizmo hover.
        if let Some(pos) = pointer_pos.filter(|p| rect.contains(*p)) {
            let (origin, dir) = self.camera.screen_ray(pos, rect);
            self.hovered_ground = self.world.terrain.raycast(origin, dir, 2000.0);
            if self.cursor_mode.gizmo_mode() != GizmoMode::None && !self.selected.is_empty() {
                if let Some(pivot) = selection_pivot(&self.world.objects, &self.selected) {
                    self.hovered_gizmo = pick_gizmo_axis(&self.camera, rect, pos, pivot);
                } else {
                    self.hovered_gizmo = None;
                }
            } else {
                self.hovered_gizmo = None;
            }
        } else {
            self.hovered_ground = None;
            self.hovered_gizmo = None;
        }

        // Scroll zoom when pointer is inside viewport.
        if response.hovered() && scroll_y.abs() > 0.01 {
            self.camera.zoom(scroll_y);
        }

        // Blender Orbit (MMB or Alt+LMB) & Pan (Shift+MMB).
        if response.dragged_by(egui::PointerButton::Middle)
            || (modifiers.alt && response.dragged_by(egui::PointerButton::Primary))
        {
            if modifiers.shift {
                self.camera.pan(drag_delta);
            } else {
                self.camera.orbit(drag_delta);
            }
            return;
        }

        // Right-mouse drag: look around + enable WASD fly camera.
        self.fly_active = secondary_down && (response.hovered() || response.dragged());
        if response.dragged_by(egui::PointerButton::Secondary) {
            self.camera.orbit(drag_delta);
            return;
        }
        if middle_down {
            return;
        }

        // Primary mouse interaction based on active CursorMode.
        match self.cursor_mode {
            CursorMode::Select => {
                if response.clicked_by(egui::PointerButton::Primary) {
                    if let Some(pos) = pointer_pos {
                        self.pick_object_at(pos, rect, modifiers.shift);
                    }
                }
            }
            CursorMode::BoxSelect => {
                if response.drag_started_by(egui::PointerButton::Primary) {
                    self.box_select_start = pointer_pos;
                    self.box_select_end = pointer_pos;
                } else if response.dragged_by(egui::PointerButton::Primary) {
                    self.box_select_end = pointer_pos;
                } else if response.drag_stopped_by(egui::PointerButton::Primary) {
                    if let (Some(a), Some(b)) =
                        (self.box_select_start.take(), self.box_select_end.take())
                    {
                        let marquee = egui::Rect::from_two_pos(a, b);
                        if !modifiers.shift {
                            self.selected.clear();
                            self.primary_selected = None;
                        }
                        for obj in &self.world.objects {
                            if !obj.visible {
                                continue;
                            }
                            if let Some(sp) = self.camera.project(Vec3::from(obj.position), rect) {
                                if marquee.contains(sp) {
                                    self.selected.insert(obj.id);
                                    self.primary_selected = Some(obj.id);
                                }
                            }
                        }
                    }
                } else if response.clicked_by(egui::PointerButton::Primary) {
                    if let Some(pos) = pointer_pos {
                        self.pick_object_at(pos, rect, modifiers.shift);
                    }
                }
            }
            CursorMode::Place => {
                if response.clicked_by(egui::PointerButton::Primary) {
                    if let (Some(path), Some(raw)) = (self.armed_asset.clone(), self.hovered_ground)
                    {
                        let pos = snap_position(
                            raw,
                            self.snapping,
                            self.grid_step,
                            &self.world.terrain,
                            &self.world.objects,
                            &HashSet::new(),
                        );
                        let id = self.world.add_object(path, pos);
                        self.selected.clear();
                        self.selected.insert(id);
                        self.primary_selected = Some(id);
                    }
                }
            }
            CursorMode::Move | CursorMode::Rotate | CursorMode::Scale => {
                if response.drag_started_by(egui::PointerButton::Primary) {
                    self.active_gizmo_axis = self.hovered_gizmo;
                    self.gizmo_undo_pushed = false;
                }
                if response.dragged_by(egui::PointerButton::Primary) && !self.selected.is_empty() {
                    if !self.gizmo_undo_pushed {
                        self.world.push_undo();
                        self.gizmo_undo_pushed = true;
                    }
                    let axis = self.active_gizmo_axis;
                    let sensitivity = (self.camera.distance * 0.004).max(0.03);
                    match self.cursor_mode {
                        CursorMode::Move => {
                            let move_delta = match axis {
                                Some(GizmoAxis::X) => Vec3::X * (drag_delta.x * sensitivity),
                                Some(GizmoAxis::Y) => Vec3::Y * (-drag_delta.y * sensitivity),
                                Some(GizmoAxis::Z) => Vec3::Z * (drag_delta.x * sensitivity),
                                None => {
                                    let right = self.camera.right();
                                    let fwd = Vec3::new(
                                        self.camera.forward().x,
                                        0.0,
                                        self.camera.forward().z,
                                    )
                                    .normalize_or_zero();
                                    (right * drag_delta.x - fwd * drag_delta.y) * sensitivity
                                }
                            };
                            let ignore = self.selected.clone();
                            for obj in &mut self.world.objects {
                                if self.selected.contains(&obj.id) && !obj.locked {
                                    let raw = Vec3::from(obj.position) + move_delta;
                                    let snapped = if axis.is_none() {
                                        snap_position(
                                            raw,
                                            self.snapping,
                                            self.grid_step,
                                            &self.world.terrain,
                                            &[],
                                            &ignore,
                                        )
                                    } else {
                                        raw
                                    };
                                    obj.position = snapped.to_array();
                                }
                            }
                        }
                        CursorMode::Rotate => {
                            let angle = drag_delta.x * 0.6;
                            for obj in &mut self.world.objects {
                                if self.selected.contains(&obj.id) && !obj.locked {
                                    match axis {
                                        Some(GizmoAxis::X) => obj.rotation_deg[0] += angle,
                                        Some(GizmoAxis::Z) => obj.rotation_deg[2] += angle,
                                        _ => obj.rotation_deg[1] += angle,
                                    }
                                }
                            }
                        }
                        CursorMode::Scale => {
                            let factor = (1.0 + drag_delta.x * 0.01).clamp(0.5, 2.0);
                            for obj in &mut self.world.objects {
                                if self.selected.contains(&obj.id) && !obj.locked {
                                    match axis {
                                        Some(GizmoAxis::X) => {
                                            obj.scale[0] = (obj.scale[0] * factor).clamp(0.05, 50.0)
                                        }
                                        Some(GizmoAxis::Y) => {
                                            obj.scale[1] = (obj.scale[1] * factor).clamp(0.05, 50.0)
                                        }
                                        Some(GizmoAxis::Z) => {
                                            obj.scale[2] = (obj.scale[2] * factor).clamp(0.05, 50.0)
                                        }
                                        None => {
                                            for c in &mut obj.scale {
                                                *c = (*c * factor).clamp(0.05, 50.0);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                } else if response.drag_stopped_by(egui::PointerButton::Primary) {
                    self.active_gizmo_axis = None;
                    self.gizmo_undo_pushed = false;
                } else if response.clicked_by(egui::PointerButton::Primary) {
                    if let Some(pos) = pointer_pos {
                        self.pick_object_at(pos, rect, modifiers.shift);
                    }
                }
            }
            CursorMode::Terrain => {
                if primary_down && (response.hovered() || response.dragged()) {
                    if let Some(hit) = self.hovered_ground {
                        if !self.sculpt_undo_pushed {
                            self.world.push_undo();
                            self.sculpt_undo_pushed = true;
                            if self.brush.mode == TerrainBrushMode::Flatten {
                                self.brush.target_height = hit.y;
                            }
                        }
                        self.world
                            .terrain
                            .apply_brush(hit.x, hit.z, &self.brush, dt);
                    }
                } else {
                    self.sculpt_undo_pushed = false;
                }
            }
        }
    }

    fn pick_object_at(&mut self, pointer: egui::Pos2, rect: egui::Rect, additive: bool) {
        let (origin, dir) = self.camera.screen_ray(pointer, rect);
        let mut best: Option<(u64, f32)> = None;
        for obj in &self.world.objects {
            if let Some(dist) = obj.ray_intersect(origin, dir) {
                if best.map_or(true, |(_, bd)| dist < bd) {
                    best = Some((obj.id, dist));
                }
            }
        }
        match best {
            Some((id, _)) => {
                if additive {
                    if !self.selected.remove(&id) {
                        self.selected.insert(id);
                        self.primary_selected = Some(id);
                    }
                } else {
                    self.selected.clear();
                    self.selected.insert(id);
                    self.primary_selected = Some(id);
                }
            }
            None if !additive => {
                self.selected.clear();
                self.primary_selected = None;
            }
            None => {}
        }
    }

    fn viewport_hud_overlay(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let hud_rect = egui::Rect::from_min_size(
            rect.min + egui::vec2(10.0, 10.0),
            egui::vec2((rect.width() - 20.0).max(200.0), 30.0),
        );
        ui.allocate_ui_at_rect(hud_rect, |ui| {
            ui.horizontal(|ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 22, 26, 195))
                    .rounding(4.0)
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button("Persp").clicked() {
                                self.camera = EditorCamera::default();
                            }
                            if ui.small_button("Top").clicked() {
                                self.camera.set_top_view();
                            }
                            if ui.small_button("Front").clicked() {
                                self.camera.set_front_view();
                            }
                            if ui.small_button("Right").clicked() {
                                self.camera.set_right_view();
                            }
                            if ui.small_button("Focus [F]").clicked() {
                                self.focus_selection();
                            }
                            ui.separator();
                            let coords = self
                                .hovered_ground
                                .map(|p| format!("X {:.1}  Y {:.1}  Z {:.1}", p.x, p.y, p.z))
                                .unwrap_or_else(|| "—".into());
                            ui.label(
                                egui::RichText::new(format!(
                                    "Objects: {}  |  Selected: {}  |  Cursor: {coords}",
                                    self.world.objects.len(),
                                    self.selected.len()
                                ))
                                .small()
                                .weak(),
                            );
                        });
                    });
            });
        });

        // Subtle controls hint at bottom-left of viewport.
        let hint_pos = egui::pos2(rect.min.x + 12.0, rect.max.y - 22.0);
        ui.painter().text(
            hint_pos,
            egui::Align2::LEFT_BOTTOM,
            "MMB / Alt+LMB: Orbit  •  Shift+MMB: Pan  •  Scroll: Zoom  •  RMB+WASD: Fly",
            egui::FontId::proportional(11.0),
            egui::Color32::from_rgba_unmultiplied(200, 206, 218, 170),
        );
    }
}

fn node_matches_filter(node: &AssetNode, search: &str, filter: AssetFilter) -> bool {
    if !node.is_dir {
        let kind = node.kind.unwrap_or(ObjectKind::Primitive);
        if !filter.matches(kind) {
            return false;
        }
        return search.is_empty()
            || node.name.to_ascii_lowercase().contains(search)
            || node.full_path.to_ascii_lowercase().contains(search);
    }
    node.children
        .iter()
        .any(|c| node_matches_filter(c, search, filter))
}

fn blender_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = egui::Color32::from_rgb(35, 37, 42);
    v.window_fill = egui::Color32::from_rgb(38, 40, 46);
    v.faint_bg_color = egui::Color32::from_rgb(44, 47, 54);
    v.extreme_bg_color = egui::Color32::from_rgb(24, 26, 30);
    v.selection.bg_fill = egui::Color32::from_rgb(56, 114, 192);
    v.widgets.inactive.bg_fill = egui::Color32::from_rgb(48, 52, 60);
    v.widgets.hovered.bg_fill = egui::Color32::from_rgb(62, 67, 78);
    v.widgets.active.bg_fill = egui::Color32::from_rgb(74, 82, 96);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scene::Scene, terrain_block, world_catalog};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pwe-architect-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn terrain_sculpt_raise_lower_flatten_smooth_and_paint() {
        let mut terrain = world::EditableTerrain::new_flat(32, 32, 2.0);
        let mut brush = TerrainBrush {
            mode: TerrainBrushMode::Raise,
            radius: 10.0,
            strength: 20.0,
            falloff: 0.5,
            target_height: 0.0,
            paint_layer: PaintLayer::Rock,
        };
        assert!(terrain.apply_brush(0.0, 0.0, &brush, 0.1));
        let raised = terrain.sample_height(0.0, 0.0);
        assert!(raised > 1.0, "expected raised height > 1.0, got {raised}");

        brush.mode = TerrainBrushMode::Lower;
        assert!(terrain.apply_brush(0.0, 0.0, &brush, 0.05));
        let lowered = terrain.sample_height(0.0, 0.0);
        assert!(
            lowered < raised,
            "expected lowered {lowered} < raised {raised}"
        );

        brush.mode = TerrainBrushMode::Smooth;
        assert!(terrain.apply_brush(0.0, 0.0, &brush, 0.1));

        brush.mode = TerrainBrushMode::Flatten;
        brush.target_height = 5.0;
        for _ in 0..20 {
            terrain.apply_brush(0.0, 0.0, &brush, 0.1);
        }
        assert!((terrain.sample_height(0.0, 0.0) - 5.0).abs() < 1.0);

        brush.mode = TerrainBrushMode::Paint;
        brush.paint_layer = PaintLayer::Snow;
        assert!(terrain.apply_brush(0.0, 0.0, &brush, 0.1));
        let cx = terrain.cells_x / 2;
        let cz = terrain.cells_z / 2;
        assert_eq!(
            terrain.paint_layers[cz * terrain.verts_x() + cx],
            PaintLayer::Snow as u8
        );
    }

    #[test]
    fn snapping_modes_snap_to_grid_terrain_object_and_smart() {
        let mut terrain = world::EditableTerrain::new_flat(32, 32, 2.0);
        let brush = TerrainBrush {
            mode: TerrainBrushMode::Raise,
            radius: 12.0,
            strength: 30.0,
            ..Default::default()
        };
        terrain.apply_brush(0.0, 0.0, &brush, 0.1);
        let ground_y = terrain.sample_height(1.3, -1.7);

        let snapped_grid = snap_position(
            Vec3::new(1.3, 4.7, -1.7),
            SnappingMode::Grid,
            2.0,
            &terrain,
            &[],
            &HashSet::new(),
        );
        assert_eq!(snapped_grid, Vec3::new(2.0, 4.0, -2.0));

        let snapped_terrain = snap_position(
            Vec3::new(1.3, 99.0, -1.7),
            SnappingMode::Terrain,
            1.0,
            &terrain,
            &[],
            &HashSet::new(),
        );
        assert!((snapped_terrain.y - ground_y).abs() < 1e-3);

        let base_obj =
            world::PlacedObject::new(1, "builtin://cube".into(), Vec3::new(0.0, 0.0, 0.0));
        let top_y = base_obj.world_aabb().1.y;
        let snapped_obj = snap_position(
            Vec3::new(0.2, 10.0, 0.2),
            SnappingMode::Object,
            1.0,
            &terrain,
            &[base_obj.clone()],
            &HashSet::new(),
        );
        assert!((snapped_obj.y - top_y).abs() < 1e-3);

        let snapped_smart = snap_position(
            Vec3::new(0.2, 10.0, 0.2),
            SnappingMode::Smart,
            1.0,
            &terrain,
            &[base_obj],
            &HashSet::new(),
        );
        assert_eq!(snapped_smart.x, 0.0);
        assert_eq!(snapped_smart.z, 0.0);
        assert!((snapped_smart.y - top_y).abs() < 1e-3);
    }

    #[test]
    fn custom_world_save_load_and_undo_erase_roundtrip() {
        let dir = temp_dir("custom");
        let mut doc = WorldDocument::new_empty();
        doc.add_object(
            "litmodels/world/house.bmd".into(),
            Vec3::new(10.0, 2.0, -5.0),
        );
        doc.add_object("trees/oak.spt".into(), Vec3::new(-4.0, 0.0, 8.0));
        assert_eq!(doc.objects.len(), 2);

        let saved_path = save_custom_world(&dir, &mut doc, "My Island").unwrap();
        assert!(saved_path.is_file());
        let listed = list_custom_worlds(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].display_name, "My Island");

        let reloaded = load_custom_world(&saved_path).unwrap();
        assert_eq!(reloaded.name, "My Island");
        assert_eq!(reloaded.objects.len(), 2);
        assert_eq!(reloaded.objects[0].asset_path, "litmodels/world/house.bmd");

        // Erase & Undo
        doc.erase_everything();
        assert!(doc.objects.is_empty());
        assert!(doc.undo());
        assert_eq!(doc.objects.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exported_game_world_map_is_valid_trn2_t2bk_ecwld_ecbsd_and_discoverable() {
        let element = temp_dir("export");
        let mut doc = WorldDocument::new_empty();
        doc.terrain.apply_brush(
            0.0,
            0.0,
            &TerrainBrush {
                mode: TerrainBrushMode::Raise,
                radius: 16.0,
                strength: 25.0,
                ..Default::default()
            },
            0.1,
        );
        doc.add_object(
            "litmodels/world/tower.bmd".into(),
            Vec3::new(12.0, 1.0, -8.0),
        );
        doc.add_object(
            "models/npcs/guard/guard.ecm".into(),
            Vec3::new(4.0, 0.0, 6.0),
        );
        doc.add_object("trees/pine.spt".into(), Vec3::new(-10.0, 0.0, 10.0));
        doc.add_object("gfx/fire.gfx".into(), Vec3::new(2.0, 3.0, 2.0));
        doc.add_object("sfx/wind.wav".into(), Vec3::new(0.0, 1.0, 0.0));

        let exported = export::export_as_game_map(&element, &doc, "Emerald Valley").unwrap();
        assert_eq!(exported.map_id, "emerald_valley");
        assert_eq!(exported.ornaments, 1);
        assert_eq!(exported.models, 1);
        assert_eq!(exported.trees, 1);
        assert_eq!(exported.effects, 1);
        assert_eq!(exported.sounds, 1);

        // 1. Discoverable by world_catalog::discover
        let catalog = world_catalog::discover(&element);
        let found = catalog
            .maps
            .iter()
            .find(|m| m.id == "emerald_valley")
            .expect("exported map must be discovered by world_catalog");
        assert_eq!(found.display_name, "Emerald Valley");

        // 2. T2BK layout passes terrain_block::layout
        let t2bk = std::fs::read(exported.dir.join("emerald_valley_0.t2bk")).unwrap();
        let layout = terrain_block::layout(&t2bk, 32).expect("valid T2BK layout");
        assert_eq!(layout.blocks.len(), 16);

        // 3. ECWLD parses with Scene::load and ECBSD ornaments parse with load_ornaments
        let scene = Scene::load(&exported.dir.join("emerald_valley.ecwld")).unwrap();
        assert_eq!(scene.rows, 4);
        assert_eq!(scene.cols, 4);
        assert_eq!(scene.tree_types.len(), 1);
        let ornaments = scene
            .load_ornaments(&exported.dir.join("emerald_valley.ecbsd"))
            .unwrap();
        assert_eq!(ornaments.len(), 1);
        assert_eq!(ornaments[0].path, "litmodels/world/tower.bmd");

        // 4. Re-importing the exported map into Architect works cleanly
        let imported = export::import_game_map(found).unwrap();
        assert_eq!(imported.objects.len(), 5);
        assert!(imported.terrain.sample_height(0.0, 0.0) > 0.5);

        let _ = std::fs::remove_dir_all(&element);
    }

    #[test]
    fn dockable_tabs_can_pin_to_left_right_bottom_and_floating() {
        let mut dock = DockLayout::default();
        assert_eq!(dock.side_of(EditorTab::WorldSelector), DockSide::Left);
        assert_eq!(dock.side_of(EditorTab::ClientExplorer), DockSide::Right);
        assert_eq!(dock.side_of(EditorTab::QuickBar), DockSide::Bottom);

        dock.move_tab(EditorTab::WorldSelector, DockSide::Right);
        assert_eq!(dock.side_of(EditorTab::WorldSelector), DockSide::Right);
        assert_eq!(
            dock.active_on(DockSide::Right),
            Some(EditorTab::WorldSelector)
        );

        dock.move_tab(EditorTab::ClientExplorer, DockSide::Bottom);
        assert_eq!(dock.side_of(EditorTab::ClientExplorer), DockSide::Bottom);

        dock.move_tab(EditorTab::QuickBar, DockSide::Floating);
        assert_eq!(dock.side_of(EditorTab::QuickBar), DockSide::Floating);

        dock.reset();
        assert_eq!(dock.side_of(EditorTab::WorldSelector), DockSide::Left);
        assert_eq!(dock.side_of(EditorTab::ClientExplorer), DockSide::Right);
        assert_eq!(dock.side_of(EditorTab::QuickBar), DockSide::Bottom);
    }

    #[test]
    fn architect_ui_renders_cleanly_in_egui_and_supports_turn_off() {
        let ctx = egui::Context::default();
        let mut architect = Architect::default();
        architect.set_active(true);
        assert!(architect.is_active());
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 900.0));
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                architect.ui(ctx);
            },
        );
        assert!(architect.is_active());
        architect.set_active(false);
        assert!(!architect.is_active());
    }
}
