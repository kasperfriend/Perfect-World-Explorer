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
    geometry_dirty: bool,
    vertex_fog: Option<f32>,
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
    slots: Vec<Slot>,
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
    terrain: Option<Box<dyn Fn(f32, f32) -> Option<f32>>>,
    producers: Vec<Producer>,
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
    /// Why the last activation pass left producers out, so a zone that draws
    /// nothing can say which gate closed on it: beyond the activation radius,
    /// outside the resident terrain window, or outside its day/night window.
    census: [usize; 3],
    resident_bounds: Option<[f32; 4]>,
    /// Retail alpha insertion order as (slot, mesh) pairs.
    alpha: Vec<model_queue::AlphaCandidate>,
    reported: Vec<String>,
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
        let slots = assets
            .instances
            .iter()
            .map(|i| Slot {
                draw: None,
                failed: false,
                transform: crate::world_models::transform(&i.placement),
                posed: None,
                geometry_dirty: true,
                vertex_fog: None,
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
            slots,
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
            effects: None,
            effect_trace_printed: false,
            effect_quads: Vec::new(),
            catalog: crate::gfx_catalog::Catalog::default(),
            catalog_spawns: 0,
            view: None,
            effect_distance: 0.,
            census: [0; 3],
            resident_bounds: None,
            alpha: Vec::new(),
            reported: Vec::new(),
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
        self.read = Box::new(move |path: &str| match gfx_entries.get(path) {
            Some(entry) => crate::pck::read_entry(&mut gfx_pack, entry),
            None => previous(path),
        });
        // Root text is a separate handle so a root load and a texture load can
        // never interleave their file positions.
        let root_entries = entries.clone();
        let mut pack = File::open(element.join("gfx.pck")).map_err(|e| format!("gfx.pck: {e}"))?;
        self.owners.set_reader(move |path: &str| {
            let entry = root_entries
                .get(path)
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
                let identity = MODEL_EVENT_IDENTITY + self.producers.len() as u64;
                let request = gfx_owner::Request {
                    identity,
                    path: gfx_owner::root_path(&path),
                    // A hooked root's local frame is the hook record and its
                    // parent is installed per frame; a hook-free root is placed
                    // by the model transform the native event inherits.
                    transform: match &record {
                        Some(_) => local.to_cols_array(),
                        None => transform.to_cols_array(),
                    },
                    speed: asset.descriptor.render_settings().default_speed,
                    start_ms: start_ms as u32,
                    span_ms,
                    outer: None,
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
        for (instance, slot) in self.assets.instances.iter().zip(&mut self.slots) {
            if !cutoff_residency::contains(bounds, instance.placement.position) {
                slot.draw = None;
                slot.posed = None;
                slot.geometry_dirty = true;
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
        let mut census = [0usize; 3];
        let updates: Vec<(u64, bool)> = self
            .producers
            .iter()
            .map(|producer| {
                let dx = producer.position[0] - x;
                let dz = producer.position[2] - z;
                let near = dx * dx + dz * dz <= radius * radius;
                let resident = cutoff_residency::contains(self.resident_bounds, producer.position);
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
                (producer.identity, near && resident && visible)
            })
            .collect();
        self.census = census;
        for (identity, active) in updates {
            self.owners.set_active(identity, active);
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
        let off = self.effect_distance <= 0.;
        let mut updates: Vec<(u64, f32)> = Vec::new();
        for producer in self.producers.iter_mut().filter(|p| p.attenuation) {
            if off {
                if producer.fade != 1. {
                    producer.fade = 1.;
                    updates.push((producer.identity, 1.));
                }
                continue;
            }
            let target = attenuation_target(self.effect_distance, producer.position, eye);
            producer.fade = attenuation_step(producer.fade, target, delta_ms);
            updates.push((producer.identity, producer.fade));
        }
        for (identity, fade) in updates {
            self.owners.set_attenuation(identity, fade);
        }
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
        F: Fn(f32, f32) -> Option<f32> + 'static,
    {
        let broken = std::cell::Cell::new(false);
        self.terrain = Some(Box::new(move |x: f32, z: f32| {
            if broken.get() {
                return None;
            }
            let read = || height(x, z);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)) {
                Ok(value) => value,
                Err(_) => {
                    broken.set(true);
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
                if !self.reported.contains(&error) {
                    self.reported.push(error);
                }
                self.effect_quads.clear();
                return;
            }
        };
        // The field and the owner manager are separate places, so the sampler
        // borrows one and the pass advances the other.
        let sampler: Option<crate::gfx_player::GroundHeightQuery<'_>> = self.terrain.as_deref();
        let quads = self
            .owners
            .effect_quads_with_ground(delta_ms, &view, sampler);
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
                quads.len(),
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
        *effect_quads = quads;
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
                    reported.push(format!("world effects: {error}"));
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
            reported.push(format!("world effects: {error}"));
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
            self.effect_quads.clear();
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
        for (instance, slot) in self.assets.instances.iter_mut().zip(&mut self.slots) {
            if !cutoff_residency::contains(bounds, instance.placement.position) {
                continue;
            }
            if slot.failed {
                continue;
            }
            let asset = &self.assets.assets[instance.asset];
            let sample = match instance.playback.as_mut() {
                Ok(clock) => {
                    if delta_ms != 0 {
                        if let Err(e) = clock.tick(delta_ms) {
                            slot.posed = Some(Err(e));
                            continue;
                        }
                    }
                    clock.sample()
                }
                Err(e) => {
                    slot.posed = Some(Err(e.clone()));
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
        let mut resolved: Vec<(u64, Result<Option<[f32; 16]>, String>)> = Vec::new();
        for producer in &self.producers {
            let Some(binding) = &producer.hook else {
                continue;
            };
            let origin = format!("producer {}", producer.identity);
            let outcome = (|| -> Result<Option<[f32; 16]>, String> {
                let slot = self
                    .slots
                    .get(binding.instance)
                    .ok_or_else(|| format!("{origin}: parent slot is gone"))?;
                let parent = self
                    .assets
                    .instances
                    .get(binding.instance)
                    .ok_or_else(|| format!("{origin}: parent instance is gone"))?;
                let posed = match &slot.posed {
                    Some(Ok(posed)) => posed,
                    Some(Err(e)) => return Err(format!("{origin}: parent pose failed: {e}")),
                    None => return Err(format!("{origin}: parent has no pose")),
                };
                let asset = self
                    .assets
                    .assets
                    .get(parent.asset)
                    .ok_or_else(|| format!("{origin}: parent asset is gone"))?;
                let hook = asset.hook_matrix(&posed.pose, &binding.record.hook)?;
                let composed = model_attachment::world(hook, binding.local)?;
                Ok(Some(composed.to_cols_array()))
            })();
            resolved.push((producer.identity, outcome));
        }
        let mut detached = Vec::new();
        for (identity, outcome) in resolved {
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
        self.producers.retain(|p| !detached.contains(&p.identity));
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
        let bounds = self.resident_bounds;
        let Self {
            assets,
            slots,
            read,
            textures,
            receivers,
            sampler,
            format,
            grid,
            reported,
            ..
        } = self;
        for (instance, slot) in assets.instances.iter().zip(slots.iter_mut()) {
            if !cutoff_residency::contains(bounds, instance.placement.position) {
                continue;
            }
            if slot.failed {
                continue;
            }
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
                slot.draw = None;
            }
        }
    }
    /// Retail alpha insertion walk over this frame's alpha meshes. Ordering uses
    /// the eight transformed initial-AABB-corner distance; the per-mesh
    /// priority weights of the original insertion routine were not recovered, so
    /// they stay zero and the comparison falls through to distance.
    fn rebuild_alpha_order(&mut self, eye: [f32; 3]) {
        let mut candidates: Vec<model_queue::AlphaCandidate> = Vec::new();
        let mut failures: Vec<(usize, String)> = Vec::new();
        for (slot, entry) in self.slots.iter().enumerate() {
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
                    Ok(distance) => candidates.push(model_queue::AlphaCandidate {
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
                    Err(e) => failures.push((slot, e)),
                }
            }
        }
        match model_queue::alpha_order(&candidates) {
            Ok(order) => self.alpha = order,
            Err(e) => {
                self.alpha.clear();
                self.reported.push(format!("alpha queue: {e}"));
            }
        }
        for (slot, error) in failures {
            self.reported
                .push(format!("alpha queue slot {slot}: {error}"));
            if let Some(entry) = self.slots.get_mut(slot) {
                entry.failed = true;
                entry.draw = None;
            }
            self.alpha.retain(|candidate| candidate.slot != slot);
        }
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
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        for slot in &self.slots {
            if let Some(draw) = &slot.draw {
                draw.draw_opaque(pass);
            }
        }
        for candidate in &self.alpha {
            if let Some(Some(draw)) = self.slots.get(candidate.slot).map(|s| &s.draw) {
                draw.draw_alpha_mesh(pass, candidate.mesh);
            }
        }
        // Blended effect quads last, after the opaque models and the alpha meshes,
        // with their own render-layer sort inside the draw object.
        if let Some(effects) = &self.effects {
            effects.draw(pass);
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
    use super::*;

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
