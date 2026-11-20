//! A private, offscreen instance of the game's GFX player. A .gfx authors
//! concurrent element timelines, not the named skeleton clips an ECM authors.
use super::{archive::LibrarianAssets, preview3d, preview_image};
use crate::{
    gfx_catalog::Catalog,
    gfx_effect_gpu::Draw,
    gfx_file,
    gfx_player::{self, Player, View},
};
use glam::Mat4;

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
    camera: [f32; 3], // yaw, pitch, zoom
    rendered_camera: Option<[f32; 3]>,
}
impl Clip {
    fn new(path: String, root: gfx_file::Root) -> Self {
        Self {
            path,
            root,
            solo: None,
            player: None,
            cursor: 0.,
            simulated: 0.,
            playing: true,
            camera: [0.6, 0.25, 1.],
            rendered_camera: None,
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
        let step = 16f32.max(end / 65536.);
        let goal = if self.cursor >= end {
            end
        } else {
            (self.cursor / step).floor() * step
        };
        let rebuild = self.player.is_none() || goal < self.simulated;
        if rebuild {
            self.player = Some(self.build(assets)?);
            self.simulated = 0.;
        }
        let player = self.player.as_mut().unwrap();
        let mut changed = rebuild || self.rendered_camera != Some(self.camera);
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
        self.rendered_camera = Some(self.camera);
        Ok(changed)
    }
}

#[derive(Default)]
pub struct Browser {
    catalog: Option<Result<Catalog, String>>,
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
                clip.root.aabb_center * scale,
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
    pub fn ui(&mut self, ui: &mut egui::Ui, assets: &mut LibrarianAssets) {
        let first = self.catalog.is_none();
        let catalog = match self.catalog.get_or_insert_with(|| assets.effect_catalog()) {
            Ok(c) => c,
            Err(e) => {
                ui.colored_label(egui::Color32::LIGHT_RED, e.as_str());
                return;
            }
        };
        let changed = ui
            .horizontal(|ui| {
                ui.label("Search effects:");
                ui.text_edit_singleline(&mut self.query).changed()
            })
            .inner;
        if first || changed {
            let q = self.query.to_lowercase();
            self.matches = catalog
                .entries()
                .iter()
                .enumerate()
                .filter(|(_, e)| e.path.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect();
        }
        ui.label(format!(
            "{} of {} effects",
            self.matches.len(),
            catalog.len()
        ));
        ui.columns(2, |cols| {
            let height = cols[0].text_style_height(&egui::TextStyle::Body);
            egui::ScrollArea::vertical()
                .id_source("librarian_effect_list")
                .show_rows(&mut cols[0], height, self.matches.len(), |ui, rows| {
                    ui.style_mut().wrap = Some(false);
                    for row in rows {
                        let i = self.matches[row];
                        let e = &catalog.entries()[i];
                        if ui
                            .add(egui::SelectableLabel::new(
                                self.selected == Some(i),
                                e.path.as_str(),
                            ))
                            .on_hover_text(&e.path)
                            .clicked()
                        {
                            self.selected = Some(i);
                            self.draw = None;
                            self.clip = None;
                            self.error = match read_root(assets, &e.path) {
                                Ok(root) => {
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
                    ui.heading(&e.path);
                    ui.small(format!("{} · {} bytes", e.group.label(), e.size));
                    if let Some(error) = &self.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                        return;
                    }
                    let Some(clip) = self.clip.as_mut() else {
                        return;
                    };
                    clip_ui(ui, clip, self.target.as_ref(), self.draw.as_ref());
                });
        });
    }
}

fn clip_ui(
    ui: &mut egui::Ui,
    clip: &mut Clip,
    target: Option<&preview3d::Target>,
    draw: Option<&Draw>,
) {
    let mut solo = clip.solo;
    let label = |i: usize| {
        format!(
            "{} · {} (type {})",
            i + 1,
            clip.root.elements[i].common.name,
            clip.root.elements[i].id
        )
    };
    egui::ComboBox::from_label("Animation")
        .selected_text(solo.map(label).unwrap_or_else(|| "Whole effect".into()))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut solo, None, "Whole effect");
            for i in 0..clip.root.elements.len() {
                ui.selectable_value(&mut solo, Some(i), label(i));
            }
        });
    if solo != clip.solo {
        clip.select(solo);
    }
    if let (Some(target), Some(_)) = (target, &clip.player) {
        let response = preview_image(ui, target.texture_id, ui.available_width().min(360.), false);
        if response.dragged() {
            let d = ui.input(|i| i.pointer.delta());
            clip.camera[0] -= d.x * 0.01;
            clip.camera[1] = (clip.camera[1] + d.y * 0.01).clamp(-1.5, 1.5);
        }
    }
    ui.add(egui::Slider::new(&mut clip.camera[2], 0.1..=4.).text("Zoom"));
    ui.small("Drag the preview to orbit.");
    let (end, open) = timeline(&clip.root, clip.solo);
    ui.horizontal(|ui| {
        ui.toggle_value(&mut clip.playing, "Play");
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
