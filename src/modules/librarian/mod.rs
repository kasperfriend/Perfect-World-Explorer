//! Optional item browser. Its archives, textures and database are owned by the
//! module and are released when it is disabled.
mod archive;
mod compare;
mod database;
mod descriptions;
mod effect_names;
mod effects;
mod elements;
mod icons;
mod npcs;
mod preview3d;

use crate::modules::designer::catalog::{Class, Gender};
use archive::LibrarianAssets;
use database::{Category, Database};
use icons::IconCache;
use std::path::PathBuf;

// A reversed UV rectangle through Image's default rectangle tessellator is
// treated as untextured. Draw a textured mesh explicitly to support flipping.
fn preview_image(
    ui: &mut egui::Ui,
    texture: egui::TextureId,
    size: f32,
    flipped: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::drag());
    let uv = if flipped {
        egui::Rect::from_min_max(egui::pos2(0., 1.), egui::pos2(1., 0.))
    } else {
        egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(1., 1.))
    };
    let mut mesh = egui::Mesh::with_texture(texture);
    mesh.add_rect_with_uv(rect, uv, egui::Color32::WHITE);
    ui.painter().add(egui::Shape::mesh(mesh));
    response
}

/// Whole numbers print without a fraction; everything else keeps two decimals.
fn format_value(value: f64) -> String {
    if value.fract() == 0. {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

/// Cell width for a comparison column.
///
/// The longest names the shipped data carries are 31 characters for an NPC and
/// 32 for a monster, at the default 12.5 px text size, so about 200 px keeps a
/// whole name on one line. A grid column's width defaults to `interact_size.x`
/// (about 40 px), and a label wrapped into that strip measures narrow, so the
/// column never grew with it -- which is what folded names onto several lines
/// and cropped them. An explicit width per cell fixes the squeeze at its cause.
const COMPARISON_CELL_WIDTH: f32 = 200.;
/// The attribute name column only has to fit the longest field name.
const COMPARISON_LABEL_WIDTH: f32 = 165.;

/// One cell of a comparison grid, sized and unwrapped so it cannot be squeezed
/// into a narrow strip.
fn comparison_cell(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        // A fixed width also makes `wrap_text()` false for this cell, because
        // that only reports true while the cell's maximum width is unbounded.
        ui.set_width(COMPARISON_CELL_WIDTH);
        add(ui);
    });
}

/// The attribute-name cell.
fn comparison_label(ui: &mut egui::Ui, text: &str) {
    ui.vertical(|ui| {
        ui.set_width(COMPARISON_LABEL_WIDTH);
        ui.add(egui::Label::new(text).wrap(false));
    });
}

/// A value cell: never wraps, and elides with a tooltip rather than being cut.
fn comparison_value(ui: &mut egui::Ui, text: impl Into<String>, highlight: bool) {
    let mut text = egui::RichText::new(text.into());
    if highlight {
        text = text.color(egui::Color32::LIGHT_GREEN);
    }
    comparison_cell(ui, |ui| {
        ui.add(egui::Label::new(text).truncate(true));
    });
}

/// Comparison grid shared by every NPC and monster column. Returns the column a
/// user asked to remove and the column a user asked to open.
fn npc_comparison_grid(
    ui: &mut egui::Ui,
    entries: &[&npcs::Entry],
) -> (Option<usize>, Option<usize>) {
    let rows = npcs::compare_rows(entries);
    let mut remove = None;
    let mut jump = None;
    egui::ScrollArea::both()
        .id_source("librarian_npc_compare")
        .show(ui, |ui| {
            egui::Grid::new("librarian_npc_comparison_grid")
                .striped(true)
                .min_col_width(COMPARISON_CELL_WIDTH)
                .spacing(egui::vec2(14., 6.))
                .show(ui, |ui| {
                    comparison_label(ui, "Attribute");
                    for (position, entry) in entries.iter().enumerate() {
                        comparison_cell(ui, |ui| {
                            // 123 shipped NPC templates carry no name.
                            let name = if entry.name.is_empty() {
                                format!("Unnamed {}", entry.kind.name())
                            } else {
                                entry.name.clone()
                            };
                            // A link is a frameless button, so it extends rather
                            // than wrapping the way a plain label does; the
                            // tooltip still carries the whole name.
                            if ui.link(name.clone()).on_hover_text(name).clicked() {
                                jump = Some(position);
                            }
                            ui.label(entry.kind.name());
                            if ui
                                .small_button("Remove")
                                .on_hover_text("Remove from the comparison")
                                .clicked()
                            {
                                remove = Some(position);
                            }
                        });
                    }
                    ui.end_row();
                    for (name, values) in &rows {
                        comparison_label(ui, name);
                        let baseline = values.first().copied().flatten();
                        let best = npcs::best(name, values);
                        for value in values {
                            match value {
                                Some(value) => {
                                    let text = match baseline {
                                        Some(base) if (value - base).abs() > f64::EPSILON => {
                                            format!(
                                                "{} ({:+.2})",
                                                format_value(*value),
                                                value - base
                                            )
                                        }
                                        _ => format_value(*value),
                                    };
                                    comparison_value(ui, text, best == Some(*value));
                                }
                                None => comparison_value(ui, "—", false),
                            }
                        }
                        ui.end_row();
                    }
                });
        });
    (remove, jump)
}

pub struct Librarian {
    root: PathBuf,
    state: Option<State>,
    error: Option<String>,
    pending_free: Vec<egui::TextureId>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Items,
    Comparison,
    Npcs,
    Effects,
}

struct State {
    assets: LibrarianAssets,
    icons: IconCache,
    database: Database,
    category: Option<Category>,
    section: Option<usize>,
    major: Option<(usize, u32)>,
    subtype: Option<(usize, u32)>,
    search: String,
    selected: Option<usize>,
    comparison: Vec<usize>,
    tab: Tab,
    effects: effects::Browser,
    npc_database: npcs::Database,
    npc_kind: Option<npcs::Kind>,
    npc_search: String,
    npc_selected: Option<usize>,
    npc_comparison: Vec<usize>,
    preview: Option<preview3d::Preview>,
    show_3d: bool,
    preview_female: bool,
    preview_class: Class,
    preview_dye: [u8; 3],
    font_checked: bool,
    has_cjk_font: bool,
}
impl Librarian {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            state: None,
            error: None,
            pending_free: Vec::new(),
        }
    }
    pub fn active(&self) -> bool {
        self.state.is_some()
    }
    pub fn status(&self) -> &str {
        if self.active() {
            "Enabled"
        } else {
            self.error.as_deref().unwrap_or("Disabled")
        }
    }
    pub fn set_active(&mut self, active: bool) {
        if !active {
            if let Some(state) = self.state.take() {
                if let Some(target) = state.effects.target {
                    self.pending_free.push(target.texture_id);
                }
                if let Some(preview) = state.preview {
                    self.pending_free.push(preview.target.texture_id);
                }
            }
            self.error = None;
            return;
        }
        if self.active() {
            return;
        }
        let mut assets = LibrarianAssets::open(&self.root);
        let elements = assets.read_elements_data();
        let mut npc_database = npcs::Database::default();
        let mut npc_error = None;
        if let Ok(bytes) = &elements {
            match npcs::Database::parse(bytes) {
                Ok(mut database) => {
                    if let Some(table) = assets.read_config("data/task_npc.data") {
                        database.attach_placements(&table);
                    }
                    npc_database = database;
                }
                Err(error) => npc_error = Some(error),
            }
        }
        if let Some(error) = npc_error {
            self.error = Some(format!("NPC records could not be read: {error}"));
            return;
        }
        match elements.and_then(|bytes| Database::parse_v27(&bytes)) {
            Ok(mut database) => {
                for name in ["configs/item_desc.txt", "configs/item_ext_desc.txt"] {
                    if let Some(bytes) = assets.read_config(name) {
                        database
                            .descriptions
                            .extend(descriptions::keyed_strings(&bytes));
                    }
                }
                if let Some(bytes) = assets.read_config("configs/item_ext_prop.txt") {
                    database.addon_types = descriptions::addon_types(&bytes);
                }
                if let Some(bytes) = assets.read_config("configs/item_color.txt") {
                    database.colors = descriptions::numeric_pairs(&bytes);
                }
                if let Some(bytes) = assets.read_config("configs/skillstr.txt") {
                    database.skill_names = descriptions::keyed_strings(&bytes);
                }
                let icons = IconCache::load(&mut assets);
                self.state = Some(State {
                    assets,
                    icons,
                    database,
                    category: None,
                    section: None,
                    major: None,
                    subtype: None,
                    search: String::new(),
                    selected: None,
                    comparison: Vec::new(),
                    tab: Tab::Items,
                    effects: Default::default(),
                    npc_database,
                    npc_kind: Some(npcs::Kind::Npc),
                    npc_search: String::new(),
                    npc_selected: None,
                    npc_comparison: Vec::new(),
                    preview: None,
                    show_3d: false,
                    preview_female: false,
                    preview_class: Class::Blademaster,
                    preview_dye: [255; 3],
                    font_checked: false,
                    has_cjk_font: false,
                });
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
    pub fn prepare_preview(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        format: wgpu::TextureFormat,
        visible: bool,
    ) {
        for id in self.pending_free.drain(..) {
            renderer.free_texture(&id);
        }
        if !visible {
            return;
        }
        let Some(state) = &mut self.state else { return };
        if state.tab == Tab::Effects {
            state
                .effects
                .prepare(&mut state.assets, device, queue, renderer);
            return;
        }
        if state.tab == Tab::Comparison {
            return;
        }
        if state.tab == Tab::Npcs {
            state.prepare_npc_preview(device, queue, renderer, format);
            return;
        }
        if !state.show_3d {
            return;
        }
        let Some(index) = state.selected else { return };
        let preview = state
            .preview
            .get_or_insert_with(|| preview3d::Preview::new(device, renderer, format));
        // An item preview keeps the model's own first clip.
        preview.idle_action = None;
        let item = &state.database.items[index];
        let gender = if state.preview_female { "f" } else { "m" };
        let hair = item.head_hair().and_then(|(hair, texture)| {
            state.database.hair_skins.get(&hair).map(|skin| {
                let texture = state
                    .database
                    .face_textures
                    .get(&texture)
                    .map(String::as_str)
                    .unwrap_or("");
                format!(
                    "hair:{}:{gender}:{skin}:{texture}",
                    state.preview_class.index()
                )
            })
        });
        let path = hair.unwrap_or_else(|| {
            item.equipment_name()
                .map(|name| format!("worn:{}:{gender}:{name}", state.preview_class.index()))
                .unwrap_or_else(|| item.model.clone())
        });
        let dye = item.equipment_name().map(|_| {
            if item.dyeable() {
                state.preview_dye
            } else {
                [255; 3]
            }
        });
        preview.update(
            device,
            queue,
            format,
            &mut state.assets,
            &path,
            dye,
            item.category != Category::Weapons,
        );
    }
    pub fn ui(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.state else { return };
        if !state.font_checked {
            state.font_checked = true;
            for name in ["fzl2jw.ttf", "fzxh1jw.ttf"] {
                let path = self.root.join("data").join(name);
                if let Ok(bytes) = std::fs::read(path) {
                    if !bytes.is_empty() && bytes.len() <= 16 * 1024 * 1024 {
                        super::extend_fonts(ctx, |fonts| {
                            fonts
                                .font_data
                                .insert("pw-client-cjk".into(), egui::FontData::from_owned(bytes));
                            for family in
                                [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
                            {
                                fonts
                                    .families
                                    .entry(family)
                                    .or_default()
                                    .push("pw-client-cjk".into());
                            }
                        });
                        state.has_cjk_font = true;
                        break;
                    }
                }
            }
        }
        egui::Window::new("Librarian")
            .default_pos(egui::pos2(320., 70.))
            .default_size([1050., 700.])
            .min_size([650., 400.])
            .resizable(true)
            .show(ctx, |ui| state.ui(ui));
    }
}
impl State {
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("All Items")
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(
                        if self.tab == Tab::Items && self.category.is_none() {
                            egui::Color32::from_rgb(50, 113, 165)
                        } else {
                            egui::Color32::from_rgb(40, 65, 90)
                        },
                    ),
                )
                .clicked()
            {
                self.tab = Tab::Items;
                self.category = None;
                self.section = None;
                self.major = None;
                self.subtype = None;
            }
            for category in Category::ALL {
                if ui
                    .selectable_label(
                        self.tab == Tab::Items && self.category == Some(category),
                        category.name(),
                    )
                    .clicked()
                {
                    self.tab = Tab::Items;
                    self.category = Some(category);
                    self.section = None;
                    self.major = None;
                    self.subtype = None;
                }
            }
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Comparison")
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(if self.tab == Tab::Comparison {
                        egui::Color32::from_rgb(158, 104, 35)
                    } else {
                        egui::Color32::from_rgb(95, 68, 33)
                    }),
                )
                .clicked()
            {
                self.tab = Tab::Comparison;
            }
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("NPCs")
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(if self.tab == Tab::Npcs {
                        egui::Color32::from_rgb(108, 76, 155)
                    } else {
                        egui::Color32::from_rgb(66, 47, 96)
                    }),
                )
                .on_hover_text(
                    "NPC and monster templates from elements.data, with their rigs, \
                     services, drop tables and comparison.",
                )
                .clicked()
            {
                self.tab = Tab::Npcs;
            }
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Effects")
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(if self.tab == Tab::Effects {
                        egui::Color32::from_rgb(36, 127, 114)
                    } else {
                        egui::Color32::from_rgb(36, 76, 70)
                    }),
                )
                .clicked()
            {
                self.tab = Tab::Effects;
            }
        });
        ui.separator();
        if self.tab == Tab::Effects {
            self.effects
                .ui(ui, &mut self.assets, &self.database.skill_names);
            return;
        }
        if !self.icons.has_atlas() {
            ui.colored_label(egui::Color32::YELLOW, "Inventory icon atlases could not be loaded from surfaces.pck; loose icons may still be available.");
        }
        if self.tab == Tab::Npcs {
            self.npcs_ui(ui);
            return;
        }
        if self.tab == Tab::Comparison {
            self.comparison_ui(ui);
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.text_edit_singleline(&mut self.search);
            ui.label(format!("{} records", self.database.items.len()));
        });
        if let Some(category) = self.category {
            let mut sections: Vec<usize> = self
                .database
                .items
                .iter()
                .filter(|item| item.category == category)
                .map(|item| item.section)
                .collect();
            sections.sort_unstable();
            sections.dedup();
            if sections.len() > 1 {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .selectable_label(self.section.is_none(), "All types")
                        .clicked()
                    {
                        self.section = None;
                        self.major = None;
                        self.subtype = None;
                    }
                    for section in sections {
                        if ui
                            .selectable_label(
                                self.section == Some(section),
                                database::section_name(section),
                            )
                            .clicked()
                        {
                            self.section = Some(section);
                            self.major = None;
                            self.subtype = None;
                        }
                    }
                });
            }
            let mut majors: Vec<((usize, u32), String)> = self
                .database
                .items
                .iter()
                .filter(|item| {
                    item.category == category
                        && self.section.map_or(true, |section| section == item.section)
                })
                .filter_map(|item| {
                    item.major.map(|id| {
                        let name = database::major_table(item.section)
                            .and_then(|table| self.database.category_names.get(&(table, id)))
                            .filter(|name| !name.is_empty())
                            .cloned()
                            .unwrap_or_else(|| format!("Type {id}"));
                        ((item.section, id), name)
                    })
                })
                .collect();
            majors.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
            majors.dedup_by_key(|entry| entry.0);
            if !majors.is_empty() {
                egui::ComboBox::from_label("Type")
                    .selected_text(
                        self.major
                            .and_then(|id| majors.iter().find(|entry| entry.0 == id))
                            .map(|entry| entry.1.as_str())
                            .unwrap_or("All types"),
                    )
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(&mut self.major, None, "All types")
                            .changed()
                        {
                            self.subtype = None;
                        }
                        for (id, name) in &majors {
                            if ui
                                .selectable_value(&mut self.major, Some(*id), name)
                                .changed()
                            {
                                self.subtype = None;
                            }
                        }
                    });
            }
            let mut subtypes: Vec<((usize, u32), String)> = self
                .database
                .items
                .iter()
                .filter(|item| {
                    item.category == category
                        && self.section.map_or(true, |section| section == item.section)
                        && self.major.map_or(true, |(section, id)| {
                            item.section == section && item.major == Some(id)
                        })
                })
                .filter_map(|item| {
                    item.subtype.map(|id| {
                        let name = database::subtype_table(item.section)
                            .and_then(|table| self.database.category_names.get(&(table, id)))
                            .filter(|name| !name.is_empty())
                            .cloned()
                            .unwrap_or_else(|| format!("Subtype {id}"));
                        ((item.section, id), name)
                    })
                })
                .collect();
            subtypes.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
            subtypes.dedup_by_key(|entry| entry.0);
            if !subtypes.is_empty() {
                egui::ComboBox::from_label("Subtype")
                    .selected_text(
                        self.subtype
                            .and_then(|id| subtypes.iter().find(|entry| entry.0 == id))
                            .map(|entry| entry.1.as_str())
                            .unwrap_or("All subtypes"),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.subtype, None, "All subtypes");
                        for (id, name) in &subtypes {
                            ui.selectable_value(&mut self.subtype, Some(*id), name);
                        }
                    });
            }
        }
        let query = self.search.to_lowercase();
        let indices: Vec<usize> = self
            .database
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                if self
                    .category
                    .is_some_and(|category| category != item.category)
                    || self.section.is_some_and(|section| item.section != section)
                    || self.major.is_some_and(|(section, id)| {
                        item.section != section || item.major != Some(id)
                    })
                    || self.subtype.is_some_and(|(section, id)| {
                        item.section != section || item.subtype != Some(id)
                    })
                    || (!query.is_empty()
                        && !item.search_name.contains(&query)
                        && !item.id.to_string().contains(&query))
                {
                    None
                } else {
                    Some(index)
                }
            })
            .collect();
        ui.label(format!("{} matching items", indices.len()));
        ui.columns(2, |columns| {
            egui::ScrollArea::vertical()
                .id_source("librarian_items")
                .show_rows(&mut columns[0], 23., indices.len(), |ui, range| {
                    for row in range {
                        let index = indices[row];
                        let item = &self.database.items[index];
                        ui.horizontal(|ui| {
                            if let Some(icon) =
                                self.icons.get(ui.ctx(), &mut self.assets, &item.icon)
                            {
                                ui.image((icon, egui::vec2(20., 20.)));
                            } else {
                                ui.allocate_space(egui::vec2(20., 20.));
                            }
                            if ui
                                .selectable_label(
                                    self.selected == Some(index),
                                    format!("{}  [{}]", item.name, item.id),
                                )
                                .clicked()
                            {
                                self.selected = Some(index);
                                if let Some(gender) = item.fashion_gender() {
                                    self.preview_female = gender == 1;
                                }
                            }
                        });
                    }
                });
            egui::ScrollArea::vertical()
                .id_source("librarian_details")
                .show(&mut columns[1], |ui| {
                    if let Some(index) = self.selected {
                        let item = &self.database.items[index];
                        ui.heading(&item.name);
                        ui.label(format!("ID {} · {}", item.id, item.category.name()));
                        if let Some(mask) = item.class_mask() {
                            let classes = Class::ALL
                                .iter()
                                .filter(|class| mask & class.bit() != 0)
                                .map(|class| class.name())
                                .collect::<Vec<_>>()
                                .join(", ");
                            ui.label(if classes.is_empty() {
                                format!("Class mask: {mask:#x}")
                            } else {
                                format!("Classes: {classes}")
                            });
                        }
                        if let Some(gender) = item.fashion_gender() {
                            ui.label(format!(
                                "Fashion gender: {}",
                                match gender {
                                    0 => "Male",
                                    1 => "Female",
                                    _ => "Unknown",
                                }
                            ));
                        }
                        if let Some(color) = self.database.colors.get(&item.id) {
                            ui.label(format!("Client color category: {color}"));
                        }
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.show_3d, false, "2D icon");
                            ui.selectable_value(&mut self.show_3d, true, "3D model");
                        });
                        if self.show_3d {
                            if item.equipment_name().is_some() {
                                egui::ComboBox::from_label("Equipment class variant")
                                    .selected_text(self.preview_class.name())
                                    .show_ui(ui, |ui| {
                                        for class in Class::ALL {
                                            ui.selectable_value(
                                                &mut self.preview_class,
                                                class,
                                                class.name(),
                                            );
                                        }
                                    });
                                let gender = if self.preview_female {
                                    Gender::Female
                                } else {
                                    Gender::Male
                                };
                                if !self.preview_class.allows(gender) {
                                    self.preview_female =
                                        self.preview_class.default_gender() == Gender::Female;
                                }
                                ui.horizontal(|ui| {
                                    ui.label("Equipment gender variant:");
                                    if self.preview_class.allows(Gender::Male) {
                                        ui.selectable_value(
                                            &mut self.preview_female,
                                            false,
                                            "Male",
                                        );
                                    }
                                    if self.preview_class.allows(Gender::Female) {
                                        ui.selectable_value(
                                            &mut self.preview_female,
                                            true,
                                            "Female",
                                        );
                                    }
                                });
                            }
                            if let Some(preview) = &mut self.preview {
                                if let Some(error) = &preview.error {
                                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                                }
                                if let Some(error) = &preview.animation_error {
                                    ui.colored_label(egui::Color32::YELLOW, error);
                                }
                                for warning in &preview.warnings {
                                    ui.colored_label(egui::Color32::YELLOW, warning);
                                }
                                if item.dyeable() {
                                    ui.horizontal(|ui| {
                                        ui.label("Dye preview:");
                                        ui.color_edit_button_srgb(&mut self.preview_dye);
                                        if ui.small_button("Reset dye").clicked() {
                                            self.preview_dye = [255; 3];
                                        }
                                    });
                                    ui.small(
                                        "Approximate color tint; actual dye masks may differ.",
                                    );
                                }
                                let size = ui.available_width().min(300.);
                                let response = preview_image(ui, preview.target.texture_id, size, preview.flip_vertical);
                                let shift = ui.input(|i| i.modifiers.shift);
                                if let Some(mode) = preview3d::drag_mode(&response, shift) {
                                    let mut delta = ui.input(|i| i.pointer.delta());
                                    if preview.flip_vertical {
                                        delta.y = -delta.y;
                                    }
                                    match mode {
                                        preview3d::Drag::Pan => preview.pan_by(delta),
                                        preview3d::Drag::Orbit => {
                                            preview.yaw = preview3d::orbit_yaw(preview.yaw, delta.x);
                                            preview.pitch = preview3d::orbit_pitch(preview.pitch, delta.y);
                                        }
                                    }
                                }
                                let mut magnification = 1. / preview.zoom;
                                if ui
                                    .add(
                                        egui::Slider::new(&mut magnification, 0.2..=1. / 0.3)
                                            .logarithmic(true)
                                            .text("Zoom"),
                                    )
                                    .changed()
                                {
                                    preview.zoom = 1. / magnification;
                                }
                                ui.checkbox(&mut preview.flip_vertical, "Flip vertically");
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut preview.turntable, "Turntable");
                                    if item.category != Category::Weapons {
                                        ui.checkbox(&mut preview.playing, "Animation");
                                    }
                                });
                                let actions = preview.action_names();
                                if item.category != Category::Weapons && !actions.is_empty() {
                                    let labels: Vec<String> = actions
                                        .iter()
                                        .enumerate()
                                        .map(|(i, name)| preview3d::action_label(name, i))
                                        .collect();
                                    egui::ComboBox::from_label("Action")
                                        .selected_text(
                                            labels
                                                .get(preview.action)
                                                .map(String::as_str)
                                                .unwrap_or("Bind pose"),
                                        )
                                        .show_ui(ui, |ui| {
                                            for (index, name) in labels.iter().enumerate() {
                                                if ui
                                                    .selectable_label(preview.action == index, name)
                                                    .on_hover_text(&actions[index])
                                                    .clicked()
                                                {
                                                    preview.select_action(index);
                                                }
                                            }
                                        });
                                }
                                if (item.category != Category::Weapons && preview.playing) || preview.turntable {
                                    ui.ctx().request_repaint();
                                }
                                ui.label("Left-drag to orbit; right-drag to move; use the slider to zoom.");
                            } else {
                                ui.label("Preparing preview…");
                            }
                        } else if let Some(icon) =
                            self.icons.get(ui.ctx(), &mut self.assets, &item.icon)
                        {
                            ui.image((icon, egui::vec2(64., 64.)));
                        }
                        if ui.button("Add to comparison").clicked()
                            && !self.comparison.contains(&index)
                        {
                            self.comparison.push(index);
                        }
                        ui.separator();
                        if let Some(description) = self.database.descriptions.get(&item.id) {
                            ui.label(descriptions::plain_text(description));
                            ui.separator();
                        }
                        for (name, value) in item.attributes() {
                            ui.label(format!("{name}: {value}"));
                        }
                        if let Some(major) = item.major {
                            let name = database::major_table(item.section)
                                .and_then(|table| self.database.category_names.get(&(table, major)))
                                .map(String::as_str)
                                .unwrap_or("Unknown");
                            ui.label(format!("Type: {name} ({major})"));
                        }
                        if let Some(subtype) = item.subtype {
                            let name = database::subtype_table(item.section)
                                .and_then(|table| {
                                    self.database.category_names.get(&(table, subtype))
                                })
                                .map(String::as_str)
                                .unwrap_or("Unknown");
                            ui.label(format!("Subtype: {name} ({subtype})"));
                        }
                        if !item.model.is_empty() {
                            ui.label(format!("Model: {}", item.model));
                        }
                        if let Some((name, kind, level, speed)) = self.database.pet_info(item) {
                            ui.separator();
                            ui.strong("Pet or mount");
                            ui.label(format!("{name} · {kind}"));
                            ui.label(format!("Required level: {level}"));
                            if speed.is_finite() {
                                ui.label(format!("Base speed: {speed:.2}"));
                            }
                        }
                        if let Some(id) = item.contained_item() {
                            if let Some(&target) = self.database.by_id.get(&id) {
                                if ui
                                    .link(format!("Contains: {}", self.database.items[target].name))
                                    .clicked()
                                {
                                    self.selected = Some(target);
                                }
                            }
                        }
                        let fixed_addons = item.fixed_addons();
                        if !fixed_addons.is_empty() {
                            ui.separator();
                            ui.strong("Item bonuses");
                            for (group, id) in fixed_addons {
                                ui.label(format!("{group}: {}", self.database.addon_label(id)));
                            }
                        }
                        let skills = item.pet_skills();
                        if !skills.is_empty() {
                            ui.separator();
                            ui.strong("Pet skills");
                            for (id, level) in skills {
                                let name = self
                                    .database
                                    .skill_names
                                    .get(&id.saturating_mul(10))
                                    .map(|name| descriptions::plain_text(name))
                                    .unwrap_or_else(|| format!("Skill {id}"));
                                ui.label(format!("{name} · level {level}"));
                            }
                        }
                        for (group, addons) in item.addon_groups() {
                            ui.separator();
                            ui.strong(format!("{group} add-on pool"));
                            for (addon_id, weight) in addons {
                                let weight = if weight.is_finite() {
                                    format!(" (weight {weight:.3})")
                                } else {
                                    String::new()
                                };
                                ui.label(format!(
                                    "{}{weight}",
                                    self.database.addon_label(addon_id)
                                ));
                            }
                        }
                        for (suite_id, name, members) in self.database.suites_for(item.id) {
                            ui.separator();
                            ui.strong(format!("Set: {name}"));
                            for member in members {
                                if let Some(&target) = self.database.by_id.get(&member) {
                                    if ui.link(&self.database.items[target].name).clicked() {
                                        self.selected = Some(target);
                                    }
                                } else {
                                    ui.label(format!("Item {member}"));
                                }
                            }
                            let bonuses = self.database.suite_addons(suite_id);
                            if !bonuses.is_empty() {
                                ui.label("Set add-ons:");
                                for bonus in bonuses {
                                    ui.label(bonus);
                                }
                            }
                        }
                    } else {
                        ui.label("Select an item to see its details.");
                    }
                });
        });
    }
    /// Runs the offscreen preview for the selected NPC or monster.
    ///
    /// The record's own `.ecm` is handed over verbatim: no body, equipment or
    /// hair path is substituted, so the rig on screen is the one the client
    /// instantiates for that template.
    fn prepare_npc_preview(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        format: wgpu::TextureFormat,
    ) {
        let Some(index) = self.npc_selected else {
            return;
        };
        let Some(entry) = self.npc_database.entries.get(index) else {
            return;
        };
        let model = entry.model.clone();
        let preview = self
            .preview
            .get_or_insert_with(|| preview3d::Preview::new(device, renderer, format));
        preview.idle_action = Some(npcs::SCENE_IDLE.to_owned());
        preview.update(device, queue, format, &mut self.assets, &model, None, true);
    }
    fn npcs_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (kind, label) in [(npcs::Kind::Npc, "NPCs"), (npcs::Kind::Monster, "Monsters")] {
                if ui
                    .selectable_label(
                        self.npc_kind == Some(kind),
                        format!("{label} ({})", self.npc_database.count(kind)),
                    )
                    .clicked()
                {
                    self.npc_kind = Some(kind);
                    self.npc_selected = None;
                }
            }
            // Comparison lives in the shared Comparison tab, so the badge here
            // says how many columns it currently holds.
            let columns = self.npc_comparison.len();
            if columns != 0 {
                ui.label(format!("{columns} in Comparison"));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.text_edit_singleline(&mut self.npc_search);
            ui.label(format!("{} templates", self.npc_database.entries.len()));
        });
        let query = self.npc_search.to_lowercase();
        let indices: Vec<usize> = self
            .npc_database
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                self.npc_kind.map_or(true, |kind| entry.kind == kind)
                    && (query.is_empty()
                        || entry.search_name.contains(&query)
                        || entry.id.to_string().contains(&query))
            })
            .map(|(index, _)| index)
            .collect();
        ui.label(format!("{} matching templates", indices.len()));
        ui.columns(2, |columns| {
            egui::ScrollArea::vertical()
                .id_source("librarian_npcs")
                .show_rows(&mut columns[0], 23., indices.len(), |ui, range| {
                    for row in range {
                        let index = indices[row];
                        let entry = &self.npc_database.entries[index];
                        // 123 shipped NPC templates carry no name and no rig;
                        // label them so the row is still selectable.
                        let label = if entry.name.is_empty() {
                            format!("Unnamed {}  [{}]", entry.kind.name(), entry.id)
                        } else {
                            format!("{}  [{}]", entry.name, entry.id)
                        };
                        if ui
                            .selectable_label(self.npc_selected == Some(index), label)
                            .clicked()
                        {
                            self.npc_selected = Some(index);
                        }
                    }
                });
            egui::ScrollArea::vertical()
                .id_source("librarian_npc_details")
                .show(&mut columns[1], |ui| self.npc_details_ui(ui));
        });
    }
    fn npc_details_ui(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.npc_selected else {
            ui.label("Select an NPC or monster to see its details.");
            return;
        };
        let Some(entry) = self.npc_database.entries.get(index) else {
            return;
        };
        let kind = entry.kind;
        let id = entry.id;
        let model = entry.model.clone();
        let sections = entry.sections(&self.npc_database);
        let drops = entry.drops();
        let drop_counts = entry.drop_counts();
        let skills = entry.skills();
        let source_monster = entry.source_monster();
        let heading = if entry.name.is_empty() {
            format!("Unnamed {}", kind.name())
        } else {
            entry.name.clone()
        };
        ui.heading(heading);
        ui.label(format!("ID {id} · {}", kind.name()));
        if model.is_empty() {
            ui.colored_label(
                egui::Color32::YELLOW,
                "This template names no rig, so there is no model to show.",
            );
        } else {
            if let Some(preview) = &mut self.preview {
                if let Some(error) = &preview.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                if let Some(error) = &preview.animation_error {
                    ui.colored_label(egui::Color32::YELLOW, error);
                }
                for warning in &preview.warnings {
                    ui.colored_label(egui::Color32::YELLOW, warning);
                }
                let size = ui.available_width().min(300.);
                let response =
                    preview_image(ui, preview.target.texture_id, size, preview.flip_vertical);
                let shift = ui.input(|i| i.modifiers.shift);
                if let Some(mode) = preview3d::drag_mode(&response, shift) {
                    let mut delta = ui.input(|i| i.pointer.delta());
                    if preview.flip_vertical {
                        delta.y = -delta.y;
                    }
                    match mode {
                        preview3d::Drag::Pan => preview.pan_by(delta),
                        preview3d::Drag::Orbit => {
                            preview.yaw = preview3d::orbit_yaw(preview.yaw, delta.x);
                            preview.pitch = preview3d::orbit_pitch(preview.pitch, delta.y);
                        }
                    }
                }
                let mut magnification = 1. / preview.zoom;
                if ui
                    .add(
                        egui::Slider::new(&mut magnification, 0.2..=1. / 0.3)
                            .logarithmic(true)
                            .text("Zoom"),
                    )
                    .changed()
                {
                    preview.zoom = 1. / magnification;
                }
                ui.checkbox(&mut preview.flip_vertical, "Flip vertically");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut preview.turntable, "Turntable");
                    ui.checkbox(&mut preview.playing, "Animation");
                });
                let actions = preview.action_names();
                if !actions.is_empty() {
                    let labels: Vec<String> = actions
                        .iter()
                        .enumerate()
                        .map(|(i, name)| preview3d::action_label(name, i))
                        .collect();
                    egui::ComboBox::from_label("Action")
                        .selected_text(
                            labels
                                .get(preview.action)
                                .map(String::as_str)
                                .unwrap_or("Bind pose"),
                        )
                        .show_ui(ui, |ui| {
                            for (action, name) in labels.iter().enumerate() {
                                if ui
                                    .selectable_label(preview.action == action, name)
                                    .on_hover_text(&actions[action])
                                    .clicked()
                                {
                                    preview.select_action(action);
                                }
                            }
                        });
                }
                if preview.playing || preview.turntable {
                    ui.ctx().request_repaint();
                }
                ui.label("Left-drag to orbit; right-drag to move; use the slider to zoom.");
            } else {
                ui.label("Preparing preview…");
            }
        }
        if ui
            .button("Add to comparison")
            .on_hover_text("Add as a column in the Comparison tab")
            .clicked()
            && !self.npc_comparison.contains(&index)
        {
            self.npc_comparison.push(index);
        }
        ui.separator();
        for (title, lines) in sections {
            ui.strong(title);
            for line in lines {
                ui.label(line);
            }
            ui.separator();
        }
        if !drops.is_empty() {
            ui.strong("Drops");
            let (times, counts) = drop_counts;
            let distribution: Vec<String> = counts
                .iter()
                .enumerate()
                .filter(|(_, chance)| **chance > 0.)
                .map(|(count, chance)| format!("{count}× {:.1}%", chance * 100.))
                .collect();
            ui.label(format!(
                "Rolls: {times}{}",
                if distribution.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", distribution.join(", "))
                }
            ));
            for (id, chance) in drops {
                let name = self
                    .database
                    .by_id
                    .get(&id)
                    .map(|&target| self.database.items[target].name.clone())
                    .unwrap_or_else(|| format!("Item {id} (not an item template)"));
                if ui
                    .link(format!("{} · {:.2}%", name, chance * 100.))
                    .clicked()
                {
                    if let Some(&target) = self.database.by_id.get(&id) {
                        self.selected = Some(target);
                        self.category = None;
                        self.section = None;
                        self.major = None;
                        self.subtype = None;
                        self.tab = Tab::Items;
                    }
                }
            }
            ui.separator();
        }
        if !skills.is_empty() {
            ui.strong("Skills");
            for (id, level, when) in skills {
                let name = self
                    .database
                    .skill_names
                    .get(&id.saturating_mul(10))
                    .map(|name| descriptions::plain_text(name))
                    .unwrap_or_else(|| format!("Skill {id}"));
                ui.label(format!("{name} · level {level} · {when}"));
            }
            ui.separator();
        }
        if let Some(source) = source_monster {
            // Resolve both the target and its label before the link, so the
            // click handler below is free to change the selection.
            if let Some(&target) = self.npc_database.by_id.get(&source) {
                let name = self.npc_database.entries[target].name.clone();
                let label = if name.is_empty() {
                    format!("Open the monster these attributes come from: monster {source}")
                } else {
                    format!("Open the monster these attributes come from: {name} ({source})")
                };
                if ui.link(label).clicked() {
                    self.npc_kind = Some(npcs::Kind::Monster);
                    self.npc_selected = Some(target);
                }
            }
        }
    }
    /// The NPC and monster columns of the shared comparison tab.
    ///
    /// NPCs and items have disjoint attributes, so they are laid out as two
    /// sections of one tab rather than one grid with a column type nobody can
    /// read across.
    fn npc_comparison_section(&mut self, ui: &mut egui::Ui) {
        if self.npc_comparison.is_empty() {
            return;
        }
        ui.heading("NPCs and monsters");
        // The grid only reads the database, so it runs from borrowed records and
        // reports the two actions back instead of mutating in place.
        let entries: Vec<&npcs::Entry> = self
            .npc_comparison
            .iter()
            .filter_map(|&index| self.npc_database.entries.get(index))
            .collect();
        if entries.is_empty() {
            self.npc_comparison.clear();
            return;
        }
        // Both actions are resolved against the list as it is now: removing a
        // column first would shift what the "open" position refers to.
        let (remove, jump) = npc_comparison_grid(ui, &entries);
        if let Some(position) = jump {
            self.npc_selected = self.npc_comparison.get(position).copied();
            self.tab = Tab::Npcs;
        }
        if let Some(position) = remove {
            if position < self.npc_comparison.len() {
                self.npc_comparison.remove(position);
            }
        }
        ui.separator();
    }
    fn comparison_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Comparison");
        self.npc_comparison_section(ui);
        if self.comparison.is_empty() {
            if self.npc_comparison.is_empty() {
                ui.label("Add items from their details, or NPCs and monsters from theirs.");
            }
            return;
        }
        let mut remove = None;
        let mut jump = None;
        let mut swap = None;
        egui::ScrollArea::both()
            .id_source("librarian_compare")
            .show(ui, |ui| {
                for category in Category::ALL {
                    let columns =
                        compare::category_columns(&self.database.items, &self.comparison, category);
                    if columns.is_empty() {
                        continue;
                    }
                    ui.heading(category.name());
                    ui.label("Comparisons and baseline are limited to this category.");
                    egui::Grid::new(("librarian_comparison_grid", category.name()))
                        .striped(true)
                        .min_col_width(COMPARISON_CELL_WIDTH)
                        .spacing(egui::vec2(14., 6.))
                        .show(ui, |ui| {
                            comparison_label(ui, "Attribute");
                            for (group_col, &col) in columns.iter().enumerate() {
                                let index = self.comparison[col];
                                let item = &self.database.items[index];
                                comparison_cell(ui, |ui| {
                                    if ui.link(&item.name).on_hover_text(&item.name).clicked() {
                                        jump = Some(index);
                                    }
                                    ui.horizontal(|ui| {
                                        if group_col > 0
                                            && ui
                                                .small_button("Left")
                                                .on_hover_text(
                                                    "Move item left within this category",
                                                )
                                                .clicked()
                                        {
                                            swap = Some((col, columns[group_col - 1]));
                                        }
                                        if group_col + 1 < columns.len()
                                            && ui
                                                .small_button("Right")
                                                .on_hover_text(
                                                    "Move item right within this category",
                                                )
                                                .clicked()
                                        {
                                            swap = Some((col, columns[group_col + 1]));
                                        }
                                        if ui
                                            .small_button("Remove")
                                            .on_hover_text("Remove item from comparison")
                                            .clicked()
                                        {
                                            remove = Some(col);
                                        }
                                    });
                                });
                            }
                            ui.end_row();
                            let items: Vec<_> = columns
                                .iter()
                                .map(|&col| &self.database.items[self.comparison[col]])
                                .collect();
                            for (name, values) in compare::rows(&items) {
                                comparison_label(ui, name);
                                let baseline = values.first().copied().flatten();
                                let best = compare::best(name, &values);
                                for value in values {
                                    match value {
                                        Some(value) => {
                                            let delta =
                                                baseline.map(|base| value - base).unwrap_or(0.);
                                            let text = if delta == 0. {
                                                format_value(value)
                                            } else {
                                                format!("{} ({:+.2})", format_value(value), delta)
                                            };
                                            comparison_value(ui, text, best == Some(value));
                                        }
                                        None => comparison_value(ui, "—", false),
                                    }
                                }
                                ui.end_row();
                            }
                        });
                    ui.separator();
                }
            });
        if let Some((a, b)) = swap {
            self.comparison.swap(a, b);
        }
        if let Some(col) = remove {
            self.comparison.remove(col);
        }
        if let Some(index) = jump {
            self.selected = Some(index);
            if let Some(gender) = self.database.items[index].fashion_gender() {
                self.preview_female = gender == 1;
            }
            self.category = None;
            self.section = None;
            self.major = None;
            self.subtype = None;
            self.tab = Tab::Items;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flipped_preview_is_a_textured_mesh_with_reversed_uvs() {
        let ctx = egui::Context::default();
        let texture = egui::TextureId::User(42);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                preview_image(ui, texture, 100., true);
            });
        });
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        let mesh = primitives
            .iter()
            .find_map(|p| match &p.primitive {
                egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == texture => Some(mesh),
                _ => None,
            })
            .expect("flipped preview must remain textured");
        assert_eq!(mesh.vertices[0].uv, egui::pos2(0., 1.));
        assert_eq!(mesh.vertices[3].uv, egui::pos2(1., 0.));
    }

    #[test]
    fn browsing_and_comparison_render_without_optional_archives() {
        let mut record = vec![0; 1412];
        record[..4].copy_from_slice(&42u32.to_le_bytes());
        for (index, unit) in "Test weapon".encode_utf16().enumerate() {
            record[12 + 2 * index..14 + 2 * index].copy_from_slice(&unit.to_le_bytes());
        }
        let database = Database::parse_v27(&elements::synthetic_file(3, &record)).unwrap();
        let mut assets = LibrarianAssets::open(std::path::Path::new("/nonexistent-librarian-test"));
        let icons = IconCache::load(&mut assets);
        let mut state = State {
            assets,
            icons,
            database,
            category: Some(Category::Weapons),
            section: None,
            major: None,
            subtype: None,
            search: String::new(),
            selected: Some(0),
            comparison: vec![0],
            tab: Tab::Items,
            effects: Default::default(),
            npc_database: npcs::Database::default(),
            npc_kind: Some(npcs::Kind::Npc),
            npc_search: String::new(),
            npc_selected: None,
            npc_comparison: Vec::new(),
            preview: None,
            show_3d: false,
            preview_female: false,
            preview_class: Class::Blademaster,
            preview_dye: [255; 3],
            font_checked: false,
            has_cjk_font: false,
        };
        let ctx = egui::Context::default();
        let run = |state: &mut State| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200., 800.),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| state.ui(ui));
                },
            );
        };
        run(&mut state);
        state.tab = Tab::Comparison;
        run(&mut state);
        assert_eq!(state.database.items[state.comparison[0]].id, 42);

        // The NPC tab renders from its own records.
        state.tab = Tab::Npcs;
        run(&mut state);
        assert!(state.npc_database.entries.is_empty() || state.npc_selected.is_none());

        // NPC columns belong to the shared Comparison tab, next to the items. A
        // stale index into an empty database must be dropped, not panicked on.
        state.npc_comparison.push(0);
        state.tab = Tab::Effects;
        run(&mut state);
        state.tab = Tab::Comparison;
        run(&mut state);
        assert!(state.npc_comparison.is_empty(), "stale NPC column dropped");
        assert_eq!(state.database.items[state.comparison[0]].id, 42);
    }

    /// The NPC tab against a real client's own records, skipped when none is
    /// installed.
    #[test]
    fn authentic_npc_and_monster_templates_when_available() {
        let Ok(path) = std::env::var("PW_LIBRARIAN_ELEMENTS") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read PW_LIBRARIAN_ELEMENTS");
        let npcs = npcs::Database::parse(&bytes).expect("parse NPC and monster records");
        assert_eq!(npcs.count(npcs::Kind::Npc), 2_838, "NPC_ESSENCE templates");
        assert_eq!(
            npcs.count(npcs::Kind::Monster),
            4_208,
            "MONSTER_ESSENCE templates"
        );

        let apothecary = &npcs.entries[npcs.by_id[&2109]];
        assert_eq!(apothecary.name, "Apothecary Cheng");
        assert_eq!(apothecary.model, "models/npcs/npc/男npc4/男npc4.ecm");
        assert_eq!(apothecary.source_monster(), Some(2111));
        assert_eq!(npcs.entries[npcs.by_id[&2111]].name, "Merchant");
        let labels: Vec<&str> = apothecary
            .services(&npcs)
            .iter()
            .map(|service| service.label)
            .collect();
        assert!(labels.contains(&"Sell goods") && labels.contains(&"Offer tasks"));
        assert!(!apothecary.greeting().is_empty());

        let tauroc = &npcs.entries[npcs.by_id[&986]];
        assert_eq!(tauroc.name, "Tauroc Valorian");
        // §39@552 is the captured pet egg; the life total is @556.
        assert_eq!(tauroc.egg(), Some(11_983));
        let life = tauroc
            .attributes()
            .into_iter()
            .find(|(name, _)| *name == "Life")
            .map(|(_, value)| value);
        assert_eq!(life, Some(10_234.));
        assert!(!tauroc.drops().is_empty());
        assert_eq!(tauroc.drops()[0].0, 8_612);

        // Every panel must render from the shipped records: these records are
        // authored data, so a panic or a wild number means the field map is
        // wrong, not that the record is odd.
        for entry in &npcs.entries {
            let _ = entry.sections(&npcs);
            let _ = entry.services(&npcs);
            let _ = entry.drop_counts();
            let _ = entry.skills();
            let _ = entry.addons(&npcs);
            for (name, value) in entry.attributes() {
                assert!(value.is_finite(), "{} {name} is not finite", entry.id);
            }
            for (id, chance) in entry.drops() {
                assert!(id > 0 && (0.0..=1.0).contains(&chance), "{}", entry.id);
            }
            let (times, counts) = entry.drop_counts();
            assert!(times <= 10, "{} rolls {times} times", entry.id);
            let total: f64 = counts.iter().map(|share| f64::from(*share)).sum();
            assert!(
                times == 0 || (total - 1.0).abs() < 0.001,
                "{} counts sum to {total}",
                entry.id
            );
        }
    }
}
