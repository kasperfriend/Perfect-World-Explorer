//! Free-standing effects: any root in the archive, placed in the world.
//!
//! The Walker already plays the roots a character's ECM events ask for, parented
//! to a skeleton hook (`walker::gfx`). This is the same animated grammar with no
//! character in it — a root from `gfx_catalog`, a world placement, and the same
//! `gfx_effect_gpu::Draw` the character effects are submitted through, so the
//! per-bucket blend factors and the texture handling stay exactly as verified.
//! Textures need no new path either: the quads carry `gfx/textures/...` and the
//! host resolves them through the Walker's own `Assets`.
//!
//! Two rules are carried over from `walker::gfx` unchanged. The text is
//! GBK-decoded and a damaged decode is an error rather than a guess. A nested
//! `GfxPath` is another member of the same archive, and the 82 corpus references
//! that name files which do not ship stay warnings on the player, so the rest of
//! the root still plays.
//!
//! One rule is deliberately different: a gallery root is never offered to
//! `gfx_decal` first. That split exists because a static root needs no per-frame
//! state and the character path had been rendering those roots long before the
//! animated grammar was recovered; here everything goes through `gfx_file` +
//! `gfx_player`, which is the full grammar and the one path the corpus tests
//! cover. A root it rejects is a named error, never an approximation.

use crate::{
    gfx_catalog, gfx_file, gfx_player,
    modules::walker::{archive::Archive, physics},
    navigation::MapSpace,
};
use glam::{Mat4, Vec3};
use std::path::PathBuf;

/// How far apart preset roots are placed, in world units. The client's own scene
/// window is 420 units, and the largest preset roots (a waterfall's spray, a
/// light pillar) are authored in tens of units, so this keeps neighbours from
/// overlapping without pushing any of them out of view.
const SPACING: f32 = 80.;
/// How far in front of the eye a preset row is placed, in world units.
const DISTANCE: f32 = 150.;
/// Most search hits shown, so a two-character query cannot build a wall of
/// buttons.
const MAX_MATCHES: usize = 40;

struct Spawn {
    path: String,
    player: gfx_player::Player,
    transform: Mat4,
}

/// A set of roots playing in the world, with the catalogue behind them.
///
/// The archive handle is opened on first use and kept, so a gallery costs
/// nothing until somebody asks it for an effect; `gfx_catalog::Catalog::open`
/// reads only the directory, so browsing and searching cost one pass over the
/// 10,104 directory records and no member bytes.
#[derive(Default)]
pub struct Gallery {
    element: Option<PathBuf>,
    archive: Option<Archive>,
    catalog: Option<gfx_catalog::Catalog>,
    spawns: Vec<Spawn>,
    enabled: bool,
    preset: usize,
    query: String,
    matches: Vec<String>,
    last_error: Option<String>,
    warnings: Vec<String>,
}

impl Gallery {
    /// A gallery that knows where the client's `element` folder is.
    pub fn new(element: PathBuf) -> Self {
        Self {
            element: Some(element),
            enabled: true,
            ..Self::default()
        }
    }

    /// Point an already-built gallery at a client root, for hosts that learn the
    /// path after construction.
    pub fn set_element(&mut self, element: PathBuf) {
        self.element = Some(element);
        self.archive = None;
        self.catalog = None;
    }

    pub fn len(&self) -> usize {
        self.spawns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spawns.is_empty()
    }

    /// Read the catalogue, opening the archive's directory on first use. Browsing
    /// by name costs one pass over the directory records and no member bytes, so
    /// a search before the first spawn is as cheap as one after.
    fn catalog(&mut self) -> Result<&gfx_catalog::Catalog, String> {
        if self.catalog.is_none() {
            let element = self
                .element
                .clone()
                .ok_or("the Walker has no client element folder")?;
            self.catalog = Some(gfx_catalog::Catalog::open(&element.join("gfx.pck"))?);
        }
        self.catalog
            .as_ref()
            .ok_or_else(|| "the GFX catalogue is not read".to_string())
    }

    /// Load one root and place it in the world. The seed is a hash of the path,
    /// so the same root replays identically and two different roots do not share
    /// a stream; the client's own seed for a scene effect is not recovered, and
    /// what a gallery needs is reproducibility rather than the retail value.
    pub fn spawn(&mut self, path: &str, transform: Mat4) -> Result<(), String> {
        if !transform.is_finite() {
            return Err(format!("{path}: non-finite placement"));
        }
        // The handle is taken out of the option for the build and put back
        // afterwards, because the nested-child loader borrows it mutably for the
        // whole parse and that cannot overlap a borrow of `self`.
        let mut archive = match self.archive.take() {
            Some(archive) => archive,
            None => {
                let element = self
                    .element
                    .clone()
                    .ok_or("the Walker has no client element folder")?;
                Archive::open_archive(&element.join("gfx.pck"))?
            }
        };
        let built = load_root(&mut archive, path);
        self.archive = Some(archive);
        let player = built?;
        for warning in player.warnings() {
            let line = format!("{path}: {warning}");
            if !self.warnings.contains(&line) {
                self.warnings.push(line);
            }
        }
        self.spawns.push(Spawn {
            path: path.to_string(),
            player,
            transform,
        });
        Ok(())
    }

    /// Stop every root and forget the warnings they reported.
    pub fn clear(&mut self) {
        self.spawns.clear();
        self.warnings.clear();
        self.last_error = None;
    }

    /// Advance every root by `dt_ms` and return this frame's quads. Roots that
    /// have run out are dropped: a preset row is meant to be looked at, not
    /// accumulated, and the character path leaves that decision to its owner.
    pub fn advance(
        &mut self,
        view: &gfx_player::View,
        dt_ms: f32,
        map: Option<&MapSpace>,
    ) -> Result<Vec<gfx_player::EffectQuad>, String> {
        let mut out = Vec::new();
        if !self.enabled {
            return Ok(out);
        }
        for spawn in &mut self.spawns {
            // One ground query per spawn per frame, at the position the row put
            // it: the owner raycasts from its own placement (`0x6e53f0`), so a
            // root standing on terrain gets the aligned frame and one floating
            // above it stays where it was authored. 459 corpus roots ask for
            // this and 393 of them change shape once a query answers.
            let ground =
                map.map(|map| physics::effect_ground(map, spawn.transform.w_axis.truncate()));
            // The drape needs the surface under the sheet, not the one sample
            // the frames are built from.
            let height = |x: f32, z: f32| map.map(|map| physics::ground(map, x, z).height);
            let terrain: Option<gfx_player::GroundHeightQuery<'_>> = Some(&height);
            spawn
                .player
                .advance_with_terrain(dt_ms, view, spawn.transform, 1., 1., ground, terrain)
                .map_err(|error| format!("{}: {error}", spawn.path))?;
            out.extend(spawn.player.quads().iter().cloned());
        }
        self.spawns.retain(|spawn| !spawn.player.finished());
        Ok(out)
    }

    /// Fill the match list from the catalogue. A query shorter than two
    /// characters matches most of a 5,459-member archive, so it is ignored
    /// rather than truncated into something misleading.
    pub fn search(&mut self) {
        let query = self.query.trim().to_string();
        let mut found: Vec<String> = Vec::new();
        if query.chars().count() >= 2 {
            // The catalogue read is scoped, so the borrowed hits are owned
            // before anything else on `self` is touched.
            let mut error = None;
            {
                match self.catalog() {
                    Ok(catalog) => found.extend(
                        catalog
                            .search(&query)
                            .into_iter()
                            .take(MAX_MATCHES)
                            .map(|entry| entry.path.clone()),
                    ),
                    Err(e) => error = Some(e),
                }
            }
            if let Some(error) = error {
                self.last_error = Some(error);
            }
        }
        self.matches = found;
    }

    /// Place the selected preset's roots in a row facing the eye. Failures are
    /// collected rather than aborting the row: one root that does not ship should
    /// not cost the other five.
    fn play_preset(&mut self, at: Vec3, facing: Vec3) {
        let Some(preset) = gfx_catalog::presets().get(self.preset) else {
            self.last_error = Some("no such preset".into());
            return;
        };
        let paths: Vec<&str> = preset.paths.to_vec();
        let right = row_axis(facing);
        let spread = (paths.len() as f32 - 1.) * 0.5;
        self.last_error = None;
        for (index, path) in paths.into_iter().enumerate() {
            let offset = (index as f32 - spread) * SPACING;
            let position = at + facing * DISTANCE + right * offset;
            if let Err(error) = self.spawn(path, Mat4::from_translation(position)) {
                self.last_error = Some(error);
            }
        }
    }

    /// The gallery's controls. `at` is the host's eye and `facing` the direction
    /// it looks in, which is all a placement needs; both are copied by the
    /// caller so this borrows nothing else from the Walker.
    pub fn ui(&mut self, ui: &mut egui::Ui, at: Vec3, facing: Vec3) {
        ui.checkbox(&mut self.enabled, "Effect gallery");
        let presets = gfx_catalog::presets();
        let current = presets
            .get(self.preset)
            .map(|preset| preset.label)
            .unwrap_or("none");
        egui::ComboBox::from_label("Preset")
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (index, preset) in presets.iter().enumerate() {
                    ui.selectable_value(&mut self.preset, index, preset.label);
                }
            });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.enabled, egui::Button::new("Play in front of me"))
                .clicked()
            {
                self.play_preset(at, facing);
            }
            if ui.button("Clear").clicked() {
                self.clear();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Search the archive:");
            let edit = ui.add(egui::TextEdit::singleline(&mut self.query).desired_width(220.));
            if ui.button("Find").clicked() || edit.lost_focus() {
                self.search();
            }
        });
        let chosen: Vec<String> = self.matches.clone();
        if !chosen.is_empty() {
            egui::ScrollArea::vertical()
                .id_source("gallery_matches")
                .max_height(180.)
                .show(ui, |ui| {
                    for path in chosen {
                        if ui
                            .add_enabled(self.enabled, egui::Button::new(path.as_str()))
                            .clicked()
                        {
                            let position = at + facing * DISTANCE;
                            if let Err(error) = self.spawn(&path, Mat4::from_translation(position))
                            {
                                self.last_error = Some(error);
                            }
                        }
                    }
                });
        }
        ui.label(self.status());
        if let Some(error) = &self.last_error {
            ui.colored_label(egui::Color32::LIGHT_RED, error.clone());
        }
        for warning in self.warnings.iter().take(4) {
            ui.label(egui::RichText::new(warning.clone()).small().weak());
        }
    }

    /// One line for the module's status area.
    pub fn status(&self) -> String {
        let members = match &self.catalog {
            Some(catalog) => format!("{} catalogue members", catalog.len()),
            None => "catalogue not read".into(),
        };
        format!(
            "{} playing, {members}, {} warnings",
            self.spawns.len(),
            self.warnings.len()
        )
    }
}

/// Read one root and build its player, resolving nested `GfxPath` children
/// against the same archive. This is `walker::gfx`'s `Effect::load` in shape: the
/// archive handle is a parameter, so the child loader can borrow it for the whole
/// build with nothing else holding it.
fn load_root(archive: &mut Archive, path: &str) -> Result<gfx_player::Player, String> {
    let bytes = archive.read(path)?;
    if !bytes.starts_with(b"MOXT") {
        return Err(format!("{path}: binary GFX container is not implemented"));
    }
    let (text, bad) = gfx_file::decode(&bytes);
    if bad {
        return Err(format!("{path}: invalid GBK GFX root"));
    }
    let root = gfx_file::parse(&text).map_err(|error| format!("{path}: {error}"))?;
    gfx_player::Player::new(root, seed_for(path), &mut |child| {
        let bytes = archive.read(child)?;
        let (text, bad) = gfx_file::decode(&bytes);
        if bad {
            return Err(format!("{child}: invalid GBK"));
        }
        gfx_file::parse(&text)
    })
    .map_err(|error| format!("{path}: {error}"))
}

/// The axis a preset row is spread along: perpendicular to the look direction
/// and flat on the ground, so a row of waterfalls reads left to right instead of
/// receding into the distance. A look direction with no horizontal component
/// (straight up or down) falls back to world X.
fn row_axis(facing: Vec3) -> Vec3 {
    let flat = Vec3::new(facing.x, 0., facing.z);
    let axis = Vec3::Y.cross(flat);
    if axis.length_squared() > 1e-9 {
        axis.normalize()
    } else {
        Vec3::X
    }
}

/// FNV-1a over the path, forced odd. The client seeds its emitter from a runtime
/// stream this viewer does not reproduce; what a replay needs is that one root
/// keeps one stream and two roots do not share one.
fn seed_for(path: &str) -> u32 {
    let mut seed = 2_166_136_261u32;
    for byte in path.as_bytes() {
        seed ^= *byte as u32;
        seed = seed.wrapping_mul(16_777_619);
    }
    seed | 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_path_keeps_one_reproducible_seed() {
        let path = "gfx/场景/瀑布水花.gfx";
        assert_eq!(seed_for(path), seed_for(path));
        assert_ne!(seed_for(path), seed_for("gfx/场景/瀑布水圈.gfx"));
        assert_eq!(seed_for(path) & 1, 1, "a zero seed stalls some streams");
    }

    #[test]
    fn a_row_is_flat_and_perpendicular_to_the_look_direction() {
        let facing = Vec3::new(0., -0.2, -1.).normalize();
        let axis = row_axis(facing);
        assert!(axis.y.abs() < 1e-6, "the row tilted: {axis:?}");
        assert!(axis.dot(facing).abs() < 1e-4, "not perpendicular: {axis:?}");
        assert!((axis.length() - 1.).abs() < 1e-5);
        // Straight down has no horizontal component, so the row falls back to a
        // fixed axis rather than normalizing a zero vector into NaN.
        let axis = row_axis(Vec3::NEG_Y);
        assert_eq!(axis, Vec3::X);
        assert!(axis.is_finite());
    }
}
