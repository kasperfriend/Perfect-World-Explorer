//! A private, offscreen instance of the game's GFX player. A .gfx authors
//! concurrent element timelines, not the named skeleton clips an ECM authors.
use super::{
    archive::LibrarianAssets,
    effect_names::{Label, Names},
    preview3d, preview_image,
};
use crate::{
    gfx_catalog::Catalog,
    gfx_effect_gpu::Draw,
    gfx_file,
    gfx_player::{self, Player, View},
};
use glam::Mat4;
use std::collections::HashMap;

fn read_root(assets: &mut LibrarianAssets, path: &str) -> Result<gfx_file::Root, String> {
    let bytes = assets.read_model_resource(path)?;
    if !bytes.starts_with(b"MOXT") {
        return Err(format!("{path}: unsupported binary effect"));
    }
    let (text, bad) = gfx_file::decode(&bytes);
    if bad {
        return Err(format!("{path}: invalid GBK text"));
    }
    gfx_file::parse(&text).map_err(|e| format!("{path}: {e}"))
}

// An infinite hold has no last frame; show its finite lead-in and a useful
// inspection interval. Finite repeats have a delay *between* plays only.
fn timeline(root: &gfx_file::Root, solo: Option<usize>) -> (f32, bool) {
    let mut open = false;
    let mut end = 0f32;
    for (i, e) in root.elements.iter().enumerate() {
        if solo.is_some_and(|n| n != i) {
            continue;
        }
        let delay = e.common.repeat_delay.max(0.);
        let repeat = e.common.repeat_count;
        let span = match e.duration_ms() {
            None => {
                open = true;
                e.keypoints
                    .iter()
                    .take_while(|k| k.time_span >= 0.)
                    .map(|k| k.time_span)
                    .sum::<f32>()
                    + 5000.
            }
            Some(span) if repeat <= 0 || repeat == i32::MAX => {
                open = true;
                span + delay
            }
            Some(span) => span * repeat as f32 + delay * (repeat - 1) as f32,
        };
        end = end.max(e.start_time + span);
    }
    (end.max(1.), open)
}

struct Clip {
    path: String,
    root: gfx_file::Root,
    solo: Option<usize>,
    player: Option<Player>,
    // The scrubber is in authored milliseconds. Rebuild on backward seek:
    // Player::reset is a game loop restart, not a rewind of particles/RNG.
    cursor: f32,
    simulated: f32,
    playing: bool,
    camera: [f32; 3], // yaw, pitch, zoom distance
    pan: glam::Vec3,  // look-at offset, in authored units
    rendered_view: Option<([f32; 3], glam::Vec3)>,
}
impl Clip {
    fn new(path: String, root: gfx_file::Root) -> Self {
        let playing = root.play_speed > 0.;
        Self {
            path,
            root,
            solo: None,
            player: None,
            cursor: 0.,
            simulated: 0.,
            playing,
            camera: [0.6, 0.25, 1.],
            pan: glam::Vec3::ZERO,
            rendered_view: None,
        }
    }
    fn select(&mut self, solo: Option<usize>) {
        self.solo = solo;
        self.cursor = 0.;
        self.player = None;
    }
    fn build(&self, assets: &mut LibrarianAssets) -> Result<Player, String> {
        let mut root = self.root.clone();
        // Preserve BindEle targets while soloing a track. Dummy elements still
        // supply their placements, but don't submit their geometry.
        if let Some(index) = self.solo {
            for (i, e) in root.elements.iter_mut().enumerate() {
                if i != index {
                    e.common.is_dummy = true;
                }
            }
        }
        // UI time is the root clock. Account for PlaySpeed when moving the
        // cursor, not twice; a zero-speed root can still be manually scrubbed.
        root.play_speed = 1.;
        let mut player = Player::new(root, gfx_player::path_seed(&self.path), &mut |path| {
            read_root(assets, path)
        })?;
        player.load_models(&self.path, &mut |path| assets.read_model_resource(path))?;
        Ok(player)
    }
    /// Seek on a fixed grid, including the exact end frame. Bound catch-up
    /// work per GUI frame so scrubbing through a particle system stays usable.
    fn update(&mut self, assets: &mut LibrarianAssets, view: &View) -> Result<bool, String> {
        let (end, _) = timeline(&self.root, self.solo);
        // Preserve sub-frame flashes while playing. Paused scrubs, unlike
        // playback ticks, must land at the exact requested instant.
        let step = (end / 16.).min(16.).max(end / 65536.);
        let goal = if !self.playing || self.cursor >= end {
            self.cursor
        } else {
            (self.cursor / step).floor() * step
        };
        let rebuild = self.player.is_none() || goal < self.simulated;
        if rebuild {
            self.player = Some(self.build(assets)?);
            self.simulated = 0.;
        }
        let player = self.player.as_mut().unwrap();
        let mut changed = rebuild || self.rendered_view != Some((self.camera, self.pan));
        if changed {
            player.advance(0., view, Mat4::IDENTITY, 1., 1.)?;
        }
        for _ in 0..64 {
            if self.simulated >= goal {
                break;
            }
            let next = (self.simulated + step).min(goal);
            player.advance(next - self.simulated, view, Mat4::IDENTITY, 1., 1.)?;
            self.simulated = next;
            changed = true;
        }
        self.rendered_view = Some((self.camera, self.pan));
        Ok(changed)
    }
}

#[derive(Default)]
pub struct Browser {
    catalog: Option<Result<Catalog, String>>,
    names: Option<Names>,
    labels: Vec<Label>,
    tracks: Vec<Label>,
    query: String,
    matches: Vec<usize>,
    selected: Option<usize>,
    clip: Option<Clip>,
    error: Option<String>,
    pub target: Option<preview3d::Target>,
    draw: Option<Draw>,
}
impl Browser {
    pub fn prepare(
        &mut self,
        assets: &mut LibrarianAssets,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
    ) {
        if self.error.is_some() {
            return;
        }
        let Some(clip) = self.clip.as_mut() else {
            return;
        };
        let result = (|| {
            let [yaw, pitch, zoom] = clip.camera;
            let scale = clip.root.default_scale;
            let matrix = preview3d::camera_matrix(
                clip.root.aabb_center * scale + clip.pan,
                (clip.root.bounds_radius() * scale).max(1.),
                zoom,
                yaw,
                pitch,
            );
            let view = View::try_from_inverse_view_projection(matrix.inverse())?;
            if !clip.update(assets, &view)? {
                return Ok(());
            }
            if self.draw.is_none() {
                self.draw = Some(Draw::new(
                    device,
                    queue,
                    wgpu::TextureFormat::Rgba8Unorm,
                    Some((
                        wgpu::TextureFormat::Depth32Float,
                        wgpu::CompareFunction::Greater,
                    )),
                )?);
            }
            let draw = self.draw.as_mut().unwrap();
            draw.update(
                device,
                queue,
                clip.player.as_ref().unwrap().quads(),
                matrix.to_cols_array_2d(),
                [0.; 4],
                view.position.extend(0.).to_array(),
                None,
                &mut |p| assets.read_model_resource(p),
            )?;
            let target = self
                .target
                .get_or_insert_with(|| preview3d::Target::new(device, renderer));
            let mut encoder = device.create_command_encoder(&Default::default());
            draw.draw(&mut target.pass(&mut encoder));
            queue.submit(Some(encoder.finish()));
            Ok(())
        })();
        self.error = result.err();
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        assets: &mut LibrarianAssets,
        skill_names: &HashMap<u32, String>,
    ) {
        let first = self.catalog.is_none();
        let catalog = match self.catalog.get_or_insert_with(|| assets.effect_catalog()) {
            Ok(c) => c,
            Err(e) => {
                ui.colored_label(egui::Color32::LIGHT_RED, e.as_str());
                return;
            }
        };
        let names = self.names.get_or_insert_with(|| Names::new(skill_names));
        if first {
            self.labels = catalog
                .entries()
                .iter()
                .map(|e| names.label(&e.path, Names::is_skill_path(&e.path)))
                .collect();
        }
        let changed = ui
            .horizontal(|ui| {
                ui.label("Search effects:");
                ui.text_edit_singleline(&mut self.query).changed()
            })
            .inner;
        if first || changed {
            let q = self.query.to_lowercase();
            if q.is_empty() {
                self.matches = (0..catalog.len()).collect();
            } else {
                self.matches = catalog
                    .entries()
                    .iter()
                    .enumerate()
                    .filter(|(i, e)| {
                        self.labels[*i].text.to_lowercase().contains(&q)
                            || e.path.to_lowercase().contains(&q)
                    })
                    .map(|(i, _)| i)
                    .collect();
            }
        }
        ui.label(format!(
            "{} of {} effects",
            self.matches.len(),
            catalog.len()
        ));
        if self.matches.is_empty() {
            ui.weak(if catalog.is_empty() {
                "No .gfx files in gfx.pck or the loose gfx folder."
            } else {
                "No effects match this search."
            });
        }
        ui.columns(2, |cols| {
            let height = cols[0]
                .text_style_height(&egui::TextStyle::Body)
                .max(cols[0].spacing().interact_size.y);
            egui::ScrollArea::both()
                .id_source("librarian_effect_list")
                .show_rows(&mut cols[0], height, self.matches.len(), |ui, rows| {
                    ui.style_mut().wrap = Some(false);
                    for row in rows {
                        let i = self.matches[row];
                        let e = &catalog.entries()[i];
                        if ui
                            .add(egui::SelectableLabel::new(
                                self.selected == Some(i),
                                self.labels[i].text.as_str(),
                            ))
                            .on_hover_text(format!("{}\n{}", self.labels[i].text, e.path))
                            .clicked()
                        {
                            self.selected = Some(i);
                            self.draw = None;
                            self.clip = None;
                            self.error = match read_root(assets, &e.path) {
                                Ok(root) => {
                                    let skill = Names::is_skill_path(&e.path);
                                    self.tracks = root
                                        .elements
                                        .iter()
                                        .map(|el| names.label(&el.common.name, skill))
                                        .collect();
                                    self.clip = Some(Clip::new(e.path.clone(), root));
                                    None
                                }
                                Err(e) => Some(e),
                            };
                        }
                    }
                });
            egui::ScrollArea::vertical()
                .id_source("librarian_effect_details")
                .show(&mut cols[1], |ui| {
                    let Some(i) = self.selected else {
                        ui.label("Select an effect to preview its animation.");
                        return;
                    };
                    let e = &catalog.entries()[i];
                    let label = &self.labels[i];
                    ui.heading(label.text.rsplit('/').next().unwrap_or(&label.text));
                    ui.small(&e.path);
                    if label.untranslated {
                        ui.small("Some terms in this path have no known translation.");
                    }
                    ui.small(format!("{} · {} bytes", e.group.label(), e.size));
                    if let Some(error) = &self.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                        return;
                    }
                    let Some(clip) = self.clip.as_mut() else {
                        return;
                    };
                    clip_ui(
                        ui,
                        clip,
                        &self.tracks,
                        self.target.as_ref(),
                        self.draw.as_ref(),
                    );
                });
        });
    }
}

fn clip_ui(
    ui: &mut egui::Ui,
    clip: &mut Clip,
    tracks: &[Label],
    target: Option<&preview3d::Target>,
    draw: Option<&Draw>,
) {
    let mut solo = clip.solo;
    let label = |i: usize| {
        format!(
            "{} · {} (type {})",
            i + 1,
            tracks[i].text,
            clip.root.elements[i].id
        )
    };
    egui::ComboBox::from_label("Animation")
        .selected_text(solo.map(label).unwrap_or_else(|| "Whole effect".into()))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut solo, None, "Whole effect");
            for i in 0..tracks.len() {
                ui.selectable_value(&mut solo, Some(i), label(i))
                    .on_hover_text(&clip.root.elements[i].common.name);
            }
        });
    if solo != clip.solo {
        clip.select(solo);
    }
    if let (Some(target), Some(_)) = (target, &clip.player) {
        let response = preview_image(ui, target.texture_id, ui.available_width().min(360.), false);
        let shift = ui.input(|i| i.modifiers.shift);
        if let Some(mode) = preview3d::drag_mode(&response, shift) {
            let delta = ui.input(|i| i.pointer.delta());
            match mode {
                preview3d::Drag::Pan => {
                    let scale = clip.root.default_scale;
                    let radius = (clip.root.bounds_radius() * scale).max(1.);
                    clip.pan +=
                        preview3d::pan_offset(clip.camera[0], radius, clip.camera[2], delta);
                }
                preview3d::Drag::Orbit => {
                    clip.camera[0] = preview3d::orbit_yaw(clip.camera[0], delta.x);
                    clip.camera[1] = (clip.camera[1] + delta.y * 0.01).clamp(-1.5, 1.5);
                }
            }
        }
    }
    // Same convention as the item preview: a larger magnification zooms in.
    let mut magnification = 1. / clip.camera[2];
    if ui
        .add(
            egui::Slider::new(&mut magnification, 0.25..=10.)
                .logarithmic(true)
                .text("Zoom"),
        )
        .changed()
    {
        clip.camera[2] = 1. / magnification;
    }
    ui.small("Left-drag to orbit; right-drag to move; use the slider to zoom.");
    let (end, open) = timeline(&clip.root, clip.solo);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(clip.root.play_speed > 0., |ui| {
            ui.toggle_value(&mut clip.playing, "Play")
        })
        .response
        .on_disabled_hover_text(
            "Authored playback speed is zero. Use the slider to inspect this effect.",
        );
        ui.label(format!("{:.2} / {:.2} s", clip.cursor / 1000., end / 1000.));
    });
    if ui
        .add(
            egui::Slider::new(&mut clip.cursor, 0.0..=end)
                .show_value(false)
                .text("Animation cycle"),
        )
        .changed()
    {
        clip.playing = false;
    }
    if open {
        ui.small(
            "Open-ended animation: the slider covers a preview interval, not a finite lifetime.",
        );
    }
    if clip.cursor - clip.simulated >= 16. {
        ui.ctx().request_repaint();
    }
    if clip.playing {
        clip.cursor =
            (clip.cursor + ui.input(|i| i.stable_dt.min(0.1)) * 1000. * clip.root.play_speed) % end;
        ui.ctx().request_repaint();
    }
    ui.separator();
    ui.label(format!(
        "GFX v{} · {} elements · {} keypoints · playback ×{}",
        clip.root.version,
        clip.root.elements.len(),
        clip.root
            .elements
            .iter()
            .map(|e| e.keypoints.len())
            .sum::<usize>(),
        clip.root.play_speed
    ));
    if let Some(p) = &clip.player {
        let s = p.stats();
        ui.label(format!(
            "{} quads · {} live particles · {} emitters · {} models · {} child effects",
            s.quads, s.particles, s.emitters, s.models, s.children
        ));
        if s.quads == 0 {
            ui.small("No drawable geometry at this time.");
        }
        if s.lights + s.sounds + s.warp + s.unrendered > 0 {
            ui.small(format!(
                "Not drawn in isolation: {} lights, {} sounds, {} warp, {} unsupported",
                s.lights, s.sounds, s.warp, s.unrendered
            ));
        }
        for w in p
            .warnings()
            .iter()
            .chain(draw.into_iter().flat_map(|d| d.failures()))
        {
            ui.colored_label(egui::Color32::YELLOW, w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const HEADER: &str = "MOXTVersion: 37\nDedaultScale: 1\nPlaySpeed: 1\nDefaultAlpha: 1\nRaytrace: 0\nFadeByDist: 0\nFadeStart: 0\nFadeEnd: 0\nVec: 0,0,0\nVec: 1,1,1\nShakeCam: 0\nGFXELEMENTCOUNT: 2\n";
    const SPRITE: &str = "GFXELEMENTID: 100\nName: glow\nSrcBlend: 5\nDestBlend: 2\nRepeatCount: 2\nRepeatDelay: 20\nTexFile: sample.png\nWidth: 0.5\nHeight: 0.5\nStartTime: 10\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: 100\nPosition: 0,0,0\nColor: -1\nScale: 1\nDirection: 0,0,0,1\nRad_2D: 0\nCtrlMethodCount: 0\n";
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("librarian-effects-{name}-{}", std::process::id()));
            std::fs::create_dir_all(dir.join("gfx/textures")).unwrap();
            let text = format!(
                "{HEADER}{SPRITE}{}",
                SPRITE
                    .replace("glow", "halo")
                    .replace("TimeSpan: 100", "TimeSpan: -1")
            );
            std::fs::write(dir.join("gfx/Test.GFX"), text).unwrap();
            std::fs::write(dir.join("gfx/unsupported.gfx"), [0u8; 4]).unwrap();
            image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 255, 255, 255]))
                .save(dir.join("gfx/textures/sample.png"))
                .unwrap();
            Self(dir)
        }
        fn load(&self) -> (LibrarianAssets, Clip) {
            let mut assets = LibrarianAssets::open(&self.0);
            let catalog = assets.effect_catalog().unwrap();
            assert_eq!(catalog.len(), 2);
            assert!(read_root(&mut assets, "gfx/unsupported.gfx").is_err());
            let path = &catalog.search("TEST")[0].path;
            let root = read_root(&mut assets, path).unwrap();
            (assets, Clip::new(path.clone(), root))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn browse_packed_and_loose_roots_together() {
        // One v2 directory record with a GBK, package-relative member name.
        // No copy of this member exists on disk: selecting it must go back
        // through the retained PCK handle, not the loose-file fallback.
        let files = Fixture::new("packed");
        let source = format!("{HEADER}{SPRITE}{SPRITE}");
        let mut pack = Vec::from_iter(0x4dca23efu32.to_le_bytes());
        pack.extend(0u32.to_le_bytes());
        pack.extend(0x56a089b7u32.to_le_bytes());
        pack.extend(source.bytes());
        let directory = pack.len() as u32;
        let mask = 0xa8937462;
        let mut record = [0u8; 276];
        let (name, _, bad) = encoding_rs::GBK.encode("场景\\Glow.GFX");
        assert!(!bad);
        record[..name.len()].copy_from_slice(&name);
        record[260..264].copy_from_slice(&12u32.to_le_bytes());
        record[264..268].copy_from_slice(&(source.len() as u32).to_le_bytes());
        record[268..272].copy_from_slice(&(source.len() as u32).to_le_bytes());
        pack.extend((276u32 ^ mask).to_le_bytes());
        pack.extend((276u32 ^ mask ^ 0x59374231).to_le_bytes());
        pack.extend(record);
        let mut footer = [0u8; 280];
        for (offset, value) in [
            (0, 0xfdfdfeee),
            (8, directory ^ mask),
            (268, 0xf00dbeef),
            (272, 1),
            (276, 0x20002),
        ] {
            footer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        pack.extend(footer);
        let end = (pack.len() as u32).to_le_bytes();
        pack[4..8].copy_from_slice(&end);
        std::fs::write(files.0.join("gfx.pck"), pack).unwrap();
        // A loose override with a different case must not be listed as a
        // separately sized asset when reads actually resolve to the pack.
        std::fs::create_dir_all(files.0.join("gfx/场景")).unwrap();
        std::fs::write(files.0.join("gfx/场景/glow.gfx"), b"shadow").unwrap();
        let mut assets = LibrarianAssets::open(&files.0);
        let catalog = assets.effect_catalog().unwrap();
        assert_eq!(catalog.len(), 3);
        let entry = catalog.search("glow")[0];
        assert_eq!(entry.path, "gfx/场景/Glow.GFX");
        assert_eq!(entry.size, source.len() as u32);
        assert_eq!(
            read_root(&mut assets, &entry.path).unwrap().elements.len(),
            2
        );
    }
    #[test]
    fn reference_effect_names() {
        let Ok(dir) = std::env::var("PW_LIBRARIAN_REFERENCE_EFFECT_ROOT") else {
            return;
        };
        let mut assets = LibrarianAssets::open(std::path::Path::new(&dir));
        let catalog = assets.effect_catalog().unwrap();
        assert_eq!(
            catalog.len(),
            5459,
            "expected the PW 1.4.2 reference gfx.pck"
        );
        let strings = assets
            .read_config("configs/skillstr.txt")
            .map(|b| super::super::descriptions::keyed_strings(&b))
            .unwrap_or_default();
        let names = Names::new(&strings);
        let mut tracks = std::collections::HashSet::new();
        for e in catalog.entries() {
            let skill = Names::is_skill_path(&e.path);
            assert!(!names.label(&e.path, skill).untranslated, "{}", e.path);
            let bytes = assets.read_model_resource(&e.path).unwrap();
            let (source, _) = gfx_file::decode(&bytes);
            for track in source
                .lines()
                .filter_map(|l| l.trim().strip_prefix("Name:"))
                .map(str::trim)
            {
                assert!(
                    !names.label(track, skill).untranslated,
                    "{}: {track}",
                    e.path
                );
                tracks.insert(track.to_owned());
            }
        }
        assert_eq!(tracks.len(), 444);
    }

    #[test]
    fn browse_solo_and_scrub_loose_effect() {
        let files = Fixture::new("clock");
        let (mut assets, mut clip) = files.load();
        assert_eq!(timeline(&clip.root, None), (5010., true));
        let view = View::default();
        clip.cursor = 32.;
        clip.update(&mut assets, &view).unwrap();
        assert_eq!(clip.player.as_ref().unwrap().stats().quads, 2);
        clip.select(Some(0));
        let (end, open) = timeline(&clip.root, clip.solo);
        assert_eq!((end, open), (230., false));
        clip.cursor = 32.;
        clip.update(&mut assets, &view).unwrap();
        let sample = clip.player.as_ref().unwrap().quads().to_vec();
        assert_eq!(sample.len(), 1);
        clip.cursor = end;
        clip.update(&mut assets, &view).unwrap();
        assert_eq!(clip.simulated, end);
        assert!(clip.player.as_ref().unwrap().quads().is_empty());
        clip.cursor = 32.;
        clip.update(&mut assets, &view).unwrap();
        assert_eq!(sample, clip.player.as_ref().unwrap().quads());
    }
    #[test]
    fn paused_seek_can_show_a_flash_between_simulation_ticks() {
        let files = Fixture::new("flash");
        let (mut assets, mut clip) = files.load();
        clip.root.elements[0].start_time = 1.;
        clip.root.elements[0].keypoints[0].time_span = 4.;
        clip.select(Some(0));
        clip.playing = false;
        clip.cursor = 2.;
        clip.update(&mut assets, &View::default()).unwrap();
        assert_eq!(clip.player.as_ref().unwrap().stats().quads, 1);
        assert_eq!(clip.simulated, 2.);
    }

    #[test]
    fn loose_dependencies_resolve_with_windows_case() {
        let files = Fixture::new("case");
        std::fs::rename(
            files.0.join("gfx/textures/sample.png"),
            files.0.join("gfx/textures/SpArK.PNG"),
        )
        .unwrap();
        let mut assets = LibrarianAssets::open(&files.0);
        assets.effect_catalog().unwrap();
        assert!(assets
            .read_model_resource("gfx/textures/spark.png")
            .unwrap()
            .starts_with(b"\x89PNG"));
    }
    // The repository's existing Librarian software-Vulkan job selects tests
    // containing this name; exercise the same target with the GFX pass too.
    #[test]
    fn effect_srgb_window_uses_encoded_unorm_preview_attachment() {
        if std::env::var_os("PW_LIBRARIAN_SYNTHETIC_GPU").is_none() {
            return;
        }
        let files = Fixture::new("gpu");
        let (mut assets, mut clip) = files.load();
        clip.select(Some(0));
        clip.cursor = 32.;
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default(), None)).unwrap();
        let mut renderer =
            egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, None, 1);
        let mut browser = Browser {
            clip: Some(clip),
            ..Default::default()
        };
        browser.prepare(&mut assets, &device, &queue, &mut renderer);
        assert_eq!(browser.error, None);
        assert!(browser.draw.as_ref().unwrap().failures().is_empty());
        let pixels = browser.target.as_ref().unwrap().pixels(&device, &queue);
        assert!(
            pixels
                .chunks_exact(4)
                .filter(|p| *p != &pixels[..4])
                .count()
                > 100
        );
        browser.clip.as_mut().unwrap().cursor = 0.;
        browser.prepare(&mut assets, &device, &queue, &mut renderer);
        assert_eq!(browser.error, None);
        let blank = browser.target.as_ref().unwrap().pixels(&device, &queue);
        assert!(blank.chunks_exact(4).all(|p| p == &blank[..4]));
    }
}
