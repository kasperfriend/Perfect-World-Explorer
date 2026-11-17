//! Optional add-on registry. Modules own their settings, assets, simulation and
//! GPU resources. The host only supplies narrow input/camera, terrain and draw
//! hooks; a disabled module has no effect on core features.

mod designer;
mod librarian;
mod package;
mod populator;
mod walker;

use crate::{model_draw_gpu, navigation::MapSpace, water::WaterArea};
use glam::Vec3;
use package::{Package, Registry};
use populator::Populator;
use std::{collections::HashSet, path::PathBuf};
use walker::Walker;
use winit::keyboard::KeyCode;

// Several modules install client fonts. Preserve earlier registrations, even
// when more than one module installs a font during the same egui frame.
fn extend_fonts(ctx: &egui::Context, extend: impl FnOnce(&mut egui::FontDefinitions)) {
    let id = egui::Id::new("module_shared_font_definitions");
    let mut definitions = ctx
        .data_mut(|data| data.get_temp::<egui::FontDefinitions>(id))
        .unwrap_or_else(|| ctx.fonts(|fonts| fonts.lock().fonts.definitions().clone()));
    extend(&mut definitions);
    ctx.data_mut(|data| data.insert_temp(id, definitions.clone()));
    ctx.set_fonts(definitions);
}

pub struct Modules {
    walker: Walker,
    librarian: librarian::Librarian,
    populator: Populator,
    packages: Registry,
    selected: usize,
    /// The world's effect catalogue, handed over by the host when the GFX
    /// archive attaches. Modules read it to find an effect by group, name or
    /// search; playing one in the world goes through the host's
    /// `World::spawn_catalog_effect`, which this handle deliberately does not
    /// reach yet — access first, wiring later.
    effect_catalog: Option<crate::gfx_catalog::Catalog>,
}

impl Default for Modules {
    fn default() -> Self {
        Self {
            walker: Walker::default(),
            librarian: librarian::Librarian::new(PathBuf::new()),
            populator: Populator::default(),
            packages: Registry::discover(package::default_root()),
            selected: 0,
            effect_catalog: None,
        }
    }
}

impl Modules {
    pub fn new(
        element: PathBuf,
        map_dir: PathBuf,
        map: MapSpace,
        water: Vec<WaterArea>,
        scene: &crate::scene::Scene,
        ornaments: Vec<crate::scene::Ornament>,
    ) -> Self {
        // The add-ons each own their state; the host only clones the two
        // handles they need (client root and the loaded world space).
        let populator = Populator::new(element.clone(), map.clone(), map_dir);
        Self {
            walker: Walker::with_host(element.clone(), map, water, scene, ornaments),
            librarian: librarian::Librarian::new(element),
            populator,
            packages: Registry::discover(package::default_root()),
            selected: 0,
            effect_catalog: None,
        }
    }

    pub fn prepare_librarian_preview(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        format: wgpu::TextureFormat,
        visible: bool,
    ) {
        self.librarian
            .prepare_preview(device, queue, renderer, format, visible);
    }

    /// True when the executable implements this package id and the module is
    /// currently enabled.
    fn module_enabled(&self, id: &str) -> bool {
        match id {
            "walker" => self.walker.is_active(),
            "populator" => self.populator.is_active(),
            "librarian" => self.librarian.active(),
            _ => false,
        }
    }

    pub fn walker_active(&self) -> bool {
        self.walker.is_active()
    }

    pub fn populator_active(&self) -> bool {
        self.populator.is_active()
    }

    pub fn walker_zoom(&mut self, steps: f32) {
        self.walker.zoom(steps);
    }

    pub fn walker_visible_in_water_pass(&self, plane: f32, reflection: bool) -> bool {
        self.walker.visible_in_water_pass(plane, reflection)
    }

    /// Returns true when the user asks to go back to the main Escape menu.
    pub fn menu_ui(&mut self, ctx: &egui::Context, spawn: Vec3, yaw: f32) -> bool {
        let mut back = false;
        egui::Window::new("MODULES  |  ESC to go back")
            .default_pos(egui::pos2(16., 16.))
            .default_size(egui::vec2(780., 650.))
            .min_size(egui::vec2(560., 480.))
            .collapsible(false)
            .resizable([true, true])
            .show(ctx, |ui| {
                // Reserve the header/footer first. The central scroll panes then
                // fill only the resized remainder instead of growing Resize back
                // to their content height after a downward drag.
                egui::TopBottomPanel::top("modules_intro")
                    .frame(egui::Frame::default())
                    .show_inside(ui, |ui| {
                        ui.label(
                            egui::RichText::new(
                                "Optional, independent add-ons. Select a module on the left to read its full description and controls.",
                            )
                            .small()
                            .weak(),
                        );
                        for error in &self.packages.errors {
                            ui.colored_label(egui::Color32::LIGHT_RED, error);
                        }
                        ui.add_space(8.);
                    });
                egui::TopBottomPanel::bottom("modules_back")
                    .frame(egui::Frame::default())
                    .show_inside(ui, |ui| {
                        ui.separator();
                        if ui.button("Back to menu").clicked() {
                            back = true;
                        }
                    });

                let mut selected = self.selected;
                egui::CentralPanel::default()
                    .frame(egui::Frame::default())
                    .show_inside(ui, |ui| {
                        self.module_browser_ui(ui, &mut selected, spawn, yaw);
                    });
                self.selected = selected;
            });
        self.librarian.ui(ctx);
        back
    }

    fn module_browser_ui(
        &mut self,
        ui: &mut egui::Ui,
        selected: &mut usize,
        spawn: Vec3,
        yaw: f32,
    ) {
        egui::SidePanel::left("installed_module_browser")
            .exact_width(190.)
            .resizable(false)
            .frame(egui::Frame::default())
            .show_inside(ui, |ui| {
                ui.heading("Installed modules");
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_source("installed_module_list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (index, installed) in self.packages.packages.iter().enumerate() {
                            let enabled = self.module_enabled(&installed.id);
                            let text = if enabled {
                                egui::RichText::new(format!("● {}", installed.name))
                                    .color(egui::Color32::LIGHT_GREEN)
                                    .strong()
                            } else {
                                egui::RichText::new(installed.name.as_str())
                            };
                            let width = ui.available_width();
                            if ui
                                .add_sized(
                                    [width, 28.],
                                    egui::SelectableLabel::new(*selected == index, text),
                                )
                                .clicked()
                            {
                                *selected = index;
                            }
                        }
                        if self.packages.packages.is_empty() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "No module packages found in {}.",
                                    self.packages.root.display()
                                ))
                                .small()
                                .weak(),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "Keep the shipped Modules folder beside Perfect-World-Explorer.exe.",
                                )
                                .small()
                                .weak(),
                            );
                        }
                    });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::default())
            .show_inside(ui, |ui| {
                let width = ui.available_width();
                egui::ScrollArea::vertical()
                    .id_source("selected_module_description")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Cap the content at the pane's existing width. Long
                        // descriptions must wrap, never size the outer Window.
                        ui.set_max_width(width);
                        if let Some(installed) = self.packages.packages.get(*selected).cloned() {
                            self.selected_module_ui(ui, installed, spawn, yaw);
                        } else {
                            ui.heading("Module details");
                            ui.label("Select a module on the left.");
                        }
                    });
            });
    }

    fn selected_module_ui(&mut self, ui: &mut egui::Ui, installed: Package, spawn: Vec3, yaw: f32) {
        ui.heading(&installed.name);
        ui.label(
            egui::RichText::new(format!("Version {}", installed.version))
                .small()
                .weak(),
        );
        ui.label(&installed.description);
        ui.label(
            egui::RichText::new(format!("Package: {}", installed.directory.display()))
                .small()
                .weak(),
        );
        ui.add_space(8.);
        match installed.id.as_str() {
            "walker" => self.walker_module_ui(ui, spawn, yaw),
            "populator" => self.populator_module_ui(ui, spawn),
            "librarian" => {
                let active = self.librarian.active();
                if ui
                    .button(if active {
                        "Disable Librarian"
                    } else {
                        "Enable Librarian"
                    })
                    .clicked()
                {
                    self.librarian.set_active(!active);
                }
                ui.label(self.librarian.status());
            }
            _ => {
                ui.label(
                    egui::RichText::new(format!(
                        "This executable does not provide an implementation for module id '{}'.",
                        installed.id
                    ))
                    .small()
                    .weak(),
                );
            }
        }
    }

    fn populator_module_ui(&mut self, ui: &mut egui::Ui, spawn: Vec3) {
        let active = self.populator.is_active();
        let label = if active {
            "Disable Populator"
        } else {
            "Enable Populator"
        };
        if ui
            .add_sized([160., 32.], egui::Button::new(label))
            .clicked()
        {
            self.populator.set_active(!active, spawn);
        }
        let status = self.populator.status();
        if active {
            ui.label(egui::RichText::new(status).color(egui::Color32::LIGHT_GREEN));
        } else {
            ui.label(egui::RichText::new(status).weak());
        }
        let settings = self.populator.settings();
        ui.label(
            egui::RichText::new(format!(
                "Spawn distance {:.0} units (the client's own scene window is 420), at most {} actors.",
                settings.spawn_distance, settings.max_visible
            ))
            .small()
            .weak(),
        );
        ui.label(
            egui::RichText::new(
                "Displays the client's placed NPCs with their model, name and idle animation.",
            )
            .small()
            .weak(),
        );
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("Live World");
            ui.label(
                egui::RichText::new("(Experimental)")
                    .color(egui::Color32::RED),
            )
            .on_hover_text(
                "Live World has no server-authored resident schedules or patrol routes in the client data. Visible residents keep the original city male/female rigs; walk/run tracks are borrowed from Guard animation data only when joint and bone names map and the resulting pose changes the resident mesh. Guard meshes are never shown. Rigs without a validated mapping still follow safe routes in bind pose. Names and routes remain synthetic, so the feature stays Experimental.",
            );
        });
        ui.vertical(|ui| {
            let mut enabled = self.populator.live_world_enabled();
            if ui
                .checkbox(&mut enabled, "Enable Live World")
                .changed()
            {
                if enabled {
                    // Live World updates are part of Populator's frame hook;
                    // enabling it must not leave its parent module off.
                    self.populator.set_active(true, spawn);
                }
                self.populator.set_live_world(enabled);
            }
            let mut radius = self.populator.live_world_radius();
            if ui.add(egui::Slider::new(&mut radius,
                crate::modules::populator::LIVE_WORLD_MIN_RADIUS..=crate::modules::populator::LIVE_WORLD_MAX_RADIUS)
                .text("city radius").suffix(" u")).changed() {
                self.populator.set_live_world_radius(radius);
            }
            let mut residents = self.populator.live_world_residents_per_zone();
            if ui.add(egui::Slider::new(&mut residents,
                0..=crate::modules::populator::LIVE_WORLD_MAX_PER_ZONE)
                .text("residents per target")).changed() {
                self.populator.set_live_world_residents_per_zone(residents);
            }
            ui.label(
                egui::RichText::new(
                    "Uses varied original male/female city rigs from models.pck. Walk/run clips are retargeted from Guard animation tracks only when the resident's bone map and sampled mesh motion validate; no Guard mesh is used. Unmatched rigs still follow locally validated outdoor routes in bind pose. Terrain, water and building collisions can reject candidates; names and routes are synthetic because server spawn and patrol-route data are not available in the client.",
                )
                .small()
                .weak(),
            );
            if enabled && !self.populator.is_active() {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Populator is off, so Live World cannot generate residents. Use Enable Populator above.",
                );
            } else if enabled {
                ui.label(
                    egui::RichText::new(self.populator.live_world_zone_status())
                        .small()
                        .weak(),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "{} residents generated from {} original model rigs; {} currently streamed residents have validated walk/run clips and {} have no active route.",
                        self.populator.live_world_generated(),
                        self.populator.live_world_model_count(),
                        self.populator.live_world_gait_count(),
                        self.populator.live_world_stationary_count()
                    ))
                    .small()
                    .color(egui::Color32::YELLOW),
                );
            } else {
                ui.label(
                    egui::RichText::new(
                        "Off. Enable Live World to scan city rigs and attempt safe resident routes.",
                    )
                    .small()
                    .weak(),
                );
            }
        });
    }

    fn walker_module_ui(&mut self, ui: &mut egui::Ui, spawn: Vec3, yaw: f32) {
        let active = self.walker.is_active();
        let label = if active {
            "Disable Walker"
        } else {
            "Enable Walker"
        };
        if ui
            .add_sized([160., 32.], egui::Button::new(label))
            .clicked()
        {
            let facing = Vec3::new(yaw.sin(), 0., -yaw.cos());
            self.walker.set_active(!active, spawn, facing);
        }
        let status = self.walker.status();
        if active {
            ui.label(egui::RichText::new(status).color(egui::Color32::LIGHT_GREEN));
        } else if status == "Disabled" {
            ui.label(egui::RichText::new(status).weak());
        } else {
            ui.colored_label(egui::Color32::LIGHT_RED, status);
        }
        ui.label(
            egui::RichText::new(
                "While enabled: WASD moves, Space jumps (press again on foot for the client's second jump), and the mouse looks around. In water, Arrow Up/Down (or Z) swims vertically; deep-water Space ascends. Open Escape to use the integrated Character Designer and movement settings beneath the minimap. Walker previews use authentic level-one equipment records, not a recovered server loadout.",
            )
            .small()
            .weak(),
        );
    }

    /// Escape-menu-owned panel. egui Window supplies persistent dragging and
    /// resizing; its initial position is directly beneath the 256 px minimap.
    pub fn settings_panel(&mut self, ctx: &egui::Context, screen_width: f32) {
        if self.walker.is_active() {
            egui::Window::new("Walker settings")
                .default_pos(egui::pos2((screen_width - 406.).max(16.), 294.))
                .default_size(egui::vec2(390., 540.))
                .min_size(egui::vec2(300., 280.))
                .collapsible(true)
                .resizable(true)
                .vscroll(true)
                .show(ctx, |ui| self.walker.settings_ui(ui));
        }
        // Both panels are independent windows: enabling one add-on must not
        // hide or move the other's settings.
        if self.populator.is_active() {
            egui::Window::new("Populator settings")
                .default_pos(egui::pos2((screen_width - 826.).max(16.), 294.))
                .default_size(egui::vec2(400., 620.))
                .min_size(egui::vec2(320., 280.))
                .collapsible(true)
                .resizable(true)
                .vscroll(true)
                .show(ctx, |ui| self.populator.settings_ui(ui));
        }
    }

    /// `eye` is the host camera position: the only thing the Populator add-on
    /// needs from the camera, and the only reason this hook is not pure input.
    pub fn update(
        &mut self,
        delta: f32,
        input_enabled: bool,
        keys: &HashSet<KeyCode>,
        yaw: f32,
        pitch: f32,
        eye: Vec3,
    ) -> Option<Vec3> {
        // The Populator stands authored placements on real BMD floors and hides
        // overhead names behind geometry, so it asks the Walker module for the
        // hulls it already owns. The loader only runs while the module is on and
        // either names or floor standing is enabled.
        let floors = self.populator.wants_floors();
        if floors {
            self.walker.prepare_object_collision();
        }
        {
            let source = floors.then(|| self.walker.floors());
            let source = source
                .as_ref()
                .map(|source| source as &dyn populator::FloorSource);
            self.populator.update(delta, eye, source);
        }
        // A module may ask the host for a one-shot camera move (the Populator's
        // "go to the nearest placement"). The Walker wins when it drives the
        // camera; otherwise the request is honoured for that frame.
        let request = self.populator.take_camera_request();
        match self.walker.update(delta, input_enabled, keys, yaw, pitch) {
            Some(position) => Some(position),
            None => request,
        }
    }

    /// Overhead world-space labels the add-ons paint in the host's UI pass. The
    /// host supplies the frame's view-projection and the logical screen rect.
    pub fn overlay_ui(
        &mut self,
        ctx: &egui::Context,
        view_projection: glam::Mat4,
        screen: egui::Rect,
    ) {
        self.populator.overlay_ui(ctx, view_projection, screen);
    }

    /// Whether any add-on needs the UI pass for a world-space overlay, so the
    /// host does not have to run egui for an empty frame.
    pub fn overlay_active(&self) -> bool {
        self.populator.names_active()
    }

    /// The effect catalogue of the attached world, once the host has handed
    /// it over: every `.gfx` member grouped by what it depicts, so a module
    /// can ask for "a waterfall" instead of a path.
    pub fn effect_catalog(&self) -> Option<&crate::gfx_catalog::Catalog> {
        self.effect_catalog.as_ref()
    }

    /// Hand the world's catalogue to the add-ons. Replaces any previous one,
    /// which is what a world reload wants.
    pub fn set_effect_catalog(&mut self, catalog: crate::gfx_catalog::Catalog) {
        self.effect_catalog = Some(catalog);
    }

    pub fn update_walker_audio(&mut self, enabled: bool, volume: f32) {
        self.walker.update_audio(enabled, volume);
    }

    pub fn walker_audio_status(&self) -> &str {
        self.walker.audio_status()
    }

    pub fn walker_audio_diagnostic(&self) -> Option<String> {
        self.walker.audio_diagnostic()
    }

    /// The host calls this only when the Walker add-on itself is visible in the
    /// pass; the two add-ons are independent.
    #[allow(clippy::too_many_arguments)]
    pub fn render_walker(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        frame: model_draw_gpu::Frame,
    ) {
        self.walker
            .render(device, queue, format, encoder, target, depth, frame);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_population(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        frame: model_draw_gpu::Frame,
    ) {
        self.populator
            .render(device, queue, format, encoder, target, depth, frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_ons_can_read_the_world_effect_catalog() {
        let mut modules = Modules::default();
        assert!(modules.effect_catalog().is_none());
        modules.set_effect_catalog(crate::gfx_catalog::Catalog::from_directory(vec![(
            "gfx/场景/瀑布.gfx",
            0,
        )]));
        let catalog = modules.effect_catalog().expect("handed over");
        let entry = catalog.find("gfx/场景/瀑布.gfx").expect("listed member");
        assert_eq!(entry.group, crate::gfx_catalog::Group::Water);
        assert!(!catalog.group(crate::gfx_catalog::Group::Water).is_empty());
        assert!(!catalog.search("瀑布").is_empty());
    }

    #[test]
    fn modules_window_bottom_edge_resizes_without_content_feedback() {
        let context = egui::Context::default();
        let mut modules = Modules::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280., 1000.));
        let mut frame = |events| {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |context| {
                    modules.menu_ui(context, Vec3::ZERO, 0.);
                },
            );
        };
        // Let the default size and panel layout settle before grabbing the edge.
        frame(Vec::new());
        frame(Vec::new());
        let id = egui::Id::new("MODULES  |  ESC to go back");
        let before = context
            .memory(|memory| memory.area_rect(id))
            .expect("Modules window area");
        let start = egui::pos2(before.center().x, before.bottom() - 2.);
        frame(vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ]);
        let end = start + egui::vec2(0., 100.);
        frame(vec![egui::Event::PointerMoved(end)]);
        frame(Vec::new());
        frame(vec![egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        }]);
        frame(Vec::new());
        let after = context
            .memory(|memory| memory.area_rect(id))
            .expect("resized Modules window area");
        assert!(
            after.height() >= before.height() + 70. && after.height() <= before.height() + 130.,
            "bottom drag should persist without content forcing the old height: {} -> {}",
            before.height(),
            after.height()
        );
    }

    #[test]
    fn long_module_description_wraps_without_resizing_the_window() {
        let context = egui::Context::default();
        let mut modules = Modules::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280., 1000.));
        fn frame(context: &egui::Context, modules: &mut Modules, screen: egui::Rect) {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |context| {
                    modules.menu_ui(context, Vec3::ZERO, 0.);
                },
            );
        }
        frame(&context, &mut modules, screen);
        frame(&context, &mut modules, screen);
        let id = egui::Id::new("MODULES  |  ESC to go back");
        let before = context
            .memory(|memory| memory.area_rect(id))
            .expect("Modules window area");
        modules.packages.packages.push(package::Package {
            id: "long-description".into(),
            name: "Long description".into(),
            version: "1".into(),
            description: "wrap this native module description without growing the window "
                .repeat(160),
            directory: PathBuf::from("Modules/long-description"),
        });
        frame(&context, &mut modules, screen);
        frame(&context, &mut modules, screen);
        frame(&context, &mut modules, screen);
        let after = context
            .memory(|memory| memory.area_rect(id))
            .expect("Modules window with long description");
        assert!(
            (after.width() - before.width()).abs() < 1.
                && (after.height() - before.height()).abs() < 1.,
            "description fed back into window size: {:?} -> {:?}",
            before,
            after
        );
    }
}
