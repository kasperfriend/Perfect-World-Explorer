//! Experimental GPU world renderer: actual client terrain, static BMDs and water masks.
//! Baked terrain material atlas, fixed lighting and flat water are not client parity.
use crate::scenery_visibility::{
    allows_class_with_context, lod_scale, Class, ModelClass, Pass, RenderContext, Visibility,
    NATIVE_DEFAULT_WATER_EFFECT,
};
use crate::Terrain;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};
use wgpu::util::DeviceExt;
use winit::{
    event::{DeviceEvent, ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::EventLoop,
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorGrabMode, Fullscreen, WindowBuilder},
};
const TILE: usize = 32;
use crate::view_settings::{DetailSettings, MapStyle, MinimapShape, ViewSettings};

const TEX: usize = 128;
const ATLAS: usize = 4096;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct Vertex {
    pub(crate) pos: [f32; 3],
    pub(crate) normal: [f32; 3],
    pub(crate) uv: [f32; 2],
    pub(crate) color: [f32; 4],
    pub(crate) kind: f32,
    pub(crate) diffuse: u32,
    pub(crate) night_diffuse: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct Uniform {
    matrix: [[f32; 4]; 4],
    direction: [f32; 4],
    clip: [f32; 4],
    bounds: [f32; 4],
    tree_bounds: [f32; 4],
    sun: [f32; 4],
    light: [f32; 4],
    region_ambient: [f32; 4],
    region_diffuse: [f32; 4],
    region_direction: [f32; 4],
    grass_eye: [f32; 4],
    grass_wind: [f32; 4],
    grass_history: [[f32; 4]; 80],
    /// `start`, `end`, `enabled`, and the terrain day/night blend factor - the
    /// last lane carries `camera.terrain_day_night` so the terrain shader can run
    /// the native blend instead of inverting the lightmap-availability flag.
    fog_range: [f32; 4],
    fog_color: [f32; 4], // encoded client ARGB
}

/// One lighting/playback snapshot per rendered frame. `render()` samples wall
/// time, ticks wind/glow/celestial/model playback and publishes scene lights
/// exactly once; reflection, refraction, minimap and full-map views reuse this
/// snapshot instead of advancing time or consuming a second publication.
#[derive(Clone, Copy)]
struct FrameSnapshot {
    elapsed: f32,
    elapsed64: f64,
    logic_delta_ms: u32,
    direction: Vec3,
    daylight: f32,
    twilight: f32,
    sun: [f32; 4],
    light: [f32; 4],
    bounds: [f32; 4],
    tree_bounds: [f32; 4],
    region_ambient: [f32; 4],
    region_diffuse: [f32; 4],
    region_direction: [f32; 4],
    grass_eye: [f32; 4],
    grass_wind: [f32; 4],
    grass_history: [[f32; 4]; 80],
    fog_range: [f32; 4],
    fog_color: [f32; 4],
    active_fog_range: [f32; 4],
    active_fog_color: [f32; 4],
    clip: [f32; 4],
    glow_parameters: [f32; 6],
    celestial_alpha: f32,
    authored: [f32; 3],
    celestial: [f32; 3],
    time_advances: u32,
    light_publications: u32,
}
impl Default for FrameSnapshot {
    fn default() -> Self {
        Self {
            elapsed: 0.,
            elapsed64: 0.,
            logic_delta_ms: 0,
            direction: Vec3::ZERO,
            daylight: 0.,
            twilight: 0.,
            sun: [0.; 4],
            light: [0.; 4],
            bounds: [0.; 4],
            tree_bounds: [0.; 4],
            region_ambient: [0.; 4],
            region_diffuse: [0.; 4],
            region_direction: [0.; 4],
            grass_eye: [0.; 4],
            grass_wind: [0.; 4],
            grass_history: [[0.; 4]; 80],
            fog_range: [0.; 4],
            fog_color: [0.; 4],
            active_fog_range: [0.; 4],
            active_fog_color: [0.; 4],
            clip: [0.; 4],
            glow_parameters: [0.; 6],
            celestial_alpha: 0.,
            authored: [0.; 3],
            celestial: [0.; 3],
            time_advances: 0,
            light_publications: 0,
        }
    }
}
impl FrameSnapshot {
    fn uniform(
        &self,
        matrix: Mat4,
        direction: [f32; 4],
        clip: [f32; 4],
        fog_range: [f32; 4],
        fog_color: [f32; 4],
    ) -> Uniform {
        Uniform {
            matrix: matrix.to_cols_array_2d(),
            direction,
            clip,
            bounds: self.bounds,
            tree_bounds: self.tree_bounds,
            sun: self.sun,
            light: self.light,
            region_ambient: self.region_ambient,
            region_diffuse: self.region_diffuse,
            region_direction: self.region_direction,
            grass_eye: self.grass_eye,
            grass_wind: self.grass_wind,
            grass_history: self.grass_history,
            fog_range,
            fog_color,
        }
    }
    fn secondary(&self, pass: Pass) -> crate::scenery_visibility::SecondaryView {
        crate::scenery_visibility::secondary_view(pass, self.elapsed)
    }
}
fn minimap_matrix(x: f32, z: f32, range: f32) -> Mat4 {
    let eye = Vec3::new(x, 10000., z);
    // Game world +Z is north. Flip horizontal clip space because the
    // right-handed top-down view otherwise puts west on the right.
    Mat4::orthographic_rh(range, -range, -range, range, 20000., 1.)
        * Mat4::look_to_rh(eye, Vec3::NEG_Y, Vec3::Z)
}
/// Keep the world-map point under the cursor fixed when its image is scaled.
fn anchored_zoom_offset(offset: egui::Vec2, cursor_in_image: egui::Vec2, ratio: f32) -> egui::Vec2 {
    (offset + cursor_in_image * (ratio - 1.)).max(egui::Vec2::ZERO)
}
/// One environment switch, resolved once.
///
/// `std::env::var_os` takes the process environment lock, and several of these
/// checks sit on hot paths: the mouse-motion handler runs one per device event,
/// the opaque draw submits two per pass, and the water and UI paths run theirs
/// every frame. The environment cannot change while the viewer runs, so each
/// switch is resolved on first use and remembered.
fn env_switch(slot: &'static std::sync::OnceLock<bool>, name: &'static str) -> bool {
    *slot.get_or_init(|| std::env::var_os(name).is_some())
}
/// `PW_INPUT_DEBUG`: echo input events. Checked on every mouse-motion event.
fn input_debug() -> bool {
    static SLOT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    env_switch(&SLOT, "PW_INPUT_DEBUG")
}
/// `PW_UI_TEST`: software-GPU diagnostic mode, checked every frame and pass.
fn ui_test() -> bool {
    static SLOT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    env_switch(&SLOT, "PW_UI_TEST")
}
/// `PW_SKIP_TREES_TEST`: drop the tree instances, checked every pass.
fn skip_trees_test() -> bool {
    static SLOT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    env_switch(&SLOT, "PW_SKIP_TREES_TEST")
}
/// `PW_WATER_DEBUG`: log the water batches once, checked every frame.
fn water_debug() -> bool {
    static SLOT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    env_switch(&SLOT, "PW_WATER_DEBUG")
}
/// `PW_GRASS_DEBUG`: log the grass bindings once, checked while preparing.
fn grass_debug() -> bool {
    static SLOT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    env_switch(&SLOT, "PW_GRASS_DEBUG")
}

/// Where a slider's "default" tick sits on its track, `0..1`.
///
/// egui distributes a logarithmic slider's track by `ln(value)`, so the tick has
/// to use the same mapping. A linear fraction put the `1.0` default of a
/// `0.1..=600` logarithmic track at 0.0015 — the far left, which reads as "the
/// default is 0.1". A degenerate or non-finite range falls back to the left edge
/// instead of producing a NaN position.
fn slider_mark_fraction(default: f32, min: f32, max: f32, logarithmic: bool) -> f32 {
    if !(default.is_finite() && min.is_finite() && max.is_finite()) || max <= min {
        return 0.;
    }
    let t = if logarithmic && min > 0. && default > 0. {
        (default / min).ln() / (max / min).ln()
    } else {
        (default - min) / (max - min)
    };
    if t.is_finite() {
        t.clamp(0., 1.)
    } else {
        0.
    }
}
/// Show the saved/default value as both a gold rail tick and a numeric label.
fn marked_slider(
    ui: &mut egui::Ui,
    slider: egui::Slider<'_>,
    label: &str,
    default: f32,
    min: f32,
    max: f32,
    logarithmic: bool,
    enabled: bool,
) -> egui::Response {
    let label = format!("{label}  (default: {default})");
    let r = ui.add_enabled(enabled, slider.text(label));
    let t = slider_mark_fraction(default, min, max, logarithmic);
    let thickness = ui
        .text_style_height(&egui::TextStyle::Body)
        .max(ui.spacing().interact_size.y);
    let radius = thickness / 2.5;
    let width = ui.spacing().slider_width;
    let x = r.rect.left() + radius + (width - 2. * radius) * t;
    let y = r.rect.center().y + thickness * 0.5 - 1.;
    ui.painter().line_segment(
        [egui::pos2(x, y - 4.), egui::pos2(x, y + 2.)],
        egui::Stroke::new(2.0f32, egui::Color32::from_rgb(255, 204, 96)),
    );
    r
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuPage {
    Closed,
    Main,
    Modules,
    Settings,
    Teleports,
    WorldMap,
}
impl MenuPage {
    fn escape(self) -> Self {
        match self {
            Self::Closed => Self::Main,
            Self::Main => Self::Closed,
            Self::Modules | Self::Settings | Self::Teleports => Self::Main,
            Self::WorldMap => Self::Closed,
        }
    }
    fn is_open(self) -> bool {
        self != Self::Closed
    }
}
/// The terrain the recovered rules select: the camera block window (`0x77ecf0`)
/// and the level of every block inside it (`0x77e360`). Comparing it with the
/// mesh being drawn is what decides whether that mesh is stale.
#[derive(Clone, PartialEq)]
struct LodState {
    window: crate::terrain_lod::Window,
    levels: Vec<u8>,
}
struct MeshData {
    terrain_upload: Option<crate::terrain_gpu::TerrainUpload>,
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    atlas: Vec<u8>,
    grass_mips: Vec<crate::grass_mips::Tile>,
    grass: crate::grass_instances::Data,
    center: (isize, isize),
    tree_center: (f32, f32),
    detail: DetailSettings,
    lod: LodState,
    /// Resident terrain blocks and the index range each occupies.
    spans: Vec<crate::terrain_residency::Span>,
    terrain_end: u32,
    object_start: u32,
    water_start: u32,
    water_batches: Vec<WaterBatch>,
    local_pages: Vec<crate::world_objects::Page>,
}
/// Which view's indirect terrain records the next submit draws. The records
/// live in one buffer, so every pass re-queues the view it draws through: the
/// main matrix (also used by both refraction passes), the water reflection's
/// mirrored matrix, the minimap's orthographic window, or the whole world for
/// the full-range map, which is meant to show everything.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewRuns {
    Full,
    Main,
    Reflected,
    Map,
}
#[derive(Clone, Debug)]
struct WaterBatch {
    // Do not merge different authored area classes.
    simple: bool,
    start: u32,
    end: u32,
    height: f32,
    bounds: [f32; 4], // min x, max x, min z, max z
}
impl WaterBatch {
    // Conservative homogeneous clip-space rejection for the rectangular bounds.
    fn visible(&self, matrix: Mat4) -> bool {
        let [x0, x1, z0, z1] = self.bounds;
        let corners = [
            Vec3::new(x0, self.height, z0),
            Vec3::new(x0, self.height, z1),
            Vec3::new(x1, self.height, z0),
            Vec3::new(x1, self.height, z1),
        ]
        .map(|p| matrix * p.extend(1.));
        !(corners.iter().all(|p| p.x < -p.w)
            || corners.iter().all(|p| p.x > p.w)
            || corners.iter().all(|p| p.y < -p.w)
            || corners.iter().all(|p| p.y > p.w)
            || corners.iter().all(|p| p.z < 0.)
            || corners.iter().all(|p| p.z > p.w))
    }
    fn distance_squared(&self, p: Vec3) -> f32 {
        let [x0, x1, z0, z1] = self.bounds;
        let x = p.x.clamp(x0, x1);
        let z = p.z.clamp(z0, z1);
        (p - Vec3::new(x, self.height, z)).length_squared()
    }
}
fn basis(v: [f32; 3]) -> Vec3 {
    let v = Vec3::from_array(v);
    if v.length_squared() < 1e-8 {
        Vec3::Y
    } else {
        v.normalize()
    }
}
fn push_quad(v: &mut Vec<Vertex>, i: &mut Vec<u32>, points: [[f32; 3]; 4], color: [f32; 4]) {
    let n = v.len() as u32;
    for p in points {
        v.push(Vertex {
            pos: p,
            normal: [0., 1., 0.],
            uv: [0., 0.],
            color,
            kind: 1.,
            diffuse: 0xffff_ffff,
            night_diffuse: 0xffff_ffff,
        })
    }
    i.extend_from_slice(&[n, n + 1, n + 2, n + 1, n + 3, n + 2]);
}
/// Recovered block window and per-block levels (`src/terrain_lod.rs`). The client
/// re-evaluates `0x77e360` for every resident block on every frame, so the viewer
/// re-runs the same rule per frame and rebuilds geometry only when the answer
/// changes; the window origin moves with the camera's block (`0x77ecf0`).
/// The window rule and its level table inputs, shared by both entry points.
fn lod_setup(
    width: usize,
    height: usize,
    spacing: f32,
    cam: [f32; 2],
    radius: isize,
) -> (
    crate::terrain_lod::Window,
    crate::terrain_lod::LodConfig,
    usize,
    usize,
) {
    let cols = width / TILE;
    let rows = height / TILE;
    let block_world = TILE as f32 * spacing;
    let gx = cam[0] / spacing + width as f32 * 0.5;
    let gy = height as f32 * 0.5 - cam[1] / spacing;
    let lod = crate::terrain_lod::LodConfig {
        block_size: block_world,
        ..Default::default()
    };
    // `world+0x1bc`: draw range in world units, one slider step = one block.
    let range = radius.max(1) as f32 * block_world;
    let win = crate::terrain_lod::window(
        [gx / TILE as f32, gy / TILE as f32],
        [cols, rows],
        range,
        block_world,
    );
    (win, lod, cols, rows)
}
/// Viewer world axis: x = (cell - width/2) * spacing, z = (height/2 - cell) * spacing.
fn lod_centre(width: usize, height: usize, spacing: f32) -> impl Fn(usize, usize) -> [f32; 2] {
    move |x, y| {
        let cx = (x * TILE) as f32 + TILE as f32 * 0.5;
        let cy = (y * TILE) as f32 + TILE as f32 * 0.5;
        [
            (cx - width as f32 * 0.5) * spacing,
            (height as f32 * 0.5 - cy) * spacing,
        ]
    }
}
fn lod_window(
    width: usize,
    height: usize,
    spacing: f32,
    cam: [f32; 2],
    radius: isize,
) -> (crate::terrain_lod::Window, crate::terrain_lod::WindowLod) {
    let (win, lod, cols, rows) = lod_setup(width, height, spacing, cam, radius);
    let levels = crate::terrain_lod::window_lod(
        win,
        &lod,
        cam,
        lod_centre(width, height, spacing),
        |x, y| x < cols && y < rows,
    );
    (win, levels)
}
/// The level grid only.
///
/// The per-frame "would the drawn mesh differ?" test needs the levels and
/// nothing else, so it skips the neighbour walk that stitches the edge cases.
fn lod_levels(
    width: usize,
    height: usize,
    spacing: f32,
    cam: [f32; 2],
    radius: isize,
) -> (crate::terrain_lod::Window, Vec<u8>) {
    let (win, lod, cols, rows) = lod_setup(width, height, spacing, cam, radius);
    let levels = crate::terrain_lod::window_levels(
        win,
        &lod,
        cam,
        lod_centre(width, height, spacing),
        |x, y| x < cols && y < rows,
    );
    (win, levels)
}
fn build(
    t: &mut Terrain,
    cx: f32,
    cz: f32,
    detail: DetailSettings,
    residency: &mut crate::terrain_residency::Residency,
    vegetation_authored_mips: bool,
) -> Result<MeshData, String> {
    if vegetation_authored_mips {
        static WARN: std::sync::Once = std::sync::Once::new();
        WARN.call_once(|| {
            let request = crate::vegetation_material::Request {
                mips: crate::vegetation_material::RequestedMips::Authored,
                anisotropy: crate::vegetation_material::LEGACY_ANISOTROPY,
                cutout_reference: Some(crate::vegetation_material::LEGACY_GRASS_ALPHA_REF),
            };
            if let Err(rejection) = crate::vegetation_material::validate(
                crate::vegetation_material::Material::Grass,
                request,
            ) {
                eprintln!(
                    "P4.07 grass authored-mip experiment: unsupported native pair kept explicit: {rejection:?}; the shared world sampler still uses the approved 16x anisotropy"
                );
            }
        });
    }
    let rad = detail.radius;
    let gx = cx / t.grid_spacing + t.width as f32 * 0.5;
    let gy = t.height as f32 * 0.5 - cz / t.grid_spacing;
    // Camera block, used as the rebuild anchor compared against `self.center`.
    let bx = (gx / TILE as f32).floor() as isize;
    let by = (gy / TILE as f32).floor() as isize;
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut atlas = vec![0u8; ATLAS * ATLAS * 4];
    // Recovered block residency, resolution levels and block-edge rule
    // (src/terrain_lod.rs). The client keeps a square window of blocks around
    // the camera's block (`0x77ecf0`), gives each one of three meshes by camera
    // distance (`0x77e360`) and closes block boundaries with the coarser
    // neighbour's edge stride (`0x7c40e6`). This is the near mesh; the persistent
    // whole-world mesh below the window keeps the rest of the world visible.
    if t.grid != TILE || TILE != crate::terrain_lod::CELLS {
        return Err(format!(
            "T2BK blocks are {} cells but the recovered block/LOD rule assumes {} \
             (elementclient 0x7e5f70); refusing to draw substitute geometry",
            t.grid,
            crate::terrain_lod::CELLS
        ));
    }
    if t.width % TILE != 0 || t.height % TILE != 0 {
        return Err(format!(
            "T2 terrain grid {}x{} is not a whole number of {TILE}-cell blocks",
            t.width, t.height
        ));
    }
    let (win, mut levels) = lod_window(t.width, t.height, t.grid_spacing, [cx, cz], rad);
    if !detail.terrain_cutoff {
        levels.stitch_far_boundary();
    }
    let resident_bounds = detail.terrain_cutoff.then(|| {
        crate::terrain_cutoff::world_bounds(
            [win.x0, win.y0, win.x1, win.y1],
            TILE,
            t.width,
            t.height,
            t.grid_spacing,
        )
    });
    let lod_state = LodState {
        window: win,
        levels: levels.levels.clone(),
    };
    // The loader stores `grid` cells per block and drops each block's last row
    // and column (the neighbour re-supplies them), so a block's offset-`TILE`
    // vertex clamps to the last stored cell at the world border.
    let sample = |cell_x: usize, cell_y: usize| {
        let x = cell_x.min(t.width - 1);
        let y = cell_y.min(t.height - 1);
        (x, y, y * t.width + x)
    };
    // `0x77e250`: the resident set follows the window. Blocks that enter are
    // meshed at the level `0x77e360` picked, blocks whose level changed are
    // re-meshed, and blocks the window leaves release their geometry.
    let mut terrain_reader = t.heights.reader();
    let census = residency.update_stitched(
        &win,
        |x, y| {
            (
                levels.level(x, y),
                levels.cases[(y - win.y0) * (win.x1 - win.x0) + x - win.x0],
            )
        },
        |x, y, level| {
            let cases = levels.cases[(y - win.y0) * (win.x1 - win.x0) + (x - win.x0)];
            let surface = crate::terrain_lod::block_surface(level, cases);
            let mut block_vertices = Vec::with_capacity(surface.grid.len());
            let mut low = [f32::INFINITY; 3];
            let mut high = [f32::NEG_INFINITY; 3];
            for g in &surface.grid {
                let (cell_x, cell_y, at) =
                    sample(x * TILE + g[0] as usize, y * TILE + g[1] as usize);
                let (height, n) = terrain_reader.cell(at)?;
                let pos = [
                    (cell_x as f32 - t.width as f32 * 0.5) * t.grid_spacing,
                    height,
                    (t.height as f32 * 0.5 - cell_y as f32) * t.grid_spacing,
                ];
                for axis in 0..3 {
                    low[axis] = low[axis].min(pos[axis]);
                    high[axis] = high[axis].max(pos[axis]);
                }
                block_vertices.push(Vertex {
                    pos,
                    normal: [n[0] as f32 / 127., n[1] as f32 / 127., n[2] as f32 / 127.],
                    uv: [0.; 2],
                    color: [1.; 4],
                    kind: 0.,
                    diffuse: 0xffff_ffff,
                    night_diffuse: 0xffff_ffff,
                });
            }
            Ok((
                block_vertices,
                surface.indices,
                [low[0], low[1], low[2], high[0], high[1], high[2]],
            ))
        },
    )?;
    let spans = residency.append(&win, &mut vertices, &mut indices);
    eprintln!(
        "Terrain residency: {} blocks resident ({} entered, {} evicted, {} re-meshed), \
         {} triangles; {} blocks submitted",
        census.resident,
        census.loaded,
        census.evicted,
        census.remeshed,
        census.triangles,
        spans.len()
    );
    let terrain_end = indices.len() as u32;
    // Trees are now entirely GPU-instanced at three runtime-generated LODs.
    let mut atlas_allocator = crate::texture_atlas::Atlas::new();
    // Client MOXB mesh and bitmask-backed placements. Transforms and streaming
    // radius are provisional until the game's randomization and fade are ported.
    let mut grass_builder = crate::grass_instances::Builder::default();
    let mut grass_sample = None;
    let mut grass_slots: HashMap<(String, u8), crate::texture_atlas::Slot> = HashMap::new();
    let mut grass_authored: HashMap<(String, u8), Vec<image::RgbaImage>> = HashMap::new();
    let mut grass_mips = Vec::new();
    // Reserve at least one 128px slot for every distinct nearby material, so
    // a few large textures cannot starve all later grass types.
    let mut grass_pending = HashSet::new();
    for area in &t.grass.areas {
        const RANGE: f32 = 200.0;
        if (area.center[0] - cx).abs() > area.width as f32 * area.grid * 0.5 + RANGE
            || (area.center[2] - cz).abs() > area.height as f32 * area.grid * 0.5 + RANGE
        {
            continue;
        }
        if let Some(Some(model)) = t.grass.models.get(area.type_id) {
            // CELGrassType initialization overrides the serialized value.
            let alpha = 84u8;
            for mesh in &model.meshes {
                if t.grass.textures.contains_key(&mesh.texture) {
                    grass_pending.insert((mesh.texture.clone(), alpha));
                }
            }
        }
    }
    // `0x653a30`: the resident grass grid is rebuilt at
    // `step = 5 - ftol(cam[+0x678]*4 + 0.5)` cells and the native early-out
    // drops every block when the budget yields 5. `cam[+0x678]` is the *first
    // lane of a colour triple*: `0x41ed99`–`0x41ee16` registers three adjacent
    // times the same authored scalar (`[env+0xa0]`, `[env+0x94]`, `[env+0x98]`)
    // into `+0x66c/+0x678/+0x684` from one lerped `[0,1]` factor, and
    // `0x41f5f1`–`0x41f661` advances each with `scalar = target` when the factor
    // is zero or `scalar -= delta*factor` otherwise. `0x7c7fc4` allocates it as
    // an 8-byte-stride array and `0x4211b9` blends it with neighbour colours, so
    // it is a light/diffuse field, not a distance. The viewer has no scene
    // colour field yet, so density keeps native full resolution (step 1) rather
    // than inventing a depth; `density_step` stays the single place that will
    // consume the recovered value.
    let grass_step = crate::grass_wind::density_step(None);
    for area in &t.grass.areas {
        let half_x = area.width as f32 * area.grid * 0.5;
        let half_z = area.height as f32 * area.grid * 0.5;
        // A recovered step of 5 is the native "no grass" early-out.
        let Some(step) = grass_step else {
            break;
        };
        // `0x653a30` only keeps cells whose mask bit is set; distances use the
        // client's own block extent rather than an invented 200-unit margin.
        if (area.center[0] - cx).abs() > half_x + t.scene.block_size
            || (area.center[2] - cz).abs() > half_z + t.scene.block_size
        {
            continue;
        }
        let Some(Some(model)) = t.grass.models.get(area.type_id) else {
            continue;
        };
        let typ = &t.grass.types[area.type_id];
        for (mesh_index, mesh) in model.meshes.iter().enumerate() {
            let Some(img) = t.grass.textures.get(&mesh.texture) else {
                continue;
            };
            let alpha_ref = 84u8;
            let material_key = (mesh.texture.clone(), alpha_ref);
            let slot = if let Some(&slot) = grass_slots.get(&material_key) {
                slot
            } else {
                let mut authored = if vegetation_authored_mips {
                    t.grass
                        .texture_mips
                        .get(&mesh.texture)
                        .and_then(|mips| {
                            if mips.len() > 7 {
                                eprintln!(
                                    "P4.07 grass {}: {} authored tail mip(s) beyond atlas LOD 7 retained in cache but not separately uploadable",
                                    mesh.texture,
                                    mips.len() - 7
                                );
                            }
                            crate::grass_mips::authored_chain(
                                mips,
                                img.width(),
                                img.height(),
                                7,
                            )
                            .ok()
                        })
                } else {
                    None
                };
                grass_pending.remove(&material_key);
                let reserve = grass_pending.len().min((ATLAS / TEX).pow(2) - 1);
                // Exact authored chains reserve a padded power-of-two rectangle
                // but keep the source pixels at native size in its upper-left
                // corner. This supports rectangular and non-power-of-two DDS
                // chains without resampling either level 0 or the lower mips.
                let authored_slot = authored.as_ref().and_then(|_| {
                    let w = img.width() as usize;
                    let h = img.height() as usize;
                    (w <= ATLAS && h <= ATLAS)
                        .then(|| atlas_allocator.allocate(w, h, reserve, false))
                        .flatten()
                });
                if authored.is_some() && authored_slot.is_none() {
                    eprintln!(
                        "P4.07 grass {}: native-size authored rectangle does not fit; using the approved generated-mip fallback",
                        mesh.texture
                    );
                    authored = None;
                }
                let slot = if let Some(slot) = authored_slot {
                    slot
                } else {
                    let n = (img.width().max(img.height()) as usize)
                        .next_power_of_two()
                        .clamp(TEX, ATLAS);
                    let Some(slot) = atlas_allocator.allocate(n, n, reserve, true) else {
                        eprintln!("Grass atlas full: {}", mesh.texture);
                        continue;
                    };
                    slot
                };
                let ox = slot.x;
                let oy = slot.y;
                if authored.is_some() {
                    let source = img.as_raw();
                    for y in 0..img.height() as usize {
                        let dst = ((oy + y) * ATLAS + ox) * 4;
                        let src = y * img.width() as usize * 4;
                        atlas[dst..dst + img.width() as usize * 4]
                            .copy_from_slice(&source[src..src + img.width() as usize * 4]);
                    }
                } else {
                    for py in 0..slot.h {
                        for px in 0..slot.w {
                            let x = (px as u32 * img.width() / slot.w as u32).min(img.width() - 1);
                            let y =
                                (py as u32 * img.height() / slot.h as u32).min(img.height() - 1);
                            let o = ((oy + py) * ATLAS + ox + px) * 4;
                            atlas[o..o + 4].copy_from_slice(&img.get_pixel(x, y).0);
                        }
                    }
                }
                if let Some(chain) = authored {
                    if !chain.is_empty() {
                        grass_authored.insert(material_key.clone(), chain.clone());
                    }
                    grass_mips.push((slot, alpha_ref, Some(chain)));
                } else {
                    grass_mips.push((slot, alpha_ref, None));
                }
                grass_slots.insert(material_key.clone(), slot);
                slot
            };
            let ox = slot.x as f32 / ATLAS as f32;
            let oy = slot.y as f32 / ATLAS as f32;
            let (scale_x, scale_y) = if grass_authored.contains_key(&material_key) {
                (
                    img.width() as f32 / ATLAS as f32,
                    img.height() as f32 / ATLAS as f32,
                )
            } else {
                (slot.w as f32 / ATLAS as f32, slot.h as f32 / ATLAS as f32)
            };
            let alpha = 84. / 255.;
            let key = (area.type_id, mesh_index);
            if !grass_builder.contains(key) {
                let (ymin, ymax) = model
                    .meshes
                    .iter()
                    .flat_map(|m| m.vertices.iter())
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                        (lo.min(v.pos[1]), hi.max(v.pos[1]))
                    });
                let height = (ymax - ymin).max(0.00001);
                let mesh_vertices = mesh
                    .vertices
                    .iter()
                    .map(|v| Vertex {
                        pos: v.pos,
                        normal: v.normal,
                        uv: [ox + v.uv[0] * scale_x, oy + v.uv[1] * scale_y],
                        color: [ox, oy, scale_x, alpha],
                        kind: 3.75,
                        diffuse: if typ.sight_range.is_finite() && typ.sight_range > 0. {
                            typ.sight_range.to_bits()
                        } else {
                            0
                        },
                        night_diffuse: ((v.pos[1] / height).powi(2) * typ.softness.max(0.))
                            .to_bits(),
                    })
                    .collect();
                grass_builder.mesh_with_scale_y(key, mesh_vertices, mesh.indices.clone(), scale_y);
            }
            for placement in area.placements(typ)? {
                let x = area.center[0] - half_x
                    + (placement.col as f32 + placement.jitter_x) * area.grid;
                let z = area.center[2] + half_z - placement.row as f32 * area.grid
                    + placement.jitter_z * area.grid;
                // `0x653bd0`/`0x653c10`: the block walk visits every
                // `step`-th cell on both axes, so anything off-stride is not a
                // resident placement in the client.
                if i64::from(placement.col) % i64::from(step) != 0
                    || i64::from(placement.row) % i64::from(step) != 0
                {
                    continue;
                }
                let reach = half_x.max(half_z) + t.scene.block_size;
                if (x - cx) * (x - cx) + (z - cz) * (z - cz) > reach * reach {
                    continue;
                }
                if !crate::terrain_cutoff::contains(resident_bounds, [x, 0., z]) {
                    continue;
                }
                let gx = x / t.grid_spacing + t.width as f32 * 0.5;
                let gy = t.height as f32 * 0.5 - z / t.grid_spacing;
                if gx < 0. || gy < 0. || gx >= (t.width - 1) as f32 || gy >= (t.height - 1) as f32 {
                    continue;
                }
                let ix = gx as usize;
                let iy = gy as usize;
                let fx = gx - ix as f32;
                let fy = gy - iy as f32;
                let a = terrain_reader.height(iy * t.width + ix);
                let b = terrain_reader.height(iy * t.width + ix + 1);
                let c = terrain_reader.height((iy + 1) * t.width + ix);
                let d = terrain_reader.height((iy + 1) * t.width + ix + 1);
                let ground = if fx + fy <= 1. {
                    a + (b - a) * fx + (c - a) * fy
                } else {
                    d + (c - d) * (1. - fx) + (b - d) * (1. - fy)
                };
                if grass_sample.is_none() {
                    grass_sample = Some([x, ground, z]);
                }
                // `0x653e93`-`0x653ec1`: the emitter stores
                // `ftol(2*|y - 10000| + rand(0,10)) % 80` as the placement's
                // wind-history slot, from that placement's own height and its
                // RANMAR selector. The retail `grass_wave` vertex shader then
                // reads `c[10 + v1.x]`, so the slot is per placement and only
                // the scale `v1.y` varies per vertex.
                // The height the scope has is `ground`, the same value the
                // instance is placed at (`ground - 0.02`), so the CPU-computed
                // slot matches the `y` the client hashed at `0x653e93`.
                let wind_index =
                    crate::grass_wind::history_index(ground, placement.wind_selector) as f32;
                grass_builder.instance_for_area(
                    key,
                    [x, ground - 0.02, z],
                    placement.scale,
                    placement.yaw,
                    wind_index,
                    area.center,
                );
            }
        }
    }
    let grass = grass_builder.finish();
    if grass_debug() {
        let (v, i, b) = grass.counts();
        eprintln!("Grass instancing: {v} mesh vertices, {i} placements, {b} batches; first position {grass_sample:?}");
    }
    // Recovered regional residency (0x424020): one placement per native
    // CECSceneBlock class, per-class load distances and half-block hysteresis.
    // Only the ornament class maps to Objects::load_ids indices; the other
    // nine classes use disjoint synthetic indices for the resident-bit set.
    let object_start = indices.len() as u32;
    let scenery_placements = ten_class_placements(t);
    let scenery_margin = 0.5 * t.scene.block_size.max(0.);
    let nearby = t.scenery_residency.select(
        &scenery_placements,
        [cx, cz],
        detail.scenery_quality,
        scenery_margin,
    );
    let ornament_count = t.objects.ornaments.len();
    let ornament_ids: Vec<usize> = nearby
        .iter()
        .copied()
        .filter(|&i| {
            i < ornament_count
                && crate::terrain_cutoff::contains(resident_bounds, t.objects.ornaments[i].position)
        })
        .collect();
    t.objects.load_ids(&ornament_ids)?;
    let tree_lod_scale = lod_scale(detail.scenery_quality, Class::Trees);
    eprintln!(
        "Scenery residency: quality {} ({} native classes, tree LOD {:.3}, ornament distance {:.1}), {} resident, {} ornaments retained, {} loads, {} evictions, {} distance culls",
        detail.scenery_quality,
        Class::ALL.len(),
        tree_lod_scale,
        crate::scenery_visibility::ornament_distance(detail.scenery_quality),
        nearby.len(),
        ornament_ids.len(),
        t.scenery_residency.loads,
        t.scenery_residency.evictions,
        t.scenery_residency.distance_culls
    );
    // Sort largest rectangles first, then use as many atlas pages as needed.
    // Unlike the legacy 128px atlas, local BMDs keep their source dimensions.
    let mut requested: HashMap<String, (usize, usize)> = HashMap::new();
    for &id in &ornament_ids {
        if let Some(model) = t.objects.get(id) {
            for mesh in &model.meshes {
                let key = mesh.texture.to_lowercase();
                // A reference the client resolves nowhere is an explicit
                // registered row (texture_resolution), not a skipped mesh: give
                // it a 1x1 white atlas slot and draw the actual geometry.
                let dims = t
                    .objects
                    .texture(&key)
                    .map(|tex| (tex.width, tex.height))
                    .unwrap_or((1, 1));
                requested.insert(key, dims);
            }
        }
    }
    let mut requested: Vec<_> = requested.into_iter().collect();
    requested.sort_by_key(|(_, (w, h))| {
        std::cmp::Reverse(w.next_power_of_two().max(TEX) * h.next_power_of_two().max(TEX))
    });
    let mut texture_slots: HashMap<String, (usize, crate::texture_atlas::Slot)> = HashMap::new();
    let mut pages: Vec<(crate::texture_atlas::Atlas, crate::world_objects::Page)> = Vec::new();
    for (key, (w, h)) in &requested {
        let mut chosen = None;
        for (i, (packing, _)) in pages.iter_mut().enumerate() {
            if let Some(slot) = packing.allocate(*w, *h, 0, false) {
                chosen = Some((i, slot));
                break;
            }
        }
        let (i, slot) = if let Some(s) = chosen {
            s
        } else {
            let mut packing = crate::texture_atlas::Atlas::new();
            let slot = packing
                .allocate(*w, *h, 0, false)
                .ok_or("local scenery texture exceeds maximum atlas size")?;
            let i = pages.len();
            pages.push((
                packing,
                crate::world_objects::Page {
                    shared_atlas: None,
                    vertices: Vec::new(),
                    indices: Vec::new(),
                    atlas: vec![0; ATLAS * ATLAS * 4],
                    processed: 0,
                    used_height: 0,
                    mips: Vec::new(),
                    placements: Vec::new(),
                },
            ));
            (i, slot)
        };
        pages[i].1.used_height = pages[i].1.used_height.max(slot.y + slot.h);
        let tex = t.objects.texture(key);
        for py in 0..slot.h {
            for px in 0..slot.w {
                let sample = if let Some(tex) = tex {
                    tex.sample_rgba(
                        (px as f32 + 0.5) / slot.w as f32,
                        (py as f32 + 0.5) / slot.h as f32,
                    )
                } else {
                    [255, 255, 255, 255]
                };
                let o = ((slot.y + py) * ATLAS + slot.x + px) * 4;
                pages[i].1.atlas[o..o + 4].copy_from_slice(&sample);
            }
        }
        texture_slots.insert(key.clone(), (i, slot));
    }
    eprintln!(
        "Local scenery: {} source-resolution textures in {} GPU pages",
        requested.len(),
        pages.len()
    );
    for id in ornament_ids {
        let Some(model) = t.objects.get(id) else {
            continue;
        };
        // EC-BSD +36 flags, recovered at 0x489f0b/0x489f12. Keep them on the
        // page's index ranges; a hidden placement must not be submitted in a
        // secondary view just because the page itself is resident.
        let visibility = Visibility::from_extra(t.objects.ornaments[id].extra);
        let z = basis(model.direction);
        let y = basis(model.up);
        let x = y.cross(z).normalize_or_zero();
        for mesh in &model.meshes {
            let key = mesh.texture.to_lowercase();
            let Some((page_id, slot)) = texture_slots.get(&key).copied() else {
                continue;
            };
            // v5 stores the client's no-cull flag in the material; the draw
            // state (stage, alpha test, culling, kind) comes from one place.
            let mut kind = match crate::bmd_baked::classify(
                mesh.version,
                !mesh.texture.trim().is_empty(),
                mesh.material.as_ref().is_some_and(|m| m.two_sided),
            ) {
                Ok(state) => state.kind,
                Err(e) => {
                    // Explicit error, not a silently dropped mesh: the BMD
                    // parser names every unrecovered version before this point.
                    return Err(format!("unsupported baked mesh {}: {e}", mesh.name));
                }
            };
            if crate::objects::SdrUvAnim::from_texture_path(&mesh.texture)
                .is_some_and(|a| a.tc_scroll[1] < 0.0)
            {
                kind += 0.02;
            }
            let page = &mut pages[page_id].1;
            let base = page.vertices.len() as u32;
            let range_start = page.indices.len() as u32;
            let ox = slot.x as f32 / ATLAS as f32;
            let oy = slot.y as f32 / ATLAS as f32;
            let sx = slot.w as f32 / ATLAS as f32;
            let sy = slot.h as f32 / ATLAS as f32;
            for vert in &mesh.vertices {
                let p = Vec3::from_array(model.position)
                    + x * (vert.position[0] * model.scale[0])
                    + y * (vert.position[1] * model.scale[1])
                    + z * (vert.position[2] * model.scale[2]);
                let n = (x * vert.normal[0] + y * vert.normal[1] + z * vert.normal[2])
                    .normalize_or_zero();
                page.vertices.push(Vertex {
                    pos: p.to_array(),
                    normal: n.to_array(),
                    uv: [ox + vert.uv[0] * sx, oy + vert.uv[1] * sy],
                    color: [ox, oy, sx, sy],
                    kind,
                    diffuse: vert.day_diffuse,
                    night_diffuse: vert.night_diffuse,
                });
            }
            page.indices
                .extend(mesh.indices.iter().map(|n| base + *n as u32));
            let range_end = page.indices.len() as u32;
            if range_end > range_start {
                page.placements.push(crate::world_objects::PlacementRange {
                    start: range_start,
                    end: range_end,
                    export_id: t.objects.ornaments[id].export_id,
                    order: 0,
                    visibility,
                });
            }
        }
    }
    let local_pages = pages
        .into_iter()
        .map(|(_, page)| page.finish())
        .filter(|p| !p.indices.is_empty())
        .collect();
    // Keep opaque indices separate so water can sample reflected/refracted scene passes.
    let water_start = indices.len() as u32;
    let mut water_batches: Vec<WaterBatch> = Vec::new();
    let mut areas: Vec<_> = t.water.iter().collect();
    areas.sort_by(|a, b| a.height.total_cmp(&b.height));
    // The water records had no channel of their own: `SceneWater` and
    // `RenderWater` were declared in the Logging tab and emitted nowhere, so a
    // report could not say whether the map carries a water surface where the
    // field saw one. `A3DWaterArea` is a horizontal grid at one `height`
    // (`src/water.rs`), so a fall cannot be one of these: an empty list around
    // the fall proves the surface is the static-mesh water of
    // A3DTerrainWater.cpp:67-84, which this loader does not read yet.
    for a in &areas {
        let cells = a.mask.iter().filter(|&&m| m != 0).count();
        crate::diag!(
            RenderWater,
            "export={} colour=0x{:08x} simple={} height={:.3} grid={:.3} cells={}x{} \
             wet={} of {} centre=({:.2},{:.2},{:.2}) shore_waves={}",
            a.export_id,
            a.color,
            a.simple,
            a.height,
            a.grid_size,
            a.columns,
            a.rows,
            cells,
            a.mask.len(),
            a.center[0],
            a.center[1],
            a.center[2],
            a.shore_waves.len()
        );
    }
    for a in areas {
        let start = indices.len() as u32;
        let mut bounds = [
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let hx = a.columns as f32 * a.grid_size * 0.5;
        let hz = a.rows as f32 * a.grid_size * 0.5;
        let rgb = [
            ((a.color >> 16) & 255) as f32 / 255.,
            ((a.color >> 8) & 255) as f32 / 255.,
            (a.color & 255) as f32 / 255.,
            1.,
        ];
        let [col0, row0, col1, row1] = crate::terrain_cutoff::cell_window(
            resident_bounds,
            [a.center[0] - hx, a.center[2] + hz],
            a.grid_size,
            a.columns,
            a.rows,
        );
        for row in row0..row1 {
            for col in col0..col1 {
                if a.mask[row * a.columns + col] & 1 == 0 {
                    continue;
                }
                let x = a.center[0] - hx + col as f32 * a.grid_size;
                let z = a.center[2] + hz - row as f32 * a.grid_size;
                let s = a.grid_size;
                let [left, right, bottom, top] =
                    resident_bounds.map_or([x, x + s, z - s, z], |b| {
                        [
                            x.max(b[0]),
                            (x + s).min(b[1]),
                            (z - s).max(b[2]),
                            z.min(b[3]),
                        ]
                    });
                if left >= right || bottom >= top {
                    continue;
                }
                bounds[0] = bounds[0].min(left);
                bounds[1] = bounds[1].max(right);
                bounds[2] = bounds[2].min(bottom);
                bounds[3] = bounds[3].max(top);
                let first = vertices.len();
                push_quad(
                    &mut vertices,
                    &mut indices,
                    [
                        [left, a.height, top],
                        [right, a.height, top],
                        [left, a.height, bottom],
                        [right, a.height, bottom],
                    ],
                    rgb,
                );
                // Retail mesh adds an integer per-cell base, then spans one
                // UV unit within each cell; integer bases wrap identically.
                let clipped = left != x || right != x + s || bottom != z - s || top != z;
                for (vertex, uv) in vertices[first..]
                    .iter_mut()
                    .zip(crate::water_gpu::cell_base_uv())
                {
                    vertex.uv = if clipped {
                        [(vertex.pos[0] - x) / s, (z - vertex.pos[2]) / s]
                    } else {
                        uv
                    };
                }
            }
        }
        let end = indices.len() as u32;
        if end > start {
            if let Some(last) = water_batches.last_mut().filter(|last| {
                last.height == a.height && last.simple == a.simple && last.end == start
            }) {
                last.end = end;
                last.bounds = [
                    last.bounds[0].min(bounds[0]),
                    last.bounds[1].max(bounds[1]),
                    last.bounds[2].min(bounds[2]),
                    last.bounds[3].max(bounds[3]),
                ];
            } else {
                water_batches.push(WaterBatch {
                    simple: a.simple,
                    start,
                    end,
                    height: a.height,
                    bounds,
                });
            }
        }
    }
    // Masks follow the same resident geometry, with one area of border safety.
    // Only rebuild atlas bindings when that footprint (or cutoff mode) changes.
    let mask_window = detail.terrain_cutoff.then(|| {
        crate::terrain_cutoff::area_window(
            [win.x0, win.y0, win.x1, win.y1],
            TILE,
            t.materials.mask_grid,
            t.width / t.materials.mask_grid,
            t.height / t.materials.mask_grid,
        )
    });
    let terrain_upload = if t.terrain_material_window != Some(mask_window) {
        let materials = if t.terrain_material_window.is_none() {
            t.materials.take_window(mask_window)
        } else {
            let dir = t.element.join("maps/world");
            let trn = std::fs::read(dir.join("world.trn2")).map_err(|e| e.to_string())?;
            crate::material::Materials::load_window(
                &dir,
                &trn,
                &t.subterrain_names,
                t.width,
                t.height,
                t.grid_spacing,
                mask_window,
                false,
            )?
        };
        let far = if detail.terrain_cutoff {
            None
        } else {
            Some(crate::terrain_gpu::world_mesh(t, 4)?)
        };
        t.terrain_material_window = Some(mask_window);
        Some(crate::terrain_gpu::TerrainUpload { materials, far })
    } else {
        None
    };
    Ok(MeshData {
        terrain_upload,
        vertices,
        indices,
        grass_mips: crate::grass_mips::prepare_with_authored(&atlas, &grass_mips, ATLAS),
        grass,
        atlas,
        center: (bx, by),
        tree_center: (cx, cz),
        detail,
        lod: lod_state,
        spans,
        terrain_end,
        object_start,
        water_start,
        water_batches,
        local_pages,
    })
}
const SHADER: &str = r#"
struct Camera { matrix: mat4x4<f32>, direction: vec4<f32>, clip: vec4<f32>, bounds:vec4<f32>, tree_bounds:vec4<f32>, sun:vec4<f32>, light:vec4<f32>, region_ambient:vec4<f32>, region_diffuse:vec4<f32>, region_direction:vec4<f32>, grass_eye:vec4<f32>, grass_wind:vec4<f32>, grass_history:array<vec4<f32>,80>, fog_range:vec4<f32>, fog_color:vec4<f32> }; @group(0) @binding(0) var<uniform> camera:Camera;
@group(0) @binding(1) var atlas:texture_2d<f32>; @group(0) @binding(2) var atlas_sampler:sampler;
fn fogged(linear:vec3<f32>, reciprocal_w:f32)->vec3<f32> {
 if camera.fog_range.z<0.5 {return linear;}
 let view_z=1./max(reciprocal_w,0.0000001);
 let span=camera.fog_range.y-camera.fog_range.x;
 let f=select(clamp((view_z-camera.fog_range.x)/select(span,0.001,abs(span)<0.001),0.,1.),select(0.,1.,view_z>=camera.fog_range.y),abs(span)<0.001);
 let c=max(linear,vec3<f32>(0.));let encoded=select(c*12.92,1.055*pow(c,vec3<f32>(1./2.4))-0.055,c>vec3<f32>(0.0031308));
 let v=mix(encoded,camera.fog_color.rgb,f);
 return select(v/12.92,pow((max(v,vec3<f32>(0.))+0.055)/1.055,vec3<f32>(2.4)),v>vec3<f32>(0.04045));
}
// Retail lm_refract_vs, c0=(1,water_height,0.075,0), 0x7a91dd.
// Unlike grass, above-plane BMD vertices have zero submerged depth.
fn bmd_refraction_transmittance(world:vec3<f32>)->f32 {
 let depth=max(camera.clip.x-world.y,0.);let delta=camera.grass_eye.xyz-world;
 if abs(delta.y)<=0.000001 {return select(1.,0.,depth>0.);}
 return exp2(-0.075*depth*length(delta)/delta.y);
}
fn bmd_pass_fog(linear:vec3<f32>,reciprocal_w:f32,transmittance:f32)->vec3<f32> {
 if camera.clip.y>=0. || camera.fog_range.z<0.5 {return fogged(linear,reciprocal_w);}
 let c=max(linear,vec3<f32>(0.));let encoded=select(c*12.92,1.055*pow(c,vec3<f32>(1./2.4))-0.055,c>vec3<f32>(0.0031308));
 let v=mix(camera.fog_color.rgb,encoded,clamp(transmittance,0.,1.));
 return select(v/12.92,pow((max(v,vec3<f32>(0.))+0.055)/1.055,vec3<f32>(2.4)),v>vec3<f32>(0.04045));
}

struct VIn { @location(0) pos:vec3<f32>, @location(1) normal:vec3<f32>, @location(2) uv:vec2<f32>, @location(3) color:vec4<f32>, @location(4) kind:f32, @location(5) diffuse:u32, @location(7) night_diffuse:u32 };
struct VOut { @builtin(position) pos:vec4<f32>, @location(9) bmd_refraction:f32, @location(0) normal:vec3<f32>, @location(1) uv:vec2<f32>, @location(2) color:vec4<f32>, @location(3) @interpolate(flat) kind:f32, @location(4) world:vec3<f32>, @location(5) diffuse:vec3<f32> };
// 0x7b9bc0 quantizes the night weight, mixes bytes with integer products,
// divides by 256 (not 255), and forces alpha to 255. Keep that arithmetic
// integer until the final vertex-color conversion; UNORM round trips can
// otherwise fall just below an exact integer at a floor boundary.
fn bmd_baked_rgb(day:u32, night:u32, night_factor:f32)->vec3<u32> {
 let w=u32(clamp(night_factor,0.,1.)*255.);
 let a=vec3<u32>((day>>16u)&255u,(day>>8u)&255u,day&255u);
 let b=vec3<u32>((night>>16u)&255u,(night>>8u)&255u,night&255u);
 return (a*(255u-w)+b*w)>>vec3<u32>(8u);
}
@vertex fn vs(i:VIn)->VOut { var o:VOut; var pos=i.pos; var norm=i.normal;
 if i.kind>4.5 { let d=normalize(camera.direction.xyz); let az=atan2(d.z,d.x); let el=asin(clamp(d.y,-1.,1.)); let rho=i.color.w; let q=vec3<f32>(cos(el)*i.normal.x-sin(el)*i.normal.z,i.normal.y,sin(el)*i.normal.x+cos(el)*i.normal.z); let a=az+rho;var v=vec3<f32>(cos(a)*q.x-sin(a)*q.y,sin(a)*q.x+cos(a)*q.y,q.z);if rho>1000. { v=i.normal; }pos+=vec3<f32>(v.x,v.z,v.y);norm=vec3<f32>(0.,1.,0.); }
 o.pos=camera.matrix*vec4<f32>(pos,1.);o.bmd_refraction=1.;if camera.clip.y<0. && i.kind>1.5 && i.kind<2.5 {o.bmd_refraction=bmd_refraction_transmittance(pos);}o.world=pos;o.normal=norm;o.uv=i.uv;o.color=i.color;o.kind=i.kind;o.diffuse=vec3<f32>(bmd_baked_rgb(i.diffuse,i.night_diffuse,1.-camera.direction.w))/255.;return o; }
@fragment fn fs(i:VOut, @builtin(front_facing) front:bool)->@location(0) vec4<f32> { if ((i.kind>1.05 && i.kind<1.15) || (i.kind>1.5 && i.kind<2.05)) && camera.clip.w == 0. && (front == (camera.clip.z != 0.)) {discard;} if camera.clip.y != 0.0 && (i.world.y-camera.clip.x)*camera.clip.y < 0.0 {discard;} if i.kind>0.5 && i.kind<1.05 {return vec4<f32>(fogged(i.color.rgb,i.pos.w),1.);} if i.kind>1.05 && i.kind<1.25 {return vec4<f32>(bmd_pass_fog(clamp(i.diffuse*2.,vec3<f32>(0.),vec3<f32>(1.)),i.pos.w,i.bmd_refraction),1.);} let uv_dx=dpdx(i.uv);let uv_dy=dpdy(i.uv);var uv=i.uv; if i.kind>1.5 {let scale=vec2<f32>(i.color.z,select(i.color.z,i.color.w,i.kind<2.5));let dims=vec2<f32>(textureDimensions(atlas));let mip=clamp(log2(max(max(length(uv_dx*dims),length(uv_dy*dims)),1.)),0.,7.);let border=0.5*exp2(mip);let half_pixel=min(vec2<f32>(border)/dims,scale*0.499);let sdr_v=select(0.,-0.5*camera.grass_wind.x,(i.kind>2.01 && i.kind<2.03)||(i.kind>2.11 && i.kind<2.13));let local=fract((i.uv-i.color.xy)/scale+vec2<f32>(0.,sdr_v))*scale;uv=i.color.xy+clamp(local,half_pixel,scale-half_pixel);} let raw_tex=textureSampleGrad(atlas,atlas_sampler,uv,uv_dx,uv_dy);let encoded=raw_tex.rgb;let linear=select(encoded/12.92,pow((encoded+0.055)/1.055,vec3<f32>(2.4)),encoded>vec3<f32>(0.04045));let tex=vec4<f32>(linear,raw_tex.a);if ((i.kind>1.5 && i.kind<2.5) || i.kind>3.5) && tex.a<select(0.5,i.color.w,i.kind>3.5 && i.kind<3.9) {discard;} if i.kind>1.5 && i.kind<2.5 {let c=clamp(raw_tex.rgb*i.diffuse*2.,vec3<f32>(0.),vec3<f32>(1.));let rgb=select(c/12.92,pow((c+0.055)/1.055,vec3<f32>(2.4)),c>vec3<f32>(0.04045));return vec4<f32>(bmd_pass_fog(rgb,i.pos.w,i.bmd_refraction),1.);} let light=camera.light.x+camera.light.y*max(dot(normalize(i.normal),normalize(camera.sun.xyz)),0.);return vec4<f32>(fogged(tex.rgb*light*camera.light.zww,i.pos.w),1.);}
"#;
struct Renderer {
    /// Optional add-ons; default-disabled and absent from headless previews.
    modules: crate::modules::Modules,
    module_format: wgpu::TextureFormat,
    models: Option<crate::world_models_gpu::World>,
    model_lights: std::sync::Arc<crate::scene_lights::SceneLights>,
    model_scene: std::sync::Arc<crate::scene::Scene>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    scenery_pipeline: wgpu::RenderPipeline,
    terrain_gpu: crate::terrain_gpu::TerrainGpu,
    sky: crate::sky_gpu::SkyGpu,
    weather: crate::weather_gpu::WeatherGpu,
    cloud: Option<crate::cloud_gpu::CloudGpu>,
    far_trees: crate::far_trees_gpu::FarTrees,
    grass: crate::grass_instances::Gpu,
    tree_center: (f32, f32),
    detail: DetailSettings,
    /// Window/levels the current terrain mesh was built for.
    lod: LodState,
    /// Resident terrain blocks (`0x77e250`) and the runs submitted last frame.
    spans: Vec<crate::terrain_residency::Span>,
    terrain_runs: Vec<(u32, u32)>,
    far_terrain_runs: Vec<(u32, u32)>,
    /// Visible runs for the water reflection's mirrored view. The mirror plane
    /// changes per water height, so this is rebuilt for each reflected submit.
    reflected_terrain_runs: Vec<(u32, u32)>,
    reflected_far_runs: Vec<(u32, u32)>,
    /// Visible runs for the realistic minimap, which is an orthographic
    /// window of `minimap_range` around the eye rather than the whole world.
    map_terrain_runs: Vec<(u32, u32)>,
    map_far_runs: Vec<(u32, u32)>,
    terrain_end: u32,
    world_width: usize,
    world_height: usize,
    world_spacing: f32,
    uniform: wgpu::Buffer,
    reflected_uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    reflected_bind: wgpu::BindGroup,
    water: crate::water_gpu::WaterGpu,
    simple_water: crate::simple_water_gpu::SimpleWaterGpu,
    shore: Option<crate::shore_gpu::ShoreGpu>,
    post: crate::post_gpu::PostGpu,
    object_start: u32,
    local_pages: Vec<crate::world_objects::GpuPage>,
    world_pages: Vec<crate::world_objects::GpuPage>,
    /// Whole-world page commands, one list per view slot (`GpuPage::world_view_slot`).
    world_draw_cmds: [Vec<crate::world_objects::DrawCmd>; crate::world_objects::WORLD_VIEWS],
    world_done: bool,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    water_start: u32,
    water_batches: Vec<WaterBatch>,
    started: Instant,
    wind_last: Instant,
    /// Snapshot computed once by `render()`; secondary views reuse it.
    frame: FrameSnapshot,
    world_glow: crate::post_gpu::WorldGlow,
    celestial_visibility: crate::client_sun::Visibility,
    /// Raw `+0x80` disc-flag word from the default area, before the viewer's
    /// unknown-word fallback, so the status line can say what the source said.
    celestial_flags: u32,
    /// Native `A3DSkySphere` swap machine (`+0x5b4`/`+0x5b8`/`+0x5b9`). Stepped
    /// exactly once per frame from `begin_frame`; reflection, refraction and map
    /// views only read the resulting snapshot (P5.07/P5.08).
    sky_transition: crate::sky_transition::Machine,
    /// Sub-tick remainder of the sky clock in milli-thirty-seconds: the native
    /// tick is a 1/30 s step and the viewer's frames are not a multiple of it.
    sky_tick_residual: u64,
    /// Stage-0 sky texture-matrix translation. `A3DSky::Tick` (`0x79a040`)
    /// advances the descriptor's x/z accumulators by `rate / 30` per 30 Hz tick
    /// and `A3DSky::Render` installs the resulting matrix on texture stage 0, so
    /// the ceiling art slides at `rate` per second in both axes.
    sky_drift: [f32; 2],
    // Native SetLightParam/SetAmbient state persists when a frame selects no
    // environment box (0x41ec65 keeps the in-flight environment). Keeping the
    // last published regional light avoids erasing ordinary model light on an
    // uncovered frame while baked surfaces stay independent of it.
    retained_regional_light: Option<crate::environment::RegionalLight>,
    glow_requested_fade: Option<f32>,
    grass_wind_state: crate::grass_wind::GrassWind,
    atlas: wgpu::Texture,
    vertex: wgpu::Buffer,
    index: wgpu::Buffer,
    count: u32,
    depth: wgpu::Texture,
    center: (isize, isize),
}
pub(crate) fn depth(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
/// Build one placement per native `CECSceneBlock` class (0x424020). ECWLD's
/// block arrays are mapped to the native class order: trees, water, ornaments,
/// box areas, grass areas, effects, EC models, critters, bezier routes, sounds.
/// Ornament indices are the `Objects::load_ids` indices; other classes get
/// disjoint synthetic indices so the same resident-bit set can cover all ten.
pub fn ten_class_placements(t: &Terrain) -> Vec<crate::scenery_visibility::Placement> {
    use crate::scenery_visibility::{Placement, Visibility};
    let mut out = Vec::new();
    let by_export: HashMap<u32, usize> = t
        .objects
        .ornaments
        .iter()
        .enumerate()
        .map(|(i, o)| (o.export_id, i))
        .collect();
    let mut next = t.objects.ornaments.len();
    let hidden = Visibility {
        reflect: false,
        refract: false,
    };
    let center_of = |b: &crate::scene::Block| -> [f32; 2] {
        [
            (b.col as f32 + 0.5) * t.scene.block_size - t.scene.width * 0.5,
            t.scene.length * 0.5 - (b.row as f32 + 0.5) * t.scene.block_size,
        ]
    };
    let mut push = |out: &mut Vec<Placement>, center: [f32; 2], class: Class| {
        out.push(Placement {
            index: next,
            center,
            visibility: hidden,
            class,
        });
        next += 1;
    };
    for b in &t.scene.blocks {
        let center = center_of(b);
        for tree in &b.trees {
            push(&mut out, [tree.position[0], tree.position[2]], Class::Trees);
        }
        for _ in &b.water {
            push(&mut out, center, Class::Water);
        }
        for r in &b.ornaments {
            if let Some(&i) = by_export.get(&r.export_id) {
                let o = &t.objects.ornaments[i];
                out.push(Placement {
                    index: i,
                    center: [o.position[0], o.position[2]],
                    visibility: Visibility::from_extra(o.extra),
                    class: Class::Ornaments,
                });
            }
        }
        for _ in &b.boxes {
            push(&mut out, center, Class::BoxAreas);
        }
        for _ in &b.grass {
            push(&mut out, center, Class::GrassAreas);
        }
        for e in &b.effects {
            push(&mut out, [e.position[0], e.position[2]], Class::Effects);
        }
        for m in &b.models {
            push(&mut out, [m.position[0], m.position[2]], Class::EcModels);
        }
        for _ in &b.critters {
            push(&mut out, center, Class::Critters);
        }
        for _ in &b.beziers {
            push(&mut out, center, Class::Beziers);
        }
        for snd in &b.sounds {
            push(&mut out, [snd.position[0], snd.position[2]], Class::Sounds);
        }
    }
    out
}

/// Reallocate every size-dependent world render target at the *scaled* render
/// resolution. The scene depth, water/refraction targets and post chain must
/// all share one extent: they are combined as attachments in the same render
/// passes. The swapchain, display-gamma ramp and UNORM present-encode
/// intermediates stay at the full window size and are resized separately.
fn resize_world_targets(renderer: &mut Renderer, w: u32, h: u32) {
    renderer.depth = depth(&renderer.device, w, h);
    // Sky keeps its dummy depth — sampling the real depth in the same
    // pass that writes it is a feedback hazard.
    renderer.water.resize(&renderer.device, w, h);
    renderer.post.resize(&renderer.device, w, h);
}
impl Renderer {
    fn resident_bounds(&self) -> Option<[f32; 4]> {
        let win = self.lod.window;
        self.detail.terrain_cutoff.then(|| {
            crate::terrain_cutoff::world_bounds(
                [win.x0, win.y0, win.x1, win.y1],
                TILE,
                self.world_width,
                self.world_height,
                self.world_spacing,
            )
        })
    }
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        w: u32,
        h: u32,
        mut m: MeshData,
        t: &mut Terrain,
        vegetation_authored_mips: bool,
    ) -> Result<Self, String> {
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera"),
            contents: bytemuck::bytes_of(&Uniform {
                matrix: Mat4::IDENTITY.to_cols_array_2d(),
                direction: [0., 0., -1., 0.],
                clip: [0.; 4],
                bounds: [0.; 4],
                tree_bounds: [0.; 4],
                sun: [-0.32, 0.82, 0.46, 0.],
                light: [0.55, 0.45, 1., 1.],
                region_ambient: [0.; 4],
                region_diffuse: [0.; 4],
                region_direction: [0.; 4],
                grass_eye: [0.; 4],
                grass_wind: [0.; 4],
                grass_history: [[0.; 4]; 80],
                fog_range: [0.; 4],
                fog_color: [0.; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let reflected_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("reflected camera"),
            contents: bytemuck::bytes_of(&Uniform {
                matrix: Mat4::IDENTITY.to_cols_array_2d(),
                direction: [0., 0., -1., 0.],
                clip: [0.; 4],
                bounds: [0.; 4],
                tree_bounds: [0.; 4],
                sun: [-0.32, 0.82, 0.46, 0.],
                light: [0.55, 0.45, 1., 1.],
                region_ambient: [0.; 4],
                region_diffuse: [0.; 4],
                region_direction: [0.; 4],
                grass_eye: [0.; 4],
                grass_wind: [0.; 4],
                grass_history: [[0.; 4]; 80],
                fog_range: [0.; 4],
                fog_color: [0.; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("world material atlas with grass mips"),
            size: wgpu::Extent3d {
                width: ATLAS as u32,
                height: ATLAS as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 8,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // D3D8 samples/filter authored color bytes without automatic sRGB decode.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = atlas.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: 16,
            ..Default::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let reflected_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("reflected world bindings"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: reflected_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let depth_tex = depth(&device, w, h);
        let sky = crate::sky_gpu::SkyGpu::new(&device, &queue, format, &t.element)?;
        let weather =
            crate::weather_gpu::WeatherGpu::new(&device, &queue, &t.element, format, &t.water);
        let cloud = t.cloud_surface.as_deref().and_then(|surface| {
            match crate::cloud_gpu::CloudGpu::new(&device, &queue, format, &t.element, surface) {
                Ok(cloud) => {
                    eprintln!("Initialized indexed world cloud billboards from world.cld");
                    Some(cloud)
                }
                Err(error) => {
                    eprintln!("World cloud renderer unavailable: {error}");
                    None
                }
            }
        });
        let water =
            crate::water_gpu::WaterGpu::new(&device, &queue, format, w, h, &t.water_normals);
        let simple_water = crate::simple_water_gpu::SimpleWaterGpu::new(
            &device,
            &queue,
            format,
            &t.water_simple_frames,
        );
        let shore =
            match crate::shore_gpu::ShoreGpu::new(&device, &queue, format, &t.element, &t.water) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Shoreline waves unavailable: {e}");
                    None
                }
            };
        let far_trees = crate::far_trees_gpu::FarTrees::new(
            &device,
            &queue,
            format,
            &t.trees,
            &uniform,
            &reflected_uniform,
            vegetation_authored_mips,
        );
        // Immutable tree meshes/motion samples and water frames now live on
        // the GPU. The streaming worker never reads these sources again.
        t.trees.types.clear();
        t.trees.types.shrink_to_fit();
        t.trees.placements.clear();
        t.trees.placements.shrink_to_fit();
        t.water_normals.clear();
        t.water_simple_frames.clear();
        let terrain_upload = m
            .terrain_upload
            .take()
            .ok_or("initial terrain upload missing")?;
        let terrain_gpu = crate::terrain_gpu::TerrainGpu::new(
            &device,
            &queue,
            format,
            t,
            &terrain_upload,
            &uniform,
            &reflected_uniform,
        )?;
        // Material masks and DDS buffers are no longer needed once uploaded.
        // Flush staging before adding native-size scenery pages to cap peak memory.
        drop(terrain_upload);
        t.materials.release_gpu_source();
        t.heights.trim();
        queue.submit(std::iter::empty());
        device.poll(wgpu::Maintain::Wait);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world WGSL"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        const ATTR: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32,5=>Uint32,7=>Uint32];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world GPU pipeline"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTR,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Greater,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        // Static scenery shares the client's single depth buffer with the
        // terrain, which is submitted first (`terrain_end..local_end`). Under
        // the reversed projection the client's fixed-point `LESSEQUAL` test
        // becomes `Greater`/`GreaterEqual`, and the tie rule matters: a
        // duplicate baked triangle submitted after an identical one must win,
        // exactly as a second draw at equal depth passes `LESSEQUAL`. Nothing
        // in the baked path changes `ZFUNC` or `ZBIAS` (the only `ZBIAS` writes
        // are the effect path's 2/0 pair at 0x417453 and 0x417471). Only the
        // comparand differs from `pipeline`; the shader and layout are the same.
        let scenery_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scenery GPU pipeline"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTR,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Baked scenery is an alpha-test cutout, not a blend. The
                    // 1.4.2 client enables `ALPHABLENDENABLE(1)` in all four
                    // scenery passes (`0x42b5a5`, `0x42bc17`, `0x42c32e`,
                    // `0x42c650`) and leaves `ZWRITEENABLE` on, so in the
                    // client an alpha-0 texel still occupies the depth buffer.
                    // Reproducing that here broke the viewer: the water pass is
                    // depth-tested against this buffer, so a transparent lily
                    // pad card occluded water as a solid square, and a canopy
                    // card hid the canopy behind it through its transparent
                    // gaps. The shared shader discards texels below the cutout
                    // threshold (`tex.a < …`) so they write neither colour nor
                    // depth; survivors are opaque and write depth as before.
                    // Keeping a blend here would write depth for the surviving
                    // partial-alpha texels against the pre-water colour, so the
                    // pass stays unblended (as it was before the blend change).
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        let vertex = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&m.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&m.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let count = m.indices.len() as u32;
        crate::grass_mips::upload(&queue, &atlas, &m.atlas, &m.grass_mips, ATLAS);
        let grass = crate::grass_instances::Gpu::new(
            &device,
            format,
            &layout,
            SHADER,
            m.grass,
            Some(&terrain_gpu.grass_light_layout),
        );
        let d = depth_tex;
        let post = crate::post_gpu::PostGpu::new(&device, format, w, h);
        let local_pages = m
            .local_pages
            .into_iter()
            .map(|page| {
                crate::world_objects::GpuPage::upload(
                    &device,
                    &queue,
                    &layout,
                    &sampler,
                    &uniform,
                    &reflected_uniform,
                    page,
                )
            })
            .collect();
        // The verified ordinary world queue is enabled by default. Unsupported
        // material/effect paths remain explicit instance failures, not stand-ins.
        // The world's effects must not depend on the map placing an EC model.
        // A block may author waterfalls, fire or glows and no animated model at
        // all, and the effect half of this object has nothing to do with the
        // model assets. Only the opt-out and a genuinely missing archive decide
        // whether the object is built; the model queue inside it is skipped
        // lazily when there are no instances.
        let models = if std::env::var_os("PW_DISABLE_WORLD_MODELS").is_none() {
            let assets = std::mem::take(&mut t.models);
            // `models.pck` holds the model textures; the effect roots come from
            // `gfx.pck` below. A map that ships no EC model still gets its
            // effects, so a missing model archive costs the model queue and
            // never the waterfalls.
            let mut world =
                match crate::world_models_gpu::World::new(&device, format, assets, &t.element) {
                    Ok(world) => world,
                    Err(error) => {
                        eprintln!("World models unavailable ({error}); world effects continue");
                        crate::world_models_gpu::World::without_models(&device, format)
                    }
                };
            // Actual world effect owners: the ECWLD placements and the hook-free
            // ECM EventType-100 roots publish into the same scene-light grid the
            // receiving models query, and the same owners now play their animated
            // roots — the waterfalls, torch fire, mist and glows the map authors —
            // into the world pass. A missing archive is a reported failure, not a
            // silent removal of either half.
            match world.attach_gfx_archive(&t.element) {
                Ok(()) => {}
                Err(e) => eprintln!("World FX light roots unavailable: {e}"),
            }
            // A placement the reader or the owner queue rejects is a reported
            // absence on the status line, not a refused world.
            let effects = world.install_scene_effects(&t.scene);
            let events = world.install_model_events();
            eprintln!(
                "Installed {effects} scene effects and {events} model events (light producers and animated roots)"
            );
            for failure in world.gfx_failures() {
                eprintln!("FX light: {failure}");
            }
            Some(world)
        } else {
            None
        };
        // Install the day/night pair the client's own `autoscene.cfg` names
        // (`sky_id_day = 0`, `sky_id_night = 1` for this build) after the
        // `sky.lst` rows have been loaded. The machine starts on the day set;
        // the first frame's phase drives the native swap.
        let mut sky_transition = crate::sky_transition::Machine::new(
            crate::sky_transition::Triple(sky.day_set().min(2) as u8),
        );
        let _ = sky_transition.author(
            crate::sky_transition::Triple(sky.day_set().min(2) as u8),
            crate::sky_transition::Triple(sky.night_set().min(2) as u8),
        );
        Ok(Self {
            modules: crate::modules::Modules::default(),
            module_format: format.remove_srgb_suffix(),
            models,
            model_lights: t.scene_lights.clone(),
            model_scene: t.scene.clone(),
            terrain_gpu,
            sky,
            weather,
            cloud,
            far_trees,
            grass,
            tree_center: m.tree_center,
            detail: m.detail,
            terrain_end: m.terrain_end,
            world_width: t.width,
            world_height: t.height,
            world_spacing: t.grid_spacing,
            device,
            queue,
            pipeline,
            scenery_pipeline,
            uniform,
            reflected_uniform,
            bind,
            reflected_bind,
            post,
            water,
            simple_water,
            shore,
            object_start: m.object_start,
            local_pages,
            world_pages: Vec::new(),
            world_draw_cmds: Default::default(),
            world_done: false,
            layout,
            sampler,
            water_start: m.water_start,
            water_batches: m.water_batches,
            started: Instant::now(),
            frame: FrameSnapshot::default(),
            world_glow: crate::post_gpu::WorldGlow::default(),
            celestial_visibility: crate::client_sun::Visibility::default(),
            celestial_flags: 0x18,
            sky_transition,
            sky_tick_residual: 0,
            sky_drift: [0.; 2],
            retained_regional_light: None,
            glow_requested_fade: None,
            grass_wind_state: crate::grass_wind::GrassWind::new(),
            // `begin_frame` turns wall time into wind steps, so the anchor
            // has to exist before the first frame.
            wind_last: Instant::now(),
            atlas,
            vertex,
            index,
            count,
            depth: d,
            center: m.center,
            lod: m.lod,
            spans: m.spans,
            terrain_runs: Vec::new(),
            far_terrain_runs: Vec::new(),
            reflected_terrain_runs: Vec::new(),
            reflected_far_runs: Vec::new(),
            map_terrain_runs: Vec::new(),
            map_far_runs: Vec::new(),
        })
    }
    /// Sample time and publish the frame's lighting/playback state exactly
    /// once. Every secondary view reuses the returned snapshot with a zero
    /// logic delta; no secondary submission calls this method.
    /// Explicit celestial state report: which disc is drawable and, when it is
    /// not, which native condition failed. Shown next to the sky settings so an
    /// unsupported state is visible instead of silently missing.
    pub fn celestial_status(&self, camera: &Camera) -> String {
        let authored = camera
            .regional_light
            .or(self.retained_regional_light)
            .map(|l| l.material_direction().toward_sun())
            .unwrap_or([0.6, -0.4, 0.8]);
        let state = match crate::sky_time::evaluate(camera.time_of_day, authored) {
            Some(state) => state,
            None => return "sky clock: non-finite time or sun direction".to_string(),
        };
        let night = crate::sky_time::night_weight(camera.time_of_day).unwrap_or(0.);
        let mut out = format!(
            "Sky clock {:.2} h: sun {:.1} deg, moon {:.1} deg, night texture set {:.0}%, night flag {}, night strength {:.2}",
            camera.time_of_day,
            state.sun_elevation.to_degrees(),
            state.moon_elevation.to_degrees(),
            night * 100.,
            if state.night_flag { "on" } else { "off" },
            state.night_strength
        );
        let flag_word = self.celestial_flags;
        let latched = self.celestial_visibility.flags();
        let flag_note = if latched & 0x18 == 0 {
            " (latched word has neither disc bit)".to_string()
        } else if camera.ignore_celestial_flags && latched != flag_word {
            format!(" (source 0x{flag_word:02x}; area disc flags ignored, latched 0x{latched:02x})")
        } else if flag_word & 0x18 == 0 {
            // `+0x80` is unpinned: the viewer keeps both discs visible instead of
            // hiding them on a word whose producer is not identified, and says so.
            format!(" (source 0x{flag_word:02x} has no disc bit; viewer latches 0x{latched:02x})")
        } else if flag_word & 0x08 == 0 {
            // 0x420992 takes the sun from bit 3 and the moon from bit 4, so a zone
            // whose word clears bit 3 really has no sun disc in the client either.
            format!(" (source 0x{flag_word:02x} clears the sun bit; 0x420992 hides the sun disc)")
        } else if flag_word & 0x10 == 0 {
            format!(" (source 0x{flag_word:02x} clears the moon bit; 0x420992 hides the moon)")
        } else {
            String::new()
        };
        out.push_str(&format!("; disc flags 0x{flag_word:02x}{flag_note}"));
        for disc in crate::sky_celestial::discs(
            &state,
            self.celestial_visibility.fade(true),
            self.celestial_visibility.fade(false),
            true,
            true,
            state.night_strength,
        ) {
            match disc {
                Ok(disc) => {
                    out.push_str(&format!(
                        "; {} {:.2} deg disc, alpha {}",
                        disc.kind.name(),
                        disc.angular_radius.to_degrees() * 2.,
                        disc.alpha
                    ));
                    let sun = disc.kind == crate::sky_celestial::DiscKind::Sun;
                    if !self.sky.disc_texture_present(sun) {
                        out.push_str(" (client texture missing)");
                    }
                    // The quad sizes below are the client's own stream
                    // templates. The flare chain is the exception: its placement
                    // generator (`0x4b5b74`) and occlusion mask are not reduced,
                    // so only its textures and its 0.4 weight are the client's.
                    // The sun's shine strip is the recovered 120 x 40 quad, drawn
                    // inside the recovered night-strength window.
                    if sun {
                        let strength = crate::sky_celestial::shine_ramp(
                            state.night_strength,
                            state.night_flag,
                        );
                        if self.sky.shine_texture_present(true) {
                            out.push_str(&format!(
                                " + sunshine.bmp 120x40 units at {:.0}%",
                                strength * 100.
                            ));
                        } else {
                            out.push_str(" (sunshine.bmp missing)");
                        }
                        if self.sky.flare_texture_present() {
                            out.push_str(" + sunflare/flares ghosts at 0.4 cover");
                            out.push_str(" (sprite placement unrecovered)");
                        } else {
                            out.push_str(" (sunflare/flares missing)");
                        }
                    } else {
                        // The moon has no halo quad: retail draws its halo from the
                        // flare chain, which this build only does for the sun, and a
                        // concentric quad landed off the painted disc.
                        out.push_str(" (no halo quad; retail takes it from the flare chain)");
                    }
                }
                Err(error) => out.push_str(&format!("; {error}")),
            }
        }
        out
    }
    fn begin_frame(&mut self, camera: &Camera, w: u32, h: u32) -> FrameSnapshot {
        let now = Instant::now();
        let dt = now.duration_since(self.wind_last).as_secs_f32();
        self.grass_wind_state.update(dt);
        self.wind_last = now;
        self.post.set_sharpen(camera.sharpen);
        self.world_glow.select(
            camera.bloom,
            crate::environment::daylight(camera.time_of_day),
            camera.underwater_depth > 0.,
            camera.bloom_mode,
            camera.bloom_tint,
        );
        if camera.bloom && self.glow_requested_fade != Some(camera.bloom_brightness_fade) {
            self.world_glow.set_fade(
                self.world_glow.brightness_fade(),
                camera.bloom_brightness_fade,
                1000,
            );
            self.glow_requested_fade = Some(camera.bloom_brightness_fade);
        }
        // AdjustEnvironment passes the frozen +0x490 logic tick to glow
        // (0x4214dc); do not resample wall time here or in any secondary view.
        self.world_glow.tick(camera.logic_delta_ms as i32);
        let glow_parameters = self.world_glow.parameters();
        self.post.set_glow_color(
            camera.bloom_tint,
            self.world_glow.brightness_fade(),
            camera.bloom_opacity_fade,
        );
        let elapsed = self.started.elapsed().as_secs_f32();
        let elapsed64 = self.started.elapsed().as_secs_f64();
        let direction = camera.direction();
        let daylight = crate::environment::daylight(camera.time_of_day);
        let angle = (camera.time_of_day - 6.0) * std::f32::consts::PI / 12.0;
        let twilight = (1. - ((angle.sin().abs()) / 0.35).clamp(0., 1.)) * daylight;
        let sun = [
            angle.cos() * 0.8,
            angle.sin().max(0.05),
            0.46,
            if camera.terrain_lightmaps {
                daylight
            } else {
                -1.
            },
        ];
        let light = [
            0.17 + 0.38 * daylight,
            0.07 + 0.38 * daylight,
            1.,
            1. - 0.18 * twilight,
        ];
        // The region's authored light, resolved once for every consumer below.
        let region_light = camera.regional_light.or(self.retained_regional_light);
        let authored = region_light
            .map(|l| l.material_direction().toward_sun())
            .unwrap_or([0.6, -0.4, 0.8]);
        // The raw word is kept for the status line; `+0x80` is not pinned, so a
        // word with neither disc bit never reaches the native latch (the sun
        // would silently disappear) and the viewer feeds `0x18` instead.
        let source_flags = region_light.map(|l| l.celestial_flags).unwrap_or(0x18);
        self.celestial_flags = source_flags;
        // World-tab switch: a zone authored without bit 3 has no sun disc in the
        // client either, so this exists to tell that apart from a viewer defect -
        // turn it on where the sun vanishes and the disc comes back.
        let latched_flags = if camera.ignore_celestial_flags || source_flags & 0x18 == 0 {
            0x18
        } else {
            source_flags
        };
        self.celestial_visibility
            .update(camera.logic_delta_ms, latched_flags);
        // Disc placement follows the render path 0x4b3e00: each object uses its
        // own recovered elevation curve and the shared regional azimuth.
        // `sun_dir` is a light direction, which 0x4b60b0 stores negated.
        let (sun, moon) = match crate::sky_time::evaluate(camera.time_of_day, authored) {
            Some(state) => {
                let fade = |sun: bool| {
                    if camera.celestial_discs {
                        self.celestial_visibility.fade(sun)
                    } else {
                        0.
                    }
                };
                // `.w` is the shine strength the shader tints the additive
                // shine quad with. Only the sun has one: `0x4b3e00` builds its
                // stage-1 texture factor as `(1 - [+0x70]) * 5.0` (times 0.8 on
                // the night set) from `sunshine.bmp`. The moon's block at
                // `0x4b4049` halves its `+0x48` colour and scales nothing; its
                // halo is `moonflare.bmp` plus the recovered `sunflare`/`flares`
                // chain, both drawn by the sky shader.
                let shine = crate::sky_celestial::glow_texture_factor(
                    state.night_strength,
                    state.night_flag,
                );
                let shine = (shine / 5.).clamp(0., 1.);
                (
                    [fade(true), state.sun_elevation, state.azimuth, shine],
                    [fade(false), state.moon_elevation, state.azimuth, 0.],
                )
            }
            None => ([0.; 4], [0.; 4]),
        };
        // The frame snapshot keeps the legacy single-disc summary for the
        // capture tests; the sky itself now consumes the per-object values above.
        let celestial = crate::client_sun::sky_object_direction(camera.time_of_day, authored)
            .unwrap_or([0., -1., 0.])
            .map(|x| -x);
        let celestial_alpha = if camera.celestial_discs {
            self.celestial_visibility.alpha(camera.time_of_day)
        } else {
            0.
        };
        // P5.07: the native sky driver (`0x4203f4..0x420807`) returns while a
        // swap is in flight and otherwise installs the day or night triple for
        // the clock phase. The viewer's phase is the model day/night factor the
        // fog channels and the celestial clock already use, so a time-slider
        // move and a region change reach the sky through the same clock. `drive`
        // only issues a swap; `step` is the native once-per-frame tick
        // (`0x79a040`), and nothing else in the frame may call either.
        let phase = crate::environment::night_factor(camera.time_of_day).clamp(0., 1.);
        // The byte read at `0x4204db`. The client derives it from the sky clock,
        // and its two roles (pick the branch, choose the pre-swap) only change
        // *which* of the two counted swaps runs, so the viewer keys it off the
        // same clock phase the driver reads. The authored flag word at
        // `0x420992` gates disc visibility and is NOT used here: nothing in the
        // driver tests it, and every region box carries one constant word.
        let day = crate::environment::daylight(camera.time_of_day) >= 0.5;
        crate::sky_transition::drive(
            &mut self.sky_transition,
            phase,
            day,
            crate::sky_transition::DAY_CLOCK_MS,
        );
        // A native fade is minutes long (up to half a game day), so it is
        // replayed on elapsed time, not once per rendered frame: `0x8f14bc` says
        // one tick is 1/30 s and `0x79a040` is the object's per-frame update.
        self.sky_transition
            .step_ms(camera.logic_delta_ms, &mut self.sky_tick_residual);
        // The same tick rebuilds `A3DSky`'s stage-0 texture matrix (`0x79a0e0`
        // driven by `0x79a040`): both accumulators of the descriptor at `+0x4cc`
        // advance by `rate / 30` per 30 Hz tick, so at an arbitrary frame rate
        // the slide is `rate` per second. The descriptor's angle slot is zero,
        // so there is no rotation to replay. `A3DSky::Render` (`0x799a40`) hangs
        // the matrix on texture stage 0, which is the ceiling only; the curtain
        // inherits the identity stage-1 matrix and keeps its authored UVs.
        let drift_step =
            crate::sky_gpu::SKY_DRIFT_RATE * (camera.logic_delta_ms.max(0) as f32 / 1000.);
        self.sky_drift[0] += drift_step;
        self.sky_drift[1] += drift_step;
        // World-tab override: publish one pinned `sky.lst` row. The machine
        // above still advances underneath, so returning to `Auto` resumes the
        // client's own ladder instead of snapping to a stale set, but nothing it
        // requests can reach the shader while a row is pinned. `Auto` is
        // `0x4203f4`; the pinned row draws with no incoming set, so it never
        // fades.
        let sets = camera
            .sky_override
            .sets()
            .unwrap_or_else(|| crate::sky_transition::frame_sets(&self.sky_transition));
        // P5.08: one snapshot, two uploads. The reflected buffer receives the
        // same hour, sets, blend, celestial direction and alpha; only the ray
        // basis differs, so the sky cannot disagree between views. `view_sets`
        // is the documented gate: a view can only read the snapshot, never
        // re-derive the fade (`0x7cca50` and the reflection target both draw the
        // same sky object).
        let main_sets = crate::sky_transition::view_sets(sets, Pass::Main.is_secondary());
        let reflected_sets =
            crate::sky_transition::view_sets(sets, Pass::Reflection.is_secondary());
        // One authored sky set covers both phases, so the dusk/night darkening is
        // the recovered regional light colour instead of a second texture set. It
        // never falls below the region's ambient level, and the recovered switch
        // weight drives it too, so a region without a lighting record still gets a
        // night sky.
        let night_weight = crate::sky_time::night_weight(camera.time_of_day).unwrap_or(0.);
        let sky_light = region_light
            .map(|l| {
                [
                    l.ambient[0].max(l.diffuse[0]),
                    l.ambient[1].max(l.diffuse[1]),
                    l.ambient[2].max(l.diffuse[2]),
                ]
            })
            .unwrap_or([1.; 3]);
        let shade = 1.0 - 0.72 * night_weight.clamp(0.0, 1.0);
        let tint = [
            sky_light[0] * shade,
            sky_light[1] * shade,
            sky_light[2] * shade,
        ];
        let frame = crate::sky_gpu::SkyFrame {
            hour: camera.time_of_day,
            night_weight,
            sun,
            moon,
            // The 38.16-72.5 degree band has no native mesh; the client covers it
            // with something drawn at 0x799f88 that this build does not, so the
            // viewer blends the frame's atmospheric fog into the cap's own rim
            // colour across that band.
            background: camera.air_fog.color,
            fog_enabled: camera.atmospheric_fog,
            tint,
            disc_brightness: if camera.celestial_discs {
                camera.celestial_brightness
            } else {
                0.
            },
            drift: self.sky_drift,
        };
        self.sky
            .update(&self.queue, direction, w, h, frame, main_sets, false);
        self.sky
            .update(&self.queue, direction, w, h, frame, reflected_sets, true);
        let resident_bounds = self.resident_bounds();
        self.far_trees.update_resident_lods(
            &self.device,
            &self.queue,
            camera.pos.to_array(),
            self.detail.tree_lod_scale * lod_scale(self.detail.scenery_quality, Class::Trees),
            resident_bounds,
        );
        // Use the actual uploaded window. The native window shifts at map
        // edges; recomputing a clamped camera square discarded the wrong area.
        let win = self.lod.window;
        let x0 = (win.x0 * TILE).min(self.world_width - 1);
        let x1 = (win.x1 * TILE).min(self.world_width - 1);
        let y0 = (win.y0 * TILE).min(self.world_height - 1);
        let y1 = (win.y1 * TILE).min(self.world_height - 1);
        let bounds = [
            (x0 as f32 - self.world_width as f32 * 0.5) * self.world_spacing,
            (x1 as f32 - self.world_width as f32 * 0.5) * self.world_spacing,
            (self.world_height as f32 * 0.5 - y1 as f32) * self.world_spacing,
            (self.world_height as f32 * 0.5 - y0 as f32) * self.world_spacing,
        ];
        // Preview only: the client scalar modulates the old sampled-motion
        // blend; it is not the DLL's live leaf-rocking implementation.
        static TREE_WIND: std::sync::OnceLock<crate::tree_wind::ClientWind> =
            std::sync::OnceLock::new();
        let tree_blend = crate::tree_wind::preview_blend(
            TREE_WIND.get_or_init(crate::tree_wind::ClientWind::new),
            elapsed,
            camera.tree_wind_strength,
            camera.client_tree_wind_preview,
        );
        let tree_bounds = [camera.pos.x, camera.pos.z, tree_blend, elapsed];
        let (region_ambient, region_diffuse, region_direction) = camera.region_uniform();
        let grass_eye = [
            camera.pos.x,
            camera.pos.y,
            camera.pos.z,
            if camera.grass_distance_fade { 1. } else { 0. },
        ];
        // x is the recovered grass clock (`+0x15c`) for diagnostics, y is the
        // on/off gate the shader tests (`> 0.5`). The client has no user scale:
        // its target is authored data (+0x6c..0x74 = 1.0, -0.5, 0.0). The
        // viewer's amplitude slider therefore scales the published history
        // below instead of pretending to be a native gain.
        let grass_wind = [
            self.grass_wind_state.time(),
            if camera.grass_wind_enabled { 1. } else { 0. },
            if camera.area_grass_lightmaps { 1. } else { 0. },
            0.,
        ];
        let grass_history = self
            .grass_wind_state
            .history_scaled(if camera.grass_wind_enabled {
                camera.grass_wind_amplitude
            } else {
                1.
            });
        let mut active_fog = camera.active_fog();
        if active_fog.enabled && camera.underwater_depth <= 0. {
            if let Some(cap) = self.resident_bounds().and_then(|b| {
                crate::terrain_cutoff::fog_cap(b, camera.pos.to_array(), w as f32 / h.max(1) as f32)
            }) {
                let range = crate::terrain_cutoff::fit_fog(active_fog.start, active_fog.end, cap);
                active_fog.start = range[0];
                active_fog.end = range[1];
            }
        }
        let (mut fog_range, fog_color) = (active_fog.range(), active_fog.color4());
        // Uniform fog_range.w is the terrain day/night factor; the shader reads
        // it instead of overloading sun.w (the lightmap-availability flag).
        fog_range[3] = camera.terrain_day_night;
        let active_fog_range = active_fog.range();
        let active_fog_color = active_fog.color4();
        let clip = [
            0.,
            0.,
            1., // horizontal projection flip reverses winding
            if camera.model_backface_culling {
                0.
            } else {
                1.
            },
        ];
        let snapshot = FrameSnapshot {
            elapsed,
            elapsed64,
            logic_delta_ms: camera.logic_delta_ms,
            direction,
            daylight,
            twilight,
            sun,
            light,
            bounds,
            tree_bounds,
            region_ambient,
            region_diffuse,
            region_direction,
            grass_eye,
            grass_wind,
            grass_history,
            fog_range,
            fog_color,
            active_fog_range,
            active_fog_color,
            clip,
            glow_parameters,
            celestial_alpha,
            authored,
            celestial,
            time_advances: 1,
            light_publications: 1,
        };
        self.frame = snapshot;
        snapshot
    }
    fn update_mesh(&mut self, mut m: MeshData) -> Result<(), String> {
        if let Some(mut upload) = m.terrain_upload.take() {
            self.terrain_gpu.update_residency(
                &self.device,
                &self.queue,
                &self.uniform,
                &self.reflected_uniform,
                &upload,
            )?;
            upload.materials.release_gpu_source();
        }
        self.grass.update(&self.device, m.grass);
        self.local_pages.clear();
        self.local_pages = m
            .local_pages
            .into_iter()
            .map(|page| {
                crate::world_objects::GpuPage::upload(
                    &self.device,
                    &self.queue,
                    &self.layout,
                    &self.sampler,
                    &self.uniform,
                    &self.reflected_uniform,
                    page,
                )
            })
            .collect();
        self.vertex = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&m.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        self.index = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&m.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        self.count = m.indices.len() as u32;
        self.center = m.center;
        self.tree_center = m.tree_center;
        self.detail = m.detail;
        self.lod = m.lod;
        self.spans = m.spans;
        self.terrain_end = m.terrain_end;
        self.object_start = m.object_start;
        self.water_start = m.water_start;
        self.water_batches = m.water_batches;
        crate::grass_mips::upload(&self.queue, &self.atlas, &m.atlas, &m.grass_mips, ATLAS);
        Ok(())
    }
    fn draw_opaque(
        &self,
        e: &mut wgpu::CommandEncoder,
        label: &str,
        view: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        bind: &wgpu::BindGroup,
        reflected: bool,
        sky_reflected: bool,
        pass_kind: Pass,
        view_runs: ViewRuns,
        terrain_depth_source: Option<&wgpu::Texture>,
        clear_color: Option<wgpu::Color>,
    ) {
        let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear_color.unwrap_or(wgpu::Color {
                        r: 0.18,
                        g: 0.32,
                        b: 0.48,
                        a: 1.,
                    })),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if clear_color.is_none() {
            self.sky.draw(&mut p, sky_reflected);
        }
        // Each pass submits only the blocks its own matrix keeps (`block+0x74`);
        // the caller has queued the matching records for this submit.
        let (runs, far_runs) = match view_runs {
            ViewRuns::Full => (None, None),
            ViewRuns::Main => (
                Some(self.terrain_runs.as_slice()),
                Some(self.far_terrain_runs.as_slice()),
            ),
            ViewRuns::Reflected => (
                Some(self.reflected_terrain_runs.as_slice()),
                Some(self.reflected_far_runs.as_slice()),
            ),
            ViewRuns::Map => (
                Some(self.map_terrain_runs.as_slice()),
                Some(self.map_far_runs.as_slice()),
            ),
        };
        self.terrain_gpu.draw(
            &mut p,
            reflected,
            &self.vertex,
            &self.index,
            self.terrain_end,
            runs,
            far_runs,
        );
        drop(p);
        if let Some(source) = terrain_depth_source {
            self.water.copy_terrain_depth(e, source);
        }
        let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("opaque scenery after terrain"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if !ui_test() && !skip_trees_test() {
            self.far_trees.draw(&mut p, reflected);
        }
        self.grass
            .draw(&mut p, bind, Some(&self.terrain_gpu.grass_light_bind));
        p.set_pipeline(&self.scenery_pipeline);
        p.set_bind_group(0, bind, &[]);
        p.set_vertex_buffer(0, self.vertex.slice(..));
        p.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
        let local_end = if self.world_done {
            self.object_start
        } else {
            self.water_start
        };
        p.draw_indexed(self.terrain_end..local_end, 0, 0..1);
        if !self.world_done {
            for page in &self.local_pages {
                page.draw_selected(&mut p, reflected, pass_kind);
            }
        }
        // Do not submit world pages on top of the same locally streamed
        // ornaments while preload is in progress: the coplanar duplicates
        // caused depth flicker. Switch atomically only after all pages arrive.
        if self.world_done {
            // The view's commands were folded once when the world finished
            // loading: its placements are frozen, so nothing here is per-frame
            // work beyond the page binds and the draws themselves.
            let slot = crate::world_objects::GpuPage::world_view_slot(pass_kind);
            crate::world_objects::GpuPage::draw_world(
                &self.world_pages,
                &self.world_draw_cmds[slot],
                &mut p,
                reflected,
            );
        }
    }
    fn draw_models(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        uniform: &Uniform,
        camera: &Camera,
        view: Pass,
        advance: bool,
    ) {
        // WaterEffect 0 (shipped default) hides EC models and home ornaments
        // in reflection/refraction (0x5b2015, 0x5b2336). RenderContext carries
        // the recovered level-2 / underwater class gates; per-placement flags
        // are applied by world_objects::GpuPage::draw_selected.
        let context = RenderContext::new(NATIVE_DEFAULT_WATER_EFFECT, camera.underwater_depth > 0.);
        if !allows_class_with_context(view, ModelClass::EcModel, context) {
            return;
        }
        // Effects outside this radius are unloaded and publish no lights. The
        // radius is in ECWLD blocks, using the scene's authored block size.
        let world_radius = self.detail.radius.max(1) as f32 * self.model_scene.block_size.max(1.);
        if let Some(light) = camera.regional_light {
            self.retained_regional_light = Some(light);
        }
        let light = camera.regional_light.or(self.retained_regional_light);
        let resident_bounds = self.resident_bounds();
        if let Some(models) = &mut self.models {
            models.set_view_center(camera.pos.x, camera.pos.z, world_radius);
            models.set_resident_bounds(resident_bounds);
            models.set_effects_enabled(camera.world_effects);
            models.set_effect_distance(camera.effect_distance);
        }
        // The effects the map authored are not a lighting feature and must not
        // wait for an environment record. `environments.lighting` returns None
        // whenever no region matches the eye — an area with no EC-BSD row, or one
        // the selector has not resolved yet — and nesting the whole model and
        // effect pass behind it meant such a map played no water, fire, mist or
        // glow at all. Lighting falls back to the neutral frame below; the
        // geometry always runs.
        let (Some(models), light) = (&mut self.models, light) else {
            return;
        };
        {
            // Reuse the root logic tick used by regions and the glow presenter.
            // A fresh wall-time sample here both bypassed the 250ms cap and
            // included all asset-loading time on the first receiving-model draw.
            // Water passes reuse the sampled pose; never tick again per view.
            let delta = if advance { camera.logic_delta_ms } else { 0 };
            let frame = receiving_model_frame(uniform, camera, light);
            models.update_pass(
                &self.device,
                &self.queue,
                frame,
                camera.pos.to_array(),
                &self.model_lights,
                crate::environment::night_factor(camera.time_of_day),
                delta,
                advance,
            );
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world receiving-model pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            models.draw(&mut pass);
        }
    }

    /// The host player is not an ornamental EC model: WaterEffect 0 must not
    /// suppress it. Native host-player reflection/refraction uses its feet-based
    /// profession AABB, independently of the world-model class gate above.
    /// Add-on pass. Each module decides for itself whether it appears in this
    /// pass; the host only supplies the frame and the water-visibility gates the
    /// Walker needs. Enabling one add-on never depends on another being on.
    fn draw_addons(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        uniform: &Uniform,
        camera: &Camera,
        view: Pass,
    ) {
        let walker = match view {
            Pass::Reflection => self
                .modules
                .walker_visible_in_water_pass(uniform.clip[0], true),
            Pass::Refraction => self
                .modules
                .walker_visible_in_water_pass(uniform.clip[0], false),
            _ => self.modules.walker_active(),
        };
        let population = self.modules.populator_active();
        if !walker && !population {
            return;
        }
        if let Some(light) = camera.regional_light {
            self.retained_regional_light = Some(light);
        }
        {
            let light = camera.regional_light.or(self.retained_regional_light);
            let frame = receiving_model_frame(uniform, camera, light);
            if walker {
                self.modules.render_walker(
                    &self.device,
                    &self.queue,
                    self.module_format,
                    encoder,
                    target,
                    depth,
                    frame,
                );
            }
            if population {
                self.modules.render_population(
                    &self.device,
                    &self.queue,
                    self.module_format,
                    encoder,
                    target,
                    depth,
                    frame,
                );
            }
        }
    }

    /// Render the actual loaded terrain and scenery from a north-up orthographic camera.
    /// Water is drawn as its real client grid with a flat color in this map pass.
    fn render_minimap(
        &mut self,
        target: &wgpu::TextureView,
        map_depth: &wgpu::TextureView,
        camera: &Camera,
        range: f32,
        full_world: bool,
    ) {
        let matrix = if full_world {
            let w = self.world_width as f32 * self.world_spacing;
            let h = self.world_height as f32 * self.world_spacing;
            Mat4::orthographic_rh(w * 0.5, -w * 0.5, -h * 0.5, h * 0.5, 20000., 1.)
                * Mat4::look_to_rh(Vec3::new(0., 10000., 0.), Vec3::NEG_Y, Vec3::Z)
        } else {
            minimap_matrix(camera.pos.x, camera.pos.z, range)
        };
        // The minimap is an orthographic window of `range` around the eye, so
        // the same conservative block test the main view uses keeps only the
        // part of the world that can reach a map pixel. The full map shows the
        // whole range by definition and keeps the full index list.
        let view_runs = if full_world || range <= 0. {
            ViewRuns::Full
        } else {
            self.queue_map_runs(matrix);
            ViewRuns::Map
        };
        let frame = self.frame;
        debug_assert!(
            frame.time_advances > 0,
            "minimap needs a main-frame snapshot"
        );
        let uniform = frame.uniform(
            matrix,
            [0., -1., 0., frame.daylight],
            frame.clip,
            [0.; 4],
            [0.; 4],
        );
        self.queue
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniform));
        let mut e = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("minimap"),
            });
        self.draw_opaque(
            &mut e,
            "minimap top down",
            target,
            map_depth,
            &self.bind,
            false,
            false,
            if full_world {
                Pass::FullMap
            } else {
                Pass::Minimap
            },
            view_runs,
            None,
            None,
        );
        {
            let mut pass = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("minimap water grids"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: map_depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.set_vertex_buffer(0, self.vertex.slice(..));
            pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(self.water_start..self.count, 0, 0..1);
        }
        self.queue.submit(Some(e.finish()));
    }
    /// Re-queues the main view's indirect terrain records for the next submit.
    /// Every pass drawn through the main matrix (`Main`, `Fog` and both
    /// refraction passes) submits with these; the water reflection overwrites
    /// the buffer with its mirrored view, so each plane puts them back first.
    fn queue_main_runs(&mut self) {
        self.terrain_gpu.prepare_runs(
            &self.device,
            &self.queue,
            Some(&self.terrain_runs),
            Some(&self.far_terrain_runs),
        );
    }
    /// Culls both terrain meshes to the top-down map's window and queues those
    /// records. `matrix` is the map's orthographic projection.
    fn queue_map_runs(&mut self, matrix: Mat4) {
        crate::terrain_residency::visible_runs_into(
            &self.spans,
            matrix,
            &mut self.map_terrain_runs,
        );
        self.terrain_gpu
            .visible_far_runs_into(matrix, &mut self.map_far_runs);
        self.terrain_gpu.prepare_runs(
            &self.device,
            &self.queue,
            Some(&self.map_terrain_runs),
            Some(&self.map_far_runs),
        );
    }
    /// Culls both terrain meshes for the water reflection's mirrored view
    /// (`matrix * flip`) and queues those records. The symmetry plane is the
    /// water height, so this runs once per reflected submit.
    fn queue_reflected_runs(&mut self, matrix: Mat4) {
        crate::terrain_residency::visible_runs_into(
            &self.spans,
            matrix,
            &mut self.reflected_terrain_runs,
        );
        self.terrain_gpu
            .visible_far_runs_into(matrix, &mut self.reflected_far_runs);
        self.terrain_gpu.prepare_runs(
            &self.device,
            &self.queue,
            Some(&self.reflected_terrain_runs),
            Some(&self.reflected_far_runs),
        );
    }
    fn render(
        &mut self,
        target: &wgpu::TextureView,
        target_raw: &wgpu::TextureView,
        matrix: Mat4,
        camera: &Camera,
        w: u32,
        h: u32,
    ) {
        // Per-frame, per-view visibility of the resident terrain (`0x77e250`):
        // the same block walk the client runs before the level rule.
        // Both buffers are reused: this runs for every view of every frame.
        crate::terrain_residency::visible_runs_into(&self.spans, matrix, &mut self.terrain_runs);
        self.terrain_gpu
            .visible_far_runs_into(matrix, &mut self.far_terrain_runs);
        // One indirect record per run, so the terrain itself is one call per mesh
        // instead of one call per run.
        self.queue_main_runs();
        let frame = self.begin_frame(camera, w, h);
        self.weather.advance(camera.logic_delta_ms);
        let selected_weather = camera.regional_light.map_or(
            crate::client_weather::WeatherSelection::default(),
            |light| light.weather_selection(),
        );
        // The World tab's override replaces the area's own flag bits before any
        // view consumes them, so the main, reflection and above-water passes all
        // see one selection. `Auto` is the client's `0x421520` answer.
        let selected_weather = camera.weather_override.apply(selected_weather);
        let main_weather = if camera.underwater_depth > 0.0 {
            crate::client_weather::WeatherSelection::default()
        } else {
            selected_weather
        };
        let glow_parameters = frame.glow_parameters;
        let direction = frame.direction;
        let daylight = frame.daylight;
        let bounds = frame.bounds;
        let tree_bounds = frame.tree_bounds;
        let sun = frame.sun;
        let light = frame.light;
        // Only real scene effects allocate the chain; the retained screen-space
        // fog diagnostic is not an input (production fog is per material).
        let use_post = crate::view_settings::post_required(
            camera.bloom,
            camera.sharpen,
            camera.underwater_depth > 0.,
            camera.client_gamma,
        );
        let post_view = if use_post {
            Some(self.post.scene_view())
        } else {
            None
        };
        let scene_target = post_view.as_ref().unwrap_or(target);
        let post_raw = if use_post {
            Some(self.post.scene_raw_view())
        } else {
            None
        };
        let scene_target_raw = post_raw.as_ref().unwrap_or(target_raw);
        // Fog is a uniform state of the main scene pass, not a separate view.
        let main_pass = if camera.active_fog().enabled {
            Pass::Fog
        } else {
            Pass::Main
        };
        let normal = frame.uniform(
            matrix,
            [direction.x, direction.y, direction.z, daylight],
            frame.clip,
            frame.fog_range,
            frame.fog_color,
        );
        self.queue
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&normal));
        self.water.update(
            &self.queue,
            &crate::water_gpu::WaterUniform {
                matrix: matrix.to_cols_array_2d(),
                inverse_matrix: matrix.inverse().to_cols_array_2d(),
                eye_time: [camera.pos.x, camera.pos.y, camera.pos.z, frame.elapsed],
                size: [
                    w as f32,
                    h as f32,
                    camera.far,
                    if camera.water_reflections { 1. } else { 0. },
                ],
                animation: [
                    crate::water_gpu::normal_phase(frame.elapsed64),
                    camera.underwater_fog.end,
                    0.,
                    crate::water_gpu::normal_offset_angle(frame.elapsed64),
                ],
                edge_mode: [if camera.water_reflections { 1. } else { 0. }, 0., 0., 0.],
                // Water-surface fog: the client binds the region's own triple
                // (white 100..400 above water, blue 10..60 below).
                fog: frame.active_fog_range,
                fog_color: frame.active_fog_color,
            },
        );
        self.simple_water.update(
            &self.queue,
            matrix,
            frame.elapsed64,
            frame.active_fog_range,
            frame.active_fog_color,
        );
        let reflection = self.water.reflection();
        let reflect_depth = self.water.depth();
        let refraction = self.water.refraction();
        let scene_color = self.water.scene();
        let scene_depth = self.depth.create_view(&Default::default());
        let mut e = self.device.create_command_encoder(&Default::default());
        self.draw_opaque(
            &mut e,
            "main scene",
            &scene_color,
            &scene_depth,
            &self.bind,
            false,
            false,
            main_pass,
            ViewRuns::Main,
            Some(&self.depth),
            None,
        );
        let model_target = self.water.scene_raw();
        self.draw_models(
            &mut e,
            &model_target,
            &scene_depth,
            &normal,
            camera,
            Pass::Main,
            true,
        );
        self.draw_addons(
            &mut e,
            &model_target,
            &scene_depth,
            &normal,
            camera,
            Pass::Main,
        );
        if let Some(cloud) = &self.cloud {
            cloud.draw(
                &self.queue,
                &mut e,
                &scene_color,
                &scene_depth,
                matrix,
                camera.yaw,
                camera.pitch,
                camera.cloud_detail,
                daylight,
                camera.active_fog(),
                normal.clip,
            );
        }
        self.weather.draw(
            &self.queue,
            &mut e,
            &scene_color,
            &scene_depth,
            matrix,
            camera.pos,
            camera.yaw,
            camera.pitch,
            camera.active_fog(),
            normal.clip,
            main_weather,
        );
        self.water.copy_scene_to_refraction(&mut e);
        self.water.copy_depth(&mut e, &self.depth);
        self.water.blit(&mut e, scene_target);
        self.queue.submit(Some(e.finish()));
        // Diagnostic mode for exercising window/input UI on memory-limited software GPUs.
        if ui_test() {
            if use_post {
                self.post.render(
                    &self.device,
                    &self.queue,
                    target,
                    &self.water.sample_depth(),
                    w,
                    h,
                    frame.elapsed,
                    camera.far,
                    camera.underwater_depth,
                    if camera.underwater_depth > 0. {
                        camera.underwater_fog
                    } else {
                        camera.air_fog
                    },
                    false, // no screen-space fog; scene geometry uses material fog
                    false,
                    camera.bloom,
                    camera.bloom_strength,
                    glow_parameters,
                    camera.client_gamma,
                    camera.gamma_setting,
                );
            }
            return;
        }

        // The reflection texture is reused only after its previous water draw is
        // submitted. Each distinct height gets its own mirror and clip plane.
        let mut visible: Vec<WaterBatch> = self
            .water_batches
            .iter()
            .filter(|b| b.visible(matrix))
            .cloned()
            .collect();
        visible.sort_by(|a, b| {
            b.distance_squared(camera.pos)
                .total_cmp(&a.distance_squared(camera.pos))
        });
        if water_debug() {
            static LOG_ONCE: std::sync::Once = std::sync::Once::new();
            LOG_ONCE.call_once(|| {
                eprintln!(
                    "Visible water reflection planes: {:?}",
                    visible.iter().map(|b| b.height).collect::<Vec<_>>()
                );
            });
        }
        if !camera.water_reflections {
            let mut e = self.device.create_command_encoder(&Default::default());
            for batch in visible {
                if batch.simple {
                    self.simple_water.draw(
                        &mut e,
                        scene_target_raw,
                        &scene_depth,
                        &self.vertex,
                        &self.index,
                        batch.start,
                        batch.end,
                    );
                    continue;
                }
                self.water.draw_water(
                    &mut e,
                    scene_target,
                    &scene_depth,
                    &self.vertex,
                    &self.index,
                    batch.start,
                    batch.end,
                );
            }
            self.queue.submit(Some(e.finish()));
            if let Some(shore) = &self.shore {
                shore.draw(
                    &self.device,
                    &self.queue,
                    scene_target_raw,
                    &scene_depth,
                    matrix,
                    camera.pos,
                    1. - daylight,
                    frame.elapsed,
                    self.resident_bounds(),
                    frame.active_fog_range,
                    frame.active_fog_color,
                );
            }
            if use_post {
                self.post.render(
                    &self.device,
                    &self.queue,
                    target,
                    &self.water.sample_depth(),
                    w,
                    h,
                    frame.elapsed,
                    camera.far,
                    camera.underwater_depth,
                    if camera.underwater_depth > 0. {
                        camera.underwater_fog
                    } else {
                        camera.air_fog
                    },
                    false, // no screen-space fog; scene geometry uses material fog
                    false,
                    camera.bloom,
                    camera.bloom_strength,
                    glow_parameters,
                    camera.client_gamma,
                    camera.gamma_setting,
                );
            }
            return;
        }
        for batch in visible {
            if batch.simple {
                let mut e = self.device.create_command_encoder(&Default::default());
                self.simple_water.draw(
                    &mut e,
                    scene_target_raw,
                    &scene_depth,
                    &self.vertex,
                    &self.index,
                    batch.start,
                    batch.end,
                );
                self.queue.submit(Some(e.finish()));
                continue;
            }
            let plane = batch.height;
            // A previous plane's reflection left its mirrored records in the
            // buffer; the refraction passes below draw through the main matrix.
            self.queue_main_runs();
            if camera.pos.y > plane {
                // Client 0x7cc6ac-0x7cc8b5: render below this plane into
                // refraction, cleared to the region's water fog color. Fog
                // distances start just beyond the eye-to-plane height and
                // extend by 55% of the configured underwater fog end.
                let fog_range = crate::water_gpu::refraction_fog_range(
                    camera.pos.y,
                    plane,
                    camera.underwater_fog.end,
                );
                let mut refracted = normal;
                refracted.clip = [plane, -1., 1., normal.clip[3]];
                // Clipped refraction binds the water fog color with the
                // plane-relative range for every material of that pass.
                let pass_fog = crate::fog_matrix::refraction_fog(
                    crate::fog_matrix::Fog {
                        start: camera.underwater_fog.start,
                        end: camera.underwater_fog.end,
                        color: camera.underwater_fog.color,
                        enabled: true,
                    },
                    fog_range[0],
                    fog_range[1],
                );
                refracted.fog_range = pass_fog.range();
                refracted.fog_color = pass_fog.color4();
                self.queue
                    .write_buffer(&self.reflected_uniform, 0, bytemuck::bytes_of(&refracted));
                let linear = |x: f32| -> f64 {
                    let x = x.clamp(0., 1.);
                    if !self.water.is_srgb() {
                        return x as f64;
                    }
                    if x <= 0.04045 {
                        (x / 12.92) as f64
                    } else {
                        (((x + 0.055) / 1.055).powf(2.4)) as f64
                    }
                };
                let fog = camera.underwater_fog.color;
                let mut re = self.device.create_command_encoder(&Default::default());
                self.draw_opaque(
                    &mut re,
                    "underwater refraction",
                    &refraction,
                    &reflect_depth,
                    &self.reflected_bind,
                    true,
                    false,
                    // Queued as `refracted = normal`: the main matrix, so the
                    // main view's blocks are the right set.
                    Pass::Refraction,
                    ViewRuns::Main,
                    None,
                    Some(wgpu::Color {
                        r: linear(fog[0]),
                        g: linear(fog[1]),
                        b: linear(fog[2]),
                        a: 1.,
                    }),
                );
                let model_view = self.water.refraction_raw();
                self.draw_addons(
                    &mut re,
                    &model_view,
                    &reflect_depth,
                    &refracted,
                    camera,
                    Pass::Refraction,
                );
                self.queue.submit(Some(re.finish()));
                self.water.render_edge(
                    &self.device,
                    &self.queue,
                    matrix,
                    plane,
                    bounds,
                    &reflect_depth,
                    self.terrain_gpu.edge_mesh(),
                    (&self.vertex, &self.index, self.terrain_end),
                );
            } else {
                // Client 0x7cca50 renders the sky and the scene above this
                // water height into its refraction target, using air fog.
                let mut above = normal;
                above.clip = [plane, 1., 1., normal.clip[3]];
                // 0x7cca50: the below-surface scene is drawn with air fog even
                // while the camera is underwater.
                let above_fog = crate::fog_matrix::below_surface_fog(crate::fog_matrix::Fog {
                    start: camera.air_fog.start,
                    end: camera.air_fog.end,
                    color: camera.air_fog.color,
                    enabled: true,
                });
                above.fog_range = above_fog.range();
                above.fog_color = above_fog.color4();
                self.queue
                    .write_buffer(&self.reflected_uniform, 0, bytemuck::bytes_of(&above));
                let mut re = self.device.create_command_encoder(&Default::default());
                self.draw_opaque(
                    &mut re,
                    "above-water refraction",
                    &refraction,
                    &reflect_depth,
                    &self.reflected_bind,
                    true,
                    false,
                    // Queued as `above = normal`: the main matrix again.
                    Pass::Refraction,
                    ViewRuns::Main,
                    None,
                    None,
                );
                if let Some(cloud) = &self.cloud {
                    cloud.draw(
                        &self.queue,
                        &mut re,
                        &refraction,
                        &reflect_depth,
                        matrix,
                        camera.yaw,
                        camera.pitch,
                        camera.cloud_detail,
                        daylight,
                        above_fog,
                        above.clip,
                    );
                }
                self.weather.draw(
                    &self.queue,
                    &mut re,
                    &refraction,
                    &reflect_depth,
                    matrix,
                    camera.pos,
                    camera.yaw,
                    camera.pitch,
                    above_fog,
                    above.clip,
                    selected_weather,
                );
                // Ornamental EC models enter refraction only from underwater;
                // the host player follows its own AABB gate in either view.
                let model_view = self.water.refraction_raw();
                if camera.underwater_depth > 0. {
                    self.draw_models(
                        &mut re,
                        &model_view,
                        &reflect_depth,
                        &above,
                        camera,
                        Pass::Refraction,
                        false,
                    );
                }
                self.draw_addons(
                    &mut re,
                    &model_view,
                    &reflect_depth,
                    &above,
                    camera,
                    Pass::Refraction,
                );
                self.water.draw_water(
                    &mut re,
                    scene_target,
                    &scene_depth,
                    &self.vertex,
                    &self.index,
                    batch.start,
                    batch.end,
                );
                self.queue.submit(Some(re.finish()));
                continue;
            }
            let flip = Mat4::from_translation(Vec3::new(0., 2. * plane, 0.))
                * Mat4::from_scale(Vec3::new(1., -1., 1.));
            let reflected = Uniform {
                matrix: (matrix * flip).to_cols_array_2d(),
                direction: [direction.x, -direction.y, direction.z, daylight],
                // The water reflection cancels the main projection's X flip,
                // so the original front-face winding applies again.
                clip: [
                    plane,
                    if camera.pos.y >= plane { 1. } else { -1. },
                    0.,
                    if camera.model_backface_culling {
                        0.
                    } else {
                        1.
                    },
                ],
                bounds,
                tree_bounds,
                sun,
                light,
                region_ambient: frame.region_ambient,
                region_diffuse: frame.region_diffuse,
                region_direction: frame.region_direction,
                grass_eye: [
                    camera.pos.x,
                    camera.pos.y,
                    camera.pos.z,
                    if camera.grass_distance_fade { 1. } else { 0. },
                ],
                grass_wind: frame.grass_wind,
                grass_history: frame.grass_history,
                fog_range: frame.fog_range,
                fog_color: frame.fog_color,
            };
            self.queue
                .write_buffer(&self.reflected_uniform, 0, bytemuck::bytes_of(&reflected));
            self.queue_reflected_runs(matrix * flip);
            let mut e = self.device.create_command_encoder(&Default::default());
            self.draw_opaque(
                &mut e,
                "reflection",
                &reflection,
                &reflect_depth,
                &self.reflected_bind,
                true,
                true,
                // `matrix * flip` mirrors the view, so this submit draws the
                // reflected block set queued just above it.
                Pass::Reflection,
                ViewRuns::Reflected,
                None,
                None,
            );
            if let Some(cloud) = &self.cloud {
                cloud.draw(
                    &self.queue,
                    &mut e,
                    &reflection,
                    &reflect_depth,
                    matrix * flip,
                    camera.yaw,
                    -camera.pitch,
                    camera.cloud_detail,
                    daylight,
                    camera.active_fog(),
                    reflected.clip,
                );
            }
            self.weather.draw(
                &self.queue,
                &mut e,
                &reflection,
                &reflect_depth,
                matrix * flip,
                camera.pos,
                camera.yaw,
                -camera.pitch,
                camera.active_fog(),
                reflected.clip,
                selected_weather,
            );
            let model_view = self.water.reflection_raw();
            self.draw_models(
                &mut e,
                &model_view,
                &reflect_depth,
                &reflected,
                camera,
                Pass::Reflection,
                false,
            );
            self.draw_addons(
                &mut e,
                &model_view,
                &reflect_depth,
                &reflected,
                camera,
                Pass::Reflection,
            );
            self.water.draw_water(
                &mut e,
                scene_target,
                &scene_depth,
                &self.vertex,
                &self.index,
                batch.start,
                batch.end,
            );
            self.queue.submit(Some(e.finish()));
        }
        if let Some(shore) = &self.shore {
            shore.draw(
                &self.device,
                &self.queue,
                scene_target_raw,
                &scene_depth,
                matrix,
                camera.pos,
                1. - daylight,
                frame.elapsed,
                self.resident_bounds(),
                frame.active_fog_range,
                frame.active_fog_color,
            );
        }
        if use_post {
            self.post.render(
                &self.device,
                &self.queue,
                target,
                &self.water.sample_depth(),
                w,
                h,
                frame.elapsed,
                camera.far,
                camera.underwater_depth,
                if camera.underwater_depth > 0. {
                    camera.underwater_fog
                } else {
                    camera.air_fog
                },
                false, // no screen-space fog; scene geometry uses material fog
                false,
                camera.bloom,
                camera.bloom_strength,
                glow_parameters,
                camera.client_gamma,
                camera.gamma_setting,
            );
        }
    }
}
fn water_depth(areas: &[crate::water::WaterArea], pos: Vec3) -> f32 {
    areas
        .iter()
        .filter(|a| a.height > pos.y && a.contains_xz(pos.x, pos.z))
        .map(|a| a.height - pos.y)
        .min_by(|a, b| a.total_cmp(b))
        .unwrap_or(0.)
}
struct Camera {
    pos: Vec3,
    yaw: f32,
    pitch: f32,
    far: f32,
    time_of_day: f32,
    water_reflections: bool,
    /// World-tab switch for the map's own animated effects. The light producers
    /// those roots carry are published either way.
    world_effects: bool,
    /// Retail `SCNRES_EFFECT` distance for the client's effect fade, in world
    /// units; `0` leaves every root at its authored alpha.
    effect_distance: f32,
    atmospheric_fog: bool,
    celestial_discs: bool,
    /// How strongly the discs are drawn; the texture itself by default.
    celestial_brightness: f32,
    /// Viewer switch: draw both discs whatever the area's flag word says.
    ignore_celestial_flags: bool,
    /// World-tab overrides. `Auto` on both is the client's own behaviour.
    weather_override: crate::client_weather::WeatherOverride,
    sky_override: crate::sky_transition::SkyOverride,
    cloud_detail: bool,
    bloom: bool,
    sharpen: bool,
    bloom_strength: f32,
    bloom_mode: u8,
    bloom_tint: u32,
    bloom_brightness_fade: f32,
    bloom_opacity_fade: f32,
    client_gamma: bool,
    gamma_setting: f32,
    underwater_depth: f32,
    underwater_fog: crate::environment::Fog,
    air_fog: crate::environment::Fog,
    terrain_lightmaps: bool,
    /// Terrain day/night blend, published as `fog_range.w` for the terrain
    /// shader. This is the viewer's stand-in for the client's `ps c3.a`
    /// (`env+0x198`): the client derives it from a light matrix the viewer has no
    /// analogue for, so it follows the app clock instead. Kept as its own field
    /// so the native factor can replace the *source* without touching the
    /// shader, the uniform layout or the availability flag in `sun.w`.
    terrain_day_night: f32,
    model_backface_culling: bool,
    tree_wind_strength: f32,
    client_tree_wind_preview: bool,
    /// Native wind has no user amplitude, only the on/off gate the old
    /// "Grass wind" slider now provides.
    grass_wind_enabled: bool,
    /// Viewer-only multiplication of the published wind history. The client
    /// has no such scale; 1.0 is the recovered amplitude.
    grass_wind_amplitude: f32,
    regional_tree_lighting: bool,
    grass_distance_fade: bool,
    area_grass_lightmaps: bool,
    regional_light: Option<crate::environment::RegionalLight>,
    logic_delta_ms: u32,
}
impl Camera {
    fn new(t: &Terrain) -> Self {
        let x = t.width / 2;
        let y = t.height / 2;
        Self {
            pos: Vec3::new(0., t.heights.at(y * t.width + x) + 160., 0.),
            yaw: 0.,
            pitch: -0.15,
            far: 16000.,
            time_of_day: 12.,
            water_reflections: true,
            world_effects: true,
            effect_distance: 0.,
            atmospheric_fog: false,
            celestial_discs: false,
            celestial_brightness: 1.,
            ignore_celestial_flags: false,
            weather_override: crate::client_weather::WeatherOverride::Auto,
            sky_override: crate::sky_transition::SkyOverride::Auto,
            cloud_detail: true,
            bloom: false,
            sharpen: false,
            bloom_strength: 1.,
            bloom_mode: 0,
            bloom_tint: 0xffffffff,
            bloom_brightness_fade: 0.,
            bloom_opacity_fade: 0.,
            client_gamma: false,
            gamma_setting: 41.,
            underwater_depth: 0.,
            underwater_fog: Default::default(),
            air_fog: Default::default(),
            terrain_lightmaps: true,
            terrain_day_night: 0.,
            model_backface_culling: true,
            tree_wind_strength: 0.2,
            client_tree_wind_preview: false,
            grass_wind_enabled: false,
            grass_wind_amplitude: 1.,
            regional_tree_lighting: false,
            grass_distance_fade: true,
            area_grass_lightmaps: true,
            regional_light: None,
            logic_delta_ms: 0,
        }
    }
    fn active_fog(&self) -> crate::fog_matrix::Fog {
        // Main/reflection table fog uses the camera's active regional set; the
        // selection (and its enable rule) lives in the audited fog matrix.
        crate::fog_matrix::camera_fog(
            self.underwater_depth > 0.,
            self.atmospheric_fog,
            crate::fog_matrix::Fog {
                start: self.air_fog.start,
                end: self.air_fog.end,
                color: self.air_fog.color,
                enabled: true,
            },
            crate::fog_matrix::Fog {
                start: self.underwater_fog.start,
                end: self.underwater_fog.end,
                color: self.underwater_fog.color,
                enabled: true,
            },
        )
    }
    fn fog_uniform(&self) -> ([f32; 4], [f32; 4]) {
        let f = self.active_fog();
        (f.range(), f.color4())
    }
    fn region_uniform(&self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        if let Some(l) = self.regional_light.filter(|_| self.regional_tree_lighting) {
            let d = Vec3::from_array(l.toward_sun).normalize_or_zero();
            if d.length_squared() > 0.5 {
                return (
                    [l.ambient[0], l.ambient[1], l.ambient[2], 1.],
                    [l.diffuse[0], l.diffuse[1], l.diffuse[2], 0.],
                    [d.x, d.y, d.z, 0.],
                );
            }
        }
        ([0.; 4], [0.; 4], [0.; 4])
    }
    fn direction(&self) -> Vec3 {
        let (s, c) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(s * cp, sp, -c * cp)
    }
    fn matrix(&self, w: u32, h: u32, far: f32) -> Mat4 {
        // Client world is left-handed; a right-handed view must reverse screen X.
        Mat4::from_scale(Vec3::new(-1., 1., 1.))
            * Mat4::perspective_rh(70f32.to_radians(), w as f32 / h.max(1) as f32, far, 1.)
            * Mat4::look_to_rh(self.pos, self.direction(), Vec3::Y)
    }
}
/// The neutral light a map gets when no environment record answers: full white
/// ambient and diffuse from straight above, which is the every-material default
/// the client's own shaders fall back to. Geometry — and therefore every effect
/// the map places — draws under it instead of being skipped.
fn neutral_regional_light() -> crate::environment::RegionalLight {
    crate::environment::RegionalLight {
        ambient: [1., 1., 1.],
        diffuse: [1., 1., 1.],
        specular: [0., 0., 0.],
        toward_sun: [0., -1., 0.],
        celestial_flags: 0,
    }
}
fn receiving_model_frame_with(
    uniform: &Uniform,
    camera: &Camera,
    light: crate::environment::RegionalLight,
) -> crate::model_draw_gpu::Frame {
    // Authored EC-BSD direction only; the animated celestial curve is not a
    // material-light input for either core world models or add-on characters.
    let d = light.material_direction().travel();
    crate::model_draw_gpu::Frame {
        blend_enabled: false,
        matrix: uniform.matrix,
        lighting: crate::model_material::Lighting {
            environment_ambient: [light.ambient[0], light.ambient[1], light.ambient[2], 1.],
            additional_ambient: [0.; 4],
            directional_diffuse: [light.diffuse[0], light.diffuse[1], light.diffuse[2], 1.],
            point_ambient: [0.; 4],
            point_diffuse: [0.; 4],
            transparency: -1.,
            point_enabled: false,
        },
        direction: d,
        point_position: [0.; 3],
        attenuation: [0., 0.4, 0.],
        eye_direction: uniform.direction[..3].try_into().unwrap(),
        directional_specular: [light.specular[0], light.specular[1], light.specular[2], 1.],
        specular_variant: true,
        vertex_fog: crate::fog_matrix::MODEL_VERTEX_FOG,
        fog_color: uniform.fog_color,
        fog_range: uniform.fog_range,
        clip_plane: [0., uniform.clip[1], 0., -uniform.clip[0] * uniform.clip[1]],
        reverse_front: uniform.clip[2] != 0.,
        alpha_compare: 5,
        alpha_reference: 84. / 255.,
    }
}
fn receiving_model_frame(
    uniform: &Uniform,
    camera: &Camera,
    light: Option<crate::environment::RegionalLight>,
) -> crate::model_draw_gpu::Frame {
    receiving_model_frame_with(
        uniform,
        camera,
        light.unwrap_or_else(neutral_regional_light),
    )
}
async fn adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), String> {
    let a = if let Ok(name) = std::env::var("WGPU_ADAPTER_NAME") {
        let query = name.to_lowercase();
        instance
            .enumerate_adapters(wgpu::Backends::all())
            .into_iter()
            .find(|a| {
                a.get_info().name.to_lowercase().contains(&query)
                    && surface.map_or(true, |s| a.is_surface_supported(s))
            })
            .ok_or_else(|| format!("No compatible GPU adapter contains {name:?}"))?
    } else {
        instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::util::power_preference_from_env()
                    .unwrap_or(wgpu::PowerPreference::HighPerformance),
                compatible_surface: surface,
                force_fallback_adapter: false,
            })
            .await
            .ok_or("No compatible GPU adapter")?
    };
    eprintln!("GPU adapter: {:?}", a.get_info());
    let (d, q) = a
        .request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                // The tree batches are one `multi_draw_indexed_indirect` call
                // when the adapter has these; `FarTrees` falls back to one
                // indirect draw per batch when it does not.
                required_features: a.features()
                    & (wgpu::Features::TEXTURE_COMPRESSION_BC
                        | wgpu::Features::MULTI_DRAW_INDIRECT
                        | wgpu::Features::INDIRECT_FIRST_INSTANCE),
                required_limits: wgpu::Limits::downlevel_defaults().using_resolution(a.limits()),
            },
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok((a, d, q))
}
pub fn run(
    mut t: Terrain,
    #[cfg(windows)] loading: Option<crate::windows_setup::LoadingWindow>,
) -> Result<(), String> {
    #[cfg(windows)]
    crate::windows_setup::loading_progress("Preparing GPU world and textures...".into());
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("Perfect World Explorer — WASD fly, E/Q altitude, arrows look, Shift speed")
            .with_maximized(true)
            .with_visible(false)
            .build(&event_loop)
            .map_err(|e| e.to_string())?,
    );
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::all()),
        ..Default::default()
    });
    let surface = instance
        .create_surface(window.clone())
        .map_err(|e| e.to_string())?;
    let (adapter, device, queue) = pollster::block_on(adapter(&instance, Some(&surface)))?;
    let software_adapter = adapter.get_info().device_type == wgpu::DeviceType::Cpu;
    window.set_title(&format!(
        "Perfect World Explorer — {} — click mouse to look, Esc menu, WASD fly",
        adapter.get_info().name
    ));
    let caps = surface.get_capabilities(&adapter);
    let surface_format = *caps
        .formats
        .iter()
        .find(|x| x.is_srgb())
        .unwrap_or(&caps.formats[0]);
    // Every material shader returns linear color and the render target performs
    // the single encode. When the swapchain has no sRGB format, composition
    // happens in an sRGB intermediate that is encoded once at presentation
    // (`present_encode`); a UNORM swapchain is never written with linear values.
    let format = crate::present_encode::render_format(surface_format);
    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width.max(1), size.height.max(1))
        .ok_or("surface configuration unavailable")?;
    config.format = surface_format;
    if surface_format.is_srgb() {
        config
            .view_formats
            .push(surface_format.remove_srgb_suffix());
    }
    surface.configure(&device, &config);
    let mut camera = Camera::new(&t);
    camera.underwater_fog = t
        .environments
        .underwater(camera.pos.to_array(), camera.time_of_day);
    camera.air_fog = t
        .environments
        .atmosphere(camera.pos.to_array(), camera.time_of_day);
    let mut settings = ViewSettings::load_with_default_fog(t.environments.auto_fog_enabled());
    // The axial card rule is read by the player, not by this loop, so it has to
    // be published before the first effect is built as well as on every change.
    crate::gfx_player::set_shaft_mode(settings.effect_shaft);
    let vegetation_authored_mips = settings.vegetation_authored_mips
        || std::env::var_os("PW_VEGETATION_AUTHORED_MIPS").is_some();
    let mut residency = crate::terrain_residency::Residency::default();
    let m = build(
        &mut t,
        camera.pos.x,
        camera.pos.z,
        settings.detail,
        &mut residency,
        vegetation_authored_mips,
    )?;
    let mut renderer = Renderer::new(
        device,
        queue,
        format,
        config.width,
        config.height,
        m,
        &mut t,
        vegetation_authored_mips,
    )?;
    // Resolution scaling: the world renders into an offscreen target at a
    // scaled extent and is bilinearly blitted to the full-size composition
    // view each frame. `applied_extent` is tracked so a window resize or a
    // slider change reallocates the offscreen and world targets exactly once.
    let max_texture_dimension = renderer.device.limits().max_texture_dimension_2d;
    let mut applied_extent = crate::render_scale::scaled_extent(
        config.width,
        config.height,
        settings.resolution_scale,
        max_texture_dimension,
    );
    let mut render_scale = crate::render_scale::RenderScale::new(
        &renderer.device,
        format,
        applied_extent.0,
        applied_extent.1,
    );
    if applied_extent != (config.width, config.height) {
        resize_world_targets(&mut renderer, applied_extent.0, applied_extent.1);
    }
    let mut display_gamma: Option<crate::display_gamma::DisplayGamma> = None;
    // Only allocated for UNORM-only swapchains; None is the normal sRGB route.
    let mut present_encode: Option<crate::present_encode::PresentEncode> =
        if crate::color_chain::present_encode_required(surface_format.is_srgb()) {
            Some(crate::present_encode::PresentEncode::new(
                &renderer.device,
                surface_format,
                config.width,
                config.height,
            ))
        } else {
            None
        };
    let mut music = crate::music::MusicPlayer::new(&t.element);
    let mut ambience = crate::music::AmbiencePlayer::new(&t.element);
    let mut world_audio = crate::world_audio::WorldAudio::new(&t.element, &t.scene);
    #[cfg(windows)]
    drop(loading);
    window.set_visible(true);
    let water_areas = t.water.clone();
    let environments = t.environments.clone();
    let mut fog_transition =
        crate::environment::FogTransition::new(&environments, camera.pos.to_array());
    let mut regional_streaming = environments.streaming();
    let mut streamed_transition = regional_streaming.as_mut().map(|active| {
        crate::environment::FogTransition::new_streamed(
            &environments,
            active,
            camera.pos.to_array(),
        )
    });
    let whole_element = t.element.clone();
    let map_space = crate::navigation::MapSpace {
        width: t.width,
        height: t.height,
        spacing: t.grid_spacing,
        heights: t.heights.clone(),
    };
    let whole_ornaments = t.objects.ornaments.clone();
    renderer.modules = crate::modules::Modules::new(
        whole_element.clone(),
        map_space.clone(),
        water_areas.clone(),
        &t.scene,
        whole_ornaments.clone(),
    );
    // Add-ons get read access to the world's effect catalogue the moment the
    // world has one: the same grouped index the World tab's census uses, so a
    // module can look up what the map ships without opening the archive
    // itself. Playing an effect stays the host's job for now.
    if let Some(models) = &mut renderer.models {
        renderer
            .modules
            .set_effect_catalog(models.effect_catalog().clone());
        // The world's effects drape their ground sheets over this grid
        // (`A3DDecalEx`'s ground branch), and the renderer is the only thing
        // that has it. The grid is shared, so its copy is a refcount bump.
        let heights = map_space.clone();
        models.set_terrain(move |x: f32, z: f32| Some(heights.ground(x, z)));
    }
    let destinations =
        crate::navigation::destinations(&whole_element.join("maps/world/world_targets.txt"));
    let mut world_rx: Option<std::sync::mpsc::Receiver<crate::world_objects::Message>> = None;
    let mut world_cancel: Option<Arc<std::sync::atomic::AtomicBool>> = None;
    let mut world_enabled = false;
    let mut world_status = String::from("Off");
    // The expensive atlas and scene rebuild runs off the UI thread. At most one
    // request is in flight; once it completes, the latest camera position is used.
    let (width, height, spacing) = (t.width, t.height, t.grid_spacing);
    let (request_tx, request_rx) = std::sync::mpsc::channel::<(f32, f32, DetailSettings)>();
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel::<Result<MeshData, String>>(1);
    std::thread::Builder::new()
        .name("world-streaming".into())
        .spawn(move || {
            let mut residency = crate::terrain_residency::Residency::default();
            while let Ok((x, z, detail)) = request_rx.recv() {
                let started = Instant::now();
                let result = build(
                    &mut t,
                    x,
                    z,
                    detail,
                    &mut residency,
                    vegetation_authored_mips,
                );
                eprintln!(
                    "Background terrain build at ({x:.1},{z:.1}) took {:.2}s",
                    started.elapsed().as_secs_f32()
                );
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| format!("cannot start world streaming thread: {e}"))?;
    let egui_ctx = egui::Context::default();
    let mut egui_state = egui_winit::State::new(
        egui_ctx.clone(),
        egui::ViewportId::ROOT,
        &*window,
        Some(window.scale_factor() as f32),
        None,
    );
    let mut egui_renderer = egui_wgpu::Renderer::new(&renderer.device, format, None, 1);
    let map_texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("live minimap"),
        size: wgpu::Extent3d {
            width: 320,
            height: 320,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    // The client's minimap tiles are 2 world units per texel (`original_minimap`
    // composites them at their native 256px). A 512-unit view - twice the default
    // half-width - is therefore 256 texels, so a 256px HUD shows the artwork one
    // texel per screen pixel instead of rescaling it.
    const MINIMAP_HUD: f32 = 256.;
    let map_view = map_texture.create_view(&Default::default());
    let map_depth_tex = depth(&renderer.device, 320, 320);
    let map_depth = map_depth_tex.create_view(&Default::default());
    let map_id = egui_renderer.register_native_texture(
        &renderer.device,
        &map_view,
        wgpu::FilterMode::Linear,
    );
    let full_map_texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("full-world overhead map"),
        size: wgpu::Extent3d {
            width: 1024,
            height: 1408,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let full_map_view = full_map_texture.create_view(&Default::default());
    let full_depth_tex = depth(&renderer.device, 1024, 1408);
    let full_map_depth = full_depth_tex.create_view(&Default::default());
    let full_map_id = egui_renderer.register_native_texture(
        &renderer.device,
        &full_map_view,
        wgpu::FilterMode::Linear,
    );
    // Client map artwork, loaded on demand in a worker thread. The world map and
    // the minimap come from different asset sets (`surfaces/maps/` and
    // `surfaces/minimaps/world/`), so each keeps its own texture and its own
    // request flag.
    let mut original_map_id: Option<egui::TextureHandle> = None;
    let mut original_map_error: Option<String> = None;
    let mut original_map_rx: Option<std::sync::mpsc::Receiver<Result<image::RgbaImage, String>>> =
        None;
    let mut original_map_requested = false;
    let mut original_minimap_id: Option<egui::TextureHandle> = None;
    let mut original_minimap_error: Option<String> = None;
    let mut original_minimap_rx: Option<
        std::sync::mpsc::Receiver<Result<image::RgbaImage, String>>,
    > = None;
    let mut original_minimap_requested = false;
    let mut map_click_teleport = false;
    let mut map_zoom = 1.0f32;
    let mut map_refresh = Instant::now() - std::time::Duration::from_secs(10);
    let mut teleport_query = String::new();
    let ui_zones = crate::music::Zones::open(&whole_element.join("maps/world/precinct.clt")).ok();
    let mut last_zone: Option<usize> = None;
    let mut zone_initialized = false;
    let mut zone_check = Instant::now() - std::time::Duration::from_secs(1);
    let mut zone_banner: Option<(String, Instant)> = None;
    let mut settings_tab = 0usize;
    let mut fullscreen_applied = false;
    let mut menu_page = MenuPage::Main;
    let mut streaming = false;
    let mut keys = HashSet::new();
    let mut captured = false;
    let mut saved_settings = settings.clone();
    let mut last_settings_save = Instant::now();
    let mut last = Instant::now();
    let mut region_tick_ms = renderer.started.elapsed().as_millis() as u64;
    event_loop
        .run(move |event, target| match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => {
                if let WindowEvent::KeyboardInput {ref event,..}=event {
                    if event.physical_key == PhysicalKey::Code(KeyCode::Escape) && event.state == ElementState::Pressed && !event.repeat {
                        menu_page=menu_page.escape();keys.clear();
                        if captured {let _=window.set_cursor_grab(CursorGrabMode::None);window.set_cursor_visible(true);captured=false;}
                        if input_debug() {eprintln!("Escape menu: {:?}",menu_page);}
                        return;
                    }
                }
                if let WindowEvent::KeyboardInput {ref event,..}=event {
                    // F8 marks the spot: the pose goes into session.log and into
                    // every channel that is on, so a report has a position and a
                    // time instead of a description of one.
                    if event.physical_key==PhysicalKey::Code(KeyCode::F8) && event.state==ElementState::Pressed && !event.repeat {
                        crate::diagnostics::mark(format!("camera ({:.1}, {:.1}, {:.1}) yaw {:.3} pitch {:.3} time {:.2}", camera.pos.x, camera.pos.y, camera.pos.z, camera.yaw, camera.pitch, camera.time_of_day));
                    }
                }
                if let WindowEvent::KeyboardInput {ref event,..}=event {
                    if event.physical_key==PhysicalKey::Code(KeyCode::KeyM) && event.state==ElementState::Pressed && !event.repeat && (menu_page!=MenuPage::Teleports) {
                        menu_page=if menu_page==MenuPage::WorldMap {MenuPage::Closed} else {MenuPage::WorldMap};
                        keys.clear(); map_refresh=Instant::now()-std::time::Duration::from_secs(10);
                        if captured {let _=window.set_cursor_grab(CursorGrabMode::None);window.set_cursor_visible(true);captured=false;}
                        return;
                    }
                }
                // Track keys before the menu's early return. Walker hotkey
                // rebinding runs while movement input is disabled, so discarding
                // these events here leaves its capture on "Press a key" forever.
                if let WindowEvent::KeyboardInput { ref event, .. } = event {
                    if let PhysicalKey::Code(k) = event.physical_key {
                        if event.state == ElementState::Pressed {
                            keys.insert(k);
                        } else {
                            keys.remove(&k);
                        }
                    }
                }
                // egui must receive focus changes even while the menu is closed;
                // otherwise it believes the window is unfocused and ignores clicks.
                if menu_page.is_open() || matches!(event,WindowEvent::Focused(_)) {
                    let response=egui_state.on_window_event(&window,&event);
                    if menu_page.is_open() && (response.consumed || matches!(event,WindowEvent::KeyboardInput{..})) && !matches!(event, WindowEvent::RedrawRequested | WindowEvent::Resized(_) | WindowEvent::CloseRequested | WindowEvent::Focused(_)) {return;}
                }
                match event {
                WindowEvent::CloseRequested => { if let Err(e)=settings.save(){eprintln!("Settings save: {e}");} target.exit(); },
                WindowEvent::KeyboardInput { .. } => {}
                WindowEvent::MouseWheel { delta, .. } if captured && !menu_page.is_open() => {
                    let steps = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(position) => position.y as f32 / 120.0,
                    };
                    renderer.modules.walker_zoom(steps);
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } if !captured && !menu_page.is_open() => {
                    let result = window
                        .set_cursor_grab(CursorGrabMode::Locked)
                        .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
                    if result.is_ok() {
                        window.set_cursor_visible(false);
                        captured = true;
                        if input_debug() {
                            eprintln!("Mouse captured");
                        }
                    } else {
                        eprintln!("Could not capture mouse: {:?}", result.err());
                    }
                }
                WindowEvent::Focused(false) if captured => {
                    let _ = window.set_cursor_grab(CursorGrabMode::None);
                    window.set_cursor_visible(true);
                    captured = false;
                    keys.clear();
                }
                WindowEvent::Resized(s) if s.width > 0 && s.height > 0 => {
                    config.width = s.width;
                    config.height = s.height;
                    surface.configure(&renderer.device, &config);
                    // Scaled world targets are reallocated by the next redraw,
                    // which notices the new window extent (see below).
                    if let Some(gamma)=display_gamma.as_mut() {gamma.resize(&renderer.device,s.width,s.height);}
                    if let Some(encode)=present_encode.as_mut() {encode.resize(&renderer.device,s.width,s.height);}
                }
                WindowEvent::RedrawRequested => {
                    // One place handles both a window resize and a Graphics-tab
                    // scale change: reallocate only when the scaled extent moves.
                    let wanted_extent = crate::render_scale::scaled_extent(config.width, config.height, settings.resolution_scale, max_texture_dimension);
                    if wanted_extent != applied_extent {
                        applied_extent = wanted_extent;
                        resize_world_targets(&mut renderer, wanted_extent.0, wanted_extent.1);
                        render_scale.resize(&renderer.device, wanted_extent.0, wanted_extent.1);
                    }
                    if settings != saved_settings && last_settings_save.elapsed().as_secs_f32() >= 1. {
                        if let Err(e)=settings.save(){eprintln!("Settings save: {e}");} else {saved_settings=settings.clone();}
                        last_settings_save=Instant::now();
                    }
                    let dt = last.elapsed().as_secs_f32().min(0.1);
                    last = Instant::now();
                    // Add-ons get an optional camera-owner hook. `None` leaves
                    // the original free-flight controls completely untouched.
                    let addon_camera = renderer.modules.update(dt, !menu_page.is_open(), &keys, camera.yaw, camera.pitch, camera.pos);
                    renderer.modules.update_walker_audio(settings.effects_enabled, settings.effects_volume);
                    if let Some(position) = addon_camera {
                        camera.pos = position;
                    } else {
                        let speed = if menu_page.is_open() { 0. } else if keys.contains(&KeyCode::ShiftLeft) {
                            settings.camera_speed * settings.boost
                        } else {
                            settings.camera_speed
                        };
                        let (s, c) = camera.yaw.sin_cos();
                        let f = Vec3::new(s, 0., -c);
                        let r = Vec3::new(-c, 0., -s);
                        if keys.contains(&KeyCode::KeyW) {
                            camera.pos += f * speed * dt
                        }
                        if keys.contains(&KeyCode::KeyS) {
                            camera.pos -= f * speed * dt
                        }
                        if keys.contains(&KeyCode::KeyA) {
                            camera.pos -= r * speed * dt
                        }
                        if keys.contains(&KeyCode::KeyD) {
                            camera.pos += r * speed * dt
                        }
                        if keys.contains(&KeyCode::KeyE) || keys.contains(&KeyCode::Space) {
                            camera.pos.y += speed * dt
                        }
                        if keys.contains(&KeyCode::KeyQ) || keys.contains(&KeyCode::ControlLeft) {
                            camera.pos.y -= speed * dt
                        }
                    }
                    if keys.contains(&KeyCode::ArrowLeft) {
                        camera.yaw += dt
                    }
                    if keys.contains(&KeyCode::ArrowRight) {
                        camera.yaw -= dt
                    }
                    if keys.contains(&KeyCode::ArrowUp) {
                        camera.pitch = (camera.pitch + dt).min(1.5)
                    }
                    if keys.contains(&KeyCode::ArrowDown) {
                        camera.pitch = (camera.pitch - dt).max(-1.5)
                    }
                    if streaming {
                        match result_rx.try_recv() {
                            Ok(Ok(mesh)) => {
                                if let Err(e) = renderer.update_mesh(mesh) {
                                    eprintln!("Terrain residency upload failed: {e}");
                                    target.exit(); return;
                                }
                                streaming = false;
                            }
                            Ok(Err(e)) => {
                                eprintln!("Streaming error: {e}");
                                target.exit();
                                return;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => {}
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                eprintln!("World streaming worker exited");
                                target.exit();
                                return;
                            }
                        }
                    }
                    // `0x77e360` runs for every resident block on every frame and
                    // the window origin follows the camera's block, so re-select
                    // both per frame and rebuild only when the drawn mesh would
                    // differ. The in-flight flag rate-limits the rebuilds.
                    let aim = if streaming {
                        None
                    } else {
                        let (win, levels) = lod_levels(
                            width,
                            height,
                            spacing,
                            [camera.pos.x, camera.pos.z],
                            settings.detail.radius,
                        );
                        Some(LodState {
                            window: win,
                            levels,
                        })
                    };
                    let stale = match &aim {
                        Some(aim) => aim != &renderer.lod || renderer.detail != settings.detail,
                        None => false,
                    };
                    if stale {
                        if request_tx.send((camera.pos.x, camera.pos.z, settings.detail)).is_err() {
                            eprintln!("World streaming worker exited");
                            target.exit();
                            return;
                        }
                        streaming = true;
                    }
                    music.update(settings.music_enabled,settings.music_volume,camera.pos.x,camera.pos.z);
                    ambience.update(settings.ambience_enabled,settings.sound_volume,camera.pos.x,camera.pos.z,crate::environment::daylight(settings.time_of_day)<0.5);
                    world_audio.update(settings.effects_enabled,settings.effects_volume,camera.pos.to_array(),camera.yaw,crate::environment::night_factor(settings.time_of_day));
                    if zone_check.elapsed()>=std::time::Duration::from_millis(350){
                        zone_check=Instant::now();let zone=ui_zones.as_ref().and_then(|z|z.find(camera.pos.x,camera.pos.z));
                        if (!zone_initialized || zone!=last_zone) && settings.show_zone_entry {if let Some(i)=zone {if let Some(db)=&ui_zones {let name=db.zones[i].name.trim();if !name.is_empty(){zone_banner=Some((name.to_string(),Instant::now()));}}}}
                        zone_initialized=true;last_zone=zone;
                    }
                    if !settings.show_zone_entry {zone_banner=None;}
                    if settings.fullscreen!=fullscreen_applied {window.set_fullscreen(if settings.fullscreen{Some(Fullscreen::Borderless(window.current_monitor()))}else{None});fullscreen_applied=settings.fullscreen;}

                    let want_world=settings.whole_world && !settings.detail.terrain_cutoff && !renderer.detail.terrain_cutoff;
                    if want_world != world_enabled {
                        world_enabled=want_world;
                        if let Some(c)=world_cancel.take(){c.store(true,std::sync::atomic::Ordering::Relaxed);}
                        world_rx=None;renderer.world_pages.clear();renderer.world_draw_cmds=Default::default();renderer.world_done=false;
                        if world_enabled {
                            let (tx,rx)=std::sync::mpsc::sync_channel(1);let cancel=Arc::new(std::sync::atomic::AtomicBool::new(false));
                            let c=cancel.clone();let root=whole_element.clone();let ornaments=whole_ornaments.clone();
                            std::thread::Builder::new().name("whole-world-objects".into()).spawn(move ||crate::world_objects::build(root,ornaments,tx,c)).expect("world preload thread");
                            world_rx=Some(rx);world_cancel=Some(cancel);world_status=String::from("Loading world objects...");
                        } else {world_status=String::from("Off");}
                    }
                    if let Some(rx)=&world_rx {match rx.try_recv(){
                        Ok(crate::world_objects::Message::Page(page))=>{
                            let processed=page.processed;
                            let uploaded=crate::world_objects::GpuPage::upload_world(&renderer.device,&renderer.queue,&renderer.layout,&renderer.sampler,&renderer.uniform,&renderer.reflected_uniform,&renderer.world_pages,page);
                            renderer.world_pages.push(uploaded);world_status=format!("Loading: {processed}/{} objects, {} GPU chunks",whole_ornaments.len(),renderer.world_pages.len());
                            eprintln!("{world_status}");
                        },
                        Ok(crate::world_objects::Message::Done{processed,skipped,stats})=>{renderer.world_draw_cmds=crate::world_objects::GpuPage::world_commands(&renderer.world_pages);renderer.world_done=true;world_status=format!("Loaded {processed} objects; {skipped} meshes use fallback texture; {} geometry chunks, {} shared atlases ({} MiB); peak decoded-texture residency {} MiB of {} MiB budget, {} evictions, {} dedup alias hits",stats.pages,stats.atlas_pages,stats.atlas_bytes>>20,stats.residency.peak_resident_bytes>>20,stats.residency.budget_bytes>>20,stats.residency.evictions,stats.residency.alias_hits);eprintln!("{world_status}");},
                        Ok(crate::world_objects::Message::Error(e))=>{world_status=format!("World load failed: {e}");eprintln!("{world_status}");},
                        Err(_)=>{}
                    }}
                    camera.far = if world_enabled {settings.far.max(32000.)} else {settings.far};
                    // Time pass: the client's own clock, replayed on wall-clock
                    // time. `sky_time::advance` is 1/600 authored hour per real
                    // second at 1x, which is `CECSunMoon::Tick`'s
                    // 1/14,400,000-per-millisecond day fraction.
                    if settings.time_pass_enabled {
                        settings.time_of_day = crate::sky_time::advance(
                            settings.time_of_day,
                            dt,
                            settings.time_pass_speed,
                        );
                    }
                    // The decoded-texture caches read this switch, not the
                    // settings struct, so keep it in step with the checkbox: the
                    // whole-world preload and later streamed batches honour it
                    // without a restart. (The terrain half is decided when the
                    // world loads.)
                    crate::objects::set_unbounded_textures(settings.all_to_vram);
                    camera.time_of_day = settings.time_of_day;
                    camera.tree_wind_strength=settings.tree_wind_strength;camera.client_tree_wind_preview=settings.client_tree_wind_preview;camera.grass_wind_enabled=settings.grass_wind_enabled;camera.grass_wind_amplitude=settings.grass_wind_amplitude;
                    camera.regional_tree_lighting=settings.regional_tree_lighting;
                    camera.grass_distance_fade=settings.grass_distance_fade;
                    camera.area_grass_lightmaps=settings.area_grass_lightmaps;
                    camera.water_reflections=settings.water_reflections;
                    camera.world_effects=settings.world_effects;
                    camera.effect_distance=settings.effect_distance;
                    crate::gfx_player::set_shaft_mode(settings.effect_shaft);
                    camera.atmospheric_fog=settings.atmospheric_fog;
                    camera.celestial_discs=settings.celestial_discs;
                    camera.celestial_brightness=settings.celestial_brightness;
                    camera.ignore_celestial_flags=settings.ignore_celestial_flags;
                    camera.weather_override=settings.weather_override;
                    camera.sky_override=settings.sky_override;
                    camera.cloud_detail=settings.cloud_detail;
                    camera.bloom=settings.bloom;
                    camera.bloom_strength=settings.bloom_strength;
                    camera.bloom_mode=settings.bloom_mode;
                    camera.bloom_tint=settings.bloom_tint;
                    camera.bloom_brightness_fade=settings.bloom_brightness_fade;
                    camera.bloom_opacity_fade=settings.bloom_opacity_fade;
                    camera.sharpen=settings.sharpen;
                    let whole_frame_gamma=crate::display_gamma::enabled(settings.client_gamma, settings.fullscreen);
                    let frame_gamma_setting=settings.gamma_setting;
                    if whole_frame_gamma && display_gamma.is_none() {display_gamma=Some(crate::display_gamma::DisplayGamma::new(&renderer.device,format,config.width,config.height));}
                    if !whole_frame_gamma {display_gamma=None;}
                    // Retail skips the display ramp in windowed mode; in fullscreen it
                    // belongs after bloom, maps and UI, never in the scene-only pass.
                    // `color_chain` makes the mutually exclusive ownership testable.
                    camera.client_gamma=crate::color_chain::scene_chain_ramp(false,whole_frame_gamma);
                    camera.gamma_setting=settings.gamma_setting;
                    camera.terrain_lightmaps=settings.terrain_lightmaps;
                    // The blend source is the app clock until the client's light
                    // matrix is reconstructed: full daylight is the day endpoint
                    // (factor 0) and full night the night endpoint (factor 1).
                    camera.terrain_day_night=crate::terrain_shading::viewer_day_night_factor(
                        crate::environment::daylight(camera.time_of_day));
                    camera.model_backface_culling=settings.model_backface_culling;
                    camera.underwater_depth=water_depth(&water_areas,camera.pos);
                    let region_tick = renderer.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                    let region_delta = crate::environment::frame_ms(region_tick.saturating_sub(region_tick_ms));
                    region_tick_ms = region_tick;
                    camera.logic_delta_ms = region_delta;
                    let legacy_fog=fog_transition.update_ms(&environments,camera.pos.to_array(),camera.time_of_day,region_delta);
                    let streamed_fog=match (streamed_transition.as_mut(),regional_streaming.as_mut()) {
                        (Some(transition),Some(active))=>Some(transition.update_streamed_ms(&environments,active,camera.pos.to_array(),camera.time_of_day,region_delta)),
                        _=>None,
                    };
                    if settings.streamed_regions && streamed_fog.is_some() {
                        (camera.air_fog,camera.underwater_fog)=streamed_fog.unwrap();
                        camera.regional_light=streamed_transition.as_ref().and_then(|t|t.lighting(camera.time_of_day));
                    } else {
                        (camera.air_fog,camera.underwater_fog)=legacy_fog;
                        camera.regional_light=fog_transition.lighting(camera.time_of_day);
                    }
                    if menu_page==MenuPage::WorldMap && settings.world_map_style==MapStyle::Original && !original_map_requested {
                        original_map_requested=true;
                        let root=whole_element.clone();let (tx,rx)=std::sync::mpsc::channel();
                        std::thread::spawn(move || {let _=tx.send(crate::navigation::original_map(&root));});
                        original_map_rx=Some(rx);
                    }
                    if settings.minimap_style==MapStyle::Original && !original_minimap_requested {
                        original_minimap_requested=true;
                        let root=whole_element.clone();let (tx,rx)=std::sync::mpsc::channel();
                        std::thread::spawn(move || {let _=tx.send(crate::navigation::original_minimap(&root));});
                        original_minimap_rx=Some(rx);
                    }
                    if let Some(rx)=&original_map_rx {if let Ok(result)=rx.try_recv(){
                        match result { Ok(img)=>{let size=[img.width() as usize,img.height() as usize];let pixels=img.into_raw();let color=egui::ColorImage::from_rgba_unmultiplied(size,&pixels);original_map_id=Some(egui_ctx.load_texture("original world map",color,egui::TextureOptions::LINEAR));},Err(e)=>{eprintln!("Original map: {e}");original_map_error=Some(e);}}
                        original_map_rx=None;
                    }}
                    if let Some(rx)=&original_minimap_rx {if let Ok(result)=rx.try_recv(){
                        match result { Ok(img)=>{let size=[img.width() as usize,img.height() as usize];let pixels=img.into_raw();let color=egui::ColorImage::from_rgba_unmultiplied(size,&pixels);original_minimap_id=Some(egui_ctx.load_texture("original minimap",color,egui::TextureOptions::LINEAR));},Err(e)=>{eprintln!("Original minimap: {e}");original_minimap_error=Some(e);}}
                        original_minimap_rx=None;
                    }}
                    match surface.get_current_texture() {
                        Ok(frame) => {
                            let surface_view = frame.texture.create_view(&Default::default());
                            // Composition target: the whole-frame gamma intermediate, else the
                            // present-encode intermediate, else the swapchain itself. This is
                            // always the full window resolution: UI and the final present chain
                            // stay sharp regardless of the 3D resolution scale.
                            let composed_view=if whole_frame_gamma {Some(display_gamma.as_ref().unwrap().view())} else if let Some(encode)=present_encode.as_ref() {Some(encode.view())} else {None};
                            let scene_view=composed_view.as_ref().unwrap_or(&surface_view);
                            // Draw the world into the scaled offscreen target, then bilinearly
                            // present it into `scene_view` before the UI pass loads that result.
                            let world_view = render_scale.view();
                            let world_raw_view = render_scale.raw_view();
                            renderer.render(
                                &world_view,
                                &world_raw_view,
                                camera.matrix(config.width, config.height, camera.far),
                                &camera,
                                applied_extent.0,
                                applied_extent.1,
                            );
                            render_scale.blit(&renderer.device, &renderer.queue, scene_view);
                            // Refresh the top-down view on every presented frame. The old
                            // 350ms timer capped minimap motion to fewer than 3 FPS.
                            // The realistic minimap is a live top-down render; the
                            // original style draws client artwork instead, so the
                            // overhead pass is skipped rather than rendered and
                            // hidden.
                            if settings.show_minimap && settings.minimap_style==MapStyle::Realistic {
                                renderer.render_minimap(&map_view,&map_depth,&camera,settings.minimap_range,false);
                            }
                            if menu_page==MenuPage::WorldMap && settings.world_map_style==MapStyle::Realistic && map_refresh.elapsed()>=std::time::Duration::from_secs(2) {
                                renderer.render_minimap(&full_map_view,&full_map_depth,&camera,0.,true);
                                map_refresh=Instant::now();
                            }
                            renderer.modules.prepare_librarian_preview(&renderer.device, &renderer.queue, &mut egui_renderer, format, menu_page==MenuPage::Modules);
                            if menu_page.is_open() || settings.show_minimap || renderer.modules.overlay_active() || zone_banner.as_ref().is_some_and(|(_,t)|t.elapsed().as_secs_f32()<4.) {
                                let input=egui_state.take_egui_input(&window);
                                let output=egui_ctx.run(input,|ctx| {
                                    if menu_page==MenuPage::Main {
                                        egui::Window::new("PERFECT WORLD EXPLORER  |  ESC MENU")
                                            .default_pos(egui::pos2(16.,16.)).default_size(egui::vec2(380.,330.)).min_size(egui::vec2(280.,260.)).collapsible(false).resizable(true).vscroll(true)
                                            .show(ctx,|ui|{
                                                ui.set_min_height(ui.available_height());
                                                ui.add_space(8.);ui.vertical_centered(|ui|{
                                                    ui.label(egui::RichText::new("WORLD EXPLORER").size(24.).strong());
                                                    ui.label(egui::RichText::new("EXPLORER  •  BUILD 2026.11").small().color(egui::Color32::LIGHT_BLUE));ui.add_space(14.);
                                                    let button_width=ui.available_width().min(320.);
                                                    if ui.add_sized([button_width,42.],egui::Button::new(egui::RichText::new("RESUME EXPLORING").size(17.))).clicked(){menu_page=MenuPage::Closed;}
                                                    ui.add_space(8.);
                                                    if ui.add_sized([button_width,52.],egui::Button::new(egui::RichText::new("SETTINGS").size(22.).strong()).fill(egui::Color32::from_rgb(38,83,116))).clicked(){settings_tab=0;menu_page=MenuPage::Settings;}
                                                    ui.add_space(8.);
                                                    if ui.add_sized([button_width,42.],egui::Button::new(egui::RichText::new("MODULES").size(18.).strong())).clicked(){menu_page=MenuPage::Modules;}
                                                    ui.add_space(8.);
                                                    if ui.add_sized([button_width,40.],egui::Button::new(egui::RichText::new("TELEPORT LIST").size(17.))).clicked(){menu_page=MenuPage::Teleports;}
                                                    ui.add_space(8.);
                                                    if ui.add_sized([button_width,40.],egui::Button::new(egui::RichText::new("WORLD MAP  [M]").size(17.))).clicked(){menu_page=MenuPage::WorldMap;map_refresh=Instant::now()-std::time::Duration::from_secs(10);}
                                                    ui.add_space(8.);
                                                    if ui.add_sized([button_width,36.],egui::Button::new("Exit Explorer")).clicked(){if let Err(e)=settings.save(){eprintln!("Settings save: {e}");}target.exit();}
                                                });
                                            });
                                    }
                                    if menu_page==MenuPage::Modules {
                                        if renderer.modules.menu_ui(ctx,camera.pos,camera.yaw){menu_page=MenuPage::Main;}
                                    }
                                    if menu_page==MenuPage::Settings {
                                        egui::Window::new("SETTINGS  |  ESC to go back")
                                            .default_pos(egui::pos2(16.,16.)).collapsible(false).resizable(true)
                                            .default_size(egui::vec2(490.,460.)).min_size(egui::vec2(310.,250.)).max_height(config.height as f32 / window.scale_factor() as f32-35.).vscroll(true)
                                            .show(ctx,|ui| {
                                                ui.horizontal_wrapped(|ui|{
                                                    let tab_width=((ui.available_width()-30.)/5.).clamp(70.,140.);
                                                    for (idx,name) in ["GRAPHICS","WORLD","SOUND","INTERFACE","LOGGING"].iter().enumerate(){
                                                        if ui.add_sized([tab_width,36.],egui::SelectableLabel::new(settings_tab==idx,*name)).clicked(){settings_tab=idx;}
                                                    }
                                                });ui.separator();ui.add_space(4.);
                                                let defaults=ViewSettings::default();
                                                ui.label(egui::RichText::new("Gold tick and label show the default value").small().weak());
                                                match settings_tab {
                                                    0=>{ui.heading("Graphics");marked_slider(ui,egui::Slider::new(&mut settings.resolution_scale,10.0..=200.0).integer().suffix("%"),"Resolution scale",defaults.resolution_scale,10.,200.,false,true);ui.label(egui::RichText::new("3D render resolution (10%–200% of the window). UI stays at native resolution.").small().weak());ui.checkbox(&mut settings.all_to_vram,"All to VRAM (maximum speed, uses more memory)").on_hover_text("Keep the whole world resident instead of paging it. Restores the pre-optimisation residency as an option: the complete terrain height/normal grid stays decoded in memory (no disk paging, no per-block decode), the decoded scenery/model texture caches stop evicting, and the whole-world, tree and model preloads keep every page they build. Load time, block-window rebuilds and revisiting areas are much faster; steady-state memory is much higher (the recovered whole-world texture corpus is about 5.4 GiB of sources before geometry, terrain and models). The terrain half is applied when the world loads, so a change takes effect after Load World; the in-viewer caches follow the checkbox live. Leave it off to keep the bounded paged caches, which is the default.");ui.checkbox(&mut settings.fullscreen,"Fullscreen (borderless)");ui.checkbox(&mut settings.bloom,"Bloom");ui.checkbox(&mut settings.sharpen,"Client fullscreen sharpen");marked_slider(ui,egui::Slider::new(&mut settings.bloom_strength,0.0..=4.0),"Bloom strength",defaults.bloom_strength,0.,4.,false,settings.bloom);egui::ComboBox::from_label("Bloom mode").selected_text(match settings.bloom_mode {4=>"Client special 4 (mono)",5=>"Client special 5 (low glow)",_=>"Automatic"}).show_ui(ui,|ui| {ui.selectable_value(&mut settings.bloom_mode,0,"Automatic");ui.selectable_value(&mut settings.bloom_mode,4,"Client special 4 (mono)");ui.selectable_value(&mut settings.bloom_mode,5,"Client special 5 (low glow)");});ui.label("Client glow caller tint / opacity (preview)");let mut tint=[((settings.bloom_tint>>16)&255)as f32/255.,((settings.bloom_tint>>8)&255)as f32/255.,(settings.bloom_tint&255)as f32/255.,(settings.bloom_tint>>24)as f32/255.];if ui.color_edit_button_rgba_unmultiplied(&mut tint).changed() {let c=tint.map(|v|(v.clamp(0.,1.)*255.).round()as u32);settings.bloom_tint=(c[3]<<24)|(c[0]<<16)|(c[1]<<8)|c[2];}marked_slider(ui,egui::Slider::new(&mut settings.bloom_brightness_fade,0.0..=1.0),"Glow fade target (1s transition)",0.,0.,1.,false,settings.bloom);marked_slider(ui,egui::Slider::new(&mut settings.bloom_opacity_fade,0.0..=1.0),"Glow opacity fade (preview)",0.,0.,1.,false,settings.bloom);ui.checkbox(&mut settings.client_gamma,"Client gamma (fullscreen only)");marked_slider(ui,egui::Slider::new(&mut settings.gamma_setting,0.0..=100.0),"Gamma",defaults.gamma_setting,0.,100.,false,settings.client_gamma);}
                                                    1=>{ui.heading("World");ui.checkbox(&mut settings.detail.terrain_cutoff,"Original-game terrain cutoff (saves memory)").on_hover_text("Live, saved terrain and scenery cutoff. Keeps the block window below; releases distant terrain geometry and unused blend-mask atlas pages. Combine independently with Client atmospheric fog. Fog keeps the authored colour and caps its distance to the resident window (viewer adaptation). Distant scenery preload is paused; global lightmaps and shared tree assets still have memory costs; realistic maps show only resident terrain.");ui.label(renderer.terrain_gpu.residency_label());let (trees,bytes)=renderer.far_trees.residency_stats();ui.label(format!("Tree LOD instances: {trees}; instance buffer {:.1} KiB",bytes as f64/1024.));ui.add_enabled(!settings.detail.terrain_cutoff,egui::Checkbox::new(&mut settings.whole_world,"Render whole world (trees, rocks, buildings)")).on_hover_text("Paused and its pages released while native cutoff is enabled.");ui.label(&world_status);if let Some(models)=&renderer.models {let (ready,failed,total)=models.status();{let clocks=models.effect_clocks();if !clocks.is_empty(){let (mut low,mut high)=(f32::MAX,f32::MIN);for (_,clock,_,_) in &clocks{low=low.min(*clock);high=high.max(*clock);}ui.label(format!("World effect clocks: {:.0}..{:.0} ms across {} playing roots",low,high,clocks.len())).on_hover_text("The clock each playing root has reached, read out of the effect player itself after the fSpeed the map record puts on it (A3DGFXEx::SetPlaySpeed). A sheet that flows reads a clock that keeps rising; a root whose number stands still is frozen however many of its cards are on screen, and then the fault is the frame delta rather than the texture. The same numbers, with the change since the previous report, are what PWE_EFFECT_CENSUS=1 writes into effect-census.txt next to the executable.");}}ui.label(format!("Animated world models: {ready}/{total}; rejected {failed}"));let g=models.gfx_status();let alpha=models.alpha_status();{let fx_status=ui.label(format!("FX light producers: {} loaded, {} pending, {} elements, {} non-light roots, {} failed, {} unsupported binary roots; world effects: {} roots playing, {} quads, {} would not build, {} beyond the radius, {} outside the resident window, {} off by the day/night window; alpha meshes ordered {}/{} ({} drawable of {} slots, {} failed, {} without bounds)",g.live,g.pending,g.emitters,g.non_light_owners,g.failures,g.unsupported,g.players,models.effect_quad_count(),g.player_failures,models.activation_census()[0],models.activation_census()[1],models.activation_census()[2],alpha.ordered,alpha.alpha_meshes,alpha.drawable,alpha.slots,alpha.failed,alpha.without_bounds));if fx_status.hovered(){fx_status.on_hover_text(format!("Loader queue: {} frame(s) ticked, one pending root admitted per frame (native 0x5b1a00). Non-light roots loaded cleanly but contain no LightType element; their sprites/particles/trails are what the world-effects count draws, from the placements world.ecwld and the model events authored. Failed counts FX roots only; world-model and alpha-queue problems are listed below. Unsupported binary roots are valid assets in a container this reader cannot parse yet. World effects are built one root per frame and only while their owner is inside the activation radius, so the playing count grows for a moment after a move. The three counts after the would-not-build count say which gate left the rest out: the radius around the camera, the resident terrain window while the cutoff is on, and each record\'s own day/night window from its iValidTime byte.{}{}{}{}",g.frames,models.gfx_failures().iter().take(4).map(|e|format!("\nFailed: {e}")).collect::<String>(),models.gfx_unsupported().iter().take(2).map(|e|format!("\nUnsupported: {e}")).collect::<String>(),models.effect_failures().iter().take(3).map(|e|format!("\nEffect graph failed: {e}")).collect::<String>(),models.effect_warnings().iter().take(3).map(|e|format!("\nWorld effect notice: {e}")).collect::<String>()));}}}ui.checkbox(&mut settings.world_effects,"World effects (waterfalls, torch fire, glows)").on_hover_text("Draws the effect roots the map itself places: every world.ecwld effect record and every ECM model event, at the position, scale, play speed and day/night window the area authored. Nothing here follows the explorer or belongs to an add-on module. Off releases the animated graph and keeps the light producers those same roots carry, so a torch stops drawing its flame but still lights the wall. On rebuilds one root per frame."); marked_slider(ui,egui::Slider::new(&mut settings.effect_distance,0.0..=120.0).suffix(" m"),"Effect fade distance",defaults.effect_distance,0.,120.,false,settings.world_effects).on_hover_text("The client's own distance cull, applied as a fade instead of a pop. A placed effect whose record sets bAttenuation (the default) fades to invisible once it is further from the player than SCNRES_EFFECT - 60 units in the near scheme, 120 in the far one (EC_SceneBlock.cpp:1109/:1133) - and back in when the player returns, over 3,000 ms each way (CECOrnamentMan::Update, EC_ManOrnament.cpp:279; FadeGFX, :961). The client measures from the character\'s ground position, so the explorer measures horizontally from the camera, X and Z only: a camera 500 units above a city is still standing on that city. 0 (the left end, and the default) turns the fade off and restores every root to the alpha the map authored, including roots an earlier distance had already dimmed; 60 reproduces a player standing among the effects. Roots that are not attenuating, and every ECM model event, ignore the radius.");ui.label("RotFromView card axis:"); for m in crate::view_settings::EffectShaft::ALL { ui.selectable_value(&mut settings.effect_shaft,m,m.label()); } ui.label(egui::RichText::new(format!("decal channel will print shaft={}",settings.effect_shaft.token())).small().weak()).on_hover_text("A RotFromView card is an axial billboard: its height runs along one axis and only its width turns to the camera. The client builds that axis from the placement's own Z (A3DDecalEx.cpp:365, GetParentDir()*kp.m_vDir), which is the Client row and the faithful answer. Root direction drops the .gfx keypoint quaternion, World up always stands the shaft up, and Horizontal flattens the client axis into the world ground plane. The row is persisted as effect_shaft= and reprinted in the decal channel, so one report says which rule produced the picture.");marked_slider(ui,egui::Slider::new(&mut settings.far,2000.0..=24000.0),"View distance",defaults.far,2000.,24000.,false,true);marked_slider(ui,egui::Slider::new(&mut settings.detail.radius,2..=12),"Terrain block window (blocks)",defaults.detail.radius as f32,2.,12.,false,true);marked_slider(ui,egui::Slider::new(&mut settings.detail.tree_lod_scale,0.25..=4.0),"Tree LOD distance scale",defaults.detail.tree_lod_scale,0.25,4.,false,true);marked_slider(ui,egui::Slider::new(&mut settings.detail.scenery_quality,0..=5),"Scenery quality preset",defaults.detail.scenery_quality as f32,0.,5.,false,true);marked_slider(ui,egui::Slider::new(&mut settings.tree_wind_strength,0.0..=1.0),"Tree leaf rocking",defaults.tree_wind_strength,0.,1.,false,true);ui.checkbox(&mut settings.client_tree_wind_preview,"Client wind signal (preview)").on_hover_text("Modulates the existing 16-second sampled leaf loop with the recovered client wind signal. This is an approximation, not the original live leaf animation.");marked_slider(ui,egui::Slider::new(&mut settings.time_of_day,0.0..=24.0),"Time of day",defaults.time_of_day,0.,24.,false,true).on_hover_text("The sky clock the recovery put back: hour 0..24, stored in the settings file. Enabling the pass below makes the client's own clock run from here.");ui.checkbox(&mut settings.time_pass_enabled,"Enable time pass").on_hover_text("Runs the sky clock with wall-clock time instead of pinning it. The rate is the client's own: CECSunMoon::Tick (elementclient 0x4b5f10) adds elapsed_ms * ds:0x8db108 to the day fraction, and that constant is 1/14,400,000, so one authored day takes four real hours (one authored hour is ten real minutes) at speed 1.0. Sun and moon elevations, the dusk/night sky-set swap, the regional light blend, weather light and the zone music day/night choice all follow the same clock, so the world really does get dark. The slider below multiplies that rate.");marked_slider(ui,egui::Slider::new(&mut settings.time_pass_speed,0.1..=600.0).logarithmic(true).suffix("x"),"Time pass speed",defaults.time_pass_speed,0.1,600.,true,settings.time_pass_enabled).on_hover_text("Multiplier on the client's 4-real-hour day: 1x is the shipped speed, 2x is a day every two real hours, 60x is a day every four minutes, 600x is the fastest offered (one day per 14.4 real seconds). The label below shows the resulting length of one authored day.");ui.label(egui::RichText::new(format!("Client rate: 1 game day = {}",crate::sky_time::day_length_label(settings.time_pass_speed))).small().weak());egui::ComboBox::from_label("Sky set").selected_text(settings.sky_override.label()).show_ui(ui,|ui|{for o in crate::sky_transition::SkyOverride::ALL{ui.selectable_value(&mut settings.sky_override,o,o.label());}}).response.on_hover_text("Pins one sky.lst row (0 晴天 clear, 1 阴天01 overcast, 2 夕阳 sunset) instead of the client's day/night ladder (0x4203f4 -> autoscene sky_id_day/sky_id_night). Auto is the shipped behaviour; a pinned row never fades and the swap machine keeps running underneath, so returning to Auto resumes where the clock says. The night row is the overcast set, not a painted night panorama: the client darkens it with the regional light colour.");egui::ComboBox::from_label("Weather").selected_text(settings.weather_override.label()).show_ui(ui,|ui|{for o in crate::client_weather::WeatherOverride::ALL{ui.selectable_value(&mut settings.weather_override,o,o.label());}}).response.on_hover_text("Auto is the area's own EC-BSD flag word (world.ecbsd+0x04 bits 1 and 2, read at 0x421520): rain and snow are independent, and a zone may ask for both. The other rows force one effect anywhere. Rain and snow are falling particle quads, not a simulated weather system: the client owns the clock (33 ms object tick) and the textures (rain.tga, snow.bmp); the viewer now spawns them with the objects' own Speed 0.5 / Size 0.04 (rain, +-30 % size, +-50 % speed) and Size 0.3 (snow). The fall volume, the streak length and the flake descent scale are viewer values.");ui.checkbox(&mut settings.water_reflections,"Water reflections");ui.checkbox(&mut settings.celestial_discs,"Client sun and moon").on_hover_text("Native billboards: centre at direction*100 at the recovered template sizes (20-unit sun, 11.42 degrees; 40-unit moon, 22.62 degrees), gated on the recovered elevation curve and the 0.0003/ms visibility ramps. Sun adds its texture, moon alpha-blends it. The sun swells by 1+2n and adds its recovered 120x40 sunshine.bmp strip inside the recovered night-strength window; the moon carries no halo quad (retail takes every moon halo from the flare chain, and a centred quad landed off the painted disc because moon.tga's art is not centred in its texture). Both discs are submitted at half their object colour: 0x4b405f packs ([+0x48] & 0xfefefe | 0x01010100) >> 1, i.e. the recovered halving applied to the object's white default, so the texture is drawn at 127/255 instead of at full strength. Which record fills +0x48 is not recovered. The sun is tinted by the region's light colour (the object's only colour data is light data) and the sun also draws the client's whole flare chain: the fifteen quads `0x4b6270` pre-builds from `textures/nature/flares.bmp` (a 4x4 atlas, six vertices each, `add ebx,0xc0` per sprite and `cmp ebp,0x3c`), with the builder's own size, offset and UV tables. Sprite i sits `offset[i]` pixels from the sun along the line from the sun through the screen centre - the direction `0x450b30` computes - and its half extent is `metric * size[i]`, where the metric is the viewport height: `0x4360a0` hands `0x4b6270` a host block, the builder copies six dwords of it from `+0x14` and reads the fifth (`+0x24`) as an integer (`fild qword [esp+0x5c]`), which is the height of the viewport rectangle that block carries. The sprite colour is the disc's, so the chain shares the disc's half colour and the region light tint. The disc sizes and textures are the client's; the status line below says which part is which.");marked_slider(ui,egui::Slider::new(&mut settings.celestial_brightness,0.1..=1.0),"Star brightness",defaults.celestial_brightness,0.1,1.,false,settings.celestial_discs).on_hover_text("How strongly the sun and moon are drawn. 1.0 (the mark) is the texture itself. 0x4b405f packs each disc as ([+0x48] & 0xfefefe | 0x01010100) >> 1, half the object colour, but drawn that way both discs read as dim grey circles, so the recovered 0.498 is not the default; slide to it to compare. The region light is not multiplied into the discs for the same reason; it stays on the shine strip and the flare sprites, where 0x4b3e22 really does apply the +0x44 colour.");ui.checkbox(&mut settings.ignore_celestial_flags,"Ignore area sun/moon flags").on_hover_text("Every zone\'s EC-BSD record carries a flag word (record +4, read at 0x420992): bit 3 draws the sun, bit 4 the moon. A zone authored without bit 3 has no sun disc in the client either, which is what makes the sun vanish in some open areas while sky and lighting stay correct. This switch latches 0x18 instead of the zone word, so the disc always draws; the status line still prints the zone\'s own word, so flipping it tells the two causes apart.");ui.checkbox(&mut settings.cloud_detail,"Cloud detail (Client CloudDetail gate)").on_hover_text("Shows the native world.cld cloud surface. Point sizing and water-view parity remain approximate.");ui.checkbox(&mut settings.atmospheric_fog,"Client atmospheric fog (limits distant visibility)");ui.checkbox(&mut settings.terrain_lightmaps,"Client terrain lightmaps");ui.checkbox(&mut settings.area_grass_lightmaps,"Grass area-centered lightmaps").on_hover_text("Client area-center selection with clamped tile edges. Disable only to compare the legacy per-vertex selection.");ui.checkbox(&mut settings.regional_tree_lighting,"Experimental regional tree lighting");ui.checkbox(&mut settings.vegetation_authored_mips,"Authored vegetation mips (experimental, restart)").on_hover_text("P4.07 opt-in: preserve authored DDS mip chains for vegetation and use SpeedTree PWTREE08 per-LOD branch/frond alpha references plus the verified leaf reference 84. Default off keeps the approved legacy path. Applied at startup; restart after changing. Native sampler parity, grass anisotropy, and deeper-than-LOD-7 tails remain unresolved.");ui.checkbox(&mut settings.streamed_regions,"Streamed regional lighting/fog (experimental)").on_hover_text("Select environment regions from nearby world blocks using client-like priority. Changes atmospheric and underwater fog; affects trees only when experimental regional tree lighting is enabled. Asynchronous client load order is not yet reproduced.");ui.checkbox(&mut settings.grass_distance_fade,"Client grass distance fade");ui.checkbox(&mut settings.grass_wind_enabled,"Grass wind").on_hover_text("Native wind from the recovered logic clock. The client scales it with authored per-area data (+0x6c..+0x74), not a user setting, so this is an on/off switch.");marked_slider(ui,egui::Slider::new(&mut settings.grass_wind_amplitude,0.0..=2.0),"Grass wind sway",defaults.grass_wind_amplitude,0.,2.,false,settings.grass_wind_enabled).on_hover_text("Viewer-only multiplier on the wind history. The mark (1.0) is the recovered amplitude: the client's only wind inputs are the noise sample and the authored axis weights (+0x6c..+0x74 = 1.0, -0.5, 0.0), with no scale of its own, so there is no single retail number to show. The spring is critically damped at the recovered stiffness, which bounds the sway by that target instead of letting it resonate.");ui.checkbox(&mut settings.model_backface_culling,"Cull building back faces (experimental)");if camera.underwater_depth>0.{ui.label("Underwater effect active");}ui.label("Whole-world texture atlases are shared; geometry and unique source textures still require GPU memory.");}
                                                    2=>{ui.heading("Sound");ui.checkbox(&mut settings.music_enabled,"Play music from current zone");marked_slider(ui,egui::Slider::new(&mut settings.music_volume,0.0..=1.0),"Music volume",defaults.music_volume,0.,1.,false,true);ui.label(music.status());let audio_diagnostic=music.diagnostic();if !audio_diagnostic.is_empty(){ui.label(audio_diagnostic);}ui.checkbox(&mut settings.ambience_enabled,"Play zone ambience (experimental)");marked_slider(ui,egui::Slider::new(&mut settings.sound_volume,0.0..=1.0),"Ambience volume",defaults.sound_volume,0.,1.,false,true);ui.label(ambience.status());ui.checkbox(&mut settings.effects_enabled,"Play world effect sounds (experimental)");marked_slider(ui,egui::Slider::new(&mut settings.effects_volume,0.0..=1.0),"Effects volume",defaults.effects_volume,0.,1.,false,true);ui.label(world_audio.status());if let Some(error)=world_audio.diagnostic(){ui.label(error);}ui.label("Zone selection, world sound timing and spatialization are approximations. Gameplay, combat and weather effects are not implemented.");}
                                                    3=>{ui.heading("Interface");ui.checkbox(&mut settings.show_zone_entry,"Show entering zone notification");ui.checkbox(&mut settings.show_minimap,"Show minimap");marked_slider(ui,egui::Slider::new(&mut settings.minimap_range,100.0..=600.0),"Minimap half-width (world units)",defaults.minimap_range,100.,600.,false,settings.show_minimap);ui.separator();egui::ComboBox::from_label("Minimap style").selected_text(settings.minimap_style.label()).show_ui(ui,|ui|{ui.selectable_value(&mut settings.minimap_style,MapStyle::Realistic,"Realistic");ui.selectable_value(&mut settings.minimap_style,MapStyle::Original,"Original");});ui.label(egui::RichText::new("Realistic renders the world from above each frame; Original shows the client's own minimap tiles (surfaces/minimaps/world) at their native resolution.").small().weak());egui::ComboBox::from_label("Minimap shape").selected_text(settings.minimap_shape.label()).show_ui(ui,|ui|{ui.selectable_value(&mut settings.minimap_shape,MinimapShape::Square,"Square");ui.selectable_value(&mut settings.minimap_shape,MinimapShape::Circular,"Circular");});egui::ComboBox::from_label("World map style").selected_text(settings.world_map_style.label()).show_ui(ui,|ui|{ui.selectable_value(&mut settings.world_map_style,MapStyle::Realistic,"Realistic");ui.selectable_value(&mut settings.world_map_style,MapStyle::Original,"Original");});ui.label(egui::RichText::new("Realistic renders the whole world from above; Original shows the client's map tiles (surfaces/maps).").small().weak());ui.separator();ui.heading("Camera controls");marked_slider(ui,egui::Slider::new(&mut settings.camera_speed,10.0..=3000.0).logarithmic(true),"Flight speed",defaults.camera_speed,10.,3000.,true,true);marked_slider(ui,egui::Slider::new(&mut settings.boost,1.0..=10.0),"Shift multiplier",defaults.boost,1.,10.,false,true);marked_slider(ui,egui::Slider::new(&mut settings.sensitivity,0.0005..=0.01).logarithmic(true),"Mouse sensitivity",defaults.sensitivity,0.0005,0.01,true,true);}
                                                    _=>{ui.heading("Logging");ui.label(egui::RichText::new("Tick a channel, or click the capture for a report that is already filed, and the viewer writes the files below while you look at the artefact. F8 drops a marker with the camera pose into the log, so a report can name the spot instead of describing it.").small().weak());let mut mask=settings.logging_mask;let mut directory=settings.logging_dir.clone();let changed=crate::diagnostics::ui(ui,&mut mask,&mut directory);if mask!=settings.logging_mask{settings.logging_mask=mask;crate::diagnostics::set_mask(mask);}if directory!=settings.logging_dir{settings.logging_dir=directory;crate::diagnostics::apply_dir(&settings.logging_dir);}}
                                                }
                                                ui.separator();ui.horizontal(|ui|{if ui.add_sized([135.,32.],egui::Button::new("Back to menu")).clicked(){menu_page=MenuPage::Main;}if ui.button("Restore defaults").clicked(){settings=ViewSettings::default();}});
                                            });
                                    }
                                    if menu_page==MenuPage::Teleports {
                                        egui::Window::new("TELEPORT LIST  |  ESC to go back").default_pos(egui::pos2(16.,16.)).default_size(egui::vec2(540.,570.)).min_size(egui::vec2(340.,280.)).max_height(config.height as f32/window.scale_factor() as f32-35.).collapsible(false).resizable(true).show(ctx,|ui|{
                                            ui.heading("Explore destinations");ui.label(egui::RichText::new("Locations and coordinates from the client’s world_targets.txt").small().weak());ui.add_space(6.);
                                            ui.add(egui::TextEdit::singleline(&mut teleport_query).hint_text("Search locations or ID...").desired_width(f32::INFINITY));
                                            match &destinations {Ok(list)=>{
                                                let q=teleport_query.trim().to_lowercase();let filtered=list.iter().filter(|p|q.is_empty() || p.name.to_lowercase().contains(&q) || p.id.to_string().contains(&q)).collect::<Vec<_>>();
                                                ui.label(format!("{} of {} destinations",filtered.len(),list.len()));ui.separator();
                                                egui::ScrollArea::vertical().max_height((ui.available_height()-42.).max(100.)).show(ui,|ui|{for place in filtered {
                                                    ui.horizontal(|ui| {ui.vertical(|ui|{ui.label(egui::RichText::new(&place.name).strong());ui.label(egui::RichText::new(format!("ID {}  •  x {:.0}   y {:.0}   z {:.0}",place.id,place.position[0],place.position[1],place.position[2])).small().weak());});
                                                    if ui.button("Teleport").clicked(){camera.pos=Vec3::from_array(place.position)+Vec3::Y*3.;menu_page=MenuPage::Closed;keys.clear();}});ui.separator();
                                                }});
                                            },Err(e)=>{ui.colored_label(egui::Color32::LIGHT_RED,format!("Could not load destinations: {e}"));}}
                                            if ui.button("Back to menu").clicked(){menu_page=MenuPage::Main;}
                                        });
                                    }
                                    if menu_page==MenuPage::WorldMap {
                                        let scale=window.scale_factor() as f32;let screen_w=config.width as f32/scale;let screen_h=config.height as f32/scale;
                                        egui::Window::new("WORLD MAP  |  M or ESC to close").default_pos(egui::pos2(16.,16.)).default_size(egui::vec2((screen_w-32.).min(760.),(screen_h-32.).min(900.))).min_size(egui::vec2(330.,300.)).max_height(screen_h-35.).collapsible(false).resizable(true).show(ctx,|ui|{
                                            ui.label(egui::RichText::new("North: up  |  East: right  |  Mouse wheel: zoom").small().strong());
                                            if settings.world_map_style==MapStyle::Realistic && !renderer.world_done {ui.horizontal_wrapped(|ui|{ui.label(egui::RichText::new("Distant scenery: ").small().weak());if settings.detail.terrain_cutoff {ui.label("Scenery follows the resident window");}else if settings.whole_world {ui.label(egui::RichText::new(&world_status).small().weak());}else if ui.button("Load whole world scenery").clicked(){settings.whole_world=true;}});}
                                            let remaining=(ui.available_height()-78.).max(100.);let visible_width=ui.available_width();let base_width=visible_width.min(remaining*8./11.).max(120.);let image_size=egui::vec2(base_width*map_zoom,base_width*map_zoom*11./8.);
                                            let texture=if settings.world_map_style==MapStyle::Original {original_map_id.as_ref().map(|t|t.id())} else {Some(full_map_id)};
                                            let mut zoom_anchor=None;
                                            let scroll=egui::ScrollArea::both().max_height(remaining).auto_shrink([false,false]).show(ui,|ui|{
                                                if let Some(id)=texture {let response=ui.add(egui::Image::new((id,image_size)).sense(egui::Sense::click()));let rect=response.rect;
                                                    if response.hovered() {let delta=ui.input(|i|i.raw_scroll_delta.y);if delta!=0. {if let Some(pointer)=ui.input(|i|i.pointer.hover_pos()){let next=(map_zoom*(delta*0.005).exp()).clamp(1.,4.);zoom_anchor=Some((pointer,rect,next/map_zoom));map_zoom=next;}}ui.input_mut(|i|{i.raw_scroll_delta=egui::Vec2::ZERO;i.smooth_scroll_delta=egui::Vec2::ZERO;});}
                                                    let uv=map_space.to_uv(camera.pos.x,camera.pos.z);if (0.0..=1.0).contains(&uv[0])&&(0.0..=1.0).contains(&uv[1]) {let p=rect.left_top()+egui::vec2(uv[0]*rect.width(),uv[1]*rect.height());ui.painter().circle_filled(p,5.,egui::Color32::from_rgb(255,210,60));ui.painter().circle_stroke(p,7.,egui::Stroke::new(2.0f32,egui::Color32::BLACK));}
                                                    if map_click_teleport && response.clicked(){if let Some(pos)=response.interact_pointer_pos(){let uv=(pos-rect.left_top())/rect.size();let [x,z]=map_space.from_uv(uv.x,uv.y);let surface=water_areas.iter().filter(|a|a.contains_xz(x,z)).fold(map_space.ground(x,z),|h,a|h.max(a.height));camera.pos=Vec3::new(x,surface+30.,z);menu_page=MenuPage::Closed;keys.clear();}}
                                                }else {if let Some(e)=&original_map_error{ui.colored_label(egui::Color32::LIGHT_RED,format!("Original map unavailable: {e}"));}else{ui.label("Loading original client map tiles...");}ui.allocate_space(egui::vec2(80.,80.));}
                                            });
                                            if let Some((pointer,rect,ratio))=zoom_anchor {let mut state=scroll.state;let local=pointer-rect.min;state.offset=anchored_zoom_offset(state.offset,local,ratio);state.store(ctx,scroll.id);ctx.request_repaint();}
                                            ui.separator();ui.horizontal_wrapped(|ui|{ui.checkbox(&mut map_click_teleport,"Click to teleport");marked_slider(ui,egui::Slider::new(&mut map_zoom,1.0..=4.0),"Zoom",1.,1.,4.,false,true);ui.label(egui::RichText::new(format!("{} style — change in Settings ▸ Interface",settings.world_map_style.label())).small().weak());});
                                        });
                                    }
                                    if settings.show_minimap {egui::Area::new(egui::Id::new("minimap-hud")).anchor(egui::Align2::RIGHT_TOP,egui::vec2(-16.,16.)).interactable(false).show(ctx,|ui|{egui::Frame::none().fill(egui::Color32::from_black_alpha(175)).inner_margin(5.).rounding(match settings.minimap_shape { MinimapShape::Circular => 256.*0.5+5., MinimapShape::Square => 0.0 }).show(ui,|ui|{
                                        // Original: crop the client's own minimap around the camera.
                                        // The crop keeps a fixed scale and slides inside the image at the
                                        // world edge, so the artwork never stretches.
                                        // Circular clips the whole HUD to a disc; the client's own
                                        // minimap is a square tile crop inside a round frame, which
                                        // is the default here.
                                        let round = match settings.minimap_shape { MinimapShape::Circular => MINIMAP_HUD*0.5, MinimapShape::Square => 0.0 };
                                        let image = if settings.minimap_style==MapStyle::Original {
                                            match (&original_minimap_id,&original_minimap_error) {
                                                (Some(tex),_) => {let [w,h]=map_space.world_size();let uv=map_space.to_uv(camera.pos.x,camera.pos.z);let hu=(settings.minimap_range/w).clamp(0.001,0.5);let hv=(settings.minimap_range/h).clamp(0.001,0.5);let cu=uv[0].clamp(hu,1.-hu);let cv=uv[1].clamp(hv,1.-hv);let rect=egui::Rect::from_min_max(egui::pos2(cu-hu,cv-hv),egui::pos2(cu+hu,cv+hv));ui.add(egui::Image::new((tex.id(),egui::vec2(MINIMAP_HUD,MINIMAP_HUD))).uv(rect).rounding(round))}
                                                (None,Some(e)) => {ui.add_sized([MINIMAP_HUD,MINIMAP_HUD],egui::Label::new(egui::RichText::new(format!("Original minimap unavailable:\n{e}")).small().color(egui::Color32::LIGHT_RED)));return}
                                                (None,None) => {ui.add_sized([MINIMAP_HUD,MINIMAP_HUD],egui::Label::new(egui::RichText::new("Loading client minimap…").small()));return}
                                            }
                                        } else { ui.add(egui::Image::new((map_id,egui::vec2(MINIMAP_HUD,MINIMAP_HUD))).rounding(round)) };
                                        let center=image.rect.center();let dir=egui::vec2(camera.yaw.sin(),camera.yaw.cos());let side=egui::vec2(-dir.y,dir.x);let points=vec![center+dir*10.,center-dir*7.+side*5.,center-dir*7.-side*5.];ui.painter().add(egui::Shape::convex_polygon(points,egui::Color32::from_rgb(255,225,130),egui::Stroke::new(1.0f32,egui::Color32::BLACK)));ui.painter().text(image.rect.left_top()+egui::vec2(5.,5.),egui::Align2::LEFT_TOP,"N",egui::FontId::proportional(14.),egui::Color32::WHITE);});});}
                                    if menu_page.is_open(){renderer.modules.settings_panel(ctx,config.width as f32/window.scale_factor() as f32);}
                                    if renderer.modules.overlay_active() {
                                        // Overhead names: the same view-projection the world
                                        // pass used, so a label sits exactly over its actor.
                                        let scale=window.scale_factor() as f32;
                                        renderer.modules.overlay_ui(
                                            ctx,
                                            camera.matrix(config.width, config.height, camera.far),
                                            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(config.width as f32/scale, config.height as f32/scale)),
                                        );
                                    }
                                    if settings.show_zone_entry {if let Some((name,started))=&zone_banner {let elapsed=started.elapsed().as_secs_f32();if elapsed<4. {let alpha=(elapsed.min(4.-elapsed)*2.).clamp(0.,1.);egui::Area::new(egui::Id::new("zone-entry")).anchor(egui::Align2::CENTER_TOP,egui::vec2(0.,65.)).interactable(false).show(ctx,|ui|{ui.label(egui::RichText::new(format!("Entering {name}")).size(24.).color(egui::Color32::from_white_alpha((alpha*255.) as u8)));});}}}
                                });
                                egui_state.handle_platform_output(&window,output.platform_output);
                                let jobs=egui_ctx.tessellate(output.shapes,output.pixels_per_point);
                                let screen=egui_wgpu::ScreenDescriptor{size_in_pixels:[config.width,config.height],pixels_per_point:output.pixels_per_point};
                                for (id,delta) in &output.textures_delta.set {egui_renderer.update_texture(&renderer.device,&renderer.queue,*id,delta);}
                                let mut encoder=renderer.device.create_command_encoder(&Default::default());
                                let cmd=egui_renderer.update_buffers(&renderer.device,&renderer.queue,&mut encoder,&jobs,&screen);
                                {let mut pass=encoder.begin_render_pass(&wgpu::RenderPassDescriptor{label:Some("settings menu"),color_attachments:&[Some(wgpu::RenderPassColorAttachment{view:scene_view,resolve_target:None,ops:wgpu::Operations{load:wgpu::LoadOp::Load,store:wgpu::StoreOp::Store}})],depth_stencil_attachment:None,timestamp_writes:None,occlusion_query_set:None});egui_renderer.render(&mut pass,&jobs,&screen);}
                                renderer.queue.submit(cmd.into_iter().chain(std::iter::once(encoder.finish())));
                                for id in &output.textures_delta.free {egui_renderer.free_texture(id);}
                            }
                            // Exactly one ramp stage, then exactly one output encode.
                            if let Some(encode) = present_encode.as_ref() {
                                // Ramp into the sRGB intermediate, then copy it once.
                                let ramp_target = encode.view();
                                if let Some(gamma) = display_gamma.as_ref() {
                                    gamma.present(
                                        &renderer.device,
                                        &renderer.queue,
                                        &ramp_target,
                                        frame_gamma_setting,
                                    );
                                }
                                encode.present(&renderer.device, &renderer.queue, &surface_view);
                            } else if let Some(gamma) = display_gamma.as_ref() {
                                gamma.present(
                                    &renderer.device,
                                    &renderer.queue,
                                    &surface_view,
                                    frame_gamma_setting,
                                );
                            }
                            frame.present();
                            if software_adapter { renderer.device.poll(wgpu::Maintain::Wait); }
                        }
                        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                            surface.configure(&renderer.device, &config)
                        }
                        Err(wgpu::SurfaceError::OutOfMemory) => target.exit(),
                        Err(e) => eprintln!("surface: {e}"),
                    }
                }
                _ => {}
            }
            },
            Event::DeviceEvent {
                event: DeviceEvent::MouseMotion { delta },
                ..
            } if captured => {
                if input_debug() {
                    eprintln!("Mouse motion: {:?}", delta);
                }
                camera.yaw -= delta.0 as f32 * settings.sensitivity;
                camera.pitch = (camera.pitch - delta.1 as f32 * settings.sensitivity).clamp(-1.5, 1.5);
            }
            Event::AboutToWait => window.request_redraw(),
            _ => {}
        })
        .map_err(|e| e.to_string())
}
/// Render one frame without a window. Useful for validation on headless systems.
/// Read a rendered target back to CPU bytes, one `RGBA` word per pixel.
///
/// The preview path needs the pixels themselves to answer questions that a
/// screenshot cannot: whether a pass changed anything, and by how much.
fn read_target_pixels(
    renderer: &Renderer,
    target: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let row = width * 4;
    let buffer = renderer.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (row as u64) * (height as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = renderer.device.create_command_encoder(&Default::default());
    e.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    renderer.queue.submit(Some(e.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    renderer.device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let bytes = buffer.slice(..).get_mapped_range().to_vec();
    Ok(bytes)
}

pub fn preview(
    mut t: Terrain,
    path: &std::path::Path,
    camera: Option<[f32; 5]>,
) -> Result<(), String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::all()),
        ..Default::default()
    });
    let (_adapter, device, queue) = pollster::block_on(adapter(&instance, None))?;
    let mut cam = if let Some([x, y, z, yaw, pitch]) = camera {
        if ![x, y, z, yaw, pitch].iter().all(|x| x.is_finite()) {
            return Err("non-finite camera".into());
        }
        Camera {
            pos: Vec3::new(x, y, z),
            yaw,
            pitch,
            far: 16000.,
            time_of_day: 12.,
            water_reflections: true,
            world_effects: true,
            effect_distance: 0.,
            atmospheric_fog: false,
            celestial_discs: false,
            celestial_brightness: 1.,
            ignore_celestial_flags: false,
            weather_override: crate::client_weather::WeatherOverride::Auto,
            sky_override: crate::sky_transition::SkyOverride::Auto,
            cloud_detail: true,
            bloom: false,
            sharpen: false,
            bloom_strength: 1.,
            bloom_mode: 0,
            bloom_tint: 0xffffffff,
            bloom_brightness_fade: 0.,
            bloom_opacity_fade: 0.,
            client_gamma: false,
            gamma_setting: 41.,
            underwater_depth: 0.,
            underwater_fog: Default::default(),
            air_fog: Default::default(),
            terrain_lightmaps: true,
            terrain_day_night: 0.,
            model_backface_culling: true,
            tree_wind_strength: 0.2,
            client_tree_wind_preview: false,
            grass_wind_enabled: false,
            grass_wind_amplitude: 1.,
            regional_tree_lighting: false,
            grass_distance_fade: true,
            area_grass_lightmaps: true,
            regional_light: None,
            logic_delta_ms: 0,
        }
    } else {
        Camera::new(&t)
    };
    cam.underwater_depth = water_depth(&t.water, cam.pos);
    if std::env::var_os("PW_TEST_NO_LIGHTMAPS").is_some() {
        cam.terrain_lightmaps = false;
    }
    cam.bloom = std::env::var_os("PW_PREVIEW_BLOOM").is_some();
    cam.bloom_tint = std::env::var("PW_PREVIEW_BLOOM_TINT")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0xffffffff);
    cam.bloom_brightness_fade = std::env::var("PW_PREVIEW_BLOOM_BRIGHTNESS_FADE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.);
    cam.bloom_opacity_fade = std::env::var("PW_PREVIEW_BLOOM_OPACITY_FADE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.);
    cam.bloom_mode = std::env::var("PW_PREVIEW_BLOOM_MODE")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|m| matches!(m, 4 | 5))
        .unwrap_or(0);
    cam.bloom_strength = std::env::var("PW_PREVIEW_BLOOM_STRENGTH")
        .ok()
        .and_then(|x| x.parse::<f32>().ok())
        .unwrap_or(1.)
        .clamp(0., 4.);
    if std::env::var_os("PW_PREVIEW_NO_REFLECTION").is_some() {
        cam.water_reflections = false;
    }
    if let Ok(hour) = std::env::var("PW_PREVIEW_HOUR") {
        if let Ok(h) = hour.parse::<f32>() {
            cam.time_of_day = h.clamp(0., 24.);
        }
    }
    cam.underwater_fog = t
        .environments
        .underwater(cam.pos.to_array(), cam.time_of_day);
    let mut residency = crate::terrain_residency::Residency::default();
    let mesh = build(
        &mut t,
        cam.pos.x,
        cam.pos.z,
        DetailSettings::default(),
        &mut residency,
        false,
    )?;
    let (width, height) = (960u32, 720u32);
    let fmt = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(device, queue, fmt, width, height, mesh, &mut t, false)?;
    if std::env::var_os("PW_PREVIEW_WORLD_TEST").is_some() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let root = t.element.clone();
        let ornaments = t.objects.ornaments.clone();
        std::thread::spawn(move || {
            crate::world_objects::build(
                root,
                ornaments,
                tx,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
        });
        for msg in rx {
            match msg {
                crate::world_objects::Message::Page(page) => {
                    eprintln!(
                        "Preview uploaded world chunk: {} vertices",
                        page.vertices.len()
                    );
                    let uploaded = crate::world_objects::GpuPage::upload_world(
                        &renderer.device,
                        &renderer.queue,
                        &renderer.layout,
                        &renderer.sampler,
                        &renderer.uniform,
                        &renderer.reflected_uniform,
                        &renderer.world_pages,
                        page,
                    );
                    renderer.world_pages.push(uploaded);
                }
                crate::world_objects::Message::Done {
                    processed,
                    skipped,
                    stats,
                } => {
                    eprintln!(
                        "Preview world preload: {processed} objects, {skipped} fallback meshes, {} pages, peak residency {} MiB / {} MiB budget, {} evictions",
                        stats.pages,
                        stats.residency.peak_resident_bytes >> 20,
                        stats.residency.budget_bytes >> 20,
                        stats.residency.evictions
                    );
                    renderer.world_draw_cmds =
                        crate::world_objects::GpuPage::world_commands(&renderer.world_pages);
                    renderer.world_done = true;
                    break;
                }
                crate::world_objects::Message::Error(e) => return Err(e),
            }
        }
    }
    let alternate_format = [fmt.remove_srgb_suffix()];
    let target = renderer.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headless world image"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: fmt,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: if fmt.is_srgb() {
            &alternate_format
        } else {
            &[]
        },
    });
    // An A/B of the world-effect pass, so "no effects" can be answered rather
    // than argued. The same camera and the same frame are rendered twice, the
    // only difference being the map's own effect roots, and the two images are
    // compared. If nothing differs, the pass is not reaching this target; if it
    // differs, the effects are on screen and any remaining "I do not see them"
    // is about what was authored where, not about the draw.
    if std::env::var_os("PW_PREVIEW_EFFECT_AB").is_some() {
        // Render the same camera twice, the only difference being the map's own
        // effect roots, and compare. If the images match, the pass is not
        // reaching this target at all; if they differ, the effects are on screen
        // and what remains is a question of what was authored where.
        cam.world_effects = true;
        let draw = |r: &mut Renderer, cam: &Camera| {
            let view = target.create_view(&Default::default());
            let raw = target.create_view(&wgpu::TextureViewDescriptor {
                format: Some(fmt.remove_srgb_suffix()),
                ..Default::default()
            });
            r.render(
                &view,
                &raw,
                cam.matrix(width, height, cam.far),
                cam,
                width,
                height,
            );
        };
        // Drain the one-root-per-frame queue first: an effect still waiting for
        // its player must not be mistaken for one that never drew.
        for _ in 0..60 {
            draw(&mut renderer, &cam);
        }
        let report = renderer
            .models
            .as_ref()
            .map(|m| {
                let g = m.gfx_status();
                format!(
                    "{} roots playing, {} quads, {} loaded, {} pending, {} failed",
                    g.players,
                    m.effect_quad_count(),
                    g.live,
                    g.pending,
                    g.failures
                )
            })
            .unwrap_or_else(|| "no world".into());
        let on = read_target_pixels(&renderer, &target, width, height).unwrap_or_default();
        cam.world_effects = false;
        draw(&mut renderer, &cam);
        let off = read_target_pixels(&renderer, &target, width, height).unwrap_or_default();
        cam.world_effects = true;
        let differing = on
            .chunks_exact(4)
            .zip(off.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        eprintln!(
            "Effect A/B: {differing} of {} pixels differ between effects on and off ({report})",
            on.len() / 4
        );
    }
    renderer.render(
        &target.create_view(&Default::default()),
        &target.create_view(&wgpu::TextureViewDescriptor {
            format: Some(fmt.remove_srgb_suffix()),
            ..Default::default()
        }),
        cam.matrix(width, height, cam.far),
        &cam,
        width,
        height,
    );
    if std::env::var_os("PW_PREVIEW_MINIMAP").is_some() {
        let d = depth(&renderer.device, width, height);
        renderer.render_minimap(
            &target.create_view(&Default::default()),
            &d.create_view(&Default::default()),
            &cam,
            256.,
            false,
        );
    }
    let row = width * 4;
    let buffer = renderer.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("image readback"),
        size: (row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = renderer.device.create_command_encoder(&Default::default());
    e.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    renderer.queue.submit(Some(e.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    renderer.device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let bytes = buffer.slice(..).get_mapped_range();
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut out = encoder.write_header().map_err(|e| e.to_string())?;
    out.write_image_data(&bytes).map_err(|e| e.to_string())?;
    out.finish().map_err(|e| e.to_string())?;
    drop(bytes);
    buffer.unmap();
    Ok(())
}

#[cfg(test)]
mod water_batch_tests {
    use super::*;
    fn batch(bounds: [f32; 4], height: f32) -> WaterBatch {
        WaterBatch {
            simple: false,
            start: 0,
            end: 6,
            height,
            bounds,
        }
    }
    #[test]
    fn escape_navigates_menu_pages() {
        assert_eq!(MenuPage::Closed.escape(), MenuPage::Main);
        assert_eq!(MenuPage::Settings.escape(), MenuPage::Main);
        assert_eq!(MenuPage::Main.escape(), MenuPage::Closed);
    }
    #[test]
    fn north_camera_east_is_right_and_turning_left_points_west() {
        // Real client targets: Raging Tide has greater X than Archosaur.
        let pos = Vec3::new(1470.490, 219.608, 1180.864);
        let yaw = std::f32::consts::PI;
        let direction = |yaw: f32| Vec3::new(yaw.sin(), 0., -yaw.cos());
        let matrix = |yaw: f32| {
            Mat4::from_scale(Vec3::new(-1., 1., 1.))
                * Mat4::perspective_rh(70f32.to_radians(), 1., 32000., 1.)
                * Mat4::look_to_rh(pos, direction(yaw), Vec3::Y)
        };
        let north = pos + Vec3::Z * 100.;
        let east = north + Vec3::X * 30.;
        assert!(matrix(yaw).project_point3(east).x > matrix(yaw).project_point3(north).x);
        let left = direction(yaw + 0.25);
        let right = direction(yaw - 0.25);
        assert!(left.x < 0. && right.x > 0.);
        let screen = |d: Vec3| egui::vec2(d.x, -d.z);
        assert!(screen(left).x < 0. && screen(right).x > 0.);
        let strafe = Vec3::new(-yaw.cos(), 0., -yaw.sin());
        assert!(strafe.x > 0.);
    }
    #[test]
    fn wheel_zoom_keeps_cursor_world_point_stationary() {
        let offset = egui::vec2(90., 210.);
        let cursor = egui::vec2(140., 110.);
        // local image point is the cursor position plus the current offset.
        let local = offset + cursor;
        let scale = 1.5;
        let new = anchored_zoom_offset(offset, local, scale);
        assert!(((local * scale - new) - cursor).length() < 1e-5);
        let restored = anchored_zoom_offset(new, local * scale, 1. / scale);
        assert!((restored - offset).length() < 1e-4);
    }
    #[test]
    fn projection_mirroring_preserves_reversed_depth() {
        let normal = Mat4::perspective_rh(70f32.to_radians(), 1., 32000., 1.);
        let mirrored = Mat4::from_scale(Vec3::new(-1., 1., 1.)) * normal;
        for p in [Vec3::new(1., 2., -20.), Vec3::new(-3., 0., -1000.)] {
            let a = normal.project_point3(p);
            let b = mirrored.project_point3(p);
            assert!(
                (a.x + b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6 && (a.z - b.z).abs() < 1e-6
            );
        }
    }
    #[test]
    fn minimap_is_north_up_and_east_right() {
        let m = minimap_matrix(120., -70., 256.);
        let center = m.project_point3(Vec3::new(120., 0., -70.));
        let east = m.project_point3(Vec3::new(220., 0., -70.));
        let north = m.project_point3(Vec3::new(120., 0., 30.));
        assert!(center.x.abs() < 0.0001 && center.y.abs() < 0.0001);
        assert!(east.x > center.x && north.y > center.y);
    }
    #[test]
    fn full_map_projection_agrees_with_client_tile_coordinates() {
        let width = 8192.;
        let height = 11264.;
        let m = Mat4::orthographic_rh(
            width * 0.5,
            -width * 0.5,
            -height * 0.5,
            height * 0.5,
            20000.,
            1.,
        ) * Mat4::look_to_rh(Vec3::new(0., 10000., 0.), Vec3::NEG_Y, Vec3::Z);
        let nav = crate::navigation::MapSpace {
            width: 4096,
            height: 5632,
            spacing: 2.,
            heights: vec![0.; 4].into(),
        };
        for (x, z) in [
            (-2825.774, 3092.210),
            (1470.490, 1180.864),
            (2681.930, -4268.359),
        ] {
            let clip = m.project_point3(Vec3::new(x, 0., z));
            let uv = nav.to_uv(x, z);
            assert!((clip.x * 0.5 + 0.5 - uv[0]).abs() < 0.00001);
            assert!((0.5 - clip.y * 0.5 - uv[1]).abs() < 0.00001);
        }
    }
    #[test]
    fn reversed_depth_keeps_far_surface_precision() {
        let projection = Mat4::perspective_rh(70f32.to_radians(), 1., 32000., 1.);
        let depth = |z: f32| projection.project_point3(Vec3::new(0., 0., -z)).z;
        assert!((depth(1.) - 1.).abs() < 1e-6);
        assert!(depth(32000.).abs() < 1e-6);
        assert!(depth(5000.) > depth(5000.01));
        let forward = Mat4::perspective_rh(70f32.to_radians(), 1., 1., 32000.);
        let d = |z: f32| forward.project_point3(Vec3::new(0., 0., -z)).z;
        assert!(
            (0..1000).any(|i| {
                let z = 5000. + i as f32 * 0.01;
                d(z) == d(z + 0.01) && depth(z) > depth(z + 0.01)
            }),
            "reversed depth should distinguish surfaces lost to forward depth precision"
        );
    }
    #[test]
    fn conservative_water_frustum() {
        let m = Mat4::perspective_rh(70f32.to_radians(), 1., 1., 100.)
            * Mat4::look_to_rh(
                Vec3::new(0., 10., 0.),
                Vec3::new(0., -0.3, -1.).normalize(),
                Vec3::Y,
            );
        assert!(batch([-3., 3., -30., -10.], 0.).visible(m));
        assert!(!batch([500., 510., -30., -10.], 0.).visible(m));
        assert!(!batch([-3., 3., 10., 20.], 0.).visible(m));
    }
    #[test]
    fn distance_to_nearest_surface_point() {
        assert_eq!(
            batch([-2., 2., -3., 3.], 4.).distance_squared(Vec3::new(5., 4., 0.)),
            9.
        );
    }
}

#[cfg(test)]
mod frame_snapshot_tests {
    use super::*;

    fn source() -> &'static str {
        include_str!("gpu.rs")
    }
    fn function(name: &str, src: &str) -> String {
        let start = src
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("missing {name}"));
        let tail = &src[start..];
        // The next top-level `fn` at four-space indentation ends this method.
        let end = tail[4..]
            .find("\n    fn ")
            .map(|i| i + 4)
            .unwrap_or(tail.len());
        tail[..end].to_string()
    }

    #[test]
    fn one_snapshot_is_computed_per_frame_and_secondary_views_reuse_it() {
        let src = source();
        let render = function("render", src);
        assert_eq!(
            render.matches("self.begin_frame(camera, w, h)").count(),
            1,
            "render must sample time and publish lights exactly once"
        );
        let minimap = function("render_minimap", src);
        assert!(minimap.contains("let frame = self.frame;"));
        assert!(minimap.contains("frame.uniform("));
        assert!(
            !minimap.contains("self.started.elapsed()"),
            "minimap must not resample wall time"
        );
        assert!(
            !minimap.contains("begin_frame("),
            "minimap must not advance the frame"
        );
        assert!(
            !minimap.contains("draw_models("),
            "minimap must not consume a second model/light publication"
        );
        assert!(src.contains("if !allows_class_with_context(view, ModelClass::EcModel, context)"));
        assert!(
            src.contains("page.draw_selected(&mut p, reflected, pass_kind)"),
            "local pages must use the data-driven pass gate"
        );
        assert!(
            src.contains("GpuPage::world_view_slot(pass_kind)"),
            "whole-world pages must pick their command list from the pass"
        );
    }

    #[test]
    fn every_secondary_view_gets_zero_logic_delta_and_the_same_elapsed() {
        let mut frame = FrameSnapshot::default();
        frame.elapsed = 12.5;
        frame.time_advances = 1;
        frame.light_publications = 1;
        for pass in Pass::ALL {
            let view = frame.secondary(pass);
            assert_eq!(view.elapsed, 12.5);
            assert_eq!(view.logic_delta_ms, 0);
            assert!(!view.advances_time);
            assert!(!view.consumes_light_publications);
        }
        assert_eq!(frame.time_advances, 1);
        assert_eq!(frame.light_publications, 1);
    }

    #[test]
    fn pass_kinds_are_submitted_from_production_call_sites() {
        let src = source();
        for needle in [
            "            main_pass,\n            ViewRuns::Main,\n            Some(&self.depth),",
            "                    Pass::Refraction,\n                    ViewRuns::Main,",
            "                Pass::Reflection,\n                ViewRuns::Reflected,",
            "                Pass::Minimap\n            },\n            view_runs,",
            "&normal, camera, Pass::Main, true);",
            "                        &above,\n                        camera,\n                        Pass::Refraction,",
            "                &reflected,\n                camera,\n                Pass::Reflection,",
        ] {
            assert!(src.contains(needle), "missing production wiring: {needle}");
        }
    }
}

#[cfg(test)]
mod slider_mark_tests {
    use super::*;

    #[test]
    fn a_logarithmic_default_sits_where_egui_puts_it() {
        // The time-pass speed slider: 0.1..=600x with a 1.0 default. A linear
        // fraction put the tick at 0.0015 of the track, which is the reported
        // "the default marker is set to 0.1".
        let t = slider_mark_fraction(1.0, 0.1, 600.0, true);
        assert!((t - 0.2647).abs() < 5e-3, "{t}");
        assert!(
            (0.2..0.35).contains(&t),
            "the mark belongs near the left third, not on the edge: {t}"
        );
        // The linear reading of the same numbers is the bug.
        assert!(slider_mark_fraction(1.0, 0.1, 600.0, false) < 0.01);
        // Flight speed: 10..=3000 with a 60 default.
        let flight = slider_mark_fraction(60., 10., 3000., true);
        assert!((flight - 0.3141).abs() < 5e-3, "{flight}");
    }

    #[test]
    fn the_ends_of_the_track_are_exact_in_both_mappings() {
        for logarithmic in [false, true] {
            assert_eq!(slider_mark_fraction(0.1, 0.1, 600.0, logarithmic), 0.);
            assert_eq!(slider_mark_fraction(600.0, 0.1, 600.0, logarithmic), 1.);
        }
    }

    #[test]
    fn a_degenerate_range_never_produces_a_nan_position() {
        for (default, min, max) in [
            (1.0, 1.0, 1.0),
            (1.0, 4.0, 4.0),
            (f32::NAN, 0.1, 600.0),
            (1.0, 0.1, f32::INFINITY),
            (0.0, 0.1, 600.0),
        ] {
            let t = slider_mark_fraction(default, min, max, true);
            assert!(t.is_finite() && (0.0..=1.0).contains(&t), "{t}");
        }
    }
}
