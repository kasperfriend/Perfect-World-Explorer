//! Populator add-on: spawns the real world population the way the game does.
//!
//! Scope, in the client's own terms: the shipped `element/data/task_npc.data`
//! places 1,572 quest NPCs, each one resolved to its `NPC_ESSENCE` template and
//! drawn with that template's `.ecm` rig out of `models.pck`. Mobs have no
//! client-side placement file (the server owns `gamedata/npcgen.data`), so the
//! module loads the 4,208 real `MONSTER_ESSENCE` templates and draws no mob
//! until a placement source is supplied. Native actors move only along real
//! navigation paths from server data. An explicitly enabled Live World
//! (Experimental) prototype tries synthetic city residents using original
//! human rigs, validated retargeted locomotion clips and safe local routes.
//!
//! Everything here is module-owned: archives, rig cache, texture cache, GPU
//! draws, simulation and settings. The host supplies a camera position (which
//! populates the streamed set) and one render hook.

mod archive;
mod data;
mod draw;
mod nameplate;
mod patrol;

use self::{
    archive::Archive,
    data::{Population, Spawn, TemplateKind},
    draw::Renderer,
    nameplate::NameFont,
    patrol::{Follow, Motion, Route, Waypoint},
};
use crate::{
    animation::{BoneTrack, KeyTrack},
    model_asset::{ModelAsset, PosedMesh, TrackCache},
    model_draw_gpu,
    model_playback::Playback,
    navigation::MapSpace,
};
use glam::{Mat4, Vec3};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
};

/// The client's shipped quality preset reserves a seven-block terrain window,
/// 7 x 64 = 420 world units (`view_settings::DetailSettings`, `world+0x1bc`;
/// preset table `0x40e41f`). The default spawn distance is therefore the value
/// the game itself uses, not a chosen one.
pub const NATIVE_SPAWN_DISTANCE: f32 = 420.;
pub const MIN_SPAWN_DISTANCE: f32 = 40.;
pub const MAX_SPAWN_DISTANCE: f32 = 1_200.;
const DEFAULT_MAX_VISIBLE: usize = 48;
const MAX_VISIBLE_LIMIT: usize = 96;
/// Per-city ambient density is configurable; generated records are bounded so
/// this remains a visual enhancement rather than a client-side server emulator.
const LIVE_WORLD_DEFAULT_RADIUS: f32 = 240.0;
pub const LIVE_WORLD_MIN_RADIUS: f32 = 120.0;
pub const LIVE_WORLD_MAX_RADIUS: f32 = 420.0;
const LIVE_WORLD_DEFAULT_PER_ZONE: usize = 18;
pub const LIVE_WORLD_MAX_PER_ZONE: usize = 48;
const LIVE_WORLD_MAX_RESIDENTS: usize = 512;
const LIVE_WORLD_ID_BASE: u32 = 0x8000_0000;
const LIVE_WORLD_BATCH: usize = 24;
const LIVE_WORLD_ATTEMPTS: u64 = 24;
const LIVE_WORLD_BUILD_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);
const LIVE_WORLD_MIN_SEPARATION: f32 = 4.5;
const LIVE_WORLD_SAMPLE_SPACING: f32 = 0.35;
const LIVE_WORLD_MAX_ROUTE_RADIUS: f32 = 14.0;
const LIVE_WORLD_RUN_SPEED: f32 = 1.9;
/// Refresh the cached nearest set only after a useful batch has accumulated;
/// rescanning and sorting the whole world for every generated citizen would
/// defeat the existing stream-cache optimization.
const LIVE_WORLD_STREAM_REFRESH: usize = 64;
/// Rigs are decompressed from an 888 MB `models.pck` on the render thread, so a
/// teleport into a crowded town fills the population over a few frames instead
/// of stalling one.
const MAX_SPAWNS_PER_UPDATE: usize = 6;
/// How far the camera must move before the placement table is searched again.
/// A still camera - the normal case while walking through a town - reuses the
/// previous nearest set instead of re-scanning and re-sorting every placement.
const STREAM_CACHE_MOVE: f32 = 2.;

/// Wall-clock budget for those spawns. A frame that already spent this long
/// decompressing rigs finishes the rest of the frame without loading another, so
/// heavy rigs cannot turn a teleport into a stutter.
const SPAWN_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);
/// Distance past which an actor is re-posed every other frame, and past which
/// every third. A 2.3 s idle sampled at 30 Hz is indistinguishable from 60 Hz at
/// this distance, and it is what keeps a town's whole crowd from costing a full
/// skinning pass and vertex upload every frame.
const POSE_STRIDE_NEAR: f32 = 80.;
const POSE_STRIDE_FAR: f32 = 200.;
/// Overhead names are outlined within this distance; farther away the outline
/// only adds four more text shapes per label, so those get the fill alone.
const NAME_OUTLINE_DISTANCE: f32 = 48.;
/// Seconds between overhead-name occlusion tests. Every test is a hull raycast
/// per actor, and a name does not change visibility faster than this; bodies are
/// never affected by the throttle.
const NAME_VISIBILITY_INTERVAL: f32 = 0.2;
/// Overhead names are legible at the client's own scene window, so they default
/// to the same radius; the slider lets a user shorten them.
pub const NATIVE_NAME_DISTANCE: f32 = NATIVE_SPAWN_DISTANCE;
pub const MIN_NAME_DISTANCE: f32 = 20.;
/// Height of one carried name above the actor's feet, used when a rig exposes no
/// usable bind-pose extent.
const NAME_FALLBACK_HEIGHT: f32 = 1.9;
/// Clearance between the top of the head and the baseline of the name.
const NAME_LIFT: f32 = 0.25;
/// How far the terrain must rise above the name's sight line before it counts
/// as covering it. The ridge of a hill hides the labels behind it, a flat plain
/// never does.
const NAME_SIGHT_CLEARANCE: f32 = 0.05;
/// Samples taken along one name's sight line against the terrain height field.
const NAME_SIGHT_STEPS: usize = 12;
/// How far above the bare terrain a placement may be lifted onto a building
/// floor. Bridge decks and interior floors sit well inside this; a rooftop far
/// overhead is not a placement surface and is not reached.
const MAX_FLOOR_LIFT: f32 = 16.;
/// Horizontal half-width and half-height of the AABB a placement stands in. Only
/// the vertical half-extent changes the support result, and a humanoid box is
/// what the client uses for its own actors.
const ACTOR_EXTENTS: Vec3 = Vec3::new(0.5, 1.0, 0.5);

/// Probe the full conservative actor box: a visibly clipped body is worse than
/// shifting a placement a little farther out of a wall.
const WALL_PROBE: Vec3 = ACTOR_EXTENTS;

/// Rings a placement is searched outward when its authored column is inside
/// geometry, nearest first. A town NPC authored into a wall is normally a metre
/// or two from the free space in front of it. The search extends to six units
/// for deep overlaps, while the UI reports placements that still have no clear spot.
const WALL_RINGS: [f32; 8] = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 6.0];

/// Sixteen directions find openings between perpendicular walls while keeping
/// the search deterministic (same placement, same result).
const WALL_DIRECTIONS: usize = 16;

/// How far the height of a nudged spot may differ from the placement's own, so
/// getting out of a wall never also moves the NPC up a storey or off a bridge.
const WALL_RING_LIFT: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// World-space radius around the camera that keeps actors spawned.
    pub spawn_distance: f32,
    /// Hard cap on simultaneously drawn actors.
    pub max_visible: usize,
    /// Play the template's own `站立` scene action.
    pub idle_animation: bool,
    /// Draw the template's own name above the actor, as the game does.
    pub show_names: bool,
    /// Distance at which overhead names are still drawn.
    pub name_distance: f32,
    /// Resolve a placement against real BMD floors (bridge decks, interior
    /// floors, walls) before falling back to the terrain sample.
    pub stand_on_objects: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            spawn_distance: NATIVE_SPAWN_DISTANCE,
            max_visible: DEFAULT_MAX_VISIBLE,
            idle_animation: true,
            show_names: true,
            name_distance: NATIVE_NAME_DISTANCE,
            stand_on_objects: true,
        }
    }
}

impl Settings {
    fn sanitized(self) -> Self {
        Self {
            spawn_distance: if self.spawn_distance.is_finite() {
                self.spawn_distance
                    .clamp(MIN_SPAWN_DISTANCE, MAX_SPAWN_DISTANCE)
            } else {
                NATIVE_SPAWN_DISTANCE
            },
            max_visible: self.max_visible.clamp(1, MAX_VISIBLE_LIMIT),
            name_distance: if self.name_distance.is_finite() {
                self.name_distance
                    .clamp(MIN_NAME_DISTANCE, MAX_SPAWN_DISTANCE)
            } else {
                NATIVE_NAME_DISTANCE
            },
            ..self
        }
    }
}

/// Real building hulls, borrowed by the host for one frame. The Populator only
/// asks where the floor is; the Walker module owns the geometry and the loader.
pub trait FloorSource {
    /// Lowest walkable building floor at or above `feet`, if the column holds
    /// one within `max_up`.
    fn lowest_floor(&self, feet: Vec3, extents: Vec3, max_up: f32) -> Option<f32>;
    /// Changes whenever more hulls have streamed in, so placements resolved
    /// against an incomplete world can be resolved again.
    fn floor_stamp(&self) -> u64;
    /// True when a building hull or a tree stands between `from` and `to`, used
    /// to keep overhead names from showing through geometry.
    fn sight_blocked(&self, from: Vec3, to: Vec3) -> bool;
    /// True when the box a placement occupies already overlaps building
    /// geometry, i.e. that NPC was authored inside a wall. Sources that cannot
    /// answer keep every placement where the files put it.
    fn solid_at(&self, feet: Vec3, extents: Vec3) -> bool {
        let _ = (feet, extents);
        false
    }
    /// Client-authored water surface at this horizontal point, if its mask
    /// contains it. Ambient residents reject points below this surface.
    fn water_height(&self, _x: f32, _z: f32) -> Option<f32> {
        None
    }
}

/// One action clip wrapped on its own clock.
///
/// `ModelAsset::pose_action_at_time` takes absolute track time and clamps past
/// the last key, so an ever-growing clock freezes a client idle after a single
/// cycle - which is exactly what a rig without a schedulable combined action
/// did. Wrapping the clock inside the clip's own authored frame range keeps it
/// looping for as long as the actor stands there.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Clip {
    start_ms: u32,
    length_ms: u32,
}

impl Clip {
    fn new(asset: &ModelAsset, action: usize) -> Option<Self> {
        let clip = asset.model.actions.get(action)?;
        let fps = clip
            .fps
            .filter(|fps| *fps > 0)
            .unwrap_or(asset.skeleton.fps);
        if fps <= 0
            || !clip.start.is_finite()
            || !clip.end.is_finite()
            || clip.start < 0.
            || clip.end < clip.start
        {
            return None;
        }
        // Same millisecond range as `Playback::new`: the client truncates both
        // endpoints and treats the clip as end - start + 1 milliseconds.
        let start = (clip.start * 1000. / fps as f32).trunc();
        let end = (clip.end * 1000. / fps as f32).trunc();
        let length = end - start + 1.;
        if !length.is_finite() || length < 1. || end >= u32::MAX as f32 {
            return None;
        }
        Some(Self {
            start_ms: start as u32,
            length_ms: length as u32,
        })
    }

    fn seconds(&self, clock: f32) -> f32 {
        if !clock.is_finite() || clock <= 0. {
            return self.start_ms as f32 * 0.001;
        }
        let elapsed = (clock as f64 * 1000.) as u64 % self.length_ms as u64;
        (self.start_ms as u64 + elapsed) as f32 * 0.001
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GaitKind {
    Walk,
    Run,
}

#[derive(Clone, Debug)]
struct GaitClip {
    clip: Clip,
    tracks: std::sync::Arc<Vec<BoneTrack>>,
}

#[derive(Clone, Debug)]
struct RetargetedGait {
    walk: GaitClip,
    run: GaitClip,
}

/// Module-owned `models.pck`/`textures.pck` rig and texture source.
struct Library {
    models: Archive,
    textures: Option<Archive>,
    tracks: TrackCache,
    assets: HashMap<String, Rc<ModelAsset>>,
    failures: HashMap<String, String>,
    gait_sources: HashMap<String, Rc<ModelAsset>>,
    gait_source_failures: HashMap<String, String>,
    retargeted_gaits: HashMap<String, Option<RetargetedGait>>,
    /// Bind-pose head height per rig, for the overhead name. Computed once per
    /// template rather than once per actor.
    heads: HashMap<String, f32>,
}

/// `R_` NPC textures store a specular mask in alpha, not surface opacity.
/// Without the same per-mesh policy used by the model designer and preview,
/// the alpha channel punches holes through the residents' bodies.
fn reflect_prefix_texture(path: &str) -> bool {
    crate::ecmodel::normalize(path)
        .rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with("r_"))
}

fn apply_reflect_prefix_alpha(asset: &mut ModelAsset) {
    for (skin_index, skin) in asset.skins.iter().enumerate() {
        for (mesh_index, mesh) in skin.data.meshes.iter().enumerate() {
            let is_reflect = usize::try_from(mesh.texture)
                .ok()
                .and_then(|texture| skin.data.textures.get(texture))
                .is_some_and(|texture| reflect_prefix_texture(texture));
            if is_reflect {
                asset.diffuse_alpha_meshes.insert((skin_index, mesh_index));
            }
        }
    }
}

fn read_archive(
    models: &mut Archive,
    textures: Option<&mut Archive>,
    path: &str,
) -> Result<Vec<u8>, String> {
    let models_error = match models.read(path) {
        Ok(bytes) => return Ok(bytes),
        Err(error) => error,
    };
    match textures {
        Some(textures) => textures.read(path),
        // A missing textures.pck is not fatal: models.pck carries the player
        // and NPC texture sheets alongside the rigs.
        None => Err(models_error),
    }
}

impl Library {
    fn open(element: &std::path::Path) -> Result<Self, String> {
        let models = Archive::open(&element.join("models.pck"))?;
        let textures = Archive::open(&element.join("textures.pck")).ok();
        Ok(Self {
            models,
            textures,
            tracks: TrackCache::default(),
            assets: HashMap::new(),
            failures: HashMap::new(),
            gait_sources: HashMap::new(),
            gait_source_failures: HashMap::new(),
            retargeted_gaits: HashMap::new(),
            heads: HashMap::new(),
        })
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
        read_archive(&mut self.models, self.textures.as_mut(), path)
    }

    /// Keep the model archive as the canonical source (it carries NPC texture
    /// sheets beside their rigs), but retain a duplicate from `textures.pck` as
    /// a decode fallback if the model-pack payload is damaged or unsupported.
    fn texture_candidates(&mut self, path: &str) -> Result<Vec<Vec<u8>>, String> {
        let mut candidates = Vec::new();
        let mut failures = Vec::new();
        match self.models.read(path) {
            Ok(bytes) => candidates.push(bytes),
            Err(error) => failures.push(error),
        }
        if let Some(textures) = self.textures.as_mut() {
            match textures.read(path) {
                Ok(bytes) => candidates.push(bytes),
                Err(error) => failures.push(error),
            }
        }
        if candidates.is_empty() {
            Err(failures.join("; "))
        } else {
            Ok(candidates)
        }
    }

    /// Height of the rig's head above its feet in the authored bind pose. A rig
    /// whose bind pose cannot be evaluated reports the humanoid default and does
    /// not fail the actor; only the name's baseline moves.
    fn head(&mut self, key: &str, asset: &ModelAsset) -> f32 {
        if let Some(height) = self.heads.get(key) {
            return *height;
        }
        let height = asset
            .pose_bind(Mat4::IDENTITY)
            .ok()
            .and_then(|meshes| {
                meshes.iter().fold(None::<f32>, |best, mesh| {
                    mesh.vertices.iter().fold(best, |best, vertex| {
                        let y = vertex.position[1];
                        if !y.is_finite() {
                            return best;
                        }
                        Some(best.map_or(y, |best: f32| best.max(y)))
                    })
                })
            })
            .filter(|height| (0.5..=8.0).contains(height))
            .unwrap_or(NAME_FALLBACK_HEIGHT);
        self.heads.insert(key.to_string(), height);
        height
    }

    /// Load a gait donor without its meshes or textures: only its authored
    /// skeleton and animation tracks are used to retarget locomotion.
    fn gait_source(&mut self, path: &str) -> Result<Rc<ModelAsset>, String> {
        let key = crate::ecmodel::normalize(path);
        if let Some(asset) = self.gait_sources.get(&key) {
            return Ok(asset.clone());
        }
        if let Some(error) = self.gait_source_failures.get(&key) {
            return Err(error.clone());
        }
        let loaded = {
            let models = &mut self.models;
            let mut read = |path: &str| models.read(path);
            ModelAsset::load_without_skins_with(&key, &mut read)
        };
        match loaded {
            Ok(asset) => {
                let asset = Rc::new(asset);
                self.gait_sources.insert(key, asset.clone());
                Ok(asset)
            }
            Err(error) => {
                self.gait_source_failures.insert(key, error.clone());
                Err(error)
            }
        }
    }

    /// Cache only validated walk/run pairs. The donor contributes animation
    /// data, never a visible model; rigs that fail validation retain the
    /// bind-pose route.
    ///
    /// The donor is the normal player's rig for the resident's gender
    /// (the client's `形象` appearance bodies). The resident plays the
    /// donor's authored local rotations verbatim: both rigs trace the same
    /// parent chain (name- and parent-checked), so the composed pose is the
    /// donor's pose rooted at the resident's route placement - at every
    /// sample the resident plays what the player plays. The previous
    /// parent-frame delta transport was refuted in game: it preserves swing
    /// *angles* but anchors every bone to the resident's combat-stance bind,
    /// which left the pre-flexed arms held across the belly and behind the
    /// back. Measured on the retail client
    /// (`Findings/recovery/gait-evidence.md`): every numbered male and
    /// female resident rig maps all 23 locomotion bones from its gender
    /// donor, walk and run, and selects the generic `行走_通用` /
    /// `奔跑_通用` cycles. Guard donors were retired as the shipped source;
    /// the gender-correct player donor is a strict superset match.
    fn retargeted_gait(&mut self, key: &str, target: &ModelAsset) -> Option<RetargetedGait> {
        if let Some(gait) = self.retargeted_gaits.get(key) {
            return gait.clone();
        }
        const MALE_DONOR: &str = "models/players/形象/武侠男/躯干/武侠男.ecm";
        const FEMALE_DONOR: &str = "models/players/形象/武侠女/躯干/武侠女.ecm";
        let path = match live_world_gender(key) {
            Some(NpcGender::Masculine) => MALE_DONOR,
            Some(NpcGender::Feminine) => FEMALE_DONOR,
            None => {
                self.retargeted_gaits.insert(key.to_string(), None);
                return None;
            }
        };
        let gait = self
            .gait_source(path)
            .ok()
            .and_then(|source| retargeted_gait_from(&source, target));
        self.retargeted_gaits.insert(key.to_string(), gait.clone());
        gait
    }

    fn asset(&mut self, path: &str) -> Result<Rc<ModelAsset>, String> {
        let key = crate::ecmodel::normalize(path);
        if let Some(asset) = self.assets.get(&key) {
            return Ok(asset.clone());
        }
        if let Some(error) = self.failures.get(&key) {
            return Err(error.clone());
        }
        let Self {
            models,
            textures,
            tracks,
            assets,
            failures,
            heads: _,
            ..
        } = self;
        let mut read = |path: &str| read_archive(models, textures.as_mut(), path);
        let loaded = ModelAsset::load_cached(&key, &mut read, tracks);
        match loaded {
            Ok(mut asset) => {
                apply_reflect_prefix_alpha(&mut asset);
                let asset = Rc::new(asset);
                assets.insert(key, asset.clone());
                Ok(asset)
            }
            Err(error) => {
                failures.insert(key, error.clone());
                Err(error)
            }
        }
    }
}

/// Bones that carry locomotion. A borrowed clip is accepted only when every
/// one of them maps onto the target rig; everything else (thumb chains,
/// finger anim joints, ponytails, prop attachment helpers) is optional.
///
/// This replaces a "most donor tracks mapped" heuristic, which rejected the
/// player rigs as donors for low-detail residents: a 26-bone resident maps
/// all 23 of these from either player body, but only ~61-71% of the donor's
/// 35-41 tracks, because the surplus tracks are joints the resident rig never
/// had.
const GAIT_CORE_BONES: [&str; 23] = [
    "Bip01",
    "Bip01 Pelvis",
    "Bip01 Spine",
    "Bip01 Spine1",
    "Bip01 Spine2",
    "Bip01 Neck",
    "Bip01 Head",
    "Bip01 L Clavicle",
    "Bip01 L UpperArm",
    "Bip01 L Forearm",
    "Bip01 L Hand",
    "Bip01 R Clavicle",
    "Bip01 R UpperArm",
    "Bip01 R Forearm",
    "Bip01 R Hand",
    "Bip01 L Thigh",
    "Bip01 L Calf",
    "Bip01 L Foot",
    "Bip01 L Toe0",
    "Bip01 R Thigh",
    "Bip01 R Calf",
    "Bip01 R Foot",
    "Bip01 R Toe0",
];

/// Ordering tier for locomotion candidates: 0 = the generic cycle names,
/// 1 = every other locomotion-named slice. Stable sorting keeps file order
/// within a tier.
fn gait_candidate_rank(name: &str) -> u8 {
    const PLAIN: [&str; 9] = [
        "行走_通用",
        "移动_通用",
        "行走",
        "移动",
        "走",
        "奔跑_通用",
        "跑_通用",
        "奔跑",
        "跑",
    ];
    u8::from(!PLAIN.contains(&name.to_lowercase().as_str()))
}

/// Rotation-only absolute rest orientations of a skeleton's bones, indexed by
/// bone index and composing parent * local like the engine does. Resolution is
/// memoised and depth-limited; corrupt cyclic parent data resolves to `None`
/// so a garbage frame can never carry an animation.
/// Determinant of a bone's rest frame; non-positive means the "rotation" is a
/// reflection or degenerate projection, not a rotation. Such a bone keeps its
/// bind pose instead of transplanting its swing onto a garbage frame.
fn rest_frame_det(bone: &crate::skeleton::Bone) -> f32 {
    let frame = Mat4::from_cols_array(&bone.relative);
    frame
        .x_axis
        .truncate()
        .cross(frame.y_axis.truncate())
        .dot(frame.z_axis.truncate())
}

fn gait_kind(name: &str) -> Option<GaitKind> {
    if !locomotion_name(name) {
        return None;
    }
    let name = name.to_ascii_lowercase();
    if ["run", "奔跑", "跑"].iter().any(|part| name.contains(part)) {
        Some(GaitKind::Run)
    } else if ["walk", "走", "行走", "move", "移动"]
        .iter()
        .any(|part| name.contains(part))
    {
        Some(GaitKind::Walk)
    } else {
        None
    }
}

fn bone_parent_name(asset: &ModelAsset, parent: i32) -> Option<&str> {
    usize::try_from(parent)
        .ok()
        .and_then(|index| asset.skeleton.bones.get(index))
        .map(|bone| bone.name.as_str())
}

fn retarget_gait_clip(
    source: &ModelAsset,
    target: &ModelAsset,
    kind: GaitKind,
) -> Option<GaitClip> {
    let target_joints: HashMap<&str, usize> = target
        .skeleton
        .joints
        .iter()
        .enumerate()
        .map(|(index, joint)| (joint.name.as_str(), index))
        .collect();
    if target_joints.len() != target.skeleton.joints.len() {
        return None;
    }
    // The client's file order puts the bound/grapple frames first. Played on a
    // resident those fold the legs into a squat or a knee-lift - what the playtest
    // saw as "sitting in air" - while the generic cycle slices (`行走_通用` /
    // `奔跑_通用` and their bare-name twins) are the upright walks real players use.
    // Plain names win outright; ties and unknowns keep file order.
    let mut candidates: Vec<usize> = source
        .model
        .actions
        .iter()
        .enumerate()
        .filter(|(_, action)| gait_kind(&action.name) == Some(kind))
        .map(|(index, _)| index)
        .collect();
    candidates.sort_by_key(|&index| gait_candidate_rank(&source.model.actions[index].name));
    for action_index in candidates {
        let action = &source.model.actions[action_index];
        let Some(clip) = Clip::new(source, action_index) else {
            continue;
        };
        let Some(source_tracks) = source
            .tracks
            .get(action_index)
            .and_then(Option::as_ref)
            .map(|tracks| tracks.tracks.as_slice())
            .or_else(|| {
                (!source.skeleton.embedded_tracks.is_empty())
                    .then_some(source.skeleton.embedded_tracks.as_slice())
            })
        else {
            continue;
        };
        if source_tracks.is_empty() {
            continue;
        }
        let mut seen = HashSet::new();
        let mut mapped_bones: HashSet<&str> = HashSet::new();
        let mut retargeted = Vec::with_capacity(source_tracks.len());
        let mut compatible = true;
        for track in source_tracks {
            let Some(source_joint) = usize::try_from(track.id)
                .ok()
                .and_then(|index| source.skeleton.joints.get(index))
            else {
                compatible = false;
                break;
            };
            let Some(&target_joint_index) = target_joints.get(source_joint.name.as_str()) else {
                // Donor rigs can carry extra attachment, finger-chain and
                // helper joints the resident rig never had. Ignore those;
                // acceptance is decided by locomotion coverage below.
                continue;
            };
            let target_joint = &target.skeleton.joints[target_joint_index];
            if source_joint.class != target_joint.class {
                continue;
            }
            let Some(source_bone) = usize::try_from(source_joint.child)
                .ok()
                .and_then(|index| source.skeleton.bones.get(index))
            else {
                compatible = false;
                break;
            };
            let Some(target_bone) = usize::try_from(target_joint.child)
                .ok()
                .and_then(|index| target.skeleton.bones.get(index))
            else {
                continue;
            };
            if source_bone.name != target_bone.name
                || bone_parent_name(source, source_bone.parent)
                    != bone_parent_name(target, target_bone.parent)
            {
                continue;
            }
            if !seen.insert(target_joint_index) {
                compatible = false;
                break;
            }
            let (source_scale, ..) =
                Mat4::from_cols_array(&source_bone.relative).to_scale_rotation_translation();
            let (target_scale, _, target_rest_position) =
                Mat4::from_cols_array(&target_bone.relative).to_scale_rotation_translation();
            if !source_scale.is_finite()
                || (source_scale - Vec3::ONE).abs().max_element() > 0.02
                || !target_rest_position.is_finite()
                || !target_scale.is_finite()
                || (target_scale - Vec3::ONE).abs().max_element() > 0.02
            {
                compatible = false;
                break;
            }
            // A rest frame that is a reflection or a projection is not a
            // rotation; playing the donor's keys through it would mirror the
            // bone. The bone stays at bind (a core bone doing so fails the
            // locomotion coverage gate below).
            if rest_frame_det(source_bone) <= 0.0 || rest_frame_det(target_bone) <= 0.0 {
                continue;
            }
            // The donor's authored local rotations are played verbatim on the
            // resident. Both rigs trace the same parent chain (name- and
            // parent-checked above), so the composed pose is the donor's
            // pose rooted at the resident's route placement: at every sample
            // the resident plays what the player plays. The previous
            // parent-frame delta transport anchored every bone's swing to the
            // resident's combat-stance bind - it preserves arc angles, but
            // the pre-flexed arms stay exactly where the stance holds them,
            // which the playtest saw as hands inside the belly (walk) and
            // arms behind the back (run).
            let mut rotation = track.rotation.clone();
            for key in &mut rotation.keys {
                let authored = glam::Quat::from_array(*key).normalize();
                if !authored.is_finite() {
                    compatible = false;
                    break;
                }
                *key = authored.to_array();
            }
            if !compatible {
                break;
            }
            mapped_bones.insert(target_bone.name.as_str());
            retargeted.push(BoneTrack {
                id: target_joint_index as i32,
                position: KeyTrack {
                    fps: track.position.fps.max(1),
                    keys: vec![target_rest_position.to_array()],
                    frames: vec![0.],
                    length_ms: 0,
                    compression: 0,
                },
                rotation,
            });
        }
        // Acceptance is defined by locomotion coverage, not by donor track
        // count: every bone the legs and the torso swing on must map. Extra
        // donor tracks (attachment joints, finger-chain anim joints, helper
        // bones) are skipped above and do not count against the clip.
        let core_complete = GAIT_CORE_BONES
            .iter()
            .all(|core| mapped_bones.contains(*core));
        if !compatible || !core_complete {
            continue;
        }
        let Ok(start) = target.pose_tracks_at_time(&retargeted, clip.seconds(0.), Mat4::IDENTITY)
        else {
            continue;
        };
        let start_signature = walk_pose_signature(&start);
        let visibly_moves = [0.13_f32, 0.21, 0.29, 0.37, 0.49]
            .into_iter()
            .any(|offset| {
                clip.seconds(offset) != clip.seconds(0.)
                    && target
                        .pose_tracks_at_time(&retargeted, clip.seconds(offset), Mat4::IDENTITY)
                        .is_ok_and(|posed| walk_pose_signature(&posed) != start_signature)
            });
        if visibly_moves {
            return Some(GaitClip {
                clip,
                tracks: std::sync::Arc::new(retargeted),
            });
        }
    }
    None
}

fn retargeted_gait_from(source: &ModelAsset, target: &ModelAsset) -> Option<RetargetedGait> {
    Some(RetargetedGait {
        walk: retarget_gait_clip(source, target, GaitKind::Walk)?,
        run: retarget_gait_clip(source, target, GaitKind::Run)?,
    })
}

/// One spawned actor.
struct Live {
    /// Index into [`Population::spawns`].
    spawn: usize,
    position: Vec3,
    yaw: f32,
    asset: Rc<ModelAsset>,
    /// Ordinary action index, used when `Playback` cannot schedule the
    /// template's combined action.
    action: Option<usize>,
    playback: Option<Playback>,
    /// Wrapped clip range for `action`, on the module clock.
    clip: Option<Clip>,
    /// Fallback clock in seconds for actors without a `Playback`.
    clock: f32,
    /// Compatible walk/run clips retargeted onto this original resident rig.
    gait: Option<RetargetedGait>,
    gait_clock: f32,
    live_world_moving: bool,
    live_world_running: bool,
    /// Height of this rig's head above its feet, for the overhead name.
    head: f32,
    /// Whether this actor stands on a building floor rather than the terrain.
    on_floor: bool,
    /// Why the idle clip could not be evaluated, when it could not: the actor is
    /// drawn in its bind pose and the report names the rig.
    pose_error: Option<String>,
    /// Whether the drawn pose has been changing, so a frozen idle is visible in
    /// the settings window instead of being something the user has to spot.
    motion: MotionProbe,
    /// Whether the camera can actually see this actor's head: the client hides
    /// a name it cannot see, so an occluded actor keeps its body and loses its
    /// label instead of showing it through a wall.
    name_visible: bool,
    /// Building-hull stamp the placement was resolved against.
    floor_stamp: u64,
    /// Moved off its authored column to get out of building geometry.
    nudged: bool,
    /// Inside geometry with no free spot found; the settings window names it.
    wedged: bool,
    /// Whether the last pass could see this actor. Off-screen actors are neither
    /// posed nor uploaded, which is what keeps a crowd behind the camera free.
    visible: bool,
    /// Frame index whose pose this actor already carries, so the host's main,
    /// reflection and refraction passes cost one pose, not three.
    pose_index: Option<u64>,
    /// Signature of the pose this frame produced.
    pose_stamp: Option<u64>,
    /// Signature of the pose currently on the GPU, so an unchanged pose only
    /// refreshes the per-pass uniforms instead of re-preparing and re-uploading
    /// every posed vertex.
    uploaded_stamp: Option<u64>,
    /// Real navigation path, when the population supplies one.
    follow: Option<Follow>,
    /// Generated Live World residents use collision-checked local routes.
    live_world: bool,
    /// City used for replanning if a streamed obstacle invalidates the route.
    live_world_zone: Option<usize>,
    /// Perturbation counter so a rejected route is not proposed again verbatim.
    route_attempt: u64,
    route_retry_seconds: f32,
    draw: Option<model_draw_gpu::Draw>,
    /// Fatal per-actor error (rig, pose or draw); the actor is not submitted.
    failure: Option<String>,
}

impl Live {
    fn world(&self) -> Mat4 {
        transform(self.position, self.yaw)
    }

    /// Posed meshes for this frame. `Playback` supplies the native scene clock
    /// when the template's combined action is a single base; otherwise the
    /// ordinary `站立` action runs on the module clock, and a rig that can
    /// evaluate neither falls back to its authored bind pose rather than
    /// disappearing.
    fn pose(&mut self) -> Result<Vec<PosedMesh>, String> {
        let world = self.world();
        // Generated residents keep their original city rig as the visible
        // model. The normal player's walk/run animation is used only when its
        // skeleton can be safely retargeted onto that rig; otherwise the
        // collision-checked route continues in bind pose. Never substitute
        // the donor's player mesh.
        if self.live_world {
            if self.live_world_moving {
                let clip = self.gait.as_ref().map(|gait| {
                    if self.live_world_running {
                        &gait.run
                    } else {
                        &gait.walk
                    }
                });
                if let Some(clip) = clip {
                    match self.asset.pose_tracks_at_time(
                        &clip.tracks,
                        clip.clip.seconds(self.gait_clock),
                        world,
                    ) {
                        Ok(meshes) => return Ok(meshes),
                        Err(error) => {
                            if self.pose_error.is_none() {
                                self.pose_error = Some(error);
                            }
                        }
                    }
                }
            }
            return self.asset.pose_bind(world);
        }
        let idle_sample = self.playback.as_ref().and_then(Playback::sample);
        let fallback = self.action.map(|action| {
            (
                action,
                self.clip
                    .map_or(self.clock, |clip| clip.seconds(self.clock)),
            )
        });
        let samples = [idle_sample, fallback, None];
        for (action, seconds) in samples.into_iter().flatten() {
            match self.asset.pose_action_at_time(action, seconds, world) {
                Ok(meshes) => return Ok(meshes),
                // A rig whose walk/idle tracks cannot be evaluated is drawn in
                // its next supported pose, then its bind pose, rather than
                // disappearing. Keep the first failure for session diagnostics.
                Err(error) => {
                    if self.pose_error.is_none() {
                        self.pose_error = Some(error);
                    }
                }
            }
        }
        self.asset.pose_bind(world)
    }
}

/// The shipped client placement records have no per-instance facing. Keep the
/// model's authored forward direction until server-authored yaw data is
/// available; invented per-record rotations make existing NPCs face randomly.
fn initial_yaw(_spawn: &Spawn) -> f32 {
    0.0
}

/// Model-to-world matrix with the actor's local `+Z` as forward, matching
/// `walker::character::character_transform`.
pub(crate) fn transform(position: Vec3, yaw: f32) -> Mat4 {
    let direction = Vec3::new(yaw.sin(), 0., -yaw.cos());
    let right = Vec3::Y.cross(direction).normalize_or_zero();
    Mat4::from_cols(
        right.extend(0.),
        Vec3::Y.extend(0.),
        direction.extend(0.),
        position.extend(1.),
    )
}

/// The height a placement stands at.
///
/// `task_npc.data` authors 0 for "the server supplies the height"; any other
/// value is the absolute height the game uses. Without an authored height the
/// actor stands on the lowest building floor above the terrain sample - a bridge
/// deck, a platform or an interior floor - and on the terrain when the column
/// holds no floor at all. That is what keeps the client's own placements out of
/// the bridge arches and the walls they were authored on top of.
/// Sampled line of sight against the terrain height field: the client's labels
/// disappear behind a hill, so a terrain sample above the sight ray hides one.
fn terrain_between(map: &MapSpace, from: Vec3, to: Vec3) -> bool {
    let delta = to - from;
    for step in 1..NAME_SIGHT_STEPS {
        let t = step as f32 / NAME_SIGHT_STEPS as f32;
        let point = from + delta * t;
        if map.ground(point.x, point.z) > point.y + NAME_SIGHT_CLEARANCE {
            return true;
        }
    }
    false
}

fn spot_height(
    authored: Option<f32>,
    anchor: [f32; 2],
    terrain: f32,
    floors: Option<&dyn FloorSource>,
    enabled: bool,
) -> f32 {
    if let Some(height) = authored {
        return height;
    }
    if !enabled {
        return terrain;
    }
    let Some(floors) = floors else {
        return terrain;
    };
    floors
        .lowest_floor(
            Vec3::new(anchor[0], terrain, anchor[1]),
            ACTOR_EXTENTS,
            MAX_FLOOR_LIFT,
        )
        .unwrap_or(terrain)
}

fn stand_height(
    spawn: &Spawn,
    terrain: f32,
    floors: Option<&dyn FloorSource>,
    enabled: bool,
) -> f32 {
    spot_height(spawn.height, spawn.anchor, terrain, floors, enabled)
}

/// Where one placement ends up, and what had to be done to get it there.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    position: Vec3,
    /// Standing on a building floor rather than the terrain sample.
    on_floor: bool,
    /// Moved off the placement the height policy resolved, because it was inside
    /// geometry: sideways out of a wall, or down to the ground under a wall cap.
    nudged: bool,
    /// Inside geometry with no free spot inside [`WALL_RINGS`].
    wedged: bool,
}

/// Resolves one placement against the world: its height first, then a sweep out
/// of building geometry.
///
/// The height policy is the authored one (authored height, else the lowest floor
/// above the terrain, else the terrain). The sweep exists because a placement
/// can be *inside* a wall at the right height, which no height query can fix: the
/// column simply lands in solid geometry. Every candidate is re-resolved with the
/// same height policy and rejected when it would move the actor more than
/// [`WALL_RING_LIFT`], so getting out of a wall never also climbs a storey or
/// drops the NPC off a bridge. A source that cannot answer `solid_at` (tests, a
/// host without hulls) leaves every placement exactly where the file has it.
fn resolve_placement(
    spawn: &Spawn,
    map: &MapSpace,
    floors: Option<&dyn FloorSource>,
    stand_on_objects: bool,
) -> Placement {
    let anchor = spawn.anchor;
    let terrain = map.ground(anchor[0], anchor[1]);
    let height = spot_height(spawn.height, anchor, terrain, floors, stand_on_objects);
    let mut placement = Placement {
        position: Vec3::new(anchor[0], height, anchor[1]),
        on_floor: spawn.height.is_none() && height > terrain + 0.5,
        nudged: false,
        wedged: false,
    };
    let Some(floors) = floors else {
        return placement;
    };
    if !floors.solid_at(placement.position, WALL_PROBE) {
        return placement;
    }
    // A placement the height policy lifted onto a plate that turns out to be
    // solid - a wall cap, a shelf, the lid of a doorway - is better off on the
    // ground under it than half inside the wall, but only when that ground is
    // within reach: a bridge deck is never traded for the river bed.
    if spawn.height.is_none() && (placement.position.y - terrain).abs() <= WALL_RING_LIFT {
        let ground = Vec3::new(anchor[0], terrain, anchor[1]);
        if !floors.solid_at(ground, WALL_PROBE) {
            return Placement {
                position: ground,
                on_floor: false,
                nudged: true,
                wedged: false,
            };
        }
    }
    for ring in WALL_RINGS {
        for step in 0..WALL_DIRECTIONS {
            let angle = step as f32 / WALL_DIRECTIONS as f32 * std::f32::consts::TAU;
            let x = anchor[0] + angle.cos() * ring;
            let z = anchor[1] + angle.sin() * ring;
            let terrain = map.ground(x, z);
            let height = spot_height(
                spawn.height,
                [x, z],
                terrain,
                Some(floors),
                stand_on_objects,
            );
            if (height - placement.position.y).abs() > WALL_RING_LIFT {
                continue;
            }
            let candidate = Vec3::new(x, height, z);
            if floors.solid_at(candidate, WALL_PROBE) {
                continue;
            }
            return Placement {
                position: candidate,
                on_floor: spawn.height.is_none() && height > terrain + 0.5,
                nudged: true,
                wedged: false,
            };
        }
    }
    // Inside geometry and nowhere to go: keep the authored column, and say so
    // instead of leaving a silent statue in a wall.
    placement.wedged = true;
    placement
}

/// Frames between pose evaluations for an actor `distance` units from the eye.
fn pose_stride(distance: f32) -> u64 {
    if distance > POSE_STRIDE_FAR {
        3
    } else if distance > POSE_STRIDE_NEAR {
        2
    } else {
        1
    }
}

/// Whether an actor is due a pose this frame. The phase comes from the spawn
/// index, so a distant crowd spreads its re-skins over the stride instead of all
/// refreshing on the same frame and hitching it.
fn pose_due(frame_index: u64, spawn: usize, stride: u64) -> bool {
    stride <= 1 || frame_index % stride == (spawn as u64) % stride
}

/// Whether this pass can actually show an actor standing at `position`.
///
/// The host renders the add-ons once per pass - main, reflection and refraction -
/// so without this test every actor is posed, re-prepared and re-uploaded three
/// times a frame even when the camera faces away from all of them. The test is a
/// pair of projections (feet and head) against a padded viewport, which costs a
/// few multiplies and never hides an actor that could reach the screen.
fn on_screen(view_projection: Mat4, position: Vec3, head: f32) -> bool {
    // Padding covers a body whose feet or head fall just outside the viewport.
    const MARGIN: f32 = 0.25;
    let limit = 1. + MARGIN;
    for offset in [0., head.max(0.)] {
        let clip = view_projection * (position + Vec3::Y * offset).extend(1.);
        if clip.w <= 0.001 {
            // That point is behind the eye; the other one may still be ahead.
            continue;
        }
        let ndc = clip.truncate() / clip.w;
        if (-limit..=limit).contains(&ndc.x) && (-limit..=limit).contains(&ndc.y) {
            return true;
        }
    }
    false
}

/// Screen position of a world point under the frame's view-projection, or `None`
/// when the point is behind the eye or outside the viewport. The matrix is the
/// host's own world-pass matrix, so a label lands exactly over its actor.
fn project(point: Vec3, view_projection: Mat4, screen: egui::Rect) -> Option<egui::Pos2> {
    let clip = view_projection * point.extend(1.);
    // Behind the eye or on the near plane: nothing to project.
    if clip.w <= 0.001 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    if !(-1.0..=1.0).contains(&ndc.x) || !(-1.0..=1.0).contains(&ndc.y) {
        return None;
    }
    Some(egui::pos2(
        screen.left() + (ndc.x * 0.5 + 0.5) * screen.width(),
        screen.top() + (0.5 - ndc.y * 0.5) * screen.height(),
    ))
}

/// The template's own scene idle. `EcModel::scene_action` is the client's
/// 0x5b3a4f lookup, including its `休闲` fallback (0x95a750).
///
/// The request is `站立`, the standing action: 1,674 of the 1,871 NPC rigs in
/// the shipped `models.pck` ship that combined action, and it is the one whose
/// base loop is authored infinite (`LoopCount: -1`). Asking for `休闲` instead
/// picked the leisure gesture (伸懒腰 / 四处张望) for the 1,486 rigs that have
/// one, so those NPCs replayed a stretch-and-look-around loop instead of
/// standing. The client's own fallback to `休闲` still applies to a rig with no
/// `站立`, which is why the request goes through `scene_action` rather than a
/// literal name search. See `Findings/npc-idle-evidence.md` for the per-rig
/// table behind this.
///
/// `Playback` only schedules a combined action that is a single base starting at
/// zero with an infinite or fixed loop count; anything else is the client's
/// event/transition scheduler, which this viewer does not implement. Those rigs
/// still have the base clip the combined action names, so that clip runs on the
/// module clock instead of the actor falling back to its bind pose.
fn idle_clock(
    asset: &ModelAsset,
    report: &mut IdleReport,
) -> (Option<usize>, Option<Clip>, Option<Playback>) {
    let combined = asset.descriptor.scene_action(STANDING_SCENE_ACTION);
    // `Playback::new` accepts "no scene action at all" as an inert playback that
    // samples nothing. Taking that as the answer left every rig without a `休闲`
    // combined action frozen in its bind pose for the whole session, so only a
    // real combined action may take this path.
    if combined.is_some() {
        if let Ok(playback) = Playback::new(asset, combined) {
            report.combined += 1;
            return (None, None, Some(playback));
        }
    }
    let base = combined
        .and_then(|index| asset.descriptor.actions.get(index))
        .and_then(|combined| combined.bases.first())
        .map(|base| base.name.clone());
    let action = base
        .and_then(|name| {
            asset
                .model
                .actions
                .iter()
                .position(|action| action.name == name)
        })
        .or_else(|| {
            // No combined scene action at all: the rig's own idle clip by the
            // names the client's templates use.
            const IDLE_NAMES: [&str; 4] = ["待机", "站立", "休闲", "idle"];
            IDLE_NAMES.iter().find_map(|name| {
                asset
                    .model
                    .actions
                    .iter()
                    .position(|action| action.name.eq_ignore_ascii_case(name))
            })
        });
    let Some(action) = action else {
        report.bind_pose += 1;
        return (None, None, None);
    };
    match Clip::new(asset, action) {
        Some(clip) => {
            report.looped += 1;
            (Some(action), Some(clip), None)
        }
        None => {
            // The clip exists but has no sampleable range or frame rate, so the
            // rig falls back to its bind pose. Reported apart from "no idle
            // action at all" so the two causes stay distinguishable.
            report.unusable_clip += 1;
            (None, None, None)
        }
    }
}

/// Prefer the client's explicit walk/run names, including combined scene
/// actions; never reinterpret a work or idle action as locomotion. Recognize
/// compound and short Chinese action names as well as English names, with their
/// optional equipment suffixes.
fn locomotion_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let positive = ["walk", "run", "move", "走", "行走", "奔跑", "跑", "移动"];
    let forbidden = [
        "attack", "fight", "skill", "idle", "work", "hurt", "dead", "jump", "swim", "攻击", "战斗",
        "技能", "休闲", "待机", "站立", "打铁", "受击", "死亡", "跳跃", "游泳", "采集", "施法",
    ];
    positive.iter().any(|part| name.contains(part))
        && !forbidden.iter().any(|part| name.contains(part))
}

/// Collect every semantically named locomotion candidate, not only the first
/// one. A rig may have an unusable combined action before its working direct
/// walk clip (or several equipment variants); validation must try alternatives.
#[cfg(test)]
fn walk_clock_candidates(asset: &ModelAsset) -> Vec<WalkAnimation> {
    let mut candidates = Vec::new();
    for (combined, scene_action) in asset.descriptor.actions.iter().enumerate() {
        let base_is_walk = scene_action
            .bases
            .iter()
            .any(|base| locomotion_name(&base.name));
        if !locomotion_name(&scene_action.name) && !base_is_walk {
            continue;
        }
        if let Ok(playback) = Playback::new(asset, Some(combined)) {
            if let Some((action, _)) = playback.sample() {
                if let Some(clip) = Clip::new(asset, action) {
                    candidates.push(WalkAnimation {
                        action,
                        clip,
                        playback: Some(playback),
                        elapsed: 0.,
                    });
                }
            }
        }
    }
    for (action, clip) in asset.model.actions.iter().enumerate() {
        if locomotion_name(&clip.name) {
            if let Some(clip) = Clip::new(asset, action) {
                candidates.push(WalkAnimation {
                    action,
                    clip,
                    playback: None,
                    elapsed: 0.,
                });
            }
        }
    }
    candidates
}

/// Select the first clip whose real skinned geometry changes. Merely naming a
/// clip "walk" is not enough; conversely, one broken first match must not hide
/// a later valid walking action on the same rig.
#[cfg(test)]
fn select_walk_animation(
    asset: &ModelAsset,
    candidates: Vec<WalkAnimation>,
) -> Option<WalkAnimation> {
    candidates
        .into_iter()
        .find(|animation| walk_animation_moves_meshes(asset, animation))
}

#[cfg(test)]
fn walk_clock(asset: &ModelAsset) -> Option<WalkAnimation> {
    select_walk_animation(asset, walk_clock_candidates(asset))
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct WalkAnimation {
    action: usize,
    clip: Clip,
    playback: Option<Playback>,
    elapsed: f32,
}

#[cfg(test)]
impl WalkAnimation {
    fn sample(&self) -> Option<(usize, f32)> {
        self.playback
            .as_ref()
            .and_then(Playback::sample)
            .or_else(|| Some((self.action, self.clip.seconds(self.elapsed))))
    }
    fn advance(&mut self, delta: f32, delta_ms: u32) {
        self.elapsed += delta;
        let failed = self
            .playback
            .as_mut()
            .is_some_and(|playback| playback.tick(delta_ms).is_err());
        if failed {
            self.playback = None;
        } else if let Some(playback) = self.playback.as_mut() {
            if playback.stopped() {
                playback.restart();
            }
        }
    }
    fn restart(&mut self) {
        self.elapsed = 0.;
        if let Some(playback) = self.playback.as_mut() {
            playback.restart();
        }
    }
}

/// A matching action name is not enough: reject clips whose tracks cannot be
/// evaluated or whose skinned geometry does not actually change. This prevents
/// a resident from sliding along a route in a bind/idle pose.
#[cfg(test)]
fn walk_animation_moves_meshes(asset: &ModelAsset, animation: &WalkAnimation) -> bool {
    let Some((action, seconds)) = animation.sample() else {
        return false;
    };
    let Ok(initial) = asset.pose_action_at_time(action, seconds, Mat4::IDENTITY) else {
        return false;
    };
    let initial_signature = walk_pose_signature(&initial);
    let mut probe = animation.clone();
    // Probe several nonuniform offsets: short authored loops can land on the
    // same pose at a single fixed 250 ms interval, and combined actions may
    // delay their base clip before it starts.
    for delta in [0.11_f32, 0.17, 0.23, 0.31, 0.43] {
        probe.advance(delta, (delta * 1000.) as u32);
        let Some((action, seconds)) = probe.sample() else {
            continue;
        };
        let Ok(meshes) = asset.pose_action_at_time(action, seconds, Mat4::IDENTITY) else {
            continue;
        };
        if walk_pose_signature(&meshes) != initial_signature {
            return true;
        }
    }
    false
}

#[derive(Clone, Debug)]
struct CityZone {
    name: String,
    center: [f32; 2],
    radius: f32,
}

fn city_zone_target_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("city") || name.contains("district")
}

fn load_live_world_city_zones(map_dir: &Path) -> Result<Vec<CityZone>, String> {
    if !map_dir
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("world"))
    {
        return Err("Live World city crowds are authored for the main world only".into());
    }
    let path = map_dir.join("world_targets.txt");
    let destinations = crate::navigation::destinations_for_world(&path, &[1])
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let zones: Vec<CityZone> = destinations
        .into_iter()
        .filter(|destination| city_zone_target_name(&destination.name))
        .map(|destination| CityZone {
            name: destination.name,
            center: [destination.position[0], destination.position[2]],
            radius: LIVE_WORLD_DEFAULT_RADIUS,
        })
        .collect();
    if zones.is_empty() {
        return Err(format!(
            "{} contains no main-world city or district targets",
            path.display()
        ));
    }
    Ok(zones)
}

fn city_zone_at(x: f32, z: f32, zones: &[CityZone]) -> Option<usize> {
    zones
        .iter()
        .enumerate()
        .filter_map(|(index, zone)| {
            let dx = x - zone.center[0];
            let dz = z - zone.center[1];
            (dx * dx + dz * dz <= zone.radius * zone.radius).then_some((index, dx * dx + dz * dz))
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(index, _)| index)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum NpcGender {
    Masculine,
    Feminine,
}

#[derive(Clone, Debug, Default)]
struct LiveWorldNameTokens {
    masculine: Vec<String>,
    feminine: Vec<String>,
}

impl LiveWorldNameTokens {
    fn for_gender(&self, gender: NpcGender) -> &[String] {
        match gender {
            NpcGender::Masculine => &self.masculine,
            NpcGender::Feminine => &self.feminine,
        }
    }
}

/// Independent name fragments, not tokens harvested from standing NPC names.
fn live_world_name_tokens() -> LiveWorldNameTokens {
    const MALE: [&str; 32] = [
        "Bo", "Cheng", "Da", "Feng", "Guo", "Han", "Jian", "Kai", "Lei", "Ming", "Peng", "Qiang",
        "Qiao", "Rui", "Shen", "Tao", "Wei", "Xiang", "Xiao", "Xuan", "Yan", "Yi", "Yong", "Yu",
        "Zhao", "Zhen", "Zhi", "Zhou", "Zong", "Bing", "Dong", "Jun",
    ];
    const FEMALE: [&str; 32] = [
        "Ai", "Cai", "Chun", "Dai", "Fei", "Fang", "Hua", "Hui", "Jia", "Jing", "Lan", "Lian",
        "Ling", "Mei", "Miao", "Ning", "Qing", "Qiu", "Rong", "Shu", "Ting", "Wan", "Xia", "Xue",
        "Xin", "Ying", "Yue", "Zhu", "Zi", "Rou", "Man", "Yun",
    ];
    LiveWorldNameTokens {
        masculine: MALE.iter().map(|part| (*part).to_string()).collect(),
        feminine: FEMALE.iter().map(|part| (*part).to_string()).collect(),
    }
}

/// Accept only the client's numbered original human male/female NPC rig
/// families. Guard, creature, monster, prop, malformed, and non-NPC paths are
/// rejected so synthetic residents vary among the native city-local models.
fn live_world_gender(model: &str) -> Option<NpcGender> {
    let model = crate::ecmodel::normalize(model);
    let relative = model.strip_prefix("models/npcs/npc/")?;
    let (family, file) = relative.split_once('/')?;
    let (prefix, gender) = if family.starts_with("男npc") {
        ("男npc", NpcGender::Masculine)
    } else if family.starts_with("女npc") {
        ("女npc", NpcGender::Feminine)
    } else {
        return None;
    };
    let number = family.strip_prefix(prefix)?;
    if number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || file != format!("{family}.ecm")
    {
        return None;
    }
    Some(gender)
}

/// These two known rigs carry a blacksmith's anvil/hammer animation.
fn live_world_rig_allowed(model: &str) -> bool {
    const PROP_RIGS: [&str; 2] = [
        "models/npcs/npc/男npc5/男npc5.ecm",
        "models/npcs/npc/妖兽npc4/妖兽npc4.ecm",
    ];
    let model = crate::ecmodel::normalize(model);
    live_world_gender(&model).is_some() && !PROP_RIGS.contains(&model.as_str())
}

/// Distinct, eligible native rigs used only as an appearance palette for their
/// nearest city target. No standing placement becomes a generated identity.
fn live_world_city_population(spawns: &[Spawn], zones: &[CityZone]) -> Vec<Vec<usize>> {
    let mut templates = vec![Vec::new(); zones.len()];
    let mut seen_models: Vec<HashSet<String>> = vec![HashSet::new(); zones.len()];
    for (index, spawn) in spawns.iter().enumerate() {
        if spawn.kind != TemplateKind::Npc || !live_world_rig_allowed(&spawn.model) {
            continue;
        }
        let Some(zone_index) = city_zone_at(spawn.anchor[0], spawn.anchor[1], zones) else {
            continue;
        };
        if seen_models[zone_index].insert(crate::ecmodel::normalize(&spawn.model)) {
            templates[zone_index].push(index);
        }
    }
    templates
}

/// Compose a fresh, same-gender name from independent fragments and avoid every
/// authored or already-generated full-name collision.
fn live_world_name(population: &Population, tokens: &[String], seed: u64) -> Option<String> {
    if tokens.len() < 2 {
        return None;
    }
    let choices = tokens.len() * (tokens.len() - 1);
    let start = (seed % choices as u64) as usize;
    for offset in 0..choices {
        let pair = (start + offset) % choices;
        let first = pair / (tokens.len() - 1);
        let mut second = pair % (tokens.len() - 1);
        if second >= first {
            second += 1;
        }
        let name = format!("{} {}", tokens[first], tokens[second]);
        if population
            .spawns
            .iter()
            .all(|spawn| !spawn.name.eq_ignore_ascii_case(&name))
        {
            return Some(name);
        }
    }
    None
}

/// Ground point accepted for ambient walking. This mode intentionally uses
/// terrain-level outdoor paths; object floors, overhead platforms, water, and
/// actor-sized building overlaps are rejected.
fn live_world_ground_point(
    x: f32,
    z: f32,
    map: &MapSpace,
    floors: &dyn FloorSource,
) -> Option<Vec3> {
    let [width, height] = map.world_size();
    if !x.is_finite()
        || !z.is_finite()
        || x < -width * 0.5 + 1.
        || x > width * 0.5 - 1.
        || z < -height * 0.5 + 1.
        || z > height * 0.5 - 1.
    {
        return None;
    }
    let y = map.ground(x, z);
    if !y.is_finite() {
        return None;
    }
    // Match the walker's first-containing-area lookup. Keep a narrow tolerance
    // for shoreline interpolation but never place a body below an actual water
    // surface simply because the terrain height field is lower.
    if floors
        .water_height(x, z)
        .is_some_and(|water| water > y + 0.1)
    {
        return None;
    }
    let feet = Vec3::new(x, y, z);
    if floors.solid_at(feet, ACTOR_EXTENTS) {
        return None;
    }
    if floors
        .lowest_floor(feet + Vec3::Y * 0.05, ACTOR_EXTENTS, MAX_FLOOR_LIFT)
        .is_some_and(|floor| floor > y + 0.65)
    {
        return None;
    }
    Some(feet)
}

/// Project a complete candidate circuit before admitting it. Every waypoint
/// and short segment is checked against terrain, water, building hulls, and
/// currently occupied actors. A failed candidate is discarded and retried.
fn build_live_world_route(
    start: Vec3,
    seed: u64,
    zone: &CityZone,
    map: &MapSpace,
    floors: &dyn FloorSource,
    occupied: &[Vec3],
    ignore_actor: Option<usize>,
) -> Option<Route> {
    let grounded = live_world_ground_point(start.x, start.z, map, floors)?;
    if (grounded.y - start.y).abs() > 0.65
        || city_zone_at(start.x, start.z, std::slice::from_ref(zone)) != Some(0)
    {
        return None;
    }
    let start = grounded;
    let mut random = seed ^ 0x94d0_49bb_1331_11eb;
    for _attempt in 0..LIVE_WORLD_ATTEMPTS {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let phase = random as u32 as f32 / u32::MAX as f32 * std::f32::consts::TAU;
        let mut points = vec![start];
        let mut valid = true;
        for waypoint in 1..=6 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let jitter = ((random as u16 as f32 / u16::MAX as f32) - 0.5) * 0.55;
            let bearing = phase + std::f32::consts::TAU * waypoint as f32 / 6.0 + jitter;
            let radius = 4.0
                + (random >> 32) as u32 as f32 / u32::MAX as f32
                    * (LIVE_WORLD_MAX_ROUTE_RADIUS - 4.0);
            let px = start.x + bearing.cos() * radius;
            let pz = start.z + bearing.sin() * radius;
            let Some(point) = live_world_ground_point(px, pz, map, floors) else {
                valid = false;
                break;
            };
            if city_zone_at(point.x, point.z, std::slice::from_ref(zone)) != Some(0) {
                valid = false;
                break;
            }
            points.push(point);
        }
        if !valid {
            continue;
        }
        for edge in 0..points.len() {
            if !ambient_path_safe(
                points[edge],
                points[(edge + 1) % points.len()],
                map,
                floors,
                occupied,
                ignore_actor,
            ) {
                valid = false;
                break;
            }
        }
        if !valid {
            continue;
        }
        let route = Route {
            waypoints: points
                .iter()
                .enumerate()
                .map(|(index, point)| Waypoint {
                    position: point.to_array(),
                    // Start quickly so movement is apparent in the first few
                    // seconds; retain occasional short pauses at route nodes.
                    wait: if index == 0 {
                        0.15 + ((seed >> 8) & 3) as f32 * 0.1
                    } else if index % 3 == 2 {
                        1.2 + ((seed >> ((index * 7) % 56)) & 7) as f32 * 0.2
                    } else {
                        0.2 + ((seed >> ((index * 7) % 56)) & 5) as f32 * 0.15
                    },
                })
                .collect(),
            speed: 1.4 + ((seed >> 20) as u8 as f32 / u8::MAX as f32) * 0.8,
            looping: true,
        };
        if route.validate().is_ok() {
            return Some(route);
        }
    }
    None
}

/// Check an entire proposed step, not just its endpoint. The small sample
/// interval bounds frame spikes and prevents a fast update from tunnelling
/// through a thin wall; terrain and water are sampled throughout the step.
fn ambient_path_safe(
    from: Vec3,
    to: Vec3,
    map: &MapSpace,
    floors: &dyn FloorSource,
    occupied: &[Vec3],
    ignore_actor: Option<usize>,
) -> bool {
    let dx = to.x - from.x;
    let dz = to.z - from.z;
    let distance = dx.hypot(dz);
    if !distance.is_finite() {
        return false;
    }
    let steps = (distance / LIVE_WORLD_SAMPLE_SPACING).ceil().max(1.) as usize;
    let mut previous_y = from.y;
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let Some(point) = live_world_ground_point(from.x + dx * t, from.z + dz * t, map, floors)
        else {
            return false;
        };
        // Reject cliffs and abrupt terrain discontinuities rather than walking
        // through them or snapping to an unrelated level.
        if (point.y - previous_y).abs() > 0.8 {
            return false;
        }
        if occupied.iter().enumerate().any(|(index, other)| {
            let dx = other.x - point.x;
            let dz = other.z - point.z;
            Some(index) != ignore_actor && dx * dx + dz * dz < 1.0
        }) {
            return false;
        }
        previous_y = point.y;
    }
    true
}

/// Build one independent resident from a city's local rig palette. Its anchor is
/// sampled around the city target, not copied or offset from a standing NPC.
fn build_live_world_spawn(
    population: &Population,
    zone_index: usize,
    resident_index: usize,
    generation_index: usize,
    template_sources: &[usize],
    name_tokens: &LiveWorldNameTokens,
    map: &MapSpace,
    floors: &dyn FloorSource,
    occupied: &[Vec3],
    zone: &CityZone,
) -> Option<(Spawn, Route)> {
    if template_sources.is_empty() {
        return None;
    }
    let seed = (generation_index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (zone_index as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let mut selected = None;
    for offset in 0..template_sources.len() {
        let template = population
            .spawns
            .get(template_sources[(resident_index + offset) % template_sources.len()])?;
        let tokens = name_tokens.for_gender(live_world_gender(&template.model)?);
        if let Some(name) = live_world_name(population, tokens, seed.wrapping_add(offset as u64)) {
            selected = Some((template, name));
            break;
        }
    }
    let (template, name) = selected?;
    let mut random = seed ^ 0x94d0_49bb_1331_11eb;
    let max_spawn_radius = (zone.radius * 0.55).min(180.0).max(8.0);
    for _attempt in 0..LIVE_WORLD_ATTEMPTS {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let angle = random as u32 as f32 / u32::MAX as f32 * std::f32::consts::TAU;
        let radial = 8.0 + (random >> 32) as u32 as f32 / u32::MAX as f32 * max_spawn_radius;
        let x = zone.center[0] + angle.cos() * radial;
        let z = zone.center[1] + angle.sin() * radial;
        let Some(start) = live_world_ground_point(x, z, map, floors) else {
            continue;
        };
        if city_zone_at(start.x, start.z, std::slice::from_ref(zone)) != Some(0) {
            continue;
        }
        let min_sep2 = LIVE_WORLD_MIN_SEPARATION * LIVE_WORLD_MIN_SEPARATION;
        if population.spawns.iter().any(|other| {
            let dx = other.anchor[0] - x;
            let dz = other.anchor[1] - z;
            dx * dx + dz * dz < min_sep2
        }) {
            continue;
        }
        let Some(route) =
            build_live_world_route(start, seed ^ random, zone, map, floors, occupied, None)
        else {
            continue;
        };
        return Some((
            Spawn {
                kind: TemplateKind::Npc,
                id: LIVE_WORLD_ID_BASE.saturating_add(generation_index as u32),
                name,
                model: template.model.clone(),
                anchor: [start.x, start.z],
                height: None,
                task_count: 0,
                route: None,
            },
            route,
        ));
    }
    None
}

/// The scene action a placed NPC asks for. `站立` ("standing") is the combined
/// action 1,674 of the 1,871 shipped NPC rigs carry, with an infinite base loop;
/// the client falls back to the literal `休闲` when a rig has no match.
const STANDING_SCENE_ACTION: &str = "站立";

/// Frames of unchanged geometry after which an actor counts as holding a still
/// pose. Half a second at 60 Hz: an idle that only breathes still clears it.
const MOTION_WINDOW: u32 = 30;

/// Watches an actor's posed geometry to answer "is this one actually moving?".
///
/// The module can resolve an idle clip and still draw a frozen character - a
/// single-frame clip, a clip whose tracks are empty, or a clock that stopped -
/// so the drawn pose itself is sampled instead of trusting the schedule.
#[derive(Clone, Copy, Debug, Default)]
struct MotionProbe {
    signature: Option<u64>,
    still_frames: u32,
}

impl MotionProbe {
    fn observe(&mut self, signature: u64) {
        match self.signature {
            Some(previous) if previous == signature => {
                self.still_frames = self.still_frames.saturating_add(1);
            }
            _ => {
                self.signature = Some(signature);
                self.still_frames = 0;
            }
        }
    }

    fn animating(&self) -> bool {
        self.still_frames < MOTION_WINDOW
    }

    fn observed(&self) -> bool {
        self.signature.is_some()
    }
}

/// Cheap fingerprint of a posed rig: a spread of vertices, quantised so float
/// noise does not read as movement. A frozen animation repeats it exactly.
fn pose_signature(meshes: &[PosedMesh]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut sampled = 0;
    for mesh in meshes {
        let step = (mesh.vertices.len() / 8).max(1);
        for vertex in mesh.vertices.iter().step_by(step).take(8) {
            for axis in vertex.position {
                hash ^= (axis * 100.0) as i64 as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            sampled += 1;
        }
        if sampled >= 16 {
            break;
        }
    }
    hash
}

/// Fingerprint every skin mesh so a short clip affecting only a small body
/// part still proves that a retargeted gait changes the resident geometry.
fn walk_pose_signature(meshes: &[PosedMesh]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for mesh in meshes {
        let step = (mesh.vertices.len() / 8).max(1);
        for vertex in mesh.vertices.iter().step_by(step).take(8) {
            for axis in vertex.position {
                hash ^= (axis * 100.0) as i64 as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash
}

/// How the population's idle animations resolved, reported once per session so a
/// frozen rig is diagnosable instead of a guess.
#[derive(Clone, Copy, Debug, Default)]
struct IdleReport {
    combined: usize,
    looped: usize,
    /// A clip was found but its range or frame rate cannot be sampled.
    unusable_clip: usize,
    /// No idle action at all: the rig is drawn in its authored bind pose.
    bind_pose: usize,
}

/// One placement search: the camera and limits it was run for, and its result.
struct StreamCache {
    eye: [f32; 2],
    distance: f32,
    limit: usize,
    nearest: Vec<(usize, f32)>,
}

pub struct Populator {
    element: PathBuf,
    /// Directory of the selected client map; city targets are map-specific.
    map_dir: PathBuf,
    /// The host world. `None` only for the registry's inert default instance.
    map: Option<MapSpace>,
    settings: Settings,
    active: bool,
    live_world: bool,
    live_world_radius: f32,
    live_world_residents_per_zone: usize,
    population: Option<Population>,
    /// The immutable client-authored prefix of `population.spawns`.
    authored_spawn_count: usize,
    /// Preserve any authored route table when removing generated residents.
    authored_route_count: usize,
    /// Next authored NPC to consider for a safe ambient companion.
    live_world_cursor: usize,
    live_world_generated: usize,
    live_world_since_stream_refresh: usize,
    city_zones: Vec<CityZone>,
    city_zone_error: Option<String>,
    /// City-local original human source rigs found in the native placements.
    live_world_template_candidates: Vec<Vec<usize>>,
    /// Structurally loadable original human rigs used as appearance templates.
    live_world_templates: Vec<Vec<usize>>,
    live_world_template_cursor: usize,
    live_world_templates_ready: bool,
    live_world_name_tokens: LiveWorldNameTokens,
    load_error: Option<String>,
    library: Option<Library>,
    library_error: Option<String>,
    renderer: Option<Renderer>,
    live: Vec<Live>,
    eye: Vec3,
    reload_pending: bool,
    /// One-shot camera move the settings window can ask the host for.
    camera_request: Option<Vec3>,
    /// Console diagnostics already emitted, so a stuck rig is reported once.
    reported_spawn: bool,
    reported_failures: usize,
    /// Actor-level pose/texture/render errors are reported once per placement.
    reported_actor_failures: HashSet<usize>,
    /// How the idle animations resolved, and whether that was logged.
    idle_report: IdleReport,
    reported_idle: bool,
    /// Seconds the population has been drawn for, and whether the "is anything
    /// actually moving" line was logged.
    motion_seconds: f32,
    reported_motion: bool,
    /// How many bind-pose fallbacks have already been logged.
    reported_pose_errors: usize,
    /// Whether the wall sweep was already logged, so it reports once.
    reported_walls: bool,
    /// Frames the host has asked the module to update for. The host calls
    /// `render` once per pass (main, reflection, refraction), so a pass only
    /// re-poses an actor whose `pose_index` is not this frame.
    frame_index: u64,
    /// Seconds since the labels were last tested for occlusion. Testing them is
    /// a hull raycast per actor, and a label does not change visibility faster
    /// than a few times a second.
    name_clock: f32,
    /// Whether the last update advanced the world clock. A paused viewer shows
    /// the pose already on the GPU instead of skinning the same frame again.
    time_advanced: bool,
    /// The last placement search and the camera/settings it was run for.
    stream_cache: Option<StreamCache>,
    /// The client's own overhead-name font, read from `configs/fonts.txt` and
    /// handed to the host's egui context on first use.
    name_font: Option<NameFont>,
}

impl Default for Populator {
    fn default() -> Self {
        Self {
            element: PathBuf::new(),
            map_dir: PathBuf::new(),
            map: None,
            settings: Settings::default(),
            active: false,
            live_world: false,
            live_world_radius: LIVE_WORLD_DEFAULT_RADIUS,
            live_world_residents_per_zone: LIVE_WORLD_DEFAULT_PER_ZONE,
            population: None,
            authored_spawn_count: 0,
            authored_route_count: 0,
            live_world_cursor: 0,
            live_world_generated: 0,
            live_world_since_stream_refresh: 0,
            city_zones: Vec::new(),
            city_zone_error: None,
            live_world_template_candidates: Vec::new(),
            live_world_templates: Vec::new(),
            live_world_template_cursor: 0,
            live_world_templates_ready: false,
            live_world_name_tokens: LiveWorldNameTokens::default(),
            load_error: None,
            library: None,
            library_error: None,
            renderer: None,
            live: Vec::new(),
            eye: Vec3::ZERO,
            reload_pending: false,
            camera_request: None,
            reported_spawn: false,
            reported_failures: 0,
            reported_actor_failures: HashSet::new(),
            idle_report: IdleReport::default(),
            reported_idle: false,
            motion_seconds: 0.,
            reported_motion: false,
            reported_pose_errors: 0,
            reported_walls: false,
            frame_index: 0,
            name_clock: NAME_VISIBILITY_INTERVAL,
            time_advanced: true,
            stream_cache: None,
            name_font: None,
        }
    }
}

impl Populator {
    pub fn new(element: PathBuf, map: MapSpace, map_dir: PathBuf) -> Self {
        let (city_zones, city_zone_error) = match load_live_world_city_zones(&map_dir) {
            Ok(zones) => (zones, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        Self {
            element,
            map_dir,
            map: Some(map),
            city_zones,
            city_zone_error,
            ..Self::default()
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn set_active(&mut self, active: bool, eye: Vec3) {
        self.eye = eye;
        if self.active == active {
            return;
        }
        self.active = active;
        // Disabling drops every rig, draw and actor; the archives and the
        // placement tables stay for an instant re-enable.
        self.live.clear();
        // Test the labels on the first frame back instead of up to
        // `NAME_VISIBILITY_INTERVAL` later.
        self.name_clock = NAME_VISIBILITY_INTERVAL;
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    pub fn live_world_enabled(&self) -> bool {
        self.live_world
    }

    pub fn live_world_generated(&self) -> usize {
        self.live_world_generated
    }
    pub fn live_world_model_count(&self) -> usize {
        self.live_world_templates.iter().map(Vec::len).sum()
    }
    pub fn live_world_gait_count(&self) -> usize {
        self.live
            .iter()
            .filter(|live| live.live_world && live.gait.is_some())
            .count()
    }
    pub fn live_world_radius(&self) -> f32 {
        self.live_world_radius
    }
    pub fn live_world_residents_per_zone(&self) -> usize {
        self.live_world_residents_per_zone
    }

    pub fn live_world_stationary_count(&self) -> usize {
        self.live
            .iter()
            .filter(|live| live.live_world && (live.wedged || live.follow.is_none()))
            .count()
    }

    pub fn live_world_zone_status(&self) -> String {
        if let Some(error) = &self.city_zone_error {
            return format!(
                "City target data unavailable for {}: {error}",
                self.map_dir.display()
            );
        }
        let choices = self
            .live_world_templates
            .iter()
            .map(Vec::len)
            .sum::<usize>();
        let total = self
            .live_world_template_candidates
            .iter()
            .map(Vec::len)
            .sum::<usize>();
        if !self.live_world_templates_ready {
            return format!(
                "Checking original models: {} of {total} local human rigs; {} city targets.",
                self.live_world_template_cursor,
                self.city_zones.len()
            );
        }
        let requested = self
            .city_zones
            .len()
            .saturating_mul(self.live_world_residents_per_zone)
            .min(LIVE_WORLD_MAX_RESIDENTS);
        format!(
            "Loadable original models: {choices}/{total} city-local human rigs. Residents: {}/{requested}; placement attempts: {}.",
            self.live_world_generated,
            self.live_world_cursor
        )
    }

    pub fn set_live_world_radius(&mut self, radius: f32) {
        let radius = if radius.is_finite() {
            radius.clamp(LIVE_WORLD_MIN_RADIUS, LIVE_WORLD_MAX_RADIUS)
        } else {
            LIVE_WORLD_DEFAULT_RADIUS
        };
        if (self.live_world_radius - radius).abs() < f32::EPSILON {
            return;
        }
        self.live_world_radius = radius;
        for zone in &mut self.city_zones {
            zone.radius = radius;
        }
        if let Some(population) = self.population.as_ref() {
            let authored =
                &population.spawns[..self.authored_spawn_count.min(population.spawns.len())];
            self.live_world_template_candidates =
                live_world_city_population(authored, &self.city_zones);
            self.live_world_templates = vec![Vec::new(); self.city_zones.len()];
            self.live_world_template_cursor = 0;
            self.live_world_templates_ready = false;
        }
        self.reset_live_world_population();
    }

    pub fn set_live_world_residents_per_zone(&mut self, count: usize) {
        let count = count.min(LIVE_WORLD_MAX_PER_ZONE);
        if self.live_world_residents_per_zone == count {
            return;
        }
        self.live_world_residents_per_zone = count;
        self.reset_live_world_population();
    }

    fn reset_live_world_population(&mut self) {
        self.live_world_cursor = 0;
        self.live_world_generated = 0;
        self.live_world_since_stream_refresh = 0;
        if let Some(population) = self.population.as_mut() {
            population.spawns.truncate(self.authored_spawn_count);
            population.routes.truncate(self.authored_route_count);
        }
        self.live.clear();
        self.stream_cache = None;
    }

    /// Toggles the local simulation without changing the authored population.
    pub fn set_live_world(&mut self, enabled: bool) {
        if self.live_world == enabled {
            return;
        }
        self.live_world = enabled;
        self.reset_live_world_population();
    }

    /// One-shot camera move requested by the settings window. The host owns the
    /// camera, so the module only asks.
    pub fn take_camera_request(&mut self) -> Option<Vec3> {
        self.camera_request.take()
    }

    /// Camera pose showing the nearest real placement, for the settings
    /// window's "go there" button: the module cannot verify what it cannot
    /// reach, and most of the map genuinely has no task NPC nearby.
    fn nearest_camera_target(&self) -> Option<Vec3> {
        let population = self.population.as_ref()?;
        let map = self.map.as_ref()?;
        let (index, spawn) = population
            .spawns
            .iter()
            .take(self.authored_spawn_count)
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let distance2 = |spawn: &Spawn| {
                    (spawn.anchor[0] - self.eye.x).powi(2) + (spawn.anchor[1] - self.eye.z).powi(2)
                };
                distance2(a)
                    .partial_cmp(&distance2(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })?;
        let ground = spawn
            .height
            .unwrap_or_else(|| map.ground(spawn.anchor[0], spawn.anchor[1]));
        Some(Vec3::new(
            spawn.anchor[0] + 30.,
            ground + 6.,
            spawn.anchor[1] + 30.,
        ))
    }

    pub fn status(&self) -> String {
        if let Some(error) = &self.load_error {
            return format!("Population unavailable: {error}");
        }
        if let Some(error) = &self.library_error {
            return format!("models.pck unavailable: {error}");
        }
        if self.map.is_none() {
            return "Waiting for a loaded world".into();
        }
        match &self.population {
            Some(population) => {
                let radius2 = self.settings.spawn_distance * self.settings.spawn_distance;
                let in_range = population
                    .spawns
                    .iter()
                    .take(self.authored_spawn_count)
                    .filter(|spawn| {
                        let dx = spawn.anchor[0] - self.eye.x;
                        let dz = spawn.anchor[1] - self.eye.z;
                        dx * dx + dz * dz <= radius2
                    })
                    .count();
                format!(
                    "{} client-placed NPCs ({} records total, {} server-placed): {} actors in range, {} drawn{}",
                    self.authored_spawn_count,
                    self.authored_spawn_count + population.unplaced + population.skipped,
                    population.unplaced,
                    in_range,
                    self.live.len(),
                    if self.live_world {
                        format!("; Live World (Experimental): {} generated", self.live_world_generated)
                    } else {
                        String::new()
                    }
                )
            }
            None => "Loading the real population...".into(),
        }
    }

    /// Whether this module needs the host's building hulls this frame.
    pub fn wants_floors(&self) -> bool {
        // Overhead names are hidden by geometry, so the hull set is needed even
        // when placements are left on the terrain.
        self.active
            && (self.settings.stand_on_objects || self.settings.show_names || self.live_world)
    }

    /// Per-frame host hook: streams actors around the camera, resolves their
    /// placement against the host's building floors and advances the ones
    /// standing on a navigation path.
    pub fn update(&mut self, delta: f32, eye: Vec3, floors: Option<&dyn FloorSource>) {
        if !self.active {
            return;
        }
        self.eye = eye;
        self.frame_index = self.frame_index.wrapping_add(1);
        if self.reload_pending {
            self.reload_pending = false;
            self.population = None;
            self.authored_spawn_count = 0;
            self.authored_route_count = 0;
            self.live_world_cursor = 0;
            self.live_world_generated = 0;
            self.live_world_since_stream_refresh = 0;
            self.live_world_template_candidates.clear();
            self.live_world_templates.clear();
            self.reported_actor_failures.clear();
            self.live_world_template_cursor = 0;
            self.live_world_templates_ready = false;
            self.live_world_name_tokens = LiveWorldNameTokens::default();
            self.stream_cache = None;
            self.library = None;
            self.load_error = None;
            self.library_error = None;
            self.live.clear();
            self.idle_report = IdleReport::default();
            self.reported_idle = false;
            self.motion_seconds = 0.;
            self.reported_motion = false;
            self.reported_pose_errors = 0;
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.forget_textures();
            }
        }
        if self.map.is_none() {
            return;
        }
        if self.population.is_none() && self.load_error.is_none() {
            match Population::open(&self.element) {
                Ok(population) => {
                    eprintln!(
                        "Populator: {} client-placed NPCs, {} server-placed records, {} skipped",
                        population.spawns.len(),
                        population.unplaced,
                        population.skipped
                    );
                    self.authored_spawn_count = population.spawns.len();
                    self.authored_route_count = population.routes.len();
                    self.live_world_template_candidates =
                        live_world_city_population(&population.spawns, &self.city_zones);
                    self.live_world_templates = vec![Vec::new(); self.city_zones.len()];
                    self.live_world_template_cursor = 0;
                    self.live_world_templates_ready = false;
                    self.live_world_name_tokens = live_world_name_tokens();
                    self.population = Some(population);
                }
                Err(error) => {
                    eprintln!("Populator: population unavailable: {error}");
                    self.load_error = Some(error);
                }
            }
        }
        if self.population.is_none() {
            return;
        }
        if self.library.is_none() && self.library_error.is_none() {
            match Library::open(&self.element) {
                Ok(library) => self.library = Some(library),
                Err(error) => {
                    eprintln!("Populator: models.pck unavailable: {error}");
                    self.library_error = Some(error);
                }
            }
        }
        if self.library.is_none() {
            return;
        }
        self.prepare_live_world_templates();
        self.extend_live_world(floors);
        self.stream(floors);
        self.advance(delta, floors);
        self.report();
    }

    /// Incrementally validate the original city-local human rigs that form the
    /// appearance palette. Locomotion is resolved separately from the normal
    /// player's track donors; corrupt/unloadable appearance rigs are rejected.
    /// Asset decompression is budgeted across frames.
    fn prepare_live_world_templates(&mut self) {
        if !self.live_world || self.live_world_templates_ready {
            return;
        }
        let total = self
            .live_world_template_candidates
            .iter()
            .map(Vec::len)
            .sum::<usize>();
        if total == 0 {
            self.live_world_templates_ready = true;
            return;
        }
        let (Some(population), Some(library)) = (self.population.as_ref(), self.library.as_mut())
        else {
            return;
        };
        let started = std::time::Instant::now();
        let mut processed = 0usize;
        while self.live_world_template_cursor < total {
            let flat = self.live_world_template_cursor;
            self.live_world_template_cursor += 1;
            processed += 1;
            let mut offset = flat;
            let mut candidate = None;
            for (zone, items) in self.live_world_template_candidates.iter().enumerate() {
                if offset < items.len() {
                    candidate = Some((zone, items[offset]));
                    break;
                }
                offset -= items.len();
            }
            let Some((zone, spawn_index)) = candidate else {
                continue;
            };
            let Some(spawn) = population.spawns.get(spawn_index) else {
                continue;
            };
            let model = spawn.model.clone();
            if let Err(error) = library.asset(&model) {
                eprintln!("Populator: Live World could not load rig {model}: {error}");
                continue;
            }
            self.live_world_templates[zone].push(spawn_index);
            if processed > 0 && started.elapsed() >= LIVE_WORLD_BUILD_BUDGET {
                break;
            }
        }
        if self.live_world_template_cursor >= total {
            self.live_world_templates_ready = true;
            let eligible = self
                .live_world_templates
                .iter()
                .map(Vec::len)
                .sum::<usize>();
            eprintln!("Populator: Live World found {eligible} loadable original human rig choices out of {total} city rigs; gait tracks are retargeted only when the skeleton maps validate");
        }
    }

    /// Generate independent city residents in bounded batches. Native placements
    /// contribute only original local human rig models, never their identities.
    fn extend_live_world(&mut self, floors: Option<&dyn FloorSource>) {
        if !self.live_world
            || !self.live_world_templates_ready
            || floors.is_none()
            || self.city_zones.is_empty()
            || self.live_world_templates.is_empty()
            || self.live_world_model_count() == 0
            || self.live_world_residents_per_zone == 0
        {
            return;
        }
        let zone_count = self.city_zones.len();
        let target_count = zone_count
            .saturating_mul(self.live_world_residents_per_zone)
            .min(LIVE_WORLD_MAX_RESIDENTS);
        if self.live_world_cursor >= target_count {
            return;
        }
        let Some(map) = self.map.as_ref() else {
            return;
        };
        let Some(population) = self.population.as_mut() else {
            return;
        };
        let floor_source = floors.expect("checked above");
        let end = (self.live_world_cursor + LIVE_WORLD_BATCH).min(target_count);
        let before = population.spawns.len();
        let started = std::time::Instant::now();
        while self.live_world_cursor < end {
            let generation = self.live_world_cursor;
            // Round-robin keeps the global cap from filling early targets only.
            let zone_index = generation % zone_count;
            let resident_index = generation / zone_count;
            if let (Some(templates), Some(zone)) = (
                self.live_world_templates.get(zone_index),
                self.city_zones.get(zone_index),
            ) {
                if let Some((mut spawn, route)) = build_live_world_spawn(
                    population,
                    zone_index,
                    resident_index,
                    generation,
                    templates,
                    &self.live_world_name_tokens,
                    map,
                    floor_source,
                    &population
                        .spawns
                        .iter()
                        .map(|other| Vec3::new(other.anchor[0], 0., other.anchor[1]))
                        .collect::<Vec<_>>(),
                    zone,
                ) {
                    spawn.route = Some(population.routes.len());
                    population.spawns.push(spawn);
                    population.routes.push(route);
                    self.live_world_generated += 1;
                }
            }
            self.live_world_cursor += 1;
            if started.elapsed() >= LIVE_WORLD_BUILD_BUDGET {
                break;
            }
        }
        let added = population.spawns.len() - before;
        self.live_world_since_stream_refresh += added;
        if self.live_world_since_stream_refresh >= LIVE_WORLD_STREAM_REFRESH
            || self.live_world_cursor >= target_count
        {
            self.stream_cache = None;
            self.live_world_since_stream_refresh = 0;
        }
    }

    /// Console diagnostics, so a user who never opens the settings window still
    /// sees whether the module is streaming and which rigs could not load.
    fn report(&mut self) {
        if !self.reported_spawn && !self.live.is_empty() {
            self.reported_spawn = true;
            eprintln!(
                "Populator: streaming {} actors within {} units",
                self.live.len(),
                self.settings.spawn_distance
            );
        }
        let failed = self
            .library
            .as_ref()
            .map_or(0, |library| library.failures.len());
        if let Some(library) = self.library.as_ref() {
            if failed > self.reported_failures {
                for (path, error) in library.failures.iter().skip(self.reported_failures).take(5) {
                    eprintln!("Populator: rig {path}: {error}");
                }
                self.reported_failures = failed;
            }
        }
        if let Some(population) = self.population.as_ref() {
            for live in &self.live {
                let (Some(error), Some(spawn)) =
                    (live.failure.as_ref(), population.spawns.get(live.spawn))
                else {
                    continue;
                };
                if self.reported_actor_failures.insert(live.spawn) {
                    eprintln!(
                        "Populator: actor {:?} ({}) failed to pose/draw: {error}",
                        spawn.name, spawn.model
                    );
                }
            }
        }
        if !self.reported_walls && !self.live.is_empty() {
            let (nudged, wedged) = (self.nudged_count(), self.wedged_count());
            if nudged > 0 || wedged > 0 {
                eprintln!(
                    "Populator: {} placements moved out of building geometry, {} still inside it (no free spot within {} units)",
                    nudged,
                    wedged,
                    WALL_RINGS.last().copied().unwrap_or(0.)
                );
            }
            self.reported_walls = true;
        }
        if !self.reported_idle && !self.live.is_empty() {
            self.reported_idle = true;
            eprintln!(
                "Populator: idle animation - {} combined actions scheduled, {} clips looped on the module clock, {} rigs whose clip cannot be sampled, {} rigs with no idle action (bind pose)",
                self.idle_report.combined,
                self.idle_report.looped,
                self.idle_report.unusable_clip,
                self.idle_report.bind_pose
            );
        }
        let pose_error_count = self
            .live
            .iter()
            .filter(|live| live.pose_error.is_some())
            .count();
        if pose_error_count > self.reported_pose_errors {
            for error in self
                .live
                .iter()
                .filter_map(|live| live.pose_error.as_ref())
                .skip(self.reported_pose_errors)
                .take(3)
            {
                eprintln!("Populator: idle tracks unusable, drawing the bind pose: {error}");
            }
            self.reported_pose_errors = pose_error_count;
        }
        // Three drawn seconds is long enough for a real idle to have moved and
        // for a frozen one to have proven it.
        if !self.reported_motion && self.motion_seconds >= 3. && !self.live.is_empty() {
            self.reported_motion = true;
            if self.settings.idle_animation {
                eprintln!(
                    "Populator: {} of {} drawn actors are animating, {} hold a still pose",
                    self.animating_count(),
                    self.live.len(),
                    self.frozen_count()
                );
            } else {
                eprintln!(
                    "Populator: idle animation is off, so all {} drawn actors hold a still pose",
                    self.live.len()
                );
            }
        }
    }

    /// Reconciles the drawn set with the placements inside the spawn distance.
    /// Actors that leave the radius are dropped, so the module never holds rigs
    /// it is not drawing.
    fn stream(&mut self, floors: Option<&dyn FloorSource>) {
        let eye = [self.eye.x, self.eye.z];
        let distance = self.settings.spawn_distance;
        let limit = self.settings.max_visible;
        let stale = match &self.stream_cache {
            Some(cache) => {
                let dx = cache.eye[0] - eye[0];
                let dz = cache.eye[1] - eye[1];
                cache.distance != distance
                    || cache.limit != limit
                    || dx * dx + dz * dz > STREAM_CACHE_MOVE * STREAM_CACHE_MOVE
            }
            None => true,
        };
        if stale {
            let nearest = match self.population.as_ref() {
                Some(population) => population.nearest(eye, distance, limit),
                None => return,
            };
            self.stream_cache = Some(StreamCache {
                eye,
                distance,
                limit,
                nearest,
            });
        }
        let Some(cache) = self.stream_cache.as_ref() else {
            return;
        };
        // The cache is borrowed from `self` while `self.live` is reconciled, so
        // copy the indices out first: the search result is at most `limit` long.
        let nearest: Vec<(usize, f32)> = cache.nearest.clone();
        // Reconcile by spawn index rather than by scanning the old actor list
        // for every placement: a full 96-actor set used to be a quadratic scan
        // on every frame, before any rig work was done.
        let mut previous: HashMap<usize, Live> = std::mem::take(&mut self.live)
            .into_iter()
            .map(|live| (live.spawn, live))
            .collect();
        // Keep every actor that is still in range, in distance order, before
        // spending the frame's rig budget on new ones.
        let mut next = Vec::with_capacity(nearest.len());
        let mut missing = Vec::new();
        for (index, _) in nearest {
            match previous.remove(&index) {
                Some(live) => next.push(live),
                None => missing.push(index),
            }
        }
        // Spawning decompresses a rig out of the archive on the render thread,
        // so the budget is wall-clock rather than a count: six tiny rigs are
        // fine in one frame, six heavy ones are a hitch.
        let started = std::time::Instant::now();
        let mut spawned = 0;
        for index in missing {
            if spawned >= MAX_SPAWNS_PER_UPDATE || started.elapsed() >= SPAWN_BUDGET {
                break;
            }
            spawned += 1;
            if let Some(live) = self.spawn_instance(index, floors) {
                next.push(live);
            }
        }
        self.live = next;
    }

    fn spawn_instance(&mut self, index: usize, floors: Option<&dyn FloorSource>) -> Option<Live> {
        let spawn = self.population.as_ref()?.spawns.get(index)?.clone();
        let placement = self.position_for(&spawn, floors);
        let position = placement.position;
        // The client placement files carry no facing angle. Preserve the
        // model's authored forward direction rather than inventing a rotation.
        let yaw = initial_yaw(&spawn);
        let live_world = index >= self.authored_spawn_count;
        let live_world_zone = live_world
            .then(|| city_zone_at(spawn.anchor[0], spawn.anchor[1], &self.city_zones))
            .flatten();
        let key = crate::ecmodel::normalize(&spawn.model);
        let library = self.library.as_mut()?;
        let asset = library.asset(&spawn.model).ok()?;
        let head = library.head(&key, asset.as_ref());
        let (action, clip, playback) = if self.settings.idle_animation && !live_world {
            idle_clock(asset.as_ref(), &mut self.idle_report)
        } else {
            (None, None, None)
        };
        let gait = live_world
            .then(|| library.retargeted_gait(&key, asset.as_ref()))
            .flatten();
        // A route remains active even when no compatible gait donor is found;
        // in that case the original resident rig safely holds its bind pose.
        let follow = self.route_for(&spawn);
        Some(Live {
            spawn: index,
            position,
            yaw,
            asset,
            action,
            playback,
            clip,
            clock: 0.,
            gait,
            gait_clock: 0.,
            live_world_moving: false,
            live_world_running: false,
            head,
            on_floor: placement.on_floor,
            name_visible: true,
            pose_error: None,
            motion: MotionProbe::default(),
            floor_stamp: floors.map_or(0, |floors| floors.floor_stamp()),
            nudged: placement.nudged,
            wedged: placement.wedged,
            visible: !placement.wedged,
            pose_index: None,
            pose_stamp: None,
            uploaded_stamp: None,
            follow,
            live_world,
            live_world_zone,
            route_attempt: 0,
            route_retry_seconds: 0.,
            draw: None,
            failure: None,
        })
    }

    /// Where one placement ends up: its height, whether it stands on a building
    /// floor, and whether it had to be moved out of geometry.
    fn position_for(&self, spawn: &Spawn, floors: Option<&dyn FloorSource>) -> Placement {
        let Some(map) = self.map.as_ref() else {
            return Placement {
                position: Vec3::new(spawn.anchor[0], 0., spawn.anchor[1]),
                on_floor: false,
                nudged: false,
                wedged: false,
            };
        };
        if spawn.id >= LIVE_WORLD_ID_BASE {
            return Placement {
                position: Vec3::new(
                    spawn.anchor[0],
                    map.ground(spawn.anchor[0], spawn.anchor[1]),
                    spawn.anchor[1],
                ),
                on_floor: false,
                nudged: false,
                wedged: false,
            };
        }
        resolve_placement(spawn, map, floors, self.settings.stand_on_objects)
    }

    fn route_for(&self, spawn: &Spawn) -> Option<Follow> {
        let route = self.population.as_ref()?.routes.get(spawn.route?)?;
        Follow::new(route).ok()
    }

    fn advance(&mut self, delta: f32, floors: Option<&dyn FloorSource>) {
        self.time_advanced = delta.is_finite() && delta > 0.;
        if !self.time_advanced {
            return;
        }
        self.resolve_floors(floors);
        // A label's occlusion changes when the camera or the actor moves, not
        // every frame, and testing it is a hull raycast per actor.
        self.name_clock += delta;
        if self.name_clock >= NAME_VISIBILITY_INTERVAL {
            self.name_clock = 0.;
            self.update_name_visibility(floors);
        }
        self.motion_seconds += delta;
        let delta_ms = (delta * 1000.).round().clamp(0., u32::MAX as f32) as u32;
        let mut occupied: Vec<Vec3> = self.live.iter().map(|live| live.position).collect();
        for (live_index, live) in self.live.iter_mut().enumerate() {
            if live.failure.is_some() || live.wedged {
                continue;
            }
            live.clock += delta;
            live.gait_clock += delta;
            live.live_world_moving = false;
            live.live_world_running = false;
            if let Some(playback) = live.playback.as_mut() {
                if playback.tick(delta_ms).is_err() {
                    live.playback = None;
                } else if playback.stopped() {
                    playback.restart();
                }
            }
            live.route_retry_seconds = (live.route_retry_seconds - delta).max(0.);
            if live.live_world && live.follow.is_none() && live.route_retry_seconds <= 0. {
                if let (Some(map), Some(floors), Some(zone)) = (
                    self.map.as_ref(),
                    floors,
                    live.live_world_zone
                        .and_then(|index| self.city_zones.get(index)),
                ) {
                    live.route_attempt = live.route_attempt.wrapping_add(1);
                    let seed = (live.spawn as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
                        ^ live.route_attempt.wrapping_mul(0xbf58_476d_1ce4_e5b9);
                    if let Some(route) = build_live_world_route(
                        live.position,
                        seed,
                        zone,
                        map,
                        floors,
                        &occupied,
                        Some(live_index),
                    ) {
                        live.follow = Follow::new(&route).ok();
                    }
                }
                if live.follow.is_none() {
                    live.route_retry_seconds = 0.5;
                }
            }
            let Some(follow) = live.follow.as_mut() else {
                continue;
            };
            let before = live.position;
            let route_speed = follow.speed();
            match follow.step(&mut live.position, delta) {
                Motion::Walking => {
                    live.live_world_moving = live.live_world;
                    live.live_world_running =
                        live.live_world && route_speed >= LIVE_WORLD_RUN_SPEED;
                    if live.live_world {
                        let safe = self.map.as_ref().zip(floors).is_some_and(|(map, floors)| {
                            ambient_path_safe(
                                before,
                                live.position,
                                map,
                                floors,
                                &occupied,
                                Some(live_index),
                            )
                        });
                        if !safe {
                            live.position = before;
                            live.follow = None;
                            live.route_retry_seconds = 0.2;
                            occupied[live_index] = live.position;
                            continue;
                        }
                    }
                    let moved = live.position - before;
                    if moved.length_squared() > 1e-8 {
                        live.yaw = patrol::yaw_towards(moved);
                    }
                    if let Some(map) = self.map.as_ref() {
                        live.position.y = map.ground(live.position.x, live.position.z);
                    }
                }
                Motion::Finished => {
                    live.follow = None;
                }
                Motion::Waiting => {}
            }
            occupied[live_index] = live.position;
        }
    }

    /// Re-resolves placements the host's hull loader has caught up with.
    ///
    /// Building hulls stream in from `models.pck` after the first frame, so an
    /// actor spawned during loading may have resolved against an incomplete
    /// world and be standing on the terrain under a bridge. The stamp changes
    /// with every batch; only the actors resolved against an older one are
    /// moved again, and actors with an authored height never are.
    /// Decide which labels the camera can actually see.
    ///
    /// The client never paints an overhead name through geometry, so a label is
    /// dropped when the terrain or a building hull stands between the camera and
    /// the head. Bodies stay visible; only the label goes away.
    fn update_name_visibility(&mut self, floors: Option<&dyn FloorSource>) {
        if !self.settings.show_names || self.settings.name_distance <= 0. {
            for live in &mut self.live {
                live.name_visible = true;
            }
            return;
        }
        let Some(map) = self.map.as_ref() else {
            return;
        };
        let eye = self.eye;
        let range = self.settings.name_distance;
        let visible: Vec<bool> = self
            .live
            .iter()
            .map(|live| {
                let anchor = live.position + Vec3::Y * (live.head + NAME_LIFT);
                if (anchor - eye).length() > range {
                    return true;
                }
                if terrain_between(map, eye, anchor) {
                    return false;
                }
                !floors.is_some_and(|floors| floors.sight_blocked(eye, anchor))
            })
            .collect();
        for (live, visible) in self.live.iter_mut().zip(visible) {
            live.name_visible = visible;
        }
    }

    fn resolve_floors(&mut self, floors: Option<&dyn FloorSource>) {
        // The sweep runs wherever hulls exist, not only with floor standing on:
        // an NPC authored inside a wall is wrong at any height policy, and the
        // host already loads the hulls for the overhead names.
        let Some(floors) = floors else {
            return;
        };
        let stamp = floors.floor_stamp();
        let Some(map) = self.map.as_ref() else {
            return;
        };
        let Some(population) = self.population.as_ref() else {
            return;
        };
        // Resolve first, apply second: the sweep reads the map and the hulls,
        // so it cannot run while `self.live` is borrowed mutably.
        let placements: Vec<Option<(usize, Placement)>> = self
            .live
            .iter()
            .map(|live| {
                if live.floor_stamp == stamp {
                    return None;
                }
                let spawn = population.spawns.get(live.spawn)?;
                // Native placements always re-resolve to their authored point.
                // A wandering resident instead re-resolves under its current
                // feet so a newly streamed hull cannot teleport it back to its
                // initial ambient spawn.
                let placement = if live.live_world {
                    let mut current = spawn.clone();
                    current.anchor = [live.position.x, live.position.z];
                    current.height = None;
                    resolve_placement(&current, map, Some(floors), false)
                } else {
                    // An authored height is the game's own answer and never
                    // moves, but the column can still be inside a wall.
                    resolve_placement(spawn, map, Some(floors), self.settings.stand_on_objects)
                };
                Some((live.spawn, placement))
            })
            .collect();
        for (live, resolved) in self.live.iter_mut().zip(placements) {
            live.floor_stamp = stamp;
            let Some((spawn_index, placement)) = resolved else {
                continue;
            };
            debug_assert_eq!(live.spawn, spawn_index);
            live.position = placement.position;
            live.on_floor = placement.on_floor;
            live.nudged = placement.nudged;
            live.wedged = placement.wedged;
        }
    }

    /// Adds the population to the host's model pass. The pass is only opened
    /// when at least one actor has a valid draw.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        frame: model_draw_gpu::Frame,
    ) {
        if !self.active || self.live.is_empty() {
            return;
        }
        if self.renderer.is_none() {
            self.renderer = Some(Renderer::new(device));
        }
        let Some(library) = self.library.as_mut() else {
            return;
        };
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        // The host calls this for its main, reflection and refraction passes, so
        // everything here is per *pass*: an actor is posed once per frame, and a
        // pass that adds nothing but a different view matrix refreshes uniforms
        // instead of re-preparing and re-uploading every posed vertex.
        let view_projection = Mat4::from_cols_array_2d(&frame.matrix);
        let frame_index = self.frame_index;
        let eye = self.eye;
        let time_advanced = self.time_advanced;
        // Evaluate every pose before touching GPU state, so one broken rig
        // cannot stop the rest of the population from appearing.
        let mut posed: Vec<Option<Vec<PosedMesh>>> = (0..self.live.len()).map(|_| None).collect();
        for index in 0..self.live.len() {
            let live = &mut self.live[index];
            if live.failure.is_some() || live.wedged {
                live.visible = false;
                continue;
            }
            // Nothing off screen is posed, uploaded or submitted: a crowd behind
            // the camera costs nothing.
            live.visible = on_screen(view_projection, live.position, live.head);
            if !live.visible || live.pose_index == Some(frame_index) {
                continue;
            }
            // Nothing advanced: the pose on the GPU is still this frame's pose.
            if !time_advanced && live.pose_index.is_some() {
                continue;
            }
            // Past `POSE_STRIDE_NEAR` an actor is re-skinned every other frame and
            // past `POSE_STRIDE_FAR` every third.
            let stride = pose_stride((live.position - eye).length());
            if !pose_due(frame_index, live.spawn, stride) {
                continue;
            }
            match live.pose() {
                Ok(meshes) => {
                    let signature = pose_signature(&meshes);
                    live.motion.observe(signature);
                    live.pose_stamp = Some(signature);
                    live.pose_index = Some(frame_index);
                    posed[index] = Some(meshes);
                }
                Err(error) => live.failure = Some(error),
            }
        }
        for index in 0..self.live.len() {
            let live = &mut self.live[index];
            if live.failure.is_some() || !live.visible {
                continue;
            }
            let asset = live.asset.clone();
            // A pose identical to the one already on the GPU (scenery rigs, a
            // held pose, another pass of this frame) needs only the constants.
            let unchanged = live.uploaded_stamp.is_some() && live.uploaded_stamp == live.pose_stamp;
            match (live.draw.take(), posed[index].take()) {
                (Some(draw), Some(meshes)) if unchanged => {
                    drop(meshes);
                    match draw.update_constants(queue, asset.as_ref(), frame) {
                        Ok(()) => live.draw = Some(draw),
                        Err(error) => live.failure = Some(error),
                    }
                }
                (Some(draw), Some(meshes)) => {
                    // Topology is fixed for a retained rig, so this skips
                    // `model_draw::prepare`'s per-frame re-validation and
                    // converts the pose straight into the vertex buffer.
                    match draw.update_posed(queue, asset.as_ref(), &meshes, frame) {
                        Ok(()) => {
                            live.uploaded_stamp = live.pose_stamp;
                            live.draw = Some(draw);
                        }
                        Err(error) => live.failure = Some(error),
                    }
                }
                (Some(draw), None) => {
                    // Posed by an earlier pass of this frame: only this pass's
                    // view matrix and light are new.
                    match draw.update_constants(queue, asset.as_ref(), frame) {
                        Ok(()) => live.draw = Some(draw),
                        Err(error) => live.failure = Some(error),
                    }
                }
                (None, Some(meshes)) => {
                    match renderer.build(
                        device,
                        queue,
                        format,
                        asset.as_ref(),
                        &meshes,
                        frame,
                        |path| library.texture_candidates(path),
                    ) {
                        Ok(draw) => {
                            live.uploaded_stamp = live.pose_stamp;
                            live.draw = Some(draw);
                        }
                        Err(error) => live.failure = Some(error),
                    }
                }
                (None, None) => {}
            }
        }
        if !self
            .live
            .iter()
            .any(|live| live.visible && live.draw.is_some())
        {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("add-on Populator pass"),
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
        for live in &self.live {
            // An actor this pass culled keeps its GPU buffers but is not
            // submitted: that is the whole point of the test.
            if !live.visible {
                continue;
            }
            if let Some(draw) = &live.draw {
                draw.draw(&mut pass);
            }
        }
    }

    fn drawn_count(&self) -> usize {
        self.live
            .iter()
            .filter(|live| live.visible && live.draw.is_some())
            .count()
    }

    /// Actors standing on a real building floor instead of the terrain, so the
    /// settings window can show whether the support query actually engaged.
    fn on_floor_count(&self) -> usize {
        self.live.iter().filter(|live| live.on_floor).count()
    }

    /// Actors whose drawn pose changed within the last half second.
    fn animating_count(&self) -> usize {
        self.live
            .iter()
            .filter(|live| live.failure.is_none() && live.motion.animating())
            .count()
    }

    /// Actors that have been drawn but whose geometry has not changed for
    /// [`MOTION_WINDOW`] frames.
    fn frozen_count(&self) -> usize {
        self.live
            .iter()
            .filter(|live| {
                live.failure.is_none() && live.motion.observed() && !live.motion.animating()
            })
            .count()
    }

    /// Actors moved off their authored column to get out of building geometry.
    fn nudged_count(&self) -> usize {
        self.live.iter().filter(|live| live.nudged).count()
    }

    /// Actors still inside geometry: no free spot inside the search rings.
    fn wedged_count(&self) -> usize {
        self.live.iter().filter(|live| live.wedged).count()
    }

    fn failed_count(&self) -> usize {
        self.live
            .iter()
            .filter(|live| live.failure.is_some())
            .count()
    }

    /// Whether overhead names will be painted this frame, so the host only runs
    /// its egui pass when there is something to draw.
    pub fn names_active(&self) -> bool {
        self.active
            && self.settings.show_names
            && !self.live.is_empty()
            && self.population.is_some()
    }

    /// Client overhead names: the template's own name, drawn with the font the
    /// client's own `configs/fonts.txt` names for player and NPC labels, at the
    /// size that table asks for, projected over the actor's head in world space.
    ///
    /// The client renders these labels as fixed-size GDI text (so they neither
    /// shrink with distance nor fade), and hides a label when the head is not
    /// visible from the camera; both behaviours are reproduced here. The layer
    /// sits above the 3D scene and below the viewer's windows, so a menu still
    /// covers the labels.
    pub fn overlay_ui(&mut self, ctx: &egui::Context, view_projection: Mat4, screen: egui::Rect) {
        if !self.names_active() || self.settings.name_distance <= 0. {
            return;
        }
        if self.name_font.is_none() {
            let element = self.element.clone();
            self.name_font = Some(NameFont::load(&element));
        }
        let font = {
            let Some(font) = self.name_font.as_mut() else {
                return;
            };
            // egui applies new font definitions at the start of the next frame,
            // so the frame that registers the client font draws no label rather
            // than forcing the family early.
            if !font.install(ctx) {
                return;
            }
            font.font_id()
        };
        let Some(population) = self.population.as_ref() else {
            return;
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("populator-nameplates"),
        ));
        let eye = self.eye;
        let range = self.settings.name_distance;
        for live in &self.live {
            if live.failure.is_some() || live.wedged || !live.name_visible {
                continue;
            }
            let Some(spawn) = population.spawns.get(live.spawn) else {
                continue;
            };
            if spawn.name.is_empty() {
                continue;
            }
            if (live.position - eye).length() > range {
                continue;
            }
            let anchor = live.position + Vec3::Y * (live.head + NAME_LIFT);
            let Some(position) = project(anchor, view_projection, screen) else {
                continue;
            };
            // The client's `字体描边` outline: one pixel of black on each side
            // of white glyphs, at the same size as the glyphs themselves. It is
            // four extra text shapes per label, so a distant label - where the
            // outline is a pixel of haze anyway - is drawn with the fill alone.
            if (live.position - eye).length() <= NAME_OUTLINE_DISTANCE {
                for (dx, dy) in [(-1., 0.), (1., 0.), (0., -1.), (0., 1.)] {
                    painter.text(
                        position + egui::vec2(dx, dy),
                        egui::Align2::CENTER_BOTTOM,
                        &spawn.name,
                        font.clone(),
                        egui::Color32::BLACK,
                    );
                }
            }
            painter.text(
                position,
                egui::Align2::CENTER_BOTTOM,
                &spawn.name,
                font.clone(),
                egui::Color32::WHITE,
            );
        }
    }

    pub fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Populator");
        ui.label(egui::RichText::new(self.status()).small().weak());
        ui.separator();

        ui.heading("Real population");
        let mut go_to_nearest = false;
        match (&self.population, &self.load_error) {
            (_, Some(error)) => {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            (Some(population), None) => {
                let npcs = self.authored_spawn_count;
                let monsters = population
                    .spawns
                    .iter()
                    .filter(|spawn| spawn.kind == TemplateKind::Monster)
                    .count();
                let linked = population
                    .spawns
                    .iter()
                    .filter(|spawn| spawn.task_count > 0)
                    .count();
                let records = self.authored_spawn_count + population.unplaced + population.skipped;
                ui.label(format!(
                    "{npcs} client-placed NPCs, {monsters} monster placements, {linked} with task links, {} server-placed, {} skipped",
                    population.unplaced, population.skipped
                ));
                ui.label(
                    egui::RichText::new(format!(
                        "task_npc.data: {} bytes, timestamp 0x{:08x}, {records} records",
                        population.table_size, population.table_timestamp
                    ))
                    .small()
                    .weak(),
                );
                for line in &population.provenance {
                    ui.label(egui::RichText::new(line).small().weak());
                }
                if self.live_world {
                    ui.label(
                        egui::RichText::new(format!(
                            "Live World (Experimental): {} residents generated; {} streamed rigs have validated walk/run retargets.",
                            self.live_world_generated,
                            self.live_world_gait_count()
                        ))
                        .small()
                        .color(egui::Color32::YELLOW),
                    );
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{} NPC templates, {} monster templates",
                        population.npc_templates, population.monster_templates
                    ))
                    .small()
                    .weak(),
                );
                let in_range =
                    population.count_within([self.eye.x, self.eye.z], self.settings.spawn_distance);
                if in_range == 0 {
                    let server_placed = if records > 0 {
                        100. * population.unplaced as f32 / records as f32
                    } else {
                        0.
                    };
                    ui.colored_label(
                        egui::Color32::from_rgb(240, 190, 80),
                        format!(
                            "Nothing is placed within {:.0} units of the camera. {server_placed:.0}% of this table is server-placed, and the client's own placements cluster in towns: widen the spawn distance, or use the button below.",
                            self.settings.spawn_distance
                        ),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(format!(
                            "{in_range} actors (native and Live World) within {:.0} units of the camera",
                            self.settings.spawn_distance
                        ))
                        .small()
                        .weak(),
                    );
                }
                if ui.button("Go to the nearest placement").clicked() {
                    go_to_nearest = true;
                }
            }
            (None, None) => {
                ui.label("Reading elements.data and task_npc.data...");
            }
        }
        if go_to_nearest {
            if let Some(target) = self.nearest_camera_target() {
                self.camera_request = Some(target);
            }
        }
        if let Some(error) = &self.library_error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        if let Some(library) = &self.library {
            ui.label(
                egui::RichText::new(format!(
                    "models.pck: {} entries, {} rigs loaded, {} failed",
                    library.models.len(),
                    library.assets.len(),
                    library.failures.len()
                ))
                .small()
                .weak(),
            );
        }
        if ui.button("Reload from the element folder").clicked() {
            self.reload_pending = true;
        }
        if let Some(library) = &self.library {
            if !library.failures.is_empty() {
                egui::CollapsingHeader::new(format!("Failed rigs ({})", library.failures.len()))
                    .show(ui, |ui| {
                        for (path, error) in library.failures.iter().take(8) {
                            ui.label(egui::RichText::new(format!("{path}: {error}")).small());
                        }
                    });
            }
        }

        ui.separator();
        ui.heading("Spawn distance");
        ui.add(
            egui::Slider::new(
                &mut self.settings.spawn_distance,
                MIN_SPAWN_DISTANCE..=MAX_SPAWN_DISTANCE,
            )
            .text("units")
            .suffix(" u"),
        );
        ui.horizontal(|ui| {
            for (label, distance) in [
                ("Scenery 120", 120.),
                ("Native 420", NATIVE_SPAWN_DISTANCE),
                ("Wide 800", 800.),
            ] {
                if ui.button(label).clicked() {
                    self.settings.spawn_distance = distance;
                }
            }
        });
        ui.label(
            egui::RichText::new(
                "420 units is the client's own scene window: its quality presets keep a seven-block terrain radius (7 x 64 units, DetailSettings.radius at world+0x1bc). Shipped detail_scene preset 1 has ornament culling at 120 units.",
            )
            .small()
            .weak(),
        );
        ui.add(
            egui::Slider::new(&mut self.settings.max_visible, 1..=MAX_VISIBLE_LIMIT).text("actors"),
        );
        ui.checkbox(&mut self.settings.idle_animation, "Idle animation")
            .on_hover_text(
                "Play the template's own `站立` scene action, the standing loop 1,674 of the 1,871 shipped NPC rigs carry. The client's combined scene action is used where it can be scheduled; otherwise the clip it names loops on the module clock, and only scenery rigs, which ship no idle action at all, stay in their bind pose.",
            );
        ui.checkbox(&mut self.settings.stand_on_objects, "Stand on building floors")
            .on_hover_text(
                "Resolve each placement against the client's own BMD collision hulls, so an NPC authored onto a bridge deck, platform or interior floor stands there instead of on the terrain under the geometry. Uses the same hulls as the Walker module's collision check and loads them when this module is enabled. The full conservative actor box is tested against walls; a placement authored into geometry is moved to the nearest clear spot up to six units away while preserving its height. If no clear spot exists, it is hidden instead of rendered inside the wall and counted under `in walls`; it can be retried when more hulls stream in.",
            );
        ui.checkbox(&mut self.settings.show_names, "Names above heads")
            .on_hover_text("Draw each template's own name over the actor, as the client does.");
        ui.add_enabled(
            self.settings.show_names,
            egui::Slider::new(
                &mut self.settings.name_distance,
                MIN_NAME_DISTANCE..=MAX_SPAWN_DISTANCE,
            )
            .text("name distance")
            .suffix(" u"),
        );
        if self.settings.show_names {
            let element = self.element.clone();
            let font = self
                .name_font
                .get_or_insert_with(|| NameFont::load(&element));
            ui.label(egui::RichText::new(font.describe()).small().weak())
                .on_hover_text(
                    "The client's own `Configs\\fonts.txt` names the font of the player and NPC labels above their heads. That row is read out of `configs.pck` and used for every name, so the typeface and the size are the client's, not the viewer's; the client's shipped CJK font is stacked behind it for glyphs the system font lacks.",
                );
        }
        self.settings = self.settings.sanitized();
        ui.label(
            egui::RichText::new(format!(
                "{} drawn, {} animating, {} on building floors, {} moved out of walls, {} in walls, {} failed, {} rigs cached, {} textures cached",
                self.drawn_count(),
                self.animating_count(),
                self.on_floor_count(),
                self.nudged_count(),
                self.wedged_count(),
                self.failed_count(),
                self.library
                    .as_ref()
                    .map_or(0, |library| library.assets.len()),
                self.renderer.as_ref().map_or(0, Renderer::texture_count)
            ))
            .small()
            .weak(),
        );

        ui.separator();
        ui.heading("Population state");
        if let Some(population) = &self.population {
            let moving = self
                .live
                .iter()
                .filter(|live| live.follow.is_some())
                .count();
            ui.label(
                egui::RichText::new(&population.mob_placement)
                    .small()
                    .weak(),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{moving} actors on movement routes (authored client placements plus any validated Live World circuits). {}",
                    population.route_source
                ))
                .small()
                .weak(),
            );
        }
        ui.label(
            egui::RichText::new(
                "Native NPCs use client placements, models, idle animation and names. Live World (Experimental) tries synthetic residents near city targets using original male/female models, the normal player's walk/run animation retargeted when their skeletons validate, and safe local routes. The donor's player mesh is never used; unmatched rigs keep moving in bind pose. It may generate zero if no rig or route passes; server spawn data is unavailable.",
            )
            .small()
            .weak(),
        );
        ui.label(
            egui::RichText::new(
                "The supplied client population files do not contain per-NPC facing. Existing placements keep the model's authored forward direction; exact server-authored directions require the server spawn data (gamedata/npcgen.data), which was not present in pw142.",
            )
            .small()
            .weak(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn spawn(height: Option<f32>) -> Spawn {
        Spawn {
            kind: TemplateKind::Npc,
            id: 2109,
            name: "Apothecary Cheng".into(),
            model: "models/npcs/npc/男npc4/男npc4.ecm".into(),
            anchor: [-1571., 2184.],
            height,
            task_count: 12,
            route: None,
        }
    }

    #[test]
    fn reflect_texture_alpha_is_a_specular_mask_not_body_opacity() {
        let (mut asset, _) = crate::model_draw_fixture::fixture();
        asset.skins[0].data.textures[0] = "R_男npc3.dds".into();
        apply_reflect_prefix_alpha(&mut asset);
        assert!(asset.diffuse_alpha_meshes.contains(&(0, 0)));

        asset.skins[0].data.textures[0] = "头发.dds".into();
        asset.diffuse_alpha_meshes.clear();
        apply_reflect_prefix_alpha(&mut asset);
        assert!(!asset.diffuse_alpha_meshes.contains(&(0, 0)));
    }

    #[test]
    fn city_target_matching_radius_and_gendered_model_rules_are_explicit() {
        assert!(city_zone_target_name("Archosaur, North District"));
        assert!(city_zone_target_name("City of a Thousand Streams"));
        assert!(city_zone_target_name("City of Raging Tides"));
        assert!(!city_zone_target_name("World Tree"));
        let zones = vec![
            CityZone {
                name: "Archosaur, North District".into(),
                center: [1470.490, 1180.864],
                radius: LIVE_WORLD_DEFAULT_RADIUS,
            },
            CityZone {
                name: "City of a Thousand Streams".into(),
                center: [-2825.774, 3092.210],
                radius: LIVE_WORLD_DEFAULT_RADIUS,
            },
        ];
        assert_eq!(city_zone_at(1470.490, 1180.864, &zones), Some(0));
        assert_eq!(city_zone_at(-2825.774, 3092.210, &zones), Some(1));
        assert_eq!(city_zone_at(1470.490 + 241., 1180.864, &zones), None);
        let mut wide = zones.clone();
        wide[0].radius = LIVE_WORLD_MAX_RADIUS;
        assert_eq!(city_zone_at(1470.490 + 400., 1180.864, &wide), Some(0));
        assert_eq!(
            live_world_gender("models/npcs/npc/男npc4/男npc4.ecm"),
            Some(NpcGender::Masculine)
        );
        assert_eq!(
            live_world_gender("models/npcs/npc/女npc7/女npc7.ecm"),
            Some(NpcGender::Feminine)
        );
        for guard in 1..=4 {
            let path = format!("models/npcs/npc/卫兵npc{guard}/卫兵npc{guard}.ecm");
            assert_eq!(live_world_gender(&path), None);
            assert!(!live_world_rig_allowed(&path));
        }
        assert_eq!(
            live_world_gender("models/npcs/npc/妖兽npc1/妖兽npc1.ecm"),
            None
        );
        assert_eq!(live_world_gender("models/monsters/dragon/dragon.ecm"), None);
        assert_eq!(live_world_gender("models/npcs/npc/男npc4/女npc4.ecm"), None);
        assert!(!live_world_rig_allowed("models/npcs/npc/男npc5/男npc5.ecm"));
        assert!(!live_world_rig_allowed(
            "models/npcs/npc/妖兽npc4/妖兽npc4.ecm"
        ));
        assert!(!live_world_rig_allowed("models/npcs/npc/宝箱/宝箱.ecm"));
        assert!(!live_world_rig_allowed(
            "models/npcs/npc/妖兽npc1/妖兽npc1.ecm"
        ));
    }

    #[test]
    fn city_rig_palette_uses_original_static_models_and_excludes_guards_and_props() {
        let mut male = spawn(None);
        male.model = "models/npcs/npc/男npc4/男npc4.ecm".into();
        male.anchor = [1470.490, 1180.864];
        let mut duplicate = male.clone();
        duplicate.id += 1;
        let mut female = male.clone();
        female.id += 2;
        female.model = "models/npcs/npc/女npc7/女npc7.ecm".into();
        let mut anvil = male.clone();
        anvil.id += 3;
        anvil.model = "models/npcs/npc/男npc5/男npc5.ecm".into();
        let mut object = male.clone();
        object.id += 4;
        object.model = "models/npcs/npc/宝箱/宝箱.ecm".into();
        let mut wilderness = male.clone();
        wilderness.id += 5;
        wilderness.anchor = [0., 0.];
        let mut creature = male.clone();
        creature.id += 6;
        creature.model = "models/npcs/npc/妖兽npc1/妖兽npc1.ecm".into();
        let mut guard = male.clone();
        guard.id += 7;
        guard.model = "models/npcs/npc/卫兵npc1/卫兵npc1.ecm".into();
        let zones = vec![CityZone {
            name: "Archosaur".into(),
            center: [1470.490, 1180.864],
            radius: LIVE_WORLD_DEFAULT_RADIUS,
        }];
        assert_eq!(
            live_world_city_population(
                &[male, duplicate, female, anvil, object, wilderness, creature, guard,],
                &zones
            ),
            vec![vec![0, 2]]
        );
    }

    #[test]
    fn synthetic_names_are_gender_specific_and_never_collide() {
        let tokens = live_world_name_tokens();
        assert_eq!(tokens.masculine.len(), 32);
        assert_eq!(tokens.feminine.len(), 32);
        assert!(tokens
            .masculine
            .iter()
            .all(|part| !tokens.feminine.contains(part)));
        let mut population = Population {
            spawns: vec![spawn(None)],
            ..Population::default()
        };
        let mut names = HashSet::new();
        for seed in 0u64..20 {
            let gender = if seed % 2 == 0 {
                NpcGender::Masculine
            } else {
                NpcGender::Feminine
            };
            let pool = tokens.for_gender(gender);
            let name = live_world_name(&population, pool, seed).unwrap();
            assert!(names.insert(name.to_ascii_lowercase()));
            let parts: Vec<_> = name.split_whitespace().collect();
            assert_eq!(parts.len(), 2);
            assert!(parts
                .iter()
                .all(|part| pool.iter().any(|token| token.as_str() == *part)));
            assert!(!population
                .spawns
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&name)));
            population.spawns.push(Spawn {
                name,
                ..spawn(None)
            });
        }
    }

    #[test]
    fn generated_resident_has_fresh_identity_and_city_sampled_collision_checked_route() {
        let mut native = spawn(None);
        native.anchor = [0., 0.];
        native.name = "Tailor Sin".into();
        let mut local = native.clone();
        local.id += 1;
        local.model = "models/npcs/npc/男npc1/男npc1.ecm".into();
        local.anchor = [12., 0.];
        let population = Population {
            spawns: vec![native, local],
            ..Population::default()
        };
        let zone = CityZone {
            name: "Test City".into(),
            center: [0., 0.],
            radius: 30.,
        };
        let map = flat_map();
        let clear = OneFloor(None, 1, false);
        let (resident, route) = build_live_world_spawn(
            &population,
            0,
            1,
            0,
            &[0, 1],
            &live_world_name_tokens(),
            &map,
            &clear,
            &[],
            &zone,
        )
        .expect("clear city route");
        assert!(resident.id >= LIVE_WORLD_ID_BASE);
        assert_ne!(resident.id, population.spawns[0].id);
        assert_ne!(resident.name, population.spawns[0].name);
        assert_ne!(resident.model, population.spawns[0].model);
        assert!(resident.anchor[0].hypot(resident.anchor[1]) >= LIVE_WORLD_MIN_SEPARATION);
        assert_eq!(resident.task_count, 0);
        assert_eq!(resident.height, None);
        assert!(route.looping);
        assert_eq!(route.waypoints.len(), 7);
        assert!(route.validate().is_ok());
        assert!(route.waypoints.iter().all(|point| {
            let dx = point.position[0] - zone.center[0];
            let dz = point.position[2] - zone.center[1];
            dx * dx + dz * dz <= zone.radius * zone.radius
        }));
        struct SolidEverywhere;
        impl FloorSource for SolidEverywhere {
            fn lowest_floor(&self, _: Vec3, _: Vec3, _: f32) -> Option<f32> {
                None
            }
            fn floor_stamp(&self) -> u64 {
                1
            }
            fn sight_blocked(&self, _: Vec3, _: Vec3) -> bool {
                false
            }
            fn solid_at(&self, _: Vec3, _: Vec3) -> bool {
                true
            }
        }
        assert!(build_live_world_spawn(
            &population,
            0,
            1,
            1,
            &[0, 1],
            &live_world_name_tokens(),
            &map,
            &SolidEverywhere,
            &[],
            &zone
        )
        .is_none());
    }

    #[test]
    fn projected_route_retries_candidates_around_blocked_ground() {
        struct EastBlocked;
        impl FloorSource for EastBlocked {
            fn lowest_floor(&self, _: Vec3, _: Vec3, _: f32) -> Option<f32> {
                None
            }
            fn floor_stamp(&self) -> u64 {
                1
            }
            fn sight_blocked(&self, _: Vec3, _: Vec3) -> bool {
                false
            }
            fn solid_at(&self, feet: Vec3, _: Vec3) -> bool {
                feet.x > 0.0
            }
        }
        let map = flat_map();
        let zone = CityZone {
            name: "Test City".into(),
            center: [0., 0.],
            radius: 30.,
        };
        let start = Vec3::new(-10., 0., 0.);
        let route = (0..16).find_map(|seed| {
            build_live_world_route(start, seed, &zone, &map, &EastBlocked, &[], None)
        });
        let route = route.expect("a clear candidate should survive retries");
        assert!(route.waypoints.iter().all(|point| point.position[0] <= 0.));
        assert!(route.waypoints[0].wait < 0.5);
    }

    #[test]
    fn live_world_controls_clamp_and_disabling_removes_only_generated_records() {
        let mut p = Populator::default();
        assert_eq!(p.live_world_radius(), LIVE_WORLD_DEFAULT_RADIUS);
        p.set_live_world_radius(f32::INFINITY);
        assert_eq!(p.live_world_radius(), LIVE_WORLD_DEFAULT_RADIUS);
        p.set_live_world_radius(1000.);
        assert_eq!(p.live_world_radius(), LIVE_WORLD_MAX_RADIUS);
        p.set_live_world_residents_per_zone(usize::MAX);
        assert_eq!(p.live_world_residents_per_zone(), LIVE_WORLD_MAX_PER_ZONE);
        p.set_live_world_residents_per_zone(0);
        assert_eq!(p.live_world_residents_per_zone(), 0);
        let route = Route {
            waypoints: vec![
                Waypoint {
                    position: [0., 0., 0.],
                    wait: 0.,
                },
                Waypoint {
                    position: [1., 0., 0.],
                    wait: 0.,
                },
            ],
            speed: 1.,
            looping: false,
        };
        p.live_world = true;
        p.authored_spawn_count = 1;
        p.authored_route_count = 1;
        p.population = Some(Population {
            spawns: vec![
                spawn(None),
                Spawn {
                    id: LIVE_WORLD_ID_BASE,
                    ..spawn(None)
                },
            ],
            routes: vec![route.clone(), route],
            ..Population::default()
        });
        p.set_live_world(false);
        let population = p.population.as_ref().unwrap();
        assert_eq!(population.spawns.len(), 1);
        assert_eq!(population.routes.len(), 1);
    }

    #[test]
    fn walk_animation_resolves_direct_and_combined_native_locomotion() {
        assert!(locomotion_name("走"));
        assert!(locomotion_name("跑步_空手_1"));
        assert!(!locomotion_name("休闲1打铁"));
        assert!(!locomotion_name("战斗移动"));
        assert_eq!(gait_kind("行走"), Some(GaitKind::Walk));
        assert_eq!(gait_kind("奔跑"), Some(GaitKind::Run));
        assert_eq!(gait_kind("休闲1打铁"), None);

        let mut direct = rig_without_combined_actions();
        direct.skeleton.fps = 30;
        direct
            .model
            .actions
            .push(idle_action("行走_空手_1", 0., 30.));
        let mut candidates = walk_clock_candidates(&direct);
        assert_eq!(candidates.len(), 1);
        let mut walk = candidates.remove(0);
        assert!(
            !walk_animation_moves_meshes(&direct, &walk),
            "a named action without usable tracks is not a walking animation"
        );
        let before = walk.sample().unwrap().1;
        walk.advance(0.5, 500);
        assert!(walk.sample().unwrap().1 > before);
        assert!(walk_clock(&direct).is_none());

        let mut combined_asset = rig_without_combined_actions();
        combined_asset.skeleton.fps = 30;
        combined_asset
            .model
            .actions
            .push(idle_action("奔跑_通用", 0., 30.));
        combined_asset
            .descriptor
            .actions
            .push(combined("快速移动", "奔跑_通用"));
        let candidates = walk_clock_candidates(&combined_asset);
        assert!(candidates
            .iter()
            .any(|animation| animation.playback.is_some()));
        assert!(walk_clock(&combined_asset).is_none());

        let mut work_only = rig_without_combined_actions();
        work_only
            .model
            .actions
            .push(idle_action("休闲1打铁", 0., 30.));
        assert!(walk_clock_candidates(&work_only).is_empty());
    }

    #[test]
    fn human_gait_tracks_retarget_by_joint_and_bone_name_onto_original_rigs() {
        let source = human_gait_asset();
        let target = human_gait_asset();
        let gait = retargeted_gait_from(&source, &target)
            .expect("both walk and run tracks map to the original skeleton");
        for clip in [&gait.walk, &gait.run] {
            let bind = target
                .pose_tracks_at_time(&clip.tracks, clip.clip.seconds(0.), Mat4::IDENTITY)
                .unwrap();
            let moving = target
                .pose_tracks_at_time(&clip.tracks, clip.clip.seconds(0.25), Mat4::IDENTITY)
                .unwrap();
            assert_ne!(walk_pose_signature(&bind), walk_pose_signature(&moving));
        }
        // Verbatim transfer copies the authored keys onto every rig, so
        // the mapped keys are the authored keys exactly.
        let finger = human_gait_joint_index(&target, "Bip01 L Finger0");
        let authored = source.tracks[0]
            .as_ref()
            .unwrap()
            .tracks
            .iter()
            .find(|track| track.id == finger as i32)
            .unwrap()
            .rotation
            .keys
            .clone();
        let retargeted = gait
            .walk
            .tracks
            .iter()
            .find(|track| track.id == finger as i32)
            .expect("an identical rig maps the thumb track")
            .rotation
            .keys
            .clone();
        for (authored, retargeted) in authored.iter().zip(retargeted.iter()) {
            let angle = glam::Quat::from_array(*authored)
                .normalize()
                .angle_between(glam::Quat::from_array(*retargeted).normalize());
            assert!(
                angle < 1.0e-4,
                "verbatim transfer drifted by {angle} radians"
            );
        }

        let mut incompatible = human_gait_asset();
        incompatible.skeleton.bones[1].name = "different pelvis".into();
        assert!(retargeted_gait_from(&source, &incompatible).is_none());
    }

    #[test]
    fn a_divergent_rest_frame_plays_the_donors_pose() {
        // The thumb chains of the resident/player rigs point their rest
        // frames a long way apart (measured 61-85 degrees). The donor's
        // authored local rotations are played verbatim, so a divergent
        // thumb stays in the clip with the exact authored keys instead of
        // being bind-kept; the composed pose matches the donor.
        let source = human_gait_asset();
        let target = human_gait_asset_with(Some(("Bip01 L Finger0", 80.0_f32)), false);
        let gait = retargeted_gait_from(&source, &target)
            .expect("the locomotion bones still map completely");
        let thumb = human_gait_joint_index(&target, "Bip01 L Finger0");
        let source_thumb = human_gait_joint_index(&source, "Bip01 L Finger0");
        let source_track = source.tracks[0]
            .as_ref()
            .expect("fixture track set")
            .tracks
            .iter()
            .find(|track| track.id == source_thumb as i32)
            .expect("the thumb has a donor track");
        for clip in [&gait.walk, &gait.run] {
            let mapped = clip
                .tracks
                .iter()
                .find(|track| track.id == thumb as i32)
                .expect("a divergent thumb stays in the clip");
            for (mapped_key, authored_key) in mapped
                .rotation
                .keys
                .iter()
                .zip(source_track.rotation.keys.iter())
            {
                let angle = glam::Quat::from_array(*mapped_key)
                    .normalize()
                    .angle_between(glam::Quat::from_array(*authored_key).normalize());
                assert!(
                    angle < 1.0e-6,
                    "verbatim transfer rewrote a key: drifted by {angle} radians"
                );
            }
        }
    }

    #[test]
    fn donor_helper_tracks_without_a_target_joint_do_not_block_the_clip() {
        // Player bodies carry attachment and finger-chain anim-joint tracks
        // that resident rigs never had; they must be ignored rather than
        // counted as coverage failures.
        let source = human_gait_asset_with_extra_helper();
        let target = human_gait_asset();
        let gait = retargeted_gait_from(&source, &target)
            .expect("donor-only helper tracks are skipped, not fatal");
        assert_eq!(
            gait.walk.tracks.len(),
            target.skeleton.joints.len(),
            "every donor helper track is skipped; the resident rig maps fully"
        );
    }

    #[test]
    fn walk_scan_uses_a_later_moving_clip_when_the_first_named_match_is_static() {
        use crate::skeleton::{Bone, Joint};

        let mut asset = rig_without_combined_actions();
        asset.skeleton.fps = 30;
        asset.skeleton.bones.push(Bone {
            name: "root".into(),
            flags: 0,
            parent: -1,
            first_joint: 0,
            relative: Mat4::IDENTITY.to_cols_array(),
            initial: Mat4::IDENTITY.to_cols_array(),
            children: Vec::new(),
        });
        asset.skeleton.joints.push(Joint {
            class: 15,
            name: "root joint".into(),
            parent: -1,
            child: 0,
            sibling: -1,
        });
        asset.skins[0].data.bones.push("root".into());
        asset.model.actions = vec![
            idle_action("walk_static", 0., 30.),
            idle_action("行走_空手_1", 0., 30.),
        ];
        asset.tracks = vec![
            Some(synthetic_walk_track([[0., 0., 0.]; 3])),
            Some(synthetic_walk_track([
                [0., 0., 0.],
                [0., 1., 0.],
                [0., 0., 0.],
            ])),
        ];

        let selected = walk_clock(&asset).expect("the second action visibly moves the rig");
        assert_eq!(selected.action, 1);
        assert!(walk_animation_moves_meshes(&asset, &selected));
    }

    #[test]
    fn live_world_falls_back_to_route_motion_when_no_gait_retargets() {
        use crate::skeleton::{Bone, Joint};

        let mut asset = rig_without_combined_actions();
        asset.skeleton.fps = 30;
        asset.skeleton.bones.push(Bone {
            name: "root".into(),
            flags: 0,
            parent: -1,
            first_joint: 0,
            relative: Mat4::IDENTITY.to_cols_array(),
            initial: Mat4::IDENTITY.to_cols_array(),
            children: Vec::new(),
        });
        asset.skeleton.joints.push(Joint {
            class: 15,
            name: "root joint".into(),
            parent: -1,
            child: 0,
            sibling: -1,
        });
        asset.skins[0].data.bones.push("root".into());
        asset.model.actions.push(idle_action("行走", 0., 30.));
        asset.tracks = vec![Some(synthetic_walk_track([
            [0., 0., 0.],
            [0., 1., 0.],
            [0., 0., 0.],
        ]))];

        let mut live = Live {
            spawn: 0,
            position: Vec3::new(4., 0., 7.),
            yaw: 0.,
            asset: std::rc::Rc::new(asset),
            action: Some(0),
            playback: None,
            clip: Some(Clip {
                start_ms: 0,
                length_ms: 1_000,
            }),
            clock: 0.5,
            gait: None,
            gait_clock: 0.,
            live_world_moving: true,
            live_world_running: false,
            head: 1.8,
            on_floor: false,
            pose_error: None,
            motion: MotionProbe::default(),
            name_visible: true,
            floor_stamp: 0,
            nudged: false,
            wedged: false,
            visible: true,
            pose_index: None,
            pose_stamp: None,
            uploaded_stamp: None,
            follow: None,
            live_world: true,
            live_world_zone: Some(0),
            route_attempt: 0,
            route_retry_seconds: 0.,
            draw: None,
            failure: None,
        };
        let world = live.world();
        let bind = live.asset.pose_bind(world).unwrap();
        let animated = live.asset.pose_action_at_time(0, 0.5, world).unwrap();
        assert_ne!(walk_pose_signature(&bind), walk_pose_signature(&animated));
        assert_eq!(
            walk_pose_signature(&live.pose().unwrap()),
            walk_pose_signature(&bind),
            "a generated resident follows routes without evaluating a walk pose"
        );
    }

    #[test]
    fn default_spawn_distance_is_the_clients_own_scene_window() {
        let settings = Settings::default();
        assert_eq!(settings.spawn_distance, 420.);
        assert!(settings.idle_animation);
        assert!(settings.show_names);
        assert!(settings.stand_on_objects);
        assert_eq!(settings.name_distance, NATIVE_NAME_DISTANCE);
        assert_eq!(settings.sanitized(), settings);
    }

    #[test]
    fn settings_are_clamped_to_their_supported_range() {
        let low = Settings {
            spawn_distance: -5.,
            max_visible: 0,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(low.spawn_distance, MIN_SPAWN_DISTANCE);
        assert_eq!(low.max_visible, 1);
        let high = Settings {
            spawn_distance: f32::INFINITY,
            max_visible: 10_000,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(high.spawn_distance, NATIVE_SPAWN_DISTANCE);
        assert_eq!(high.max_visible, MAX_VISIBLE_LIMIT);
    }

    /// A stand-in for the Walker's building hulls: one floor at a fixed height.
    struct OneFloor(Option<f32>, u64, bool);

    impl FloorSource for OneFloor {
        fn lowest_floor(&self, _feet: Vec3, _extents: Vec3, _max_up: f32) -> Option<f32> {
            self.0
        }

        fn floor_stamp(&self) -> u64 {
            self.1
        }

        fn sight_blocked(&self, _from: Vec3, _to: Vec3) -> bool {
            self.2
        }
    }

    fn flat_map() -> MapSpace {
        MapSpace {
            width: 32,
            height: 32,
            spacing: 2.,
            heights: vec![0.; 32 * 32].into(),
        }
    }

    struct WaterSurface(Option<f32>);

    impl FloorSource for WaterSurface {
        fn lowest_floor(&self, _: Vec3, _: Vec3, _: f32) -> Option<f32> {
            None
        }
        fn floor_stamp(&self) -> u64 {
            1
        }
        fn sight_blocked(&self, _: Vec3, _: Vec3) -> bool {
            false
        }
        fn water_height(&self, _: f32, _: f32) -> Option<f32> {
            self.0
        }
    }

    #[test]
    fn live_world_ground_and_projected_edges_reject_submerged_points() {
        let map = flat_map();
        let dry = WaterSurface(None);
        let submerged = WaterSurface(Some(3.0));
        assert!(live_world_ground_point(0., 0., &map, &dry).is_some());
        assert!(live_world_ground_point(0., 0., &map, &submerged).is_none());
        assert!(!ambient_path_safe(
            Vec3::new(-2., 0., 0.),
            Vec3::new(2., 0., 0.),
            &map,
            &WaterSurface(Some(3.0)),
            &[],
            None,
        ));
    }

    /// A building whose wall stands at `free_from`: everything west of that line
    /// is solid. `rise` lifts the floor on the free side, so a sweep that would
    /// also climb a storey can be told apart from one that just steps sideways.
    struct Wall {
        free_from: f32,
        rise: f32,
        stamp: u64,
    }

    impl Wall {
        fn at(free_from: f32, rise: f32) -> Self {
            Self {
                free_from,
                rise,
                stamp: 1,
            }
        }
    }

    impl FloorSource for Wall {
        fn lowest_floor(&self, feet: Vec3, _extents: Vec3, max_up: f32) -> Option<f32> {
            let height = feet.y
                + if feet.x >= self.free_from {
                    self.rise
                } else {
                    0.
                };
            (height - feet.y <= max_up).then_some(height)
        }

        fn floor_stamp(&self) -> u64 {
            self.stamp
        }

        fn sight_blocked(&self, _from: Vec3, _to: Vec3) -> bool {
            false
        }

        fn solid_at(&self, feet: Vec3, extents: Vec3) -> bool {
            feet.x - extents.x < self.free_from
        }
    }

    /// A floor one unit up that is solid, over free ground: a wall cap.
    struct WallCap;

    impl FloorSource for WallCap {
        fn lowest_floor(&self, feet: Vec3, _extents: Vec3, _max_up: f32) -> Option<f32> {
            Some(feet.y + 1.)
        }

        fn floor_stamp(&self) -> u64 {
            1
        }

        fn sight_blocked(&self, _from: Vec3, _to: Vec3) -> bool {
            false
        }

        fn solid_at(&self, feet: Vec3, _extents: Vec3) -> bool {
            // Only the lifted plate is solid; the ground under it is not.
            feet.y > 0.5
        }
    }

    #[test]
    fn a_placement_lifted_into_a_wall_cap_drops_to_the_ground_under_it() {
        let map = flat_map();
        let mut spawn = spawn(None);
        spawn.anchor = [0.9, 0.];
        let placement = resolve_placement(&spawn, &map, Some(&WallCap), true);
        assert!(placement.nudged, "{placement:?}");
        assert!(!placement.on_floor);
        assert_eq!(placement.position, Vec3::new(0.9, 0., 0.));
    }

    #[test]
    fn a_bridge_deck_is_never_traded_for_the_ground_below_it() {
        // Same solid plate, but ten units up: out of reach, so the placement
        // keeps the deck instead of dropping into the river bed.
        struct Deck;
        impl FloorSource for Deck {
            fn lowest_floor(&self, feet: Vec3, _extents: Vec3, _max_up: f32) -> Option<f32> {
                Some(feet.y + 10.)
            }
            fn floor_stamp(&self) -> u64 {
                1
            }
            fn sight_blocked(&self, _from: Vec3, _to: Vec3) -> bool {
                false
            }
            fn solid_at(&self, feet: Vec3, _extents: Vec3) -> bool {
                feet.y > 0.5
            }
        }
        let map = flat_map();
        let mut spawn = spawn(None);
        spawn.anchor = [0.9, 0.];
        let placement = resolve_placement(&spawn, &map, Some(&Deck), true);
        // Rings cannot reach a free spot at 10 units either, and the ground is
        // out of the lift guard's range, so the deck placement is reported.
        assert_eq!(placement.position.y, 10.);
        assert!(placement.wedged, "{placement:?}");
    }

    #[test]
    fn a_placement_inside_a_wall_is_moved_to_the_free_side() {
        let map = flat_map();
        let mut spawn = spawn(None);
        spawn.anchor = [0.9, 0.];
        let wall = Wall::at(1.0, 0.);
        let placement = resolve_placement(&spawn, &map, Some(&wall), true);
        assert!(placement.nudged, "{placement:?}");
        assert!(!placement.wedged);
        // Out of the wall, not away from it: the nearest free ring is 0.5 units.
        assert!(
            placement.position.x >= 1.0 + ACTOR_EXTENTS.x - 1e-3,
            "{placement:?}"
        );
        assert!(placement.position.x <= 2.0, "{placement:?}");
        assert_eq!(placement.position.y, 0.);
        // The authored column is untouched, so the sweep is repeatable.
        assert_eq!(spawn.anchor, [0.9, 0.]);
    }

    #[test]
    fn a_placement_with_nowhere_to_go_is_reported_instead_of_moved() {
        let map = flat_map();
        let mut spawn = spawn(None);
        spawn.anchor = [0.9, 0.];
        let solid = Wall::at(f32::INFINITY, 0.);
        let placement = resolve_placement(&spawn, &map, Some(&solid), true);
        assert!(placement.wedged);
        assert!(!placement.nudged);
        assert_eq!(placement.position, Vec3::new(0.9, 0., 0.));
    }

    #[test]
    fn a_nudge_that_would_climb_a_storey_is_refused() {
        let map = flat_map();
        let mut spawn = spawn(None);
        spawn.anchor = [0.9, 0.];
        // Free ground exists, five units up: the actor stays where it was
        // reported instead of being lifted onto the wall's top.
        let steep = Wall::at(1.0, WALL_RING_LIFT + 3.);
        let placement = resolve_placement(&spawn, &map, Some(&steep), true);
        assert!(placement.wedged, "{placement:?}");
        assert_eq!(placement.position.y, 0.);
    }

    #[test]
    fn a_nudge_keeps_the_floor_the_free_spot_stands_on() {
        let map = flat_map();
        let mut spawn = spawn(None);
        spawn.anchor = [0.9, 0.];
        let step = Wall::at(1.0, 1.);
        let placement = resolve_placement(&spawn, &map, Some(&step), true);
        assert!(placement.nudged, "{placement:?}");
        assert_eq!(placement.position.y, 1.);
        assert!(placement.on_floor);
    }

    #[test]
    fn a_nudge_never_changes_an_authored_height() {
        let map = flat_map();
        let mut spawn = spawn(Some(120.));
        spawn.anchor = [0.9, 0.];
        let wall = Wall::at(1.0, 5.);
        let placement = resolve_placement(&spawn, &map, Some(&wall), true);
        assert!(placement.nudged, "{placement:?}");
        assert_eq!(placement.position.y, 120.);
    }

    #[test]
    fn a_source_that_cannot_answer_keeps_every_placement_where_it_was_authored() {
        let map = flat_map();
        let mut spawn = spawn(None);
        spawn.anchor = [0.9, 0.];
        // `OneFloor` uses the trait's default `solid_at`: nothing to sweep with.
        let silent = OneFloor(None, 1, false);
        let placement = resolve_placement(&spawn, &map, Some(&silent), true);
        assert!(!placement.nudged && !placement.wedged);
        assert_eq!(placement.position, Vec3::new(0.9, 0., 0.));
    }

    /// `w = -z`, so a point with a positive `z` is behind a camera that looks
    /// down its own negative `z`.
    fn camera() -> Mat4 {
        let mut matrix = Mat4::IDENTITY;
        matrix.z_axis.w = -1.;
        matrix.w_axis.w = 0.;
        matrix
    }

    #[test]
    fn a_distant_actor_is_re_skinned_less_often_but_never_starved() {
        assert_eq!(pose_stride(0.), 1);
        assert_eq!(pose_stride(POSE_STRIDE_NEAR), 1);
        assert_eq!(pose_stride(POSE_STRIDE_NEAR + 1.), 2);
        assert_eq!(pose_stride(POSE_STRIDE_FAR), 2);
        assert_eq!(pose_stride(POSE_STRIDE_FAR + 1.), 3);
        // Every actor is posed on its own phase, and each one is posed within
        // `stride` frames: the crowd is spread, not stalled.
        for stride in 1..=3u64 {
            for spawn in 0..8usize {
                let posed: Vec<u64> = (0..12).filter(|&f| pose_due(f, spawn, stride)).collect();
                assert!(!posed.is_empty(), "stride {stride} spawn {spawn}");
                let worst = posed.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0);
                assert!(worst <= stride, "stride {stride} spawn {spawn}: {posed:?}");
            }
        }
        // A still camera keeps two neighbours from sharing a re-skin frame.
        assert_ne!(
            (0..3).position(|f| pose_due(f, 0, 3)),
            (0..3).position(|f| pose_due(f, 1, 3))
        );
    }

    #[test]
    fn an_actor_ahead_of_the_camera_is_posed_and_one_behind_it_is_not() {
        let camera = camera();
        assert!(on_screen(camera, Vec3::new(0., 0., -20.), 1.9));
        // Either the feet or the head on screen is enough.
        assert!(on_screen(camera, Vec3::new(4., 0., -20.), 1.9));
        // Behind the eye: the host would not submit it in this pass.
        assert!(!on_screen(camera, Vec3::new(0., 0., 20.), 1.9));
        // In front but well outside the viewport, including the margin.
        assert!(!on_screen(camera, Vec3::new(40., 0., -20.), 1.9));
        assert!(!on_screen(camera, Vec3::new(0., 40., -20.), 1.9));
    }

    #[test]
    fn authored_height_wins_and_a_real_floor_beats_the_terrain() {
        let none = OneFloor(None, 1, false);
        let bridge = OneFloor(Some(146.), 1, false);
        // An authored height is the game's own value, never the terrain.
        assert_eq!(
            stand_height(&spawn(Some(276.)), 120., Some(&bridge), true),
            276.
        );
        // "0" means the server's height: trust a real floor above the terrain.
        assert_eq!(stand_height(&spawn(None), 120., Some(&bridge), true), 146.);
        // No floor in the column, or the option off, keeps the terrain.
        assert_eq!(stand_height(&spawn(None), 120., Some(&none), true), 120.);
        assert_eq!(stand_height(&spawn(None), 120., Some(&bridge), false), 120.);
        assert_eq!(stand_height(&spawn(None), 120., None, true), 120.);
    }

    #[test]
    fn an_idle_clip_wraps_inside_its_own_authored_range() {
        // Half a second into the clip, one second long: the sampled time must
        // stay inside 500..1500 ms however long the actor stands there, because
        // the sampler clamps past the last key instead of wrapping.
        let clip = Clip {
            start_ms: 500,
            length_ms: 1_000,
        };
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(close(clip.seconds(0.), 0.5));
        assert!(close(clip.seconds(0.25), 0.75));
        assert!(close(clip.seconds(1.0), 0.5));
        assert!(close(clip.seconds(1.5), 1.0));
        assert!(close(clip.seconds(-1.), 0.5));
        // Long-running actors wrap instead of freezing on the last key.
        assert!(close(clip.seconds(3_601.25), 0.75));
    }

    /// A rig with no scene actions at all, the case that used to be mistaken for
    /// a scheduled combined action and froze the actor in its bind pose.
    fn rig_without_combined_actions() -> ModelAsset {
        let (asset, _) = crate::model_draw_fixture::fixture();
        asset
    }

    fn idle_action(name: &str, start: f32, end: f32) -> crate::ecmodel::Action {
        crate::ecmodel::Action {
            name: name.into(),
            start,
            end,
            fps: Some(30),
            track_file: String::new(),
            joints: Vec::new(),
        }
    }

    /// The full locomotion rig the synthetic donor/target tests share. Bone
    /// names are the client's biped conventions, split clavicles and
    /// three-spine layout included, so the real core-coverage gate applies.
    const HUMAN_GAIT_RIG: [(&str, i32); 25] = [
        ("Bip01", -1),
        ("Bip01 Pelvis", 0),
        ("Bip01 Spine", 1),
        ("Bip01 Spine1", 2),
        ("Bip01 Spine2", 3),
        ("Bip01 Neck", 4),
        ("Bip01 Head", 5),
        ("Bip01 L Clavicle", 4),
        ("Bip01 L UpperArm", 7),
        ("Bip01 L Forearm", 8),
        ("Bip01 L Hand", 9),
        ("Bip01 R Clavicle", 4),
        ("Bip01 R UpperArm", 11),
        ("Bip01 R Forearm", 12),
        ("Bip01 R Hand", 13),
        ("Bip01 L Thigh", 1),
        ("Bip01 L Calf", 15),
        ("Bip01 L Foot", 16),
        ("Bip01 L Toe0", 17),
        ("Bip01 R Thigh", 1),
        ("Bip01 R Calf", 19),
        ("Bip01 R Foot", 20),
        ("Bip01 R Toe0", 21),
        ("Bip01 L Finger0", 10),
        ("Bip01 R Finger0", 14),
    ];

    fn human_gait_asset() -> ModelAsset {
        human_gait_asset_with(None, false)
    }

    /// `divergent` rotates one bone's rest frame by the given degrees about
    /// the same axis, modelling the resident/player rest-frame gap of the
    /// thumb chains; `extra_helper` adds a donor-only anim-joint track that
    /// no resident rig has, like the player bodies' `BoneNN_AnimJoint` set.
    fn human_gait_asset_with(divergent: Option<(&str, f32)>, extra_helper: bool) -> ModelAsset {
        use crate::skeleton::{Bone, Joint};
        let mut asset = rig_without_combined_actions();
        asset.skeleton.fps = 30;
        for (index, (name, parent)) in HUMAN_GAIT_RIG.iter().enumerate() {
            let divergence = match divergent {
                Some((divergent_name, degrees)) if divergent_name == *name => degrees.to_radians(),
                _ => 0.0,
            };
            let relative = (Mat4::from_rotation_z(0.1 * index as f32 + divergence)
                * Mat4::from_translation(Vec3::new(0., -0.35, 0.)))
            .to_cols_array();
            asset.skeleton.bones.push(Bone {
                name: (*name).into(),
                flags: 0,
                parent: *parent,
                first_joint: index as i32,
                relative,
                initial: relative,
                children: Vec::new(),
            });
            asset.skeleton.joints.push(Joint {
                class: 15,
                name: (*name).into(),
                parent: *parent,
                child: index as i32,
                sibling: -1,
            });
            asset.skins[0].data.bones.push((*name).into());
        }
        asset.model.actions = vec![idle_action("walk", 0., 30.), idle_action("run", 0., 30.)];
        if extra_helper {
            // A donor-only attachment joint (like the player bodies'
            // `BoneNN_AnimJoint` set): real in the source rig, absent from
            // the target.
            let helper = HUMAN_GAIT_RIG.len();
            let relative = Mat4::IDENTITY;
            asset.skeleton.bones.push(Bone {
                name: "Bone01".into(),
                flags: 0,
                parent: -1,
                first_joint: helper as i32,
                relative: relative.to_cols_array(),
                initial: relative.to_cols_array(),
                children: Vec::new(),
            });
            asset.skeleton.joints.push(Joint {
                class: 15,
                name: "Bone01_AnimJoint".into(),
                parent: -1,
                child: helper as i32,
                sibling: -1,
            });
            let mut track_set = synthetic_full_gait_track();
            let mut tracks = Vec::with_capacity(track_set.tracks.len() + 1);
            tracks.extend_from_slice(&track_set.tracks);
            tracks.push(crate::animation::BoneTrack {
                id: helper as i32,
                position: crate::animation::KeyTrack {
                    fps: 30,
                    keys: vec![[0., 0., 0.]; 3],
                    frames: vec![0., 15., 30.],
                    length_ms: 1000,
                    compression: 1,
                },
                rotation: crate::animation::KeyTrack {
                    fps: 30,
                    keys: vec![[0., 0., 0., 1.]; 3],
                    frames: vec![0., 15., 30.],
                    length_ms: 1000,
                    compression: 1,
                },
            });
            track_set = std::sync::Arc::new(crate::animation::TrackSet {
                tracks,
                ..(*track_set).clone()
            });
            asset.tracks = vec![Some(track_set.clone()), Some(track_set)];
        } else {
            let track_set = synthetic_full_gait_track();
            asset.tracks = vec![Some(track_set.clone()), Some(track_set)];
        }
        asset
    }

    fn human_gait_asset_with_extra_helper() -> ModelAsset {
        human_gait_asset_with(None, true)
    }

    fn human_gait_joint_index(asset: &ModelAsset, name: &str) -> usize {
        asset
            .skeleton
            .joints
            .iter()
            .position(|joint| joint.name == name)
            .expect("fixture joint exists")
    }

    fn synthetic_full_gait_track() -> std::sync::Arc<crate::animation::TrackSet> {
        use crate::animation::{BoneTrack, KeyTrack, TrackSet};
        let frames = vec![0., 15., 30.];
        let swing = glam::Quat::from_rotation_z(0.6).to_array();
        let tracks = (0..HUMAN_GAIT_RIG.len())
            .map(|index| BoneTrack {
                id: index as i32,
                position: KeyTrack {
                    fps: 30,
                    keys: vec![[0., 0., 0.]; 3],
                    frames: frames.clone(),
                    length_ms: 1000,
                    compression: 1,
                },
                rotation: KeyTrack {
                    fps: 30,
                    keys: vec![[0., 0., 0., 1.], swing, [0., 0., 0., 1.]],
                    frames: frames.clone(),
                    length_ms: 1000,
                    compression: 1,
                },
            })
            .collect();
        std::sync::Arc::new(TrackSet {
            version: 2,
            start: 0,
            end: 30,
            fps: 30,
            tracks,
        })
    }

    fn synthetic_walk_track(
        positions: [[f32; 3]; 3],
    ) -> std::sync::Arc<crate::animation::TrackSet> {
        use crate::animation::{BoneTrack, KeyTrack, TrackSet};
        let frames = vec![0., 15., 30.];
        std::sync::Arc::new(TrackSet {
            version: 2,
            start: 0,
            end: 30,
            fps: 30,
            tracks: vec![BoneTrack {
                id: 0,
                position: KeyTrack {
                    fps: 30,
                    keys: positions.to_vec(),
                    frames: frames.clone(),
                    length_ms: 1000,
                    compression: 1,
                },
                rotation: KeyTrack {
                    fps: 30,
                    keys: vec![[0., 0., 0., 1.]; 3],
                    frames,
                    length_ms: 1000,
                    compression: 1,
                },
            }],
        })
    }

    #[test]
    fn a_rig_without_a_combined_idle_still_loops_its_own_idle_clip() {
        // No descriptor (combined) actions at all: the old code accepted an
        // inert `Playback` here and never looked for the rig's own clip.
        let mut asset = rig_without_combined_actions();
        asset.model.actions.push(idle_action("待机", 0., 30.));
        let mut report = IdleReport::default();
        let (action, clip, playback) = idle_clock(&asset, &mut report);
        assert!(playback.is_none(), "no combined action to schedule");
        assert_eq!(action, Some(0));
        let clip = clip.expect("the rig's own idle clip is used");
        assert_eq!(report.looped, 1);
        assert_eq!(report.combined, 0);
        assert_eq!(report.bind_pose, 0);
        // A 30-frame clip at 30 fps runs for a second and moves through it.
        assert!((clip.length_ms as f32 - 1_001.).abs() < 1.);
        assert!((clip.seconds(0.5) - clip.seconds(0.0) - 0.5).abs() < 1e-4);
    }

    fn combined(name: &str, base: &str) -> crate::ecmodel::CombinedAction {
        crate::ecmodel::CombinedAction {
            name: name.into(),
            loops: Some(1),
            speed: Some(1.),
            bases: vec![crate::ecmodel::BaseAction {
                name: base.into(),
                start_ms: Some(0),
                loop_min: Some(-1),
                loop_max: Some(-1),
            }],
        }
    }

    #[test]
    fn a_combined_scene_idle_goes_through_the_playback_clock() {
        let mut asset = rig_without_combined_actions();
        asset.model.actions.push(idle_action("base", 0., 30.));
        asset.skeleton.fps = 30;
        asset.descriptor.actions.push(combined("站立", "base"));
        let mut report = IdleReport::default();
        let (action, clip, playback) = idle_clock(&asset, &mut report);
        let mut playback = playback.expect("the combined action is scheduled");
        assert!(action.is_none() && clip.is_none());
        assert_eq!(report.combined, 1);
        let (_, first) = playback.sample().expect("the combined action samples");
        playback.tick(250).expect("the clock advances");
        let (_, later) = playback.sample().expect("still sampling");
        assert!(later > first, "{first} -> {later}");
    }

    /// The request is the standing action; the leisure gesture is only the
    /// client's fallback, so a rig that ships both must stand, not stretch.
    #[test]
    fn the_standing_scene_action_wins_over_the_leisure_gesture() {
        let mut asset = rig_without_combined_actions();
        asset.model.actions.push(idle_action("stand", 0., 30.));
        asset.model.actions.push(idle_action("stretch", 0., 30.));
        asset.skeleton.fps = 30;
        asset.descriptor.actions.push(combined("休闲", "stretch"));
        asset.descriptor.actions.push(combined("站立", "stand"));
        let mut report = IdleReport::default();
        let (_, _, playback) = idle_clock(&asset, &mut report);
        let playback = playback.expect("the standing action is scheduled");
        let (action, _) = playback.sample().expect("the standing clip samples");
        assert_eq!(asset.model.actions[action].name, "stand");
    }

    /// No `站立` anywhere: the client's own fallback to the literal `休闲`
    /// still finds the rig's leisure action instead of leaving it in bind pose.
    #[test]
    fn a_rig_without_a_standing_action_falls_back_to_the_leisure_one() {
        let mut asset = rig_without_combined_actions();
        asset.model.actions.push(idle_action("stretch", 0., 30.));
        asset.skeleton.fps = 30;
        asset.descriptor.actions.push(combined("休闲", "stretch"));
        let mut report = IdleReport::default();
        let (_, _, playback) = idle_clock(&asset, &mut report);
        let playback = playback.expect("the fallback action is scheduled");
        let (action, _) = playback.sample().expect("the fallback clip samples");
        assert_eq!(asset.model.actions[action].name, "stretch");
    }

    #[test]
    fn a_rig_with_no_idle_action_at_all_is_reported_as_a_bind_pose() {
        let asset = rig_without_combined_actions();
        let mut report = IdleReport::default();
        let (action, clip, playback) = idle_clock(&asset, &mut report);
        assert!(action.is_none() && clip.is_none() && playback.is_none());
        assert_eq!(report.bind_pose, 1);
        assert_eq!(report.looped, 0);
    }

    #[test]
    fn a_single_frame_idle_holds_the_files_own_pose_instead_of_the_bind_pose() {
        // A one-frame clip is a posed stand, not a missing animation: sample it
        // at its own single frame rather than falling back to the bind pose.
        let mut asset = rig_without_combined_actions();
        asset.model.actions.push(idle_action("站立", 7., 7.));
        let mut report = IdleReport::default();
        let (action, clip, _) = idle_clock(&asset, &mut report);
        assert_eq!(action, Some(0));
        let clip = clip.expect("a one-frame clip is still a clip");
        assert_eq!(clip.length_ms, 1);
        assert_eq!(report.looped, 1);
        assert_eq!(report.bind_pose, 0);
    }

    #[test]
    fn a_clip_whose_range_cannot_be_sampled_is_reported_apart_from_a_missing_one() {
        let mut asset = rig_without_combined_actions();
        asset.model.actions.push(idle_action("待机", 5., 2.));
        let mut report = IdleReport::default();
        let (action, clip, playback) = idle_clock(&asset, &mut report);
        assert!(action.is_none() && clip.is_none() && playback.is_none());
        assert_eq!(report.unusable_clip, 1);
        assert_eq!(report.bind_pose, 0);
    }

    #[test]
    fn a_pose_that_never_changes_reads_as_frozen() {
        let mut probe = MotionProbe::default();
        // Nothing observed yet: not proven frozen either.
        assert!(!probe.observed());
        probe.observe(7);
        assert!(probe.animating());
        for _ in 0..MOTION_WINDOW {
            probe.observe(7);
        }
        assert!(!probe.animating());
        // One changed pose is movement again.
        probe.observe(8);
        assert!(probe.animating());
    }

    fn posed(y: f32) -> Vec<PosedMesh> {
        vec![PosedMesh {
            skin: 0,
            mesh: 0,
            vertices: vec![crate::skin_pose::PosedVertex {
                position: [0., y, 0.],
                normal: [0., 1., 0.],
            }],
        }]
    }

    #[test]
    fn the_pose_signature_notices_a_moving_rig_and_ignores_sub_millimetre_noise() {
        assert_eq!(pose_signature(&posed(1.)), pose_signature(&posed(1.)));
        assert_ne!(pose_signature(&posed(1.)), pose_signature(&posed(1.5)));
        // A tenth of a millimetre is not a visible move.
        assert_eq!(pose_signature(&posed(1.)), pose_signature(&posed(1.0001)));
    }

    #[test]
    fn a_world_point_projects_to_its_own_screen_position() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800., 600.));
        // An identity matrix projects clip space straight to the screen, so the
        // view axis lands in the middle and a point above it lands on the top
        // edge: the mapping a label uses for the actor's head.
        let centre = project(Vec3::new(0., 0., 0.5), Mat4::IDENTITY, screen)
            .expect("view-axis point is visible");
        assert!((centre.x - 400.).abs() < 1e-3);
        assert!((centre.y - 300.).abs() < 1e-3);
        let above = project(Vec3::new(0., 1., 0.5), Mat4::IDENTITY, screen).expect("visible");
        assert!((above.y - 0.).abs() < 1e-3);
        let left = project(Vec3::new(-0.5, 0., 0.5), Mat4::IDENTITY, screen).expect("visible");
        assert!((left.x - 200.).abs() < 1e-3);
        // Off screen and behind the eye are not labels to draw.
        assert!(project(Vec3::new(1.5, 0., 0.5), Mat4::IDENTITY, screen).is_none());
        let behind = Mat4::from_cols_array_2d(&[
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., -1.],
            [0., 0., 0., 0.],
        ]);
        assert!(project(Vec3::new(0., 0., 1.), behind, screen).is_none());
    }

    #[test]
    fn missing_spawn_facing_preserves_model_forward_without_invented_variation() {
        let a = Spawn {
            id: 101,
            anchor: [12., -4.],
            ..spawn(None)
        };
        let b = Spawn {
            id: 102,
            anchor: [13., -4.],
            ..spawn(None)
        };
        assert_eq!(initial_yaw(&a), 0.0);
        assert_eq!(initial_yaw(&b), 0.0);
    }

    #[test]
    fn transform_maps_local_axes_into_the_world() {
        let at = Vec3::new(3., 4., 5.);
        let identity = transform(at, 0.);
        assert!((identity.transform_point3(Vec3::ZERO) - at).length() < 1e-5);
        // Local +Z is forward, so yaw 0 points at world -Z.
        let forward = identity.transform_vector3(Vec3::Z);
        assert!((forward - Vec3::NEG_Z).length() < 1e-5);
        let up = identity.transform_vector3(Vec3::Y);
        assert!((up - Vec3::Y).length() < 1e-5);
        // A quarter turn puts forward on world +X.
        let quarter = transform(at, std::f32::consts::FRAC_PI_2);
        let forward = quarter.transform_vector3(Vec3::Z);
        assert!((forward - Vec3::X).length() < 1e-5);
    }

    #[test]
    fn an_inert_module_reports_why_it_cannot_draw() {
        let mut populator = Populator::default();
        populator.set_active(true, Vec3::ZERO);
        populator.update(0.016, Vec3::ZERO, None);
        assert!(populator.live.is_empty());
        assert_eq!(populator.status(), "Waiting for a loaded world");
    }
}
