//! Ordinary world receiving-model submission in main and clipped water passes.
//! Owns the real scene/model effect light producers (ECWLD effects and ECM
//! EventType-100 roots, including hook-attached ones) and the receiving queues
//! they publish into.
//! Never sends baked terrain/BMD/tree vertices through receiving-model lighting.
use crate::{
    gfx_effect_gpu, gfx_owner, model_attachment,
    model_draw_gpu::{Draw, Frame},
    model_queue,
    scene_lights::{DynamicLight, LightPublications, SceneLights, StaticGrid},
    world_models::Assets,
};
use std::{collections::HashMap, fs::File, path::Path};
#[path = "terrain_cutoff.rs"]
mod cutoff_residency;

/// Identity space for ECM model-event owners, kept disjoint from scene effects.
const MODEL_EVENT_IDENTITY: u64 = 1 << 40;
/// On-demand catalogue spawns. Scene effects take the low identities and model
/// events take `1 << 40`, so a gallery spawn cannot collide with either.
const CATALOG_EFFECT_IDENTITY: u64 = 3 << 40;

/// One light-producing owner installed from the scene or from a model event.
struct Producer {
    identity: u64,
    /// ECWLD effect time eligibility; `None` for model events and all-day roots.
    valid_time: Option<i32>,
    position: [f32; 3],
    /// Skeleton hook this root is attached to (ECM `HookName`).
    hook: Option<HookBinding>,
    /// The record's `bAttenuation` byte (`ECWDFILEEFFECT12.bAttenuation`,
    /// EC_WorldFile.h:248). ECM model events carry no such record and stay at
    /// full alpha.
    attenuation: bool,
    /// Current distance fade, 0..1, ramped toward the target at 1/3000 per ms.
    fade: f32,
}

/// `FadeGFX`'s transition length: `SetAlphaTransition(max_alpha * fAlpha, 3000)`
/// over three seconds, `EC_ManOrnament.cpp:961-966`.
const FADE_MS: f32 = 3000.;

/// One frame of a `FadeGFX` transition: linear in elapsed time, so a full
/// 0 -> 1 ramp takes exactly `FADE_MS` no matter the frame rate, and a shorter
/// frame never overshoots the target.
fn attenuation_step(fade: f32, target: f32, delta_ms: u32) -> f32 {
    let step = (delta_ms as f32 / FADE_MS).clamp(0., 1.);
    (fade + (target - fade).clamp(-step, step)).clamp(0., 1.)
}

const MODEL_SPATIAL_CELL_SIZE: f32 = 256.0;
const MODEL_SPATIAL_RADIUS: f32 = 128.0;

struct ModelSpatialCell {
    min: [f32; 3],
    max: [f32; 3],
    slots: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActivationKey {
    view_x: u32,
    view_z: u32,
    radius: u32,
    night: u32,
    resident_bounds: Option<[u32; 4]>,
    producer_count: usize,
}

fn build_model_spatial_cells(
    positions: impl IntoIterator<Item = (usize, [f32; 3])>,
) -> Vec<ModelSpatialCell> {
    let mut cells = std::collections::BTreeMap::<(i32, i32), ModelSpatialCell>::new();
    for (slot, position) in positions {
        let valid = position.iter().all(|value| value.is_finite());
        let key = if valid {
            (
                (position[0] / MODEL_SPATIAL_CELL_SIZE).floor() as i32,
                (position[2] / MODEL_SPATIAL_CELL_SIZE).floor() as i32,
            )
        } else {
            (i32::MIN, i32::MIN)
        };
        let (min, max) = if valid {
            (
                [
                    position[0] - MODEL_SPATIAL_RADIUS,
                    position[1] - MODEL_SPATIAL_RADIUS,
                    position[2] - MODEL_SPATIAL_RADIUS,
                ],
                [
                    position[0] + MODEL_SPATIAL_RADIUS,
                    position[1] + MODEL_SPATIAL_RADIUS,
                    position[2] + MODEL_SPATIAL_RADIUS,
                ],
            )
        } else {
            ([-f32::MAX; 3], [f32::MAX; 3])
        };
        let cell = cells.entry(key).or_insert_with(|| ModelSpatialCell {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            slots: Vec::new(),
        });
        for axis in 0..3 {
            cell.min[axis] = cell.min[axis].min(min[axis]);
            cell.max[axis] = cell.max[axis].max(max[axis]);
        }
        cell.slots.push(slot);
    }
    cells.into_values().collect()
}

/// Conservative broad phase for the existing per-model 128-unit sphere test.
/// Every kept slot still runs that exact sphere test before it is drawn.
fn model_cell_intersects_frustum(planes: &[[f32; 4]; 6], min: [f32; 3], max: [f32; 3]) -> bool {
    if min.iter().chain(max.iter()).any(|value| !value.is_finite())
        || (0..3).any(|axis| min[axis] > max[axis])
    {
        return true;
    }
    for plane in planes {
        let positive = [
            if plane[0] >= 0.0 { max[0] } else { min[0] },
            if plane[1] >= 0.0 { max[1] } else { min[1] },
            if plane[2] >= 0.0 { max[2] } else { min[2] },
        ];
        if plane[0] * positive[0] + plane[1] * positive[1] + plane[2] * positive[2] + plane[3] < 0.0
        {
            return false;
        }
    }
    true
}

fn append_visible_model_slots(
    cells: &[ModelSpatialCell],
    frustum: Option<&[[f32; 4]; 6]>,
    bounds: Option<[f32; 4]>,
    out: &mut Vec<usize>,
) {
    for cell in cells {
        if let Some([x0, x1, z0, z1]) = bounds {
            if cell.max[0] < x0 || cell.min[0] >= x1 || cell.max[2] < z0 || cell.min[2] >= z1 {
                continue;
            }
        }
        if frustum.is_some_and(|planes| !model_cell_intersects_frustum(planes, cell.min, cell.max))
        {
            continue;
        }
        out.extend_from_slice(&cell.slots);
    }
}

/// The fade a root asks for: full alpha when the fade is switched off, else
/// `CECOrnamentMan::Update`'s ground-plane distance test against
/// `SCNRES_EFFECT`. `eye` is the camera; its height is skipped on purpose.
fn attenuation_target(effect_distance: f32, position: [f32; 3], eye: [f32; 3]) -> f32 {
    if effect_distance <= 0. {
        return 1.;
    }
    let dx = position[0] - eye[0];
    let dz = position[2] - eye[2];
    if dx * dx + dz * dz >= effect_distance * effect_distance {
        0.
    } else {
        1.
    }
}

/// A root bound to a hook of one of this world's models.
struct HookBinding {
    /// Index into `assets.instances` of the parent model.
    instance: usize,
    /// `HookName` plus the authored hook-local transform.
    record: model_attachment::Attachment,
    /// Authored hook-local matrix, resolved once at install time.
    local: glam::Mat4,
    /// A parent without an active action never changes pose. Cache the first
    /// resolved hook matrix so world effects attached to static scenery do not
    /// traverse its skeleton on every camera turn/frame.
    static_parent: Option<[f32; 16]>,
}

/// This frame's pose and deformed meshes for one slot. Secondary views reuse
/// it so reflection/refraction never advance playback or re-sample a hook.
struct SlotPose {
    sample: Option<(usize, f32)>,
    pose: crate::skeleton_pose::Pose,
    meshes: Vec<crate::model_asset::PosedMesh>,
}

struct Slot {
    draw: Option<Draw>,
    failed: bool,
    transform: Result<glam::Mat4, String>,
    /// Last evaluated pose, reused by every pass of the same frame.
    posed: Option<Result<SlotPose, String>>,
    /// Static bind poses are queued only until first evaluated or evicted.
    static_pose_pending: bool,
    geometry_dirty: bool,
    vertex_fog: Option<f32>,
    visible: bool,
}

/// Per-frame alpha-pass diagnostics; see [`World::alpha_status`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AlphaStatus {
    /// Models in this frame's alpha insertion order.
    pub ordered: usize,
    /// Alpha meshes the loaded models actually declare.
    pub alpha_meshes: usize,
    /// Alpha meshes skipped because their model published no bounds.
    pub without_bounds: usize,
    /// Slots with a live draw.
    pub drawable: usize,
    /// Slots that failed to load.
    pub failed: usize,
    /// Slots tracked in total.
    pub slots: usize,
}

pub struct World {
    assets: Assets,
    keep_unbounded: bool,
    slots: Vec<Slot>,
    model_spatial_cells: Vec<ModelSpatialCell>,
    /// Static models needing one bind-pose evaluation (not a per-frame scan).
    pending_static_pose_slots: Vec<usize>,
    /// Models with actual animation playback; these alone are ticked each frame.
    animated_pose_slots: Vec<usize>,
    /// Indices of slots visible in the most recently prepared view.
    visible_slots: Vec<usize>,
    /// Last main-view alpha order key. Rotation changes the frustum but not
    /// transparent-mesh sort distance; reuse while the eye and visible set match.
    alpha_order_eye: Option<[f32; 3]>,
    alpha_order_slots: Vec<usize>,
    /// Reused candidate storage for the spatial-cell query.
    scratch_model_candidates: Vec<usize>,
    read: Box<dyn FnMut(&str) -> Result<Vec<u8>, String>>,
    textures: HashMap<String, (std::sync::Arc<wgpu::Texture>, bool)>,
    receivers: crate::model_draw_gpu::ReceiverCache,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    grid: StaticGrid,
    publications: LightPublications,
    owners: gfx_owner::Manager,
    /// The map's height grid, when the renderer installed one. It is the only
    /// terrain the effect pass can see, and it is what lets a `MatchGrnd` decal
    /// be draped over the surface (`A3DDecalEx`'s ground branch) rather than
    /// drawn as one flat card at the record's own height.
    /// Surface height under a world X/Z, when the renderer installed the map's
    /// height grid. Boxed rather than typed on `navigation::MapSpace` because
    /// this module is also compiled into binaries whose crate root has no
    /// `navigation` (the shipped survey tools), and the query is all the effect
    /// pass needs anyway.
    terrain: Option<Box<dyn Fn(f32, f32) -> Option<f32> + Send + Sync>>,
    producers: Vec<Producer>,
    /// Reused scratch for resolving model-hook effect parents; retaining these
    /// allocations prevents per-frame Vec churn when many effects are attached.
    attachment_results: Vec<(u64, Result<Option<[f32; 16]>, String>)>,
    detached_producers: Vec<u64>,
    /// The animated half of the world's effects. Created on the first update that
    /// needs it, because `with_reader` has no queue to build a draw object with.
    effects: Option<gfx_effect_gpu::Draw>,
    /// The one-line effect trace has been printed for this world.
    effect_trace_printed: bool,
    /// Quads the installed roots drew this frame, uploaded to [`Self::effects`].
    effect_quads: Vec<crate::gfx_player::EffectQuad>,
    /// Every `.gfx` member of the attached archive, grouped by its authored name
    /// so a module can ask for "a waterfall" instead of a path.
    catalog: crate::gfx_catalog::Catalog,
    /// Identities handed out to on-demand catalogue spawns.
    catalog_spawns: u64,
    /// Explorer position and effect activation radius in world units.
    view: Option<(f32, f32, f32)>,
    /// Retail `SCNRES_EFFECT` distance in world units; 0 disables the fade and
    /// leaves every root at its authored alpha (the explorer default).
    effect_distance: f32,
    /// Last inputs used for the CPU activation scan. Rotation alone does not
    /// change activation; position, radius, night, residency or root membership does.
    activation_key: Option<ActivationKey>,
    /// Last eye/distance key and per-root target used by the attenuation ramp.
    attenuation_key: Option<(u32, u32, u32, usize)>,
    attenuation_targets: Vec<f32>,
    attenuation_settled: bool,
    /// Why the last activation pass left producers out, so a zone that draws
    /// nothing can say which gate closed on it: beyond the activation radius,
    /// outside the resident terrain window, or outside its day/night window.
    census: [usize; 3],
    resident_bounds: Option<[f32; 4]>,
    /// Retail alpha insertion order as (slot, mesh) pairs.
    alpha: Vec<model_queue::AlphaCandidate>,
    reported: Vec<String>,
    frustum: Option<[[f32; 4]; 6]>,
    /// Plane normal lengths are invariant for a frame's frustum. Cache them
    /// when the camera matrix changes instead of taking six square roots for
    /// every world-model instance on every frame.
    frustum_plane_lengths: Option<[f32; 6]>,
    scratch_candidates: Vec<model_queue::AlphaCandidate>,
    scratch_failures: Vec<(usize, String)>,
}

impl World {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        assets: Assets,
        element: &Path,
    ) -> Result<Self, String> {
        let mut pack = File::open(element.join("models.pck")).map_err(|e| e.to_string())?;
        let entries: HashMap<String, crate::pck::Entry> = crate::pck::directory(&mut pack)?
            .into_iter()
            .map(|e| (crate::ecmodel::normalize(&e.name), e))
            .collect();
        Self::with_reader(device, format, assets, move |path| {
            let entry = entries
                .get(path)
                .ok_or_else(|| format!("missing model texture: {path}"))?;
            crate::pck::read_entry(&mut pack, entry)
        })
    }
    pub fn with_reader(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        mut assets: Assets,
        read: impl FnMut(&str) -> Result<Vec<u8>, String> + 'static,
    ) -> Result<Self, String> {
        if assets
            .instances
            .iter()
            .any(|i| i.asset >= assets.assets.len())
        {
            return Err("world model instance asset index out of range".into());
        }
        let seed = match std::env::var("PW_MODEL_RANDOM_SEED") {
            Ok(value) => value
                .parse::<u32>()
                .map_err(|_| "invalid PW_MODEL_RANDOM_SEED")?,
            Err(_) => 1,
        };
        let mut random = crate::model_playback::SceneRandom::new(seed);
        for instance in &mut assets.instances {
            let initial = random.initial_tick();
            if let Ok(clock) = &mut instance.playback {
                clock.tick(initial)?;
            }
        }
        let model_spatial_cells = build_model_spatial_cells(
            assets
                .instances
                .iter()
                .enumerate()
                .map(|(index, instance)| (index, instance.placement.position)),
        );
        let mut pending_static_pose_slots = Vec::new();
        let mut animated_pose_slots = Vec::new();
        let slots = assets
            .instances
            .iter()
            .enumerate()
            .map(|(index, instance)| {
                let static_pose_pending = instance.action.is_none();
                if static_pose_pending {
                    pending_static_pose_slots.push(index);
                } else {
                    animated_pose_slots.push(index);
                }
                Slot {
                    draw: None,
                    failed: false,
                    transform: crate::world_models::transform(&instance.placement),
                    posed: None,
                    static_pose_pending,
                    geometry_dirty: true,
                    vertex_fog: None,
                    // Only the current view's spatial query may mark a slot visible.
                    visible: false,
                }
            })
            .collect();
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ordinary model wrap sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            assets,
            keep_unbounded: false,
            slots,
            model_spatial_cells,
            pending_static_pose_slots,
            animated_pose_slots,
            visible_slots: Vec::new(),
            alpha_order_eye: None,
            alpha_order_slots: Vec::new(),
            scratch_model_candidates: Vec::new(),
            read: Box::new(read),
            textures: HashMap::new(),
            receivers: Default::default(),
            sampler,
            format: format.remove_srgb_suffix(),
            grid: StaticGrid::default(),
            publications: LightPublications::default(),
            owners: gfx_owner::Manager::new(),
            terrain: None,
            producers: Vec::new(),
            attachment_results: Vec::new(),
            detached_producers: Vec::new(),
            effects: None,
            effect_trace_printed: false,
            effect_quads: Vec::new(),
            catalog: crate::gfx_catalog::Catalog::default(),
            catalog_spawns: 0,
            view: None,
            effect_distance: 0.,
            activation_key: None,
            attenuation_key: None,
            attenuation_targets: Vec::new(),
            attenuation_settled: false,
            census: [0; 3],
            resident_bounds: None,
            alpha: Vec::new(),
            reported: Vec::new(),
            frustum: None,
            frustum_plane_lengths: None,
            scratch_candidates: Vec::new(),
            scratch_failures: Vec::new(),
        })
    }
    /// A world with no model assets of its own.
    ///
    /// The animated effect half of this object does not need `models.pck` or a
    /// single EC model instance: every root it plays comes from `gfx.pck` and
    /// every placement from the map. This exists so a map whose model archive
    /// the viewer cannot open still shows its waterfalls, fire and glows instead
    /// of losing them with the models, which is what nesting both behind one
    /// archive used to do.
    pub fn without_models(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::with_reader(device, format, Assets::default(), |path: &str| {
            Err(format!("no model archive attached: {path}"))
        })
        .expect("an empty world model set cannot be invalid")
    }
    pub fn set_keep_unbounded(&mut self, keep: bool) {
        self.keep_unbounded = keep;
    }

    pub fn set_frustum(&mut self, frustum: Option<[[f32; 4]; 6]>) {
        self.frustum_plane_lengths = frustum.map(frustum_plane_lengths);
        self.frustum = frustum;
    }
    /// Attach the real GFX archive so installed producers can load their roots.
    /// Root text is GBK-decoded here; the owner manager never guesses an
    /// encoding and refuses binary containers explicitly.
    ///
    /// The same archive carries the effects' **textures**. A root's elements name
    /// `TexFile` values that resolve to `gfx/textures/...`, and those members
    /// ship in `gfx.pck` — not in the `models.pck` this object was constructed
    /// with. Handing the effect draw the model reader therefore made every
    /// texture lookup miss, and `gfx_effect_gpu::Draw::update` drops a whole
    /// bucket whose texture will not load, so a world with thousands of
    /// correctly placed roots still drew nothing at all. One handle over
    /// `gfx.pck` now answers both questions, which is what the client's own
    /// `Gfx\` root means by a relative path.
    pub fn attach_gfx_archive(&mut self, element: &Path) -> Result<(), String> {
        let mut pack = File::open(element.join("gfx.pck")).map_err(|e| format!("gfx.pck: {e}"))?;
        let directory = crate::pck::directory(&mut pack)?;
        // The catalogue is the directory read once: names and sizes only, so
        // opening it costs nothing per member until somebody asks to play one.
        self.catalog = crate::gfx_catalog::Catalog::from_directory(
            directory
                .iter()
                .map(|e| (e.name.as_str(), e.size))
                .collect(),
        );
        // Keyed on `normalize`, which lowercases: the pack stores its members in
        // lower case and the roots name them the way the artist typed them, and
        // this map has always resolved the pair.
        let entries: HashMap<String, crate::pck::Entry> = directory
            .iter()
            .map(|e| (crate::ecmodel::normalize(&e.name), e.clone()))
            .collect();
        // Asset reads: everything inside `gfx.pck`, textures included. The
        // model reader this object was built with keeps its own archive and is
        // still tried second, so a model texture that a root happens to name
        // still resolves.
        let gfx_entries = entries.clone();
        let mut gfx_pack = pack;
        let mut previous = std::mem::replace(
            &mut self.read,
            Box::new(move |path: &str| Err(format!("unset reader: {path}"))),
        );
        self.read = Box::new(move |path: &str| {
            let key = crate::ecmodel::normalize(path);
            match gfx_entries.get(&key) {
                Some(entry) => crate::pck::read_entry(&mut gfx_pack, entry),
                None => previous(path),
            }
        });
        // Root text is a separate handle so a root load and a texture load can
        // never interleave their file positions.
        let root_entries = entries.clone();
        let mut pack = File::open(element.join("gfx.pck")).map_err(|e| format!("gfx.pck: {e}"))?;
        self.owners.set_reader(move |path: &str| {
            let key = crate::ecmodel::normalize(path);
            let entry = root_entries
                .get(&key)
                .ok_or_else(|| format!("missing gfx.pck entry: {path}"))?;
            let bytes = crate::pck::read_entry(&mut pack, entry)?;
            if !bytes.starts_with(b"MOXT") {
                return Err("binary GFX container is not implemented".into());
            }
            let (text, _, bad) = encoding_rs::GBK.decode(&bytes);
            if bad {
                return Err("invalid GBK GFX text".into());
            }
            Ok(text.into_owned())
        });
        // Effect-archive models are binary and live in the same package, under
        // `gfx/models/`. Element 160 names them relative to its own folder, so
        // the key here is the archive's own path, prefix included.
        let model_entries = entries.clone();
        let mut model_pack =
            File::open(element.join("gfx.pck")).map_err(|e| format!("gfx.pck: {e}"))?;
        self.owners.set_model_reader(move |path: &str| {
            let key = crate::ecmodel::normalize(path);
            let name = if key.starts_with("gfx/models/") {
                key.clone()
            } else {
                format!("gfx/models/{key}")
            };
            let entry = model_entries
                .get(&name)
                .or_else(|| model_entries.get(&key))
                .ok_or_else(|| format!("{name}: missing gfx.pck entry"))?;
            crate::pck::read_entry(&mut model_pack, entry)
        });
        Ok(())
    }
    /// Attach an arbitrary root reader (diagnostics and GPU regressions).
    pub fn attach_gfx_reader(
        &mut self,
        read: impl FnMut(&str) -> Result<String, String> + 'static,
    ) {
        self.owners.set_reader(read);
    }
    /// The attached archive's effect catalogue: every `.gfx` member, grouped by
    /// the name its author gave it. Empty until `attach_gfx_archive` runs.
    pub fn effect_catalog(&self) -> &crate::gfx_catalog::Catalog {
        &self.catalog
    }

    /// How many members fall in each catalogue group, for a status line.
    pub fn effect_census(&self) -> Vec<(crate::gfx_catalog::Group, usize)> {
        self.catalog.census()
    }

    /// Play one catalogue member on demand, at a world transform, through the
    /// same owner queue the scene and model effects use: one root loads per
    /// frame, the priority gate and the light publications are shared, and a
    /// root neither reader accepts is a named failure rather than an
    /// approximation. The returned identity is the one to `set_active` or
    /// `remove` later.
    pub fn spawn_catalog_effect(
        &mut self,
        path: &str,
        transform: [f32; 16],
        origin: &str,
    ) -> Result<u64, String> {
        let request = self.catalog.request(
            CATALOG_EFFECT_IDENTITY + self.catalog_spawns,
            path,
            transform,
            origin,
        )?;
        let identity = request.identity;
        self.owners.insert(request)?;
        self.catalog_spawns += 1;
        Ok(identity)
    }

    /// Install the actual ECWLD effect placements as scene-light producers.
    /// The owners stay pending and are loaded one root per update, as 0x5b1a00.
    pub fn install_scene_effects(&mut self, scene: &crate::scene::Scene) -> usize {
        let mut installed = 0;
        for block in &scene.blocks {
            for effect in &block.effects {
                let origin = format!(
                    "world.ecwld ({},{}) effect {}",
                    block.row, block.col, effect.index
                );
                // One malformed placement is a reported absence, never a
                // refused world: the client loads the map and plays what it
                // can, and so does the viewer.
                let transform = match gfx_owner::placement_transform(
                    effect.position,
                    effect.direction,
                    effect.up,
                    effect.scale,
                ) {
                    Ok(transform) => transform,
                    Err(error) => {
                        self.reported.push(format!("{origin}: {error}"));
                        continue;
                    }
                };
                let identity = self.producers.len() as u64;
                let request = gfx_owner::Request {
                    identity,
                    path: gfx_owner::root_path(&effect.path),
                    transform,
                    speed: effect.speed,
                    start_ms: 0,
                    span_ms: None,
                    // 0x5b33f3 copies the record's alpha into the GFX node; the
                    // player folds it into every element's colour.
                    outer: gfx_owner::scene_alpha(effect.alpha),
                    attachment: None,
                    active: false,
                    origin: origin.clone(),
                };
                if let Err(error) = self.owners.insert(request) {
                    self.reported.push(format!("{origin}: {error}"));
                    continue;
                }
                // A fresh root starts where `LoadGFXFromFile` + `FadeGFX(true)`
                // leave it: at zero alpha, three seconds from full, or at full
                // alpha when the record says it never attenuates.
                self.producers.push(Producer {
                    identity,
                    valid_time: Some(effect.valid_time),
                    position: effect.position,
                    hook: None,
                    attenuation: effect.attenuation,
                    fade: if effect.attenuation { 0. } else { 1. },
                });
                installed += 1;
            }
        }
        installed
    }
    /// Install ECM EventType-100 roots, including hook-attached ones.
    ///
    /// A hooked root keeps its authored hook-local transform and receives the
    /// parent's posed hook matrix every frame
    /// (`gfx_owner::Request::attachment`); a hook-free root is placed by the
    /// model transform the event inherits. An unknown hook name or a malformed
    /// hook record is an explicit failure, never the model origin.
    pub fn install_model_events(&mut self) -> usize {
        let mut installed = 0;
        for (index, instance) in self.assets.instances.iter().enumerate() {
            let asset = &self.assets.assets[instance.asset];
            let transform = match self.slots[index].transform.as_ref() {
                Ok(transform) => *transform,
                Err(error) => {
                    self.reported
                        .push(format!("{}: {error}", instance.placement.path));
                    continue;
                }
            };
            for (event_index, event) in asset.descriptor.events.iter().enumerate() {
                let Some(path) = event.gfx_path() else {
                    continue;
                };
                let described = format!(
                    "{} export {} event {event_index}",
                    instance.placement.path, instance.placement.export_id
                );
                let record = match model_attachment::from_fields(&event.fields) {
                    Ok(record) => record,
                    Err(error) => {
                        self.reported.push(format!("{described}: {error}"));
                        continue;
                    }
                };
                let local = match &record {
                    Some(record) => match record.local() {
                        Ok(local) => local,
                        Err(error) => {
                            self.reported.push(format!("{described}: {error}"));
                            continue;
                        }
                    },
                    None => glam::Mat4::IDENTITY,
                };
                let start_ms = match event_milliseconds(event, "StartTime") {
                    Ok(value) => value.max(0.),
                    Err(error) => {
                        self.reported.push(format!("{described}: {error}"));
                        continue;
                    }
                };
                let span_ms = match event_milliseconds(event, "TimeSpan") {
                    Ok(value) if value < 0. => None,
                    Ok(value) => Some(value.trunc() as u32),
                    Err(error) => {
                        self.reported.push(format!("{described}: {error}"));
                        continue;
                    }
                };
                let event_scale = event
                    .fields
                    .iter()
                    .find(|(k, _)| k == "GfxScale")
                    .and_then(|(_, v)| v.trim().parse::<f32>().ok())
                    .filter(|v| v.is_finite() && *v > 0.)
                    .unwrap_or(1.);
                let event_alpha = event
                    .fields
                    .iter()
                    .find(|(k, _)| k == "GfxAlpha")
                    .and_then(|(_, v)| v.trim().parse::<f32>().ok())
                    .and_then(gfx_owner::scene_alpha);
                let scale_mat = if event_scale != 1. {
                    glam::Mat4::from_scale(glam::Vec3::splat(event_scale))
                } else {
                    glam::Mat4::IDENTITY
                };
                let identity = MODEL_EVENT_IDENTITY + self.producers.len() as u64;
                let request = gfx_owner::Request {
                    identity,
                    path: gfx_owner::root_path(&path),
                    // A hooked root's local frame is the hook record and its
                    // parent is installed per frame; a hook-free root is placed
                    // by the model transform the native event inherits.
                    transform: match &record {
                        Some(_) => (local * scale_mat).to_cols_array(),
                        None => (transform * scale_mat).to_cols_array(),
                    },
                    speed: asset.descriptor.render_settings().default_speed,
                    start_ms: start_ms as u32,
                    span_ms,
                    outer: event_alpha,
                    attachment: None,
                    active: false,
                    origin: described.clone(),
                };
                if let Err(error) = self.owners.insert(request) {
                    self.reported.push(format!("{described}: {error}"));
                    continue;
                }
                self.producers.push(Producer {
                    identity,
                    valid_time: None,
                    position: instance.placement.position,
                    attenuation: false,
                    fade: 1.,
                    hook: record.map(|record| HookBinding {
                        instance: index,
                        record,
                        local,
                        static_parent: None,
                    }),
                });
                installed += 1;
            }
        }
        installed
    }
    /// Release receiving-model geometry and poses outside the installed terrain
    /// window, rather than merely disabling their effect-light producers.
    pub fn set_resident_bounds(&mut self, bounds: Option<[f32; 4]>) {
        if self.resident_bounds == bounds {
            return;
        }
        self.resident_bounds = bounds;
        for (index, (instance, slot)) in self
            .assets
            .instances
            .iter()
            .zip(&mut self.slots)
            .enumerate()
        {
            if !cutoff_residency::contains(bounds, instance.placement.position) {
                slot.draw = None;
                slot.posed = None;
                slot.geometry_dirty = true;
                // No static pose work is needed while this object is evicted.
                // It is queued again only if a later resident window includes it.
                slot.static_pose_pending = false;
            } else if instance.action.is_none() && slot.posed.is_none() && !slot.static_pose_pending
            {
                slot.static_pose_pending = true;
                self.pending_static_pose_slots.push(index);
            }
        }
        if bounds.is_some() {
            // Draw-owned Arcs pin active/shared textures; do not keep every
            // texture and pipeline ever visited after its last draw is evicted.
            self.textures
                .retain(|_, (texture, _)| std::sync::Arc::strong_count(texture) > 1);
            self.receivers.trim_unreferenced();
        }
    }
    /// Effect activation radius (independent of resident geometry bounds).
    pub fn set_view_center(&mut self, x: f32, z: f32, radius: f32) {
        self.view = Some((x, z, radius.max(0.)));
    }
    /// Producers the last activation pass left out, by reason: beyond the
    /// activation radius, outside the resident terrain window, outside their
    /// day/night window. The three overlap, so they are not a partition.
    pub fn activation_census(&self) -> [usize; 3] {
        self.census
    }
    /// Radius of the client's effect distance fade, in world units.
    ///
    /// `CECSceneBlock::SetResLoadDists` sets `SCNRES_EFFECT` to 60 units in its
    /// near scheme and 120 in its far one (`EC_SceneBlock.cpp:1109`, `:1133`),
    /// and `CECOrnamentMan::Update` fades any `bAttenuation` node outside that
    /// radius of the host player to zero over three seconds. The explorer has no
    /// player, so the camera stands in for one and the test is horizontal, as the
    /// client's ground positions make it. `0` turns the fade off: every root is
    /// restored to full alpha, including one an earlier distance had dimmed.
    pub fn set_effect_distance(&mut self, distance: f32) {
        self.effect_distance = if distance.is_finite() {
            distance.max(0.)
        } else {
            0.
        };
    }
    /// Manager light-priority limit (native `[[0x9b3eec]+0x28]+0x134`, written
    /// from an authored scene/environment byte at `0x40dc97`/`0x40e394`).
    /// Elements above the limit stop publishing but keep their keypoint clocks,
    /// `0x70ba3c..0x70ba44`. This reader does not decode the authored record, so
    /// the default is "no suppression" (the client at its highest light setting)
    /// rather than an invented threshold that would silently drop lights.
    pub fn set_light_priority_limit(&mut self, limit: i32) {
        self.owners.set_priority_limit(limit);
    }
    pub fn light_priority_limit(&self) -> i32 {
        self.owners.priority_limit()
    }
    pub fn gfx_status(&self) -> gfx_owner::Status {
        self.owners.status()
    }
    /// Direct publication entry point for an already-resolved producer.
    /// Production uses the owner manager below; this stays public because the
    /// publication bridge regressions drive it with explicit identities.
    pub fn publish_dynamic_light(&mut self, identity: u64, light: DynamicLight) {
        self.publications.publish(identity, light);
    }
    /// Execute one decoded retail light element and publish its current params.
    /// The caller owns the scaled tick, resolved parent, visibility/priority and
    /// destruction (`unregister_dynamic_light`).
    pub fn tick_gfx_light(
        &mut self,
        identity: u64,
        emitter: &mut crate::gfx_light::Emitter,
        scaled_delta_ms: u32,
        visible: bool,
        priority: i32,
        parent: [f32; 16],
        outer: Option<[f32; 4]>,
    ) -> Result<bool, String> {
        if let Some(light) = emitter.tick(scaled_delta_ms, visible, priority, parent, outer)? {
            self.publish_dynamic_light(identity, light);
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn unregister_dynamic_light(&mut self, identity: u64) {
        self.publications.unregister(identity);
    }
    /// Roots that could not be read because their container format is not
    /// implemented. Kept out of [`Self::gfx_failures`] so a known-unread format
    /// is not reported as a broken asset.
    pub fn gfx_unsupported(&self) -> Vec<String> {
        self.owners.unsupported().to_vec()
    }
    /// Load problems that are this reader's fault or the asset's: unreadable
    /// roots, failed world models and rejected alpha queues.
    pub fn gfx_failures(&self) -> Vec<String> {
        let mut failures = self.owners.failures().to_vec();
        failures.extend(self.reported.iter().cloned());
        failures
    }
    fn apply_activation(&mut self, night: f32, delta_ms: u32, eye: [f32; 3]) {
        if self.producers.is_empty() {
            return;
        }
        let (x, z, radius) = self.view.unwrap_or((0., 0., -1.));
        let key = ActivationKey {
            view_x: x.to_bits(),
            view_z: z.to_bits(),
            radius: radius.to_bits(),
            night: night.to_bits(),
            resident_bounds: self.resident_bounds.map(|bounds| bounds.map(f32::to_bits)),
            producer_count: self.producers.len(),
        };
        if self.activation_key != Some(key) {
            let radius_sq = radius * radius;
            let resident_bounds = self.resident_bounds;
            let mut census = [0usize; 3];
            let Self {
                producers, owners, ..
            } = self;
            for producer in producers.iter() {
                let dx = producer.position[0] - x;
                let dz = producer.position[2] - z;
                let near = dx * dx + dz * dz <= radius_sq;
                let resident = cutoff_residency::contains(resident_bounds, producer.position);
                let visible = match producer.valid_time {
                    Some(mode) => crate::scene::effect_visible(mode, night),
                    None => true,
                };
                if !near {
                    census[0] += 1;
                }
                if !resident {
                    census[1] += 1;
                }
                if !visible {
                    census[2] += 1;
                }
                owners.set_active(producer.identity, near && resident && visible);
            }
            self.census = census;
            self.activation_key = Some(key);
        }
        self.apply_attenuation(delta_ms, eye);
    }
    /// Ramp every attenuating root's alpha toward "visible inside the retail
    /// effect distance, transparent outside it".
    ///
    /// `CECOrnamentMan::Update` (EC_ManOrnament.cpp:279-292) tests
    /// `a3d_Magnitude(pNode->vPos - vHostPos) >= SCNRES_EFFECT` and calls
    /// `FadeGFX(node, true/false)` accordingly; the transition is 3,000 ms
    /// either way, so a root crossing the radius takes three seconds to appear
    /// or vanish instead of popping. The step is linear in elapsed time, which
    /// is what `SetAlphaTransition` interpolates.
    ///
    /// The test is horizontal. `vHostPos` is the character's position and both
    /// it and `vPos` sit on the map's ground, so the recovered magnitude is a
    /// ground-plane distance; the explorer's camera stands in for the character
    /// and only its X and Z are used. Reading the camera's height into the
    /// distance would empty a whole city the moment the camera rose above it,
    /// which is not what the client's fade does to a player standing there.
    ///
    /// A distance of zero means the fade is off, and off must leave every root
    /// at the alpha the map authored. That is a restore, not an early return: a
    /// root faded out under a previous distance keeps the alpha the owner
    /// stored, and nothing else in the frame writes it back, so returning early
    /// here would hold that root invisible for the rest of the session.
    fn apply_attenuation(&mut self, delta_ms: u32, eye: [f32; 3]) {
        let effect_distance = self.effect_distance;
        let off = effect_distance <= 0.;
        let key = (
            effect_distance.to_bits(),
            eye[0].to_bits(),
            eye[2].to_bits(),
            self.producers.len(),
        );
        if self.attenuation_key != Some(key)
            || self.attenuation_targets.len() != self.producers.len()
        {
            self.attenuation_targets.clear();
            self.attenuation_targets
                .extend(self.producers.iter().map(|producer| {
                    if producer.attenuation && !off {
                        attenuation_target(effect_distance, producer.position, eye)
                    } else {
                        1.
                    }
                }));
            self.attenuation_key = Some(key);
            self.attenuation_settled = false;
        }
        // Once every fade reaches its current target, a stationary camera and
        // unchanged distance setting need no producer walk or owner writes.
        if self.attenuation_settled {
            return;
        }
        let mut moving = false;
        let targets = &self.attenuation_targets;
        let (producers, owners) = (&mut self.producers, &mut self.owners);
        for (producer, &target) in producers.iter_mut().zip(targets) {
            if !producer.attenuation || producer.fade == target {
                continue;
            }
            let old = producer.fade;
            let next = if off {
                1.
            } else {
                attenuation_step(old, target, delta_ms)
            };
            if next != old {
                producer.fade = next;
                owners.set_attenuation(producer.identity, next);
            }
            moving |= next != target;
        }
        self.attenuation_settled = !moving;
    }
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: Frame,
        center: [f32; 3],
        lights: &SceneLights,
        night: f32,
        delta_ms: u32,
    ) {
        self.update_pass(device, queue, frame, center, lights, night, delta_ms, true);
    }
    /// The first/main pass advances the frame, ticks producers and consumes
    /// publications. Reflection/refraction only update pass-dependent
    /// matrices/fog/clip; they must reuse the same lighting snapshot, playback
    /// time and alpha order.
    pub fn update_pass(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: Frame,
        center: [f32; 3],
        lights: &SceneLights,
        night: f32,
        delta_ms: u32,
        advance: bool,
    ) {
        if advance {
            // 1. Playback advances once per frame and each model is posed once.
            self.advance_poses(delta_ms);
            // 2. Hook-attached producers resolve their parent from those poses
            //    (0x7b2520), then publish before the grid consumes, so a light
            //    created or moved on this tick already lights this frame.
            self.resolve_attachments();
            self.apply_activation(night, delta_ms, center);
            self.owners.update(delta_ms, &mut self.publications);
            self.update_effects(device, queue, frame, delta_ms);
            self.grid.update(lights, center);
            self.publications.consume_into(&mut self.grid);
        }
        self.draw_models(device, queue, frame, night);
        if advance {
            self.rebuild_alpha_order(center);
        }
    }
    /// Install the surface query the effect pass needs to drape a
    /// ground-matched decal over the terrain.
    ///
    /// The renderer owns the map's height grid and builds it once per world, so
    /// it hands a query over after the world is constructed; the heights
    /// themselves sit behind an `Arc`, so the closure costs a refcount bump
    /// rather than a second copy of the map. Without this the ground branch of
    /// `A3DDecalEx::Fill_Verts_3D` cannot run and every ground sheet stays one
    /// flat quad.
    ///
    /// The height function is wrapped here rather than at the call site: a
    /// `navigation::MapSpace::ground` call goes through `Heights::height`,
    /// which `expect`s on a damaged backing block, and the drape reaches it
    /// every frame where the minimap teleport only reached it on a click. One
    /// panic is caught, the sheet stays flat, and the grid is dropped for the
    /// rest of the run — a corrupt terrain cache must not turn into a crash on
    /// the first frame.
    pub fn set_terrain<F>(&mut self, height: F)
    where
        F: Fn(f32, f32) -> Option<f32> + Send + Sync + 'static,
    {
        let broken = std::sync::atomic::AtomicBool::new(false);
        self.terrain = Some(Box::new(move |x: f32, z: f32| {
            if broken.load(std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            let read = || height(x, z);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)) {
                Ok(value) => value,
                Err(_) => {
                    broken.store(true, std::sync::atomic::Ordering::Relaxed);
                    None
                }
            }
        }));
    }

    /// Advance the world's animated effects and upload this frame's quads.
    ///
    /// Main pass only, straight after the light producers, so the geometry and the
    /// lights read the same owner clocks and cannot disagree about which effects
    /// are alive. The placements are the world's own: every ECWLD effect record
    /// and every ECM model event this world installed, which is where the retail
    /// waterfalls, torch fire and glows already are — nothing here is positioned
    /// by an add-on module or by the explorer's own body.
    fn update_effects(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: Frame,
        delta_ms: u32,
    ) {
        if !self.owners.effects_enabled() {
            self.effect_quads.clear();
            return;
        }
        // The billboards need a camera basis and `Frame` carries only the
        // view-projection, so the eye comes back out of its inverse. A frame
        // whose matrix cannot be undone is reported rather than replaced with a
        // default view: the default puts the eye at the world origin, which
        // turns every billboard away from a player standing in a waterfall and
        // looks exactly like "the map has no effects".
        let view = match crate::gfx_player::View::try_from_inverse_view_projection(
            glam::Mat4::from_cols_array_2d(&frame.matrix).inverse(),
        ) {
            Ok(view) => view,
            Err(error) => {
                if self.reported.len() < 256 && !self.reported.contains(&error) {
                    self.reported.push(error);
                }
                self.effect_quads.clear();
                return;
            }
        };
        // The field and the owner manager are separate places, so the sampler
        // borrows one and the pass advances the other. Reuse `self.effect_quads`
        // directly to avoid reallocating and copying every frame.
        {
            let Self {
                owners,
                terrain,
                effect_quads,
                ..
            } = self;
            let sampler: Option<crate::gfx_player::GroundHeightQuery<'_>> = terrain
                .as_deref()
                .map(|f| f as crate::gfx_player::GroundHeightQuery<'_>);
            owners.effect_quads_with_ground_into(delta_ms, &view, sampler, effect_quads);
            const EFFECT_QUAD_TRIM_THRESHOLD: usize = 4096;
            if effect_quads.capacity() > EFFECT_QUAD_TRIM_THRESHOLD
                && effect_quads.len() < effect_quads.capacity() / 8
            {
                effect_quads.shrink_to(effect_quads.len().max(EFFECT_QUAD_TRIM_THRESHOLD));
            }
        }
        // Say once, on stderr, what the invisible pipeline is doing. Nothing in
        // the world told a user *which* stage was empty when no effect appeared:
        // the roots could be unloaded, out of radius, failed to parse, or drawing
        // off screen, and all four look identical from the outside. This costs
        // one line per run.
        if !self.effect_trace_printed {
            let status = self.owners.status();
            eprintln!(
                "World effects: {} installed, {} loaded, {} pending, {} playing, \
                 {} frames ticked, {} quads this frame, eye ({:.0},{:.0},{:.0}); \
                 {} load failures, {} graph failures, {} warnings",
                self.producers.len(),
                status.live,
                status.pending,
                status.players,
                status.frames,
                self.effect_quads.len(),
                view.position.x,
                view.position.y,
                view.position.z,
                status.failures,
                status.player_failures,
                self.owners.effect_warnings().len(),
            );
            for line in self.owners.failures().iter().take(3) {
                eprintln!("World effects: load failure: {line}");
            }
            for line in self.owners.effect_failures().iter().take(3) {
                eprintln!("World effects: graph failure: {line}");
            }
            self.effect_trace_printed = true;
        }
        let Self {
            effects,
            effect_quads,
            read,
            format,
            reported,
            ..
        } = self;

        if effects.is_none() {
            // The same reverse-Z depth buffer the world's models use, so an
            // effect in front of a wall is occluded by nothing and one behind it
            // is hidden — but with `LESSEQUAL`'s tie rule, not `LESS`.
            //
            // The client's effect path never writes `D3DRS_ZFUNC`: the slot
            // record in `A3DGFXRenderSlot.h` carries `m_bZTestEnable` (the
            // ZENABLE byte) and no Z-func field, so every effect slot runs on the
            // D3D8 device default, `D3DCMP_LESSEQUAL`. Under the reversed
            // projection that is `GreaterEqual`. The strict `Greater` this pass
            // used to ask for dropped every sheet drawn at the same depth as one
            // before it — a decal and its overlay glow, the two halves of a card
            // pair, the coplanar layers of a portal — and because those depths
            // are computed per vertex in floating point, the loser swapped with
            // each hair of camera movement: the flicker. Two blended draws at
            // equal depth both pass in the client, and both must pass here.
            match gfx_effect_gpu::Draw::new(
                device,
                queue,
                *format,
                Some((
                    wgpu::TextureFormat::Depth32Float,
                    wgpu::CompareFunction::GreaterEqual,
                )),
            ) {
                Ok(draw) => *effects = Some(draw),
                Err(error) => {
                    if reported.len() < 256 {
                        reported.push(format!("world effects: {error}"));
                    }
                    return;
                }
            }
        }
        let Some(draw) = effects.as_mut() else {
            return;
        };
        // Element 160 is the same `A3DSkinModel` a scenery model is, so it takes
        // the same `LIGHTINFO`: this frame's ambient and directional colour, and
        // its direction. `bPtLight` is forced off inside the pass, as
        // `A3DGFXModel::UpdateModel` does for every model it builds.
        let light = gfx_effect_gpu::EffectLight {
            lighting: frame.lighting,
            direction: frame.direction,
            fog_range: frame.fog_range,
            fog_color: frame.fog_color,
            // `A3DGFXModel::ChangeModel` enables specular on every effect model,
            // so the pass needs the light's specular colour and the eye the
            // Blinn half-vector is built from.
            directional_specular: frame.directional_specular,
            eye_direction: frame.eye_direction,
        };
        if let Err(error) = draw.update(
            device,
            queue,
            effect_quads,
            frame.matrix,
            frame.clip_plane,
            [view.position.x, view.position.y, view.position.z, 0.],
            Some(light),
            &mut **read,
        ) {
            if reported.len() < 256 {
                reported.push(format!("world effects: {error}"));
            }
        }
    }

    /// Switch the world's animated effects on or off: the sprites, particles,
    /// ribbons, trails and fractals inside every installed root. The light
    /// producers are untouched either way, so a torch whose flame is switched off
    /// still lights the wall around it. Off releases every player the owners hold;
    /// on rebuilds them one root per frame.
    pub fn set_effects_enabled(&mut self, enabled: bool) {
        self.owners.set_effects_enabled(enabled);
        if !enabled {
            self.effects = None;
            self.effect_quads.clear();
            self.effect_quads.shrink_to_fit();
        }
    }

    pub fn effects_enabled(&self) -> bool {
        self.owners.effects_enabled()
    }

    /// `(path, clock ms, live play speed, authored PlaySpeed)` for every root this
    /// world has admitted. The headless preview prints it so a "900 ms" shot can
    /// quote the clock the effect actually reached, and a report can say whether
    /// the map's `fSpeed` overrode the file's `PlaySpeed`.
    pub fn effect_clocks(&self) -> Vec<(String, f32, f32, f32)> {
        self.owners.player_clocks()
    }

    /// Quads the world's animated effects produced this frame.
    pub fn effect_quad_count(&self) -> usize {
        self.effect_quads.len()
    }

    /// Textures the effect pass could not decode, newest first. A bucket whose
    /// map will not load is drawn with a transparent stand-in, so a preview that
    /// submitted quads and covered no pixels is exactly this list being non-empty.
    pub fn effect_texture_failures(&self) -> &[String] {
        match &self.effects {
            Some(draw) => draw.failures(),
            None => &[],
        }
    }

    /// This frame's submitted effect quads. The headless preview in
    /// `effect_preview` frames its shot from these, and a test can measure what
    /// a root actually draws instead of trusting its authored numbers.
    pub fn effect_quads(&self) -> &[crate::gfx_player::EffectQuad] {
        &self.effect_quads
    }

    /// Roots whose animated graph would not build, as `origin: error`. Their
    /// lights still run, so these are kept apart from [`Self::gfx_failures`].
    pub fn effect_failures(&self) -> Vec<String> {
        self.owners.effect_failures().to_vec()
    }

    /// Warnings the animated roots reported, nearly all of them a nested `GfxPath`
    /// the archive does not ship. The parent plays without the child, which is
    /// what the client does, so these are not failures.
    pub fn effect_warnings(&self) -> Vec<String> {
        self.owners.effect_warnings().to_vec()
    }

    /// Tick every playback clock once and evaluate each model's pose once.
    /// 0x7b2520 resolves hooks from this pose, and every pass of the frame
    /// (main, reflection, refraction) reuses it.
    fn advance_poses(&mut self, delta_ms: u32) {
        let bounds = self.resident_bounds;
        let Self {
            assets,
            slots,
            pending_static_pose_slots,
            animated_pose_slots,
            ..
        } = self;

        // Static world props never change pose. Evaluate a bind pose once when
        // first resident, instead of ticking every one of them on every frame.
        // Drain in place instead of taking the Vec and reserving a replacement:
        // the old implementation discarded the queue's allocation every time
        // a residency update queued a batch of static poses.
        for index in pending_static_pose_slots.drain(..) {
            let Some(instance) = assets.instances.get(index) else {
                continue;
            };
            let Some(slot) = slots.get_mut(index) else {
                continue;
            };
            if !slot.static_pose_pending {
                continue;
            }
            if !cutoff_residency::contains(bounds, instance.placement.position) {
                slot.static_pose_pending = false;
                continue;
            }
            if slot.failed {
                slot.static_pose_pending = false;
                continue;
            }
            let asset = &assets.assets[instance.asset];
            let result = (|| -> Result<SlotPose, String> {
                let transform = *slot.transform.as_ref().map_err(Clone::clone)?;
                let pose = asset.bind_pose(transform)?;
                let meshes = asset.meshes_from_pose(&pose)?;
                Ok(SlotPose {
                    sample: None,
                    pose,
                    meshes,
                })
            })();
            slot.geometry_dirty = true;
            slot.posed = Some(result);
            slot.static_pose_pending = false;
        }

        // Only placements with a real action reach this loop. The old path
        // walked every placement and created up to 16 scoped OS threads every
        // frame, even though most map props have no playback clock to advance.
        for &index in animated_pose_slots.iter() {
            let Some(instance) = assets.instances.get_mut(index) else {
                continue;
            };
            let Some(slot) = slots.get_mut(index) else {
                continue;
            };
            if !cutoff_residency::contains(bounds, instance.placement.position) || slot.failed {
                continue;
            }
            let asset = &assets.assets[instance.asset];
            let sample = match instance.playback.as_mut() {
                Ok(clock) => {
                    if delta_ms != 0 {
                        if let Err(error) = clock.tick(delta_ms) {
                            slot.posed = Some(Err(error));
                            continue;
                        }
                    }
                    clock.sample()
                }
                Err(error) => {
                    slot.posed = Some(Err(error.clone()));
                    continue;
                }
            };
            if matches!(&slot.posed, Some(Ok(previous)) if previous.sample == sample) {
                continue;
            }
            let result = (|| -> Result<SlotPose, String> {
                let transform = *slot.transform.as_ref().map_err(Clone::clone)?;
                let pose = match sample {
                    Some((action, time)) => asset.action_pose(action, time, transform)?,
                    None => asset.bind_pose(transform)?,
                };
                let meshes = asset.meshes_from_pose(&pose)?;
                Ok(SlotPose {
                    sample,
                    pose,
                    meshes,
                })
            })();
            slot.geometry_dirty = true;
            slot.posed = Some(result);
        }
    }
    /// Resolve every hook-attached producer against its parent's posed hook.
    /// A model that failed to pose, a model that does not own the named hook and
    /// a destroyed parent all surface as recorded failures instead of leaving a
    /// light attached to a stale or invented transform.
    fn resolve_attachments(&mut self) {
        let mut resolved = std::mem::take(&mut self.attachment_results);
        resolved.clear();
        for producer in &mut self.producers {
            let Some(binding) = producer.hook.as_mut() else {
                continue;
            };
            let id = producer.identity;
            let outcome = (|| -> Result<Option<[f32; 16]>, String> {
                let slot = self
                    .slots
                    .get(binding.instance)
                    .ok_or_else(|| format!("producer {id}: parent slot is gone"))?;
                let parent = self
                    .assets
                    .instances
                    .get(binding.instance)
                    .ok_or_else(|| format!("producer {id}: parent instance is gone"))?;
                if !cutoff_residency::contains(self.resident_bounds, parent.placement.position) {
                    return Ok(None);
                }

                // Static placements use the bind pose forever. Once one of
                // their hook matrices has been validated, reuse it after future
                // camera/window changes instead of walking the skeleton each
                // frame. Still check residency above so an evicted parent
                // detaches just as before.
                let static_parent = parent.action.is_none();
                if static_parent {
                    if let Some(matrix) = binding.static_parent {
                        // Both the hook matrix and its local composition were
                        // validated when cached; neither changes for a static
                        // parent. Residency was checked above, so no math or
                        // matrix reconstruction is needed in the steady state.
                        return Ok(Some(matrix));
                    }
                }

                let posed = match &slot.posed {
                    Some(Ok(posed)) => posed,
                    Some(Err(e)) => return Err(format!("producer {id}: parent pose failed: {e}")),
                    None => return Ok(None),
                };
                let asset = self
                    .assets
                    .assets
                    .get(parent.asset)
                    .ok_or_else(|| format!("producer {id}: parent asset is gone"))?;
                let hook = asset.hook_matrix(&posed.pose, &binding.record.hook)?;
                // Validate hook * local composition without baking `local` into
                // the attachment parent: `Request::transform` already holds
                // `local`, and `Request::world_transform` computes
                // `parent * self.transform`.
                let _ = model_attachment::world(hook, binding.local)?;
                let matrix = hook.to_cols_array();
                if static_parent {
                    binding.static_parent = Some(matrix);
                }
                Ok(Some(matrix))
            })();
            resolved.push((producer.identity, outcome));
        }
        let mut detached = std::mem::take(&mut self.detached_producers);
        detached.clear();
        for (identity, outcome) in resolved.drain(..) {
            match outcome {
                Ok(parent) => {
                    if !self.owners.set_attachment(identity, parent) {
                        detached.push(identity);
                    }
                }
                Err(e) => {
                    self.reported.push(e);
                    self.owners.remove(identity, &mut self.publications);
                    detached.push(identity);
                }
            }
        }
        // A producer whose owner is gone or whose hook cannot be resolved is
        // dropped instead of retrying against a stale parent every frame.
        if !detached.is_empty() {
            self.producers.retain(|p| !detached.contains(&p.identity));
        }
        self.attachment_results = resolved;
        self.detached_producers = detached;
    }
    /// Upload this frame's already-evaluated poses. Runs for every pass without
    /// touching playback, producer clocks or publications.
    fn draw_models(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: Frame,
        night: f32,
    ) {
        if self.slots.is_empty() {
            return;
        }
        // Clear the previous view's visible flags, then visit only spatial
        // cells that can intersect this view. This replaces a full instance
        // scan for main + reflection + refraction on every camera turn.
        for &index in &self.visible_slots {
            if let Some(slot) = self.slots.get_mut(index) {
                slot.visible = false;
            }
        }
        self.visible_slots.clear();

        let bounds = self.resident_bounds;
        let frustum = self.frustum;
        let frustum_plane_lengths = self.frustum_plane_lengths.unwrap_or([0.; 6]);
        let keep_unbounded = self.keep_unbounded;
        let Self {
            assets,
            slots,
            model_spatial_cells,
            visible_slots,
            scratch_model_candidates,
            read,
            textures,
            receivers,
            sampler,
            format,
            grid,
            reported,
            ..
        } = self;

        scratch_model_candidates.clear();
        if frustum.is_none() && bounds.is_none() {
            scratch_model_candidates.extend(0..assets.instances.len());
        } else {
            append_visible_model_slots(
                model_spatial_cells,
                frustum.as_ref(),
                bounds,
                scratch_model_candidates,
            );
            scratch_model_candidates.sort_unstable();
        }

        for &index in scratch_model_candidates.iter() {
            let Some(instance) = assets.instances.get(index) else {
                continue;
            };
            let Some(slot) = slots.get_mut(index) else {
                continue;
            };
            if !cutoff_residency::contains(bounds, instance.placement.position) {
                slot.visible = false;
                if !keep_unbounded {
                    slot.draw = None;
                    slot.posed = None;
                }
                continue;
            }
            if slot.failed {
                slot.visible = false;
                continue;
            }
            if let Some(planes) = frustum {
                if !sphere_in_frustum(
                    &planes,
                    &frustum_plane_lengths,
                    instance.placement.position,
                    MODEL_SPATIAL_RADIUS,
                ) {
                    slot.visible = false;
                    continue;
                }
            }
            slot.visible = true;
            visible_slots.push(index);
            let asset = &assets.assets[instance.asset];
            let outcome = draw_one(
                device,
                queue,
                frame,
                night,
                instance.placement.position,
                asset,
                slot,
                grid,
                sampler,
                *format,
                read,
                textures,
                receivers,
            );
            if let Err(e) = outcome {
                reported.push(format!(
                    "world model {} ({}): {e}",
                    instance.placement.export_id, instance.placement.path
                ));
                slot.failed = true;
                slot.visible = false;
                visible_slots.pop();
                slot.draw = None;
            }
        }
    }
    /// Retail alpha insertion walk over this frame's alpha meshes. Ordering uses
    /// the eight transformed initial-AABB-corner distance; the per-mesh
    /// priority weights of the original insertion routine were not recovered, so
    /// they stay zero and the comparison falls through to distance.
    fn rebuild_alpha_order(&mut self, eye: [f32; 3]) {
        // Alpha distance is measured from the eye position and the visible
        // model set, not from camera yaw/pitch. A rotation-only frame with an
        // unchanged set therefore has exactly the same ordered candidates.
        if self.alpha_order_eye == Some(eye)
            && self.alpha_order_slots.as_slice() == self.visible_slots.as_slice()
        {
            return;
        }
        self.scratch_candidates.clear();
        self.scratch_failures.clear();
        for &slot in &self.visible_slots {
            let Some(entry) = self.slots.get(slot) else {
                continue;
            };
            if !entry.visible {
                continue;
            }
            let Some(draw) = &entry.draw else {
                continue;
            };
            let Ok(transform) = &entry.transform else {
                continue;
            };
            for &mesh in draw.alpha_meshes() {
                let Some((min, max)) = draw.mesh_bounds(mesh) else {
                    continue;
                };
                match model_queue::alpha_corner_distance(
                    min,
                    max,
                    transform.to_cols_array_2d(),
                    eye,
                ) {
                    Ok(distance) => self.scratch_candidates.push(model_queue::AlphaCandidate {
                        slot,
                        mesh,
                        node: model_queue::AlphaNode {
                            model: slot,
                            mesh_weight: 0,
                            skin_weight: 0,
                            group: 0,
                            group_weight: 0,
                            min,
                            max,
                            distance,
                        },
                    }),
                    Err(e) => self.scratch_failures.push((slot, e)),
                }
            }
        }
        if self.scratch_candidates.is_empty() {
            self.alpha.clear();
        } else {
            match model_queue::alpha_order(&self.scratch_candidates) {
                Ok(order) => self.alpha = order,
                Err(e) => {
                    self.alpha.clear();
                    self.reported.push(format!("alpha queue: {e}"));
                }
            }
        }
        for (slot, error) in &self.scratch_failures {
            self.reported
                .push(format!("alpha queue slot {slot}: {error}"));
            if let Some(entry) = self.slots.get_mut(*slot) {
                entry.failed = true;
                entry.draw = None;
            }
            self.alpha.retain(|candidate| candidate.slot != *slot);
        }
        self.alpha_order_eye = Some(eye);
        self.alpha_order_slots.clone_from(&self.visible_slots);
    }
    /// Include dependency failures in the UI; never count a skipped asset ready.
    /// Entries owned by the world cache, excluding live draw references.
    #[allow(dead_code)]
    pub fn residency_cache_counts(&self) -> (usize, usize) {
        (self.textures.len(), self.receivers.len())
    }
    pub fn status(&self) -> (usize, usize, usize) {
        let ready = self.slots.iter().filter(|s| s.draw.is_some()).count();
        let failed = self.slots.iter().filter(|s| s.failed).count() + self.assets.unloaded.len();
        (ready, failed, self.slots.len() + self.assets.unloaded.len())
    }
    /// Opaque meshes first, then the retail alpha list in its insertion order.
    ///
    /// The blended effect sheets close the pass. Their motion is published by
    /// `draw_effect_motion` afterwards, in a pass of its own, and belongs to the
    /// main view alone: the mirror and refraction depth feeds the water, and the
    /// motion vectors never read it.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw_models_only(pass);
        self.draw_effects(pass);
    }

    /// Draws opaque and alpha-blended world models without world effects.
    /// Used by the main pass before water so water surfaces cannot overwrite
    /// blended effect quads that do not write depth.
    pub fn draw_models_only<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        for &index in &self.visible_slots {
            let Some(slot) = self.slots.get(index) else {
                continue;
            };
            if !slot.visible {
                continue;
            }
            if let Some(draw) = &slot.draw {
                draw.draw_opaque(pass);
            }
        }
        for candidate in &self.alpha {
            let Some(slot) = self.slots.get(candidate.slot) else {
                continue;
            };
            // Alpha order is built by the main view and reused by reflection /
            // refraction. Respect the visibility produced by each secondary view
            // instead of resubmitting every main-view alpha mesh into the water.
            if !slot.visible {
                continue;
            }
            if let Some(draw) = &slot.draw {
                draw.draw_alpha_mesh(pass, candidate.mesh);
            }
        }
    }

    /// Draws blended world effect quads. In the main view this runs after
    /// water surfaces and shoreline waves so effects in front of water are
    /// composited over the water surface and depth-tested against it.
    pub fn draw_effects<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if let Some(effects) = &self.effects {
            effects.draw(pass);
        }
    }

    pub fn has_effect_quads(&self) -> bool {
        self.effects.is_some() && !self.effect_quads.is_empty()
    }

    /// Re-binds the main-pass camera matrix, clip plane and lighting onto the
    /// effect renderer after water reflection/refraction passes may have
    /// overwritten its uniform buffer, without advancing effect clocks.
    pub fn refresh_effects_view(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: Frame,
    ) {
        if !self.owners.effects_enabled() || self.effect_quads.is_empty() {
            return;
        }
        let Ok(view) = crate::gfx_player::View::try_from_inverse_view_projection(
            glam::Mat4::from_cols_array_2d(&frame.matrix).inverse(),
        ) else {
            return;
        };
        let _ = device;
        let Self {
            effects, reported, ..
        } = self;
        let Some(draw) = effects.as_mut() else {
            return;
        };
        let light = gfx_effect_gpu::EffectLight {
            lighting: frame.lighting,
            direction: frame.direction,
            fog_range: frame.fog_range,
            fog_color: frame.fog_color,
            directional_specular: frame.directional_specular,
            eye_direction: frame.eye_direction,
        };
        // Only the frame uniform constants changed after reflection/refraction;
        // reuse the already-uploaded vertex buffer and bucket order instead of
        // re-bucketing and re-uploading every effect quad a second time.
        if let Err(error) = draw.update_params(
            queue,
            frame.matrix,
            frame.clip_plane,
            [view.position.x, view.position.y, view.position.z, 0.],
            Some(light),
        ) {
            if reported.len() < 256 {
                reported.push(format!("world effects: {error}"));
            }
        }
    }

    /// Publish the blended sheets' own motion into the object-motion attachment.
    ///
    /// Called in its own pass right after the colour pass of the main view — the
    /// attachment it writes is not the scene colour — and before any other view
    /// rewrites the frame block or the bucket order. Everything drawn after the
    /// sheets (the water surface, the add-ons) therefore sees the depth buffer the
    /// way the sheets found it: blended content that occludes nothing
    /// (`docs/blockers.md` B-26).
    pub fn draw_effect_motion<'a>(
        &'a mut self,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'a>,
        current_vp: [[f32; 4]; 4],
        extent: (u32, u32),
    ) {
        if let Some(effects) = self.effects.as_mut() {
            effects.draw_motion(queue, pass, current_vp, extent);
        }
    }
    pub fn alpha_order(&self) -> Vec<(usize, usize)> {
        self.alpha.iter().map(|c| (c.slot, c.mesh)).collect()
    }

    /// Why the alpha pass does or does not have anything to order.
    ///
    /// An alpha queue of zero is a *plausible* steady state in this build: the
    /// pass only orders meshes a model declares as alpha-blended, and retail
    /// monsters/props are mostly opaque. The line was unreadable because it
    /// printed the ordered count alone, so "0" could mean "no alpha meshes",
    /// "no model meshes have bounds" or "every slot failed to load" — all
    /// visibly different problems. These counters separate them without
    /// inventing geometry.
    pub fn alpha_status(&self) -> AlphaStatus {
        let mut status = AlphaStatus {
            ordered: self.alpha.len(),
            slots: self.slots.len(),
            ..Default::default()
        };
        for entry in &self.slots {
            if entry.draw.is_some() {
                status.drawable += 1;
            }
            if entry.failed {
                status.failed += 1;
            }
            let Some(draw) = &entry.draw else {
                continue;
            };
            for &mesh in draw.alpha_meshes() {
                status.alpha_meshes += 1;
                if draw.mesh_bounds(mesh).is_none() {
                    status.without_bounds += 1;
                }
            }
        }
        status
    }
}

#[inline]
fn frustum_plane_lengths(planes: [[f32; 4]; 6]) -> [f32; 6] {
    planes.map(|[a, b, c, _]| (a * a + b * b + c * c).sqrt())
}

fn sphere_in_frustum(
    planes: &[[f32; 4]; 6],
    plane_lengths: &[f32; 6],
    center: [f32; 3],
    radius: f32,
) -> bool {
    for (&[a, b, c, d], &len) in planes.iter().zip(plane_lengths) {
        if len > 0.0 && a * center[0] + b * center[1] + c * center[2] + d < -radius * len {
            return false;
        }
    }
    true
}

/// Upload one slot's already-evaluated pose. Split out of `World::draw_models`
/// so the alpha-order snapshot and the attachment parent are read from the same
/// pose the pass draws.
#[allow(clippy::too_many_arguments)]
fn draw_one(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: Frame,
    night: f32,
    position: [f32; 3],
    asset: &crate::model_asset::ModelAsset,
    slot: &mut Slot,
    grid: &StaticGrid,
    sampler: &wgpu::Sampler,
    format: wgpu::TextureFormat,
    read: &mut Box<dyn FnMut(&str) -> Result<Vec<u8>, String>>,
    textures: &mut HashMap<String, (std::sync::Arc<wgpu::Texture>, bool)>,
    receivers: &mut crate::model_draw_gpu::ReceiverCache,
) -> Result<(), String> {
    let posed = match &slot.posed {
        Some(Ok(posed)) => posed,
        Some(Err(e)) => return Err(e.clone()),
        None => return Err("model has no pose for this pass".into()),
    };
    let mut f = frame;
    let point = grid.query(position, night);
    // Ordinary loaded ECMs enable the specular selector through
    // 0x6cfa64 -> 0x7924b0/0x7924c1; the point constants depend on whether the
    // scene query returned a candidate.
    let assignment = crate::model_receiver_class::assign(
        crate::model_receiver_class::MeshKind::Weighted,
        true,
        point.is_some(),
    )?;
    f.specular_variant = assignment.directional_specular;
    f.lighting.point_enabled = assignment.point_constants;
    if let Some(point) = point {
        f.lighting.point_ambient = point.ambient;
        f.lighting.point_diffuse = point.diffuse;
        f.point_position = point.position;
        f.attenuation = point.stored_attenuation;
    }
    if let Some(draw) = &slot.draw {
        if slot.geometry_dirty || slot.vertex_fog != Some(f.vertex_fog) {
            draw.update(queue, asset, &posed.meshes, f)?;
            slot.geometry_dirty = false;
            slot.vertex_fog = Some(f.vertex_fog);
        } else {
            draw.update_constants(queue, asset, f)?;
        }
        return Ok(());
    }
    let created = Draw::with_textures(
        device,
        queue,
        format,
        Some((
            wgpu::TextureFormat::Depth32Float,
            wgpu::CompareFunction::Greater,
        )),
        true,
        sampler,
        asset,
        &posed.meshes,
        f,
        receivers,
        |path| {
            if let Some(texture) = textures.get(path) {
                return Ok(texture.clone());
            }
            let data = read(path)?;
            let (texture, alpha) = crate::model_texture_gpu::upload_with_alpha(
                device,
                queue,
                &data,
                crate::model_texture_gpu::UploadMode::Auto,
            )?;
            let texture = std::sync::Arc::new(texture);
            textures.insert(path.into(), (texture.clone(), alpha));
            Ok((texture, alpha))
        },
    )?;
    slot.geometry_dirty = false;
    slot.vertex_fog = Some(f.vertex_fog);
    slot.draw = Some(created);
    Ok(())
}

/// Event milliseconds from an ECM event field. Missing fields are explicit
/// failures: the light owner must not invent a start time or a lifetime.
fn event_milliseconds(event: &crate::ecmodel::EcmEvent, key: &str) -> Result<f32, String> {
    let Some((_, value)) = event.fields.iter().find(|(name, _)| name == key) else {
        return Err(format!("GFX event is missing {key}"));
    };
    let value: f32 = value
        .parse()
        .map_err(|_| format!("invalid GFX event {key}: {value}"))?;
    if !value.is_finite() {
        return Err(format!("nonfinite GFX event {key}"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    #[test]
    fn model_spatial_cells_preserve_original_indices_and_keep_sphere_overlaps() {
        let cells = super::build_model_spatial_cells([
            (0, [100.0, 0.0, 0.0]),
            (1, [200.0, 0.0, 0.0]),
            (2, [1024.0, 0.0, 0.0]),
        ]);
        let frustum = [
            [1.0, 0.0, 0.0, 100.0],
            [-1.0, 0.0, 0.0, 100.0],
            [0.0, 1.0, 0.0, 100.0],
            [0.0, -1.0, 0.0, 100.0],
            [0.0, 0.0, 1.0, 100.0],
            [0.0, 0.0, -1.0, 100.0],
        ];
        let mut visible = Vec::new();
        super::append_visible_model_slots(&cells, Some(&frustum), None, &mut visible);
        visible.sort_unstable();
        assert_eq!(visible, vec![0, 1]);
    }

    use super::*;

    #[test]
    fn world_model_frustum_plane_lengths_are_cached_without_changing_sphere_tests() {
        let planes = [
            [2., 0., 0., 20.],
            [-3., 0., 0., 30.],
            [0., 4., 0., 40.],
            [0., -5., 0., 50.],
            [0., 0., 6., 60.],
            [0., 0., 0., 1.],
        ];
        let lengths = frustum_plane_lengths(planes);
        assert_eq!(lengths, [2., 3., 4., 5., 6., 0.]);
        assert!(sphere_in_frustum(&planes, &lengths, [0., 0., 0.], 128.));
        assert!(!sphere_in_frustum(&planes, &lengths, [-100., 0., 0.], 1.));
        // A zero-normal plane retains the old rule: it never rejects a sphere.
        let mut zero_only = [[0.; 4]; 6];
        zero_only[0][3] = -1000.;
        let zero_lengths = frustum_plane_lengths(zero_only);
        assert!(sphere_in_frustum(&zero_only, &zero_lengths, [0.; 3], 0.));
    }

    /// `FadeGFX` is `SetAlphaTransition(alpha, 3000)`: a full ramp takes three
    /// seconds at any frame rate, and the step never overshoots.
    #[test]
    fn the_distance_fade_takes_three_seconds_either_way() {
        let mut fade = 1.;
        for _ in 0..30 {
            fade = attenuation_step(fade, 0., 100);
        }
        assert!(fade < 1e-6);
        let mut fade = 0.;
        for _ in 0..60 {
            fade = attenuation_step(fade, 1., 50);
        }
        assert!(fade > 1. - 1e-6);
        assert_eq!(attenuation_step(0.25, 1., 3000), 1.);
        assert_eq!(attenuation_step(0.25, 0., 3000), 0.);
        // A stalled clock steps nothing; one huge frame lands on the target.
        assert_eq!(attenuation_step(0.5, 1., 0), 0.5);
        assert_eq!(attenuation_step(0.5, 1., 100_000), 1.);
    }

    /// The fade follows `vHostPos`, a ground position, so the camera's height is
    /// not part of the distance, and a distance of zero is the fade switched
    /// off: every root it had dimmed comes back to the alpha the map authored.
    #[test]
    fn the_fade_is_horizontal_and_zero_restores_full_alpha() {
        let ground = [100., 200., -4000.];
        assert_eq!(attenuation_target(60., ground, [100., 700., -4000.]), 1.);
        assert_eq!(attenuation_target(60., ground, [1000., 200., -4000.]), 0.);
        assert_eq!(attenuation_target(0., ground, [9000., 0., 9000.]), 1.);
        assert_eq!(attenuation_target(60., ground, [159.9, 200., -4000.]), 1.);
        assert_eq!(attenuation_target(60., ground, [160.1, 200., -4000.]), 0.);
    }
}
