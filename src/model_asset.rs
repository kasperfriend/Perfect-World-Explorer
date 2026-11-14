//! Retained ordinary model geometry and animation data for a receiving-model
//! draw. No automatic ECM scheduling, textures/material draws or GFX playback
//! are implied by successfully loading this asset.
use crate::{animation, ecmodel, skeleton, skeleton_pose, skin, skin_pose};
use glam::Mat4;
use std::collections::{HashMap, HashSet};

pub struct SkinAsset {
    pub path: String,
    pub data: skin::Skin,
}
impl SkinAsset {
    /// Retail 0x794952–0x794add tries <ski-dir>/Tex_<ski-stem>/
    /// first, then <ski-dir>/Textures/. It retries on texture-load failure,
    /// not only when the preferred file is absent.
    pub fn texture_paths(&self, index: usize) -> Result<[String; 2], String> {
        let texture = self
            .data
            .textures
            .get(index)
            .ok_or("skin texture index out of range")?;
        let path = ecmodel::normalize(&self.path);
        let (parent, file) = path.rsplit_once('/').unwrap_or(("", &path));
        let stem = file.rsplit_once('.').map_or(file, |x| x.0);
        Ok([
            ecmodel::normalize(&format!("{parent}/tex_{stem}/{texture}")),
            ecmodel::normalize(&format!("{parent}/textures/{texture}")),
        ])
    }
    pub fn load_texture_with<T>(
        &self,
        index: usize,
        mut load: impl FnMut(&str) -> Result<T, String>,
    ) -> Result<T, String> {
        let paths = self.texture_paths(index)?;
        let mut failures = Vec::new();
        for path in paths {
            match load(&path) {
                Ok(t) => return Ok(t),
                Err(e) => failures.push(format!("{path}: {e}")),
            }
        }
        Err(failures.join("; "))
    }
}
/// Authentic native pixel shader attached to one retained mesh.
/// Ordinary model modulation remains in `mesh_tints`; these shaders have
/// separate constants and must not be collapsed into that single tint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaceShader {
    /// `Shaders/ps/facerender/facerender.txt` and `noserender.txt`: base ×
    /// face colour, then untinted vertex light/specular and base-alpha opacity.
    /// Ordinary material modulation is not equivalent: it also tints light and
    /// specular, visibly splitting the head from the body's skin tone.
    Face { color: u32 },
    /// `Shaders/ps/facerender/browrender.txt`: base × skin colour, highlight ×
    /// brow colour, highlight-alpha interpolation, then vertex light/specular.
    Brow {
        color: u32,
        skin_color: u32,
        highlight: String,
    },
    /// `Shaders/ps/facerender/eyerender.txt`: base × face colour and
    /// highlight × eye colour, interpolated by highlight alpha.
    Eye {
        color: u32,
        skin_color: u32,
        highlight: String,
    },
    /// `Shaders/ps/facerender/mouthrender.txt`: base × face colour and
    /// highlight × lip colour, highlight-alpha interpolation, then lipreflect
    /// gloss after vertex lighting. The mouth mesh includes surrounding skin;
    /// tinting that entire mesh with the lip colour causes a lower-face seam.
    Mouth {
        color: u32,
        skin_color: u32,
        highlight: String,
    },
    /// `Shaders/ps/facerender/moustacherender.txt`: the same two-colour
    /// operation as Brow, with moustache colour restricted to the highlight.
    Moustache {
        color: u32,
        skin_color: u32,
        highlight: String,
    },
    /// `Shaders/ps/facerender/eyeballrender.txt`: pupil-coloured base mixed
    /// with the highlight, vertex lighting, then the eyereflect gloss stage.
    EyeBall { color: u32, highlight: String },
    /// `Shaders/ps/bodyrender.txt`: texture alpha selects between the base
    /// colour and the base colour multiplied by `colorBody`. It is a colour
    /// mask, not opacity; the native shader writes vertex diffuse alpha.
    Body { color: u32 },
}

impl FaceShader {
    pub fn highlight(&self) -> Option<&str> {
        match self {
            Self::Brow { highlight, .. }
            | Self::Eye { highlight, .. }
            | Self::Mouth { highlight, .. }
            | Self::Moustache { highlight, .. }
            | Self::EyeBall { highlight, .. } => Some(highlight),
            Self::Face { .. } | Self::Body { .. } => None,
        }
    }

    pub fn gloss(&self) -> Option<&'static str> {
        match self {
            Self::Mouth { .. } => Some("textures/face/lipreflect.tga"),
            Self::EyeBall { .. } => Some("textures/face/eyereflect.tga"),
            _ => None,
        }
    }
}

pub struct ModelAsset {
    pub descriptor: ecmodel::EcModel,
    pub model: ecmodel::SkinModel,
    pub skeleton: skeleton::Skeleton,
    pub skins: Vec<SkinAsset>,
    /// Same order as model.actions. None denotes the older embedded BON data,
    /// not permission to silently substitute another action's external tracks.
    pub tracks: Vec<Option<std::sync::Arc<animation::TrackSet>>>,
    /// Rooted replacements keyed by this asset's isolated skin and texture
    /// slots. Face parts therefore never mutate a texture shared by another
    /// skin, body, or character.
    pub texture_overrides: HashMap<(usize, usize), String>,
    /// Per-mesh ARGB modulation used by ordinary native material/face paths.
    pub mesh_tints: HashMap<(usize, usize), u32>,
    /// Authentic dedicated native shaders (face parts and body) whose
    /// constants/textures cannot be represented by one diffuse texture and
    /// one mesh tint.
    pub face_shaders: HashMap<(usize, usize), FaceShader>,
    /// Native per-skin visibility, notably the body head hidden by CECFace.
    /// Hidden meshes remain fully posed and validated but are not submitted.
    pub hidden_meshes: HashSet<(usize, usize)>,
    /// Armor/fashion meshes whose native shader writes diffuse alpha directly.
    /// Their base-texture alpha is data (armor uses it for specular), not an
    /// opacity cutout. Ordinary body/face/hair/world meshes are absent and keep
    /// the usual texture-alpha times diffuse-alpha behavior.
    pub diffuse_alpha_meshes: HashSet<(usize, usize)>,
}
/// World matrix of a named skeleton hook for an evaluated pose (the native
/// combination order is documented on `ModelAsset::hook_matrix`).
pub fn hook_matrix(
    skeleton: &skeleton::Skeleton,
    pose: &skeleton_pose::Pose,
    name: &str,
) -> Result<Mat4, String> {
    let hook = skeleton
        .hooks
        .iter()
        .find(|h| h.name == name)
        .ok_or_else(|| format!("model has no hook named {name:?}"))?;
    let local = Mat4::from_cols_array(&hook.matrix);
    if !local.is_finite() {
        return Err(format!("hook {name:?} has a non-finite matrix"));
    }
    if hook.bone < 0 {
        return Ok(pose.model * local);
    }
    let bone = usize::try_from(hook.bone)
        .ok()
        .and_then(|i| pose.absolute.get(i))
        .ok_or_else(|| format!("hook {name:?} binds missing bone {}", hook.bone))?;
    Ok(*bone * local)
}
pub struct PosedMesh {
    pub skin: usize,
    pub mesh: usize,
    pub vertices: Vec<skin_pose::PosedVertex>,
}

pub fn has_nonfinite_weights(vertex: &skin::Vertex) -> bool {
    vertex.weights.iter().any(|weight| !weight.is_finite())
}

/// Triangles made non-finite by an authored bad weight cannot be rasterized by
/// the native client either. Return the unchanged-finite subset, or `None`
/// when no recovery is needed. The source mesh and its weights are never
/// rewritten, and no replacement bind pose/weights are invented.
pub fn indices_without_nonfinite_weights(mesh: &skin::Mesh) -> Option<Vec<u16>> {
    if !mesh.vertices.iter().any(has_nonfinite_weights) {
        return None;
    }
    let invalid = mesh
        .vertices
        .iter()
        .map(has_nonfinite_weights)
        .collect::<Vec<_>>();
    let mut indices = Vec::with_capacity(mesh.indices.len());
    for triangle in mesh.indices.chunks_exact(3) {
        if triangle.iter().all(|&index| !invalid[index as usize]) {
            indices.extend_from_slice(triangle);
        }
    }
    Some(indices)
}

pub fn nonfinite_weight_triangle_count(mesh: &skin::Mesh) -> usize {
    indices_without_nonfinite_weights(mesh)
        .map(|indices| (mesh.indices.len() - indices.len()) / 3)
        .unwrap_or(0)
}

/// Strip one trailing `amd` segment from the *directory* of a path.
///
/// The retail pack ships skin-less duplicate folders where only the leaf
/// directory carries the suffix: `models/npcs/npc/女npc21amd/女npc21.ski` does
/// not exist, while `models/npcs/npc/女npc21/女npc21.ski` does and the two
/// `.smd` files are otherwise identical. Matched case-insensitively and only as
/// a whole segment, so an ordinary name that merely ends in those letters is
/// left alone. Returns the input unchanged when there is nothing to strip.
fn without_variant_suffix(path: &str) -> String {
    let Some((dir, leaf)) = path.rsplit_once('/') else {
        return path.to_string();
    };
    let Some((head, folder)) = dir.rsplit_once('/') else {
        return path.to_string();
    };
    if folder.len() <= 3 || !folder.to_lowercase().ends_with("amd") {
        return path.to_string();
    }
    let base = &folder[..folder.len() - 3];
    format!("{head}/{base}/{leaf}")
}
fn relative(descriptor: &str, reference: &str) -> String {
    let reference = ecmodel::normalize(reference);
    if reference.starts_with("models/") {
        reference
    } else {
        let parent = descriptor.rsplit_once('/').map_or("", |p| p.0);
        ecmodel::normalize(&format!("{parent}/{reference}"))
    }
}
/// Track files are immutable and often shared by many authored ECM variants.
/// Retain one parsed copy per archive path for the world, not per descriptor.
#[derive(Default)]
pub struct TrackCache(std::collections::HashMap<String, std::sync::Arc<animation::TrackSet>>);

impl ModelAsset {
    /// The caller owns archive I/O and caching. Every read receives a normalized
    /// package path. Errors retain the actual dependency path, including GBK
    /// names, instead of replacing missing geometry with a placeholder.
    pub fn load_with(
        descriptor_path: &str,
        read: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        Self::load_impl(descriptor_path, read, true)
    }

    /// Load `LSF_UNIQUESKIN` geometry while preserving the native missing-SKI
    /// behavior. `A3DSkinModel::AddSkinFile` returns `false` (zero), not `-1`,
    /// when a skin cannot be read, so `A3DSkinModelMan`'s negative-result check
    /// continues without that skin. Return every skipped dependency as a
    /// warning; never substitute another skin or hide rig/action failures.
    pub fn load_unique_with(
        descriptor_path: &str,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<(Self, Vec<String>), String> {
        let mut warnings = Vec::new();
        let asset = Self::load_cached_impl(
            descriptor_path,
            &mut read,
            &mut TrackCache::default(),
            true,
            Some(&mut warnings),
        )?;
        Ok((asset, warnings))
    }

    /// Load an ECM/SMD/BON rig without reading its authored skin menu. This is
    /// the native `A3DSkinModel::LSF_NOSKIN` path used by `CECPlayer`: the
    /// caller installs its selected body/equipment/head skins afterwards.
    pub fn load_without_skins_with(
        descriptor_path: &str,
        read: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        Self::load_impl(descriptor_path, read, false)
    }

    fn load_impl(
        descriptor_path: &str,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
        load_skins: bool,
    ) -> Result<Self, String> {
        Self::load_cached_impl(
            descriptor_path,
            &mut read,
            &mut TrackCache::default(),
            load_skins,
            None,
        )
    }
    pub fn load_cached(
        descriptor_path: &str,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
        cache: &mut TrackCache,
    ) -> Result<Self, String> {
        Self::load_cached_impl(descriptor_path, &mut read, cache, true, None)
    }
    fn load_cached_impl(
        descriptor_path: &str,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
        cache: &mut TrackCache,
        load_skins: bool,
        mut missing_skins: Option<&mut Vec<String>>,
    ) -> Result<Self, String> {
        let descriptor_path = ecmodel::normalize(descriptor_path);
        let mut bytes = |p: &str| read(p).map_err(|e| format!("{p}: {e}"));
        let descriptor = ecmodel::parse_ecm(&bytes(&descriptor_path)?)
            .map_err(|e| format!("{descriptor_path}: {e}"))?;
        let mut model = ecmodel::parse_smd(&bytes(&descriptor.skin_model_path)?)
            .map_err(|e| format!("{}: {e}", descriptor.skin_model_path))?;
        let skeleton_path = relative(&descriptor.skin_model_path, &model.skeleton);
        let skeleton = skeleton::Skeleton::parse(&bytes(&skeleton_path)?)
            .map_err(|e| format!("{skeleton_path}: {e}"))?;
        let mut skins = Vec::new();
        let mut loaded_skin_paths = Vec::new();
        // The descriptor's own AddiSkinPath list is authoritative. The `.smd`
        // list is a menu of skins that exist for the model family; in
        // models.pck 228 of its 247 unresolvable entries are simply the naming
        // of a variant that the `.ecm` selects explicitly (`冬1.ecm` ->
        // `冬1.SKI`). Using the descriptor list first is what keeps those
        // models drawable instead of failing the whole load.
        //
        // `AddiSkinPath` values are stored *pack-root relative but without the
        // `models/` prefix*: `NPCs\\怪物\\31水生其它\\旋龟\\浮游锯齿.ski` is
        // `models/npcs/怪物/31水生其它/旋龟/浮游锯齿.ski` in the archive.
        // Prefixing `models/` resolves 1,455 of the 1,473 references; resolving
        // against the `.ecm`'s own directory instead resolves almost none,
        // because these values already carry their full directory.
        let smd_dir = descriptor
            .skin_model_path
            .rsplit_once('/')
            .map(|p| p.0.to_string());
        let references: Vec<(String, String)> = if !load_skins {
            Vec::new()
        } else if descriptor.additional_skins.is_empty() {
            model
                .skins
                .iter()
                .map(|r| (relative(&descriptor.skin_model_path, r), String::new()))
                .collect()
        } else {
            descriptor
                .additional_skins
                .iter()
                .map(|r| {
                    // Keep the directory the value already carries; only add the
                    // missing `models/` prefix.
                    let full = ecmodel::normalize(r);
                    let primary = if full.starts_with("models/") {
                        full.clone()
                    } else {
                        format!("models/{full}")
                    };
                    // Secondary: the bare leaf next to the `.smd`, which is how
                    // the remaining family variants are found.
                    let leaf = full.rsplit('/').next().unwrap_or(full.as_str());
                    let beside = smd_dir.as_ref().map(|d| format!("{d}/{leaf}"));
                    (primary, beside.unwrap_or_default())
                })
                .collect()
        };
        for (primary, smd_fallback) in &references {
            let smd_fallback = smd_fallback.clone();
            // Some authored model variants are skin-less siblings: an `amd`
            // folder carries its own .ecm/.smd/.bon but reuses the base
            // model's skin from the sibling folder without the suffix. In
            // models.pck that accounts for 247 of 2,228 .smd models. Strict
            // callers retry against the base folder and still fail loudly if
            // neither resolves. Native unique-skin callers skip those Explorer
            // recovery candidates and report the authored missing path instead.
            // `path` is reassigned to whichever candidate actually loaded, so
            // the stored asset names its real source rather than the first
            // guess.
            let mut path = primary.clone();
            let data = match bytes(&path) {
                Ok(b) => skin::Skin::parse(&b).map_err(|e| format!("{path}: {e}"))?,
                Err(first) => {
                    // Try the same name relative to the `.smd`, then the
                    // skin-less `amd` sibling folder, and only then give up.
                    let candidates = [smd_fallback.clone(), without_variant_suffix(&path)];
                    let mut parsed = None;
                    for candidate in candidates
                        .iter()
                        .filter(|c| missing_skins.is_none() && !c.is_empty() && **c != *primary)
                    {
                        // Go through `bytes`, not the raw `read`: `read` is
                        // already mutably borrowed by that wrapper, and it also
                        // adds the failing path to the error.
                        if let Ok(b) = bytes(candidate) {
                            parsed = Some(
                                skin::Skin::parse(&b).map_err(|e| format!("{candidate}: {e}"))?,
                            );
                            path = candidate.clone();
                            break;
                        }
                    }
                    match parsed {
                        Some(skin) => skin,
                        None => {
                            let error = format!("{path}: {first}");
                            if let Some(warnings) = missing_skins.as_deref_mut() {
                                warnings.push(error);
                                continue;
                            }
                            return Err(error);
                        }
                    }
                }
            };
            if missing_skins.is_some() {
                loaded_skin_paths.push(path.clone());
            }
            skins.push(SkinAsset { path, data });
        }
        if missing_skins.is_some() {
            // Keep retained skin indices and their authored path slots aligned
            // after native missing-SKI omissions.
            model.skins = loaded_skin_paths;
        }
        let mut tracks = Vec::new();
        for action in &model.actions {
            let set = if let Some(path) =
                model.action_track_path(&descriptor.skin_model_path, action)
            {
                let path = ecmodel::normalize(&path);
                let t = match cache.0.get(&path) {
                    Some(t) => t.clone(),
                    None => {
                        let t = std::sync::Arc::new(animation::TrackSet::parse(&bytes(&path)?)?);
                        cache.0.insert(path, t.clone());
                        t
                    }
                };
                // Each skeleton still validates its own binding, even on a hit.
                t.validate_joint_count(skeleton.joints.len())?;
                Some(t)
            } else {
                None
            };
            tracks.push(set);
        }
        // Full model loads retain strict combined-action validation. The
        // skinless player-rig path only needs the direct bind/action data:
        // authentic Wizard ECMs contain two `回城循环` references while their
        // SMD calls that action `回城循环_通用`. Do not let that combined-action
        // alias prevent the static body/head/equipment assembly from loading.
        if load_skins {
            for combined in &descriptor.actions {
                for base in &combined.bases {
                    if !model.actions.iter().any(|a| a.name == base.name) {
                        return Err(format!(
                            "{descriptor_path}: {} -> missing SMD action {}",
                            combined.name, base.name
                        ));
                    }
                }
            }
        }
        Ok(Self {
            descriptor,
            model,
            skeleton,
            skins,
            tracks,
            texture_overrides: HashMap::new(),
            mesh_tints: HashMap::new(),
            face_shaders: HashMap::new(),
            hidden_meshes: HashSet::new(),
            diffuse_alpha_meshes: HashSet::new(),
        })
    }

    /// Native static-skin constructor used by `CECFace`: bind one BON skeleton
    /// directly and attach SKI resources without inventing an ECM body part.
    /// Render defaults come from `EcModel::render_settings` (white, speed 1,
    /// SRCALPHA/INVSRCALPHA), and there are no implicit animation tracks.
    pub fn load_static_skins_with(
        skeleton_path: &str,
        skin_paths: &[String],
        read: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        Self::load_static_smd_with(
            "",
            ecmodel::SkinModel {
                version: 0,
                skeleton: skeleton_path.to_string(),
                skins: skin_paths.to_vec(),
                physique: String::new(),
                track_directory: String::new(),
                actions: Vec::new(),
                hangers: Vec::new(),
                properties: Vec::new(),
            },
            skeleton_path,
            skin_paths,
            read,
        )
    }

    /// The same static rig, keeping the `.SMD`'s action list and the bone tracks
    /// those actions name.
    ///
    /// `CECFace` never animates a face part, which is why the constructor above
    /// drops both. Element 160 does animate: `A3DGFXModel::UpdateModel` calls
    /// `pSkinModel->PlayActionByNameDC(m_strActName, m_nLoops, 0)` on its first
    /// tick, so the rig has to carry the clips for `pose_action_at_time`.
    ///
    /// A track file that is missing or unreadable, or one whose joint count does
    /// not match this skeleton, leaves that single action with no external
    /// tracks instead of refusing the model: the client's `BindTrackSet` failing
    /// makes `PlayActionByName` return false and play nothing, while the bind
    /// pose is still drawable.
    pub fn load_static_smd_with(
        smd_path: &str,
        model: ecmodel::SkinModel,
        skeleton_path: &str,
        skin_paths: &[String],
        mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        let smd_path = ecmodel::normalize(smd_path);
        let skeleton_path = ecmodel::normalize(skeleton_path);
        let mut bytes = |path: &str| read(path).map_err(|error| format!("{path}: {error}"));
        let skeleton = skeleton::Skeleton::parse(&bytes(&skeleton_path)?)
            .map_err(|error| format!("{skeleton_path}: {error}"))?;
        let mut skins = Vec::with_capacity(skin_paths.len());
        for reference in skin_paths {
            let path = ecmodel::normalize(reference);
            let data = skin::Skin::parse(&bytes(&path)?).map_err(|e| format!("{path}: {e}"))?;
            skins.push(SkinAsset { path, data });
        }
        // Track files are per action and usually shared with the model's other
        // descriptors, so one cache per rig keeps a repeated action cheap.
        let mut cache = TrackCache::default();
        let mut tracks = Vec::with_capacity(model.actions.len());
        for action in &model.actions {
            let mut set = None;
            if let Some(path) = model.action_track_path(&smd_path, action) {
                let path = ecmodel::normalize(&path);
                let parsed = match cache.0.get(&path).cloned() {
                    Some(tracks) => Some(tracks),
                    None => match bytes(&path) {
                        Ok(raw) => match animation::TrackSet::parse(&raw) {
                            Ok(tracks) => {
                                let tracks = std::sync::Arc::new(tracks);
                                cache.0.insert(path, tracks.clone());
                                Some(tracks)
                            }
                            Err(_) => None,
                        },
                        Err(_) => None,
                    },
                };
                if let Some(tracks) = parsed {
                    if tracks.validate_joint_count(skeleton.joints.len()).is_ok() {
                        set = Some(tracks);
                    }
                }
            }
            tracks.push(set);
        }
        Ok(Self {
            descriptor: ecmodel::EcModel {
                version: 0,
                skin_model_path: skeleton_path.clone(),
                original_color: None,
                source_blend: None,
                destination_blend: None,
                default_speed: None,
                actions: Vec::new(),
                events: Vec::new(),
                additional_skins: Vec::new(),
            },
            model,
            skeleton,
            skins,
            tracks,
            texture_overrides: HashMap::new(),
            mesh_tints: HashMap::new(),
            face_shaders: HashMap::new(),
            hidden_meshes: HashSet::new(),
            diffuse_alpha_meshes: HashSet::new(),
        })
    }

    pub fn texture_paths(&self, skin_index: usize, texture: usize) -> Result<[String; 2], String> {
        let skin = self
            .skins
            .get(skin_index)
            .ok_or("skin index out of range")?;
        if let Some(path) = self.texture_overrides.get(&(skin_index, texture)) {
            return Ok([path.clone(), path.clone()]);
        }
        skin.texture_paths(texture)
    }

    pub fn mesh_color(&self, skin: usize, mesh: usize, default: u32) -> u32 {
        self.mesh_tints
            .get(&(skin, mesh))
            .copied()
            .unwrap_or(default)
    }

    /// Absolute track time in seconds. Intentionally NOT an elapsed-time
    /// scheduler: action-loop duration, event timing, combined/direct namespace
    /// priority and random-loop selection must be supplied by verified playback.
    pub fn pose_action_at_time(
        &self,
        action: usize,
        seconds: f32,
        world: Mat4,
    ) -> Result<Vec<PosedMesh>, String> {
        let pose = self.action_pose(action, seconds, world)?;
        self.meshes_from_pose(&pose)
    }
    /// Pose this rig with a compatible clip borrowed from another skeleton.
    /// Callers must remap track joint IDs to this skeleton and validate the
    /// resulting pose before using it in a scene.
    pub fn pose_tracks_at_time(
        &self,
        tracks: &[animation::BoneTrack],
        seconds: f32,
        world: Mat4,
    ) -> Result<Vec<PosedMesh>, String> {
        let pose = skeleton_pose::evaluate(&self.skeleton, tracks, seconds, world)?;
        self.meshes_from_pose(&pose)
    }
    /// The evaluated pose itself, for consumers that need bone/hook matrices
    /// (an attached effect resolves its hook from this pose, not from the
    /// deformed vertices).
    pub fn action_pose(
        &self,
        action: usize,
        seconds: f32,
        world: Mat4,
    ) -> Result<skeleton_pose::Pose, String> {
        let set = self
            .tracks
            .get(action)
            .ok_or("model action index out of range")?;
        let tracks = match set {
            Some(t) => &t.tracks,
            None if !self.skeleton.embedded_tracks.is_empty() => &self.skeleton.embedded_tracks,
            None => return Err("action has neither external nor embedded bone tracks".into()),
        };
        skeleton_pose::evaluate(&self.skeleton, tracks, seconds, world)
    }
    /// No active action: use authored local bone transforms, not the first
    /// named animation. This is also useful for inspecting scene-play failures.
    pub fn pose_bind(&self, world: Mat4) -> Result<Vec<PosedMesh>, String> {
        let pose = self.bind_pose(world)?;
        self.meshes_from_pose(&pose)
    }
    pub fn bind_pose(&self, world: Mat4) -> Result<skeleton_pose::Pose, String> {
        skeleton_pose::evaluate(&self.skeleton, &[], 0., world)
    }
    /// World matrix of a named skeleton hook for an evaluated pose.
    ///
    /// 0x7b25e6 combines the authored hook matrix with the posed bone matrix for
    /// a bone-bound hook; 0x7b2543 combines it with the model matrix when the
    /// hook's bind index is negative. The client stores row-vector matrices, so
    /// its `hook * bone` product is `bone * hook` in this column-vector reader.
    /// A hook name the model does not own is an explicit error, never the model
    /// origin.
    pub fn hook_matrix(&self, pose: &skeleton_pose::Pose, name: &str) -> Result<Mat4, String> {
        hook_matrix(&self.skeleton, pose, name)
    }
    pub fn meshes_from_pose(&self, pose: &skeleton_pose::Pose) -> Result<Vec<PosedMesh>, String> {
        let mut meshes = Vec::new();
        for (skin, asset) in self.skins.iter().enumerate() {
            // Bind/validate all SKI names even for a rigid-only skin. Rigid
            // vertices below do not use this remapped weighted palette.
            let palette = skeleton_pose::palette(&asset.data, &self.skeleton, pose)?;
            for (mesh, data) in asset.data.meshes.iter().enumerate() {
                let rigid = data
                    .rigid_bone
                    .map(|bone| skeleton_pose::rigid_matrix(pose, bone))
                    .transpose()?;
                let mut vertices = Vec::with_capacity(data.vertices.len());
                for v in &data.vertices {
                    vertices.push(if let Some(matrix) = rigid {
                        skin_pose::deform_rigid(v.position, v.normal, matrix)?
                    } else if has_nonfinite_weights(v) {
                        // This vertex is not referenced by the retained draw
                        // indices. Keep a finite upload tombstone without
                        // altering its authentic source weights or inventing a
                        // replacement deformation for the discarded triangle.
                        skin_pose::PosedVertex {
                            position: [0.0; 3],
                            normal: [0.0; 3],
                        }
                    } else {
                        skin_pose::deform(
                            v.position,
                            v.normal,
                            [v.weights[0], v.weights[1], v.weights[2]],
                            v.joints,
                            &palette,
                        )?
                    });
                }
                meshes.push(PosedMesh {
                    skin,
                    mesh,
                    vertices,
                });
            }
        }
        Ok(meshes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn skin() -> SkinAsset {
        SkinAsset {
            path: "Models/A/test.extra.ski".into(),
            data: skin::Skin {
                version: 9,
                bones: Vec::new(),
                textures: vec!["IMAGE.DDS".into()],
                materials: Vec::new(),
                meshes: Vec::new(),
            },
        }
    }
    #[test]
    fn texture_order_matches_skin_specific_then_generic_directory() {
        let s = skin();
        assert_eq!(
            s.texture_paths(0).unwrap(),
            [
                "models/a/tex_test.extra/image.dds",
                "models/a/textures/image.dds"
            ]
        );
        let mut visits = Vec::new();
        let result = s
            .load_texture_with(0, |p| {
                visits.push(p.to_owned());
                Ok(7)
            })
            .unwrap();
        assert_eq!(result, 7);
        assert_eq!(visits.len(), 1);
        assert!(s.texture_paths(1).is_err());
    }
    #[test]
    fn texture_decoder_failure_retries_fallback_and_keeps_both_errors() {
        let s = skin();
        let mut visits = Vec::new();
        let result = s
            .load_texture_with(0, |p| {
                visits.push(p.to_owned());
                if p.contains("/textures/") {
                    Ok(9)
                } else {
                    Err("bad DDS".into())
                }
            })
            .unwrap();
        assert_eq!(result, 9);
        assert_eq!(visits.len(), 2);
        let error = s
            .load_texture_with::<()>(0, |_| Err("bad image".into()))
            .unwrap_err();
        assert!(error.contains("tex_test.extra/image.dds: bad image"));
        assert!(error.contains("textures/image.dds: bad image"));
    }
    #[test]
    fn package_references_keep_rooted_paths_and_resolve_local_names() {
        assert_eq!(
            relative("models/a/test.smd", "test.bon"),
            "models/a/test.bon"
        );
        assert_eq!(
            relative("models/a/test.smd", "Models\\other\\test.ski"),
            "models/other/test.ski"
        );
    }
    fn hook_file() -> skeleton::Skeleton {
        let mut s = skeleton::Skeleton {
            version: 6,
            start: 0,
            end: 0,
            fps: 0,
            bones: Vec::new(),
            joints: Vec::new(),
            hooks: Vec::new(),
            embedded_tracks: Vec::new(),
        };
        s.hooks.push(skeleton::Hook {
            name: "weapon".into(),
            kind: 0,
            bone: 0,
            matrix: Mat4::from_translation(glam::Vec3::new(1., 0., 0.)).to_cols_array(),
        });
        s.hooks.push(skeleton::Hook {
            name: "root".into(),
            kind: 0,
            bone: -1,
            matrix: Mat4::from_translation(glam::Vec3::new(2., 0., 0.)).to_cols_array(),
        });
        s
    }

    fn hook_pose() -> skeleton_pose::Pose {
        skeleton_pose::Pose {
            absolute: vec![
                Mat4::from_translation(glam::Vec3::new(0., 2., 0.)),
                Mat4::from_translation(glam::Vec3::new(0., 4., 0.)),
            ],
            skin: Vec::new(),
            model: Mat4::from_translation(glam::Vec3::new(5., 0., 0.)),
        }
    }

    #[test]
    fn hook_matrix_binds_the_posed_bone_and_composes_the_hook_record() {
        let s = hook_file();
        let pose = hook_pose();
        let world = hook_matrix(&s, &pose, "weapon").unwrap();
        let point = world.transform_point3(glam::Vec3::ZERO);
        assert!(
            (point - glam::Vec3::new(1., 2., 0.)).length() < 1e-6,
            "{point}"
        );
    }

    #[test]
    fn negative_bind_index_uses_the_model_matrix() {
        let s = hook_file();
        let pose = hook_pose();
        let world = hook_matrix(&s, &pose, "root").unwrap();
        let point = world.transform_point3(glam::Vec3::ZERO);
        assert!(
            (point - glam::Vec3::new(7., 0., 0.)).length() < 1e-6,
            "{point}"
        );
    }

    #[test]
    fn hook_world_follows_a_moving_bone_or_model_across_frames() {
        let s = hook_file();
        // Bone-bound hook: 0x7b25e6 composes bone * hook, so a bone that moves
        // by d moves the attached root by exactly d on the next evaluated pose.
        let mut pose = hook_pose();
        let first = hook_matrix(&s, &pose, "weapon")
            .unwrap()
            .transform_point3(glam::Vec3::ZERO);
        pose.absolute[0] = Mat4::from_translation(glam::Vec3::new(3., 2., 0.));
        let second = hook_matrix(&s, &pose, "weapon")
            .unwrap()
            .transform_point3(glam::Vec3::ZERO);
        assert!(
            (second - first - glam::Vec3::new(3., 0., 0.)).length() < 1e-6,
            "{first} -> {second}"
        );
        // Model-bound hook (negative bind index): 0x7b2543 composes
        // model * hook and applies the model matrix once, not per frame.
        let mut pose = hook_pose();
        let first = hook_matrix(&s, &pose, "root")
            .unwrap()
            .transform_point3(glam::Vec3::ZERO);
        pose.model = Mat4::from_translation(glam::Vec3::new(9., 0., 0.));
        let second = hook_matrix(&s, &pose, "root")
            .unwrap()
            .transform_point3(glam::Vec3::ZERO);
        assert!(
            (second - first - glam::Vec3::new(4., 0., 0.)).length() < 1e-6,
            "{first} -> {second}"
        );
        assert!((second - glam::Vec3::new(11., 0., 0.)).length() < 1e-6);
    }

    #[test]
    fn unknown_hook_and_missing_bone_are_explicit_errors() {
        let s = hook_file();
        let pose = hook_pose();
        let error = hook_matrix(&s, &pose, "shield").unwrap_err();
        assert!(error.contains("no hook named"), "{error}");
        let mut broken = hook_file();
        broken.hooks[0].bone = 9;
        let error = hook_matrix(&broken, &pose, "weapon").unwrap_err();
        assert!(error.contains("binds missing bone 9"), "{error}");
        let mut bad = hook_file();
        bad.hooks[0].matrix[0] = f32::NAN;
        let error = hook_matrix(&bad, &pose, "weapon").unwrap_err();
        assert!(error.contains("non-finite matrix"), "{error}");
    }

    fn put_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn put_string(bytes: &mut Vec<u8>, value: &str) {
        put_u32(bytes, value.len() as u32);
        bytes.extend_from_slice(value.as_bytes());
    }

    fn rig_files(skin_count: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let ecm = b"MOXTVersion: 40\r\nSkinModelPath: Models\\a.smd\r\n\
                    ComActCount: 1\r\nCombineActName: combined\r\n\
                    BaseActCount: 1\r\nBaseActName: missing\r\nEventCount: 0\r\n"
            .to_vec();
        let mut smd = b"MOXBDMSA".to_vec();
        for value in [8, skin_count, 0, 0, 0] {
            put_u32(&mut smd, value);
        }
        smd.resize(88, 0);
        put_string(&mut smd, "rig.bon");
        for _ in 0..skin_count {
            put_string(&mut smd, "unselected.ski");
        }
        put_string(&mut smd, "");
        put_string(&mut smd, "");

        let mut bon = b"MOXBEKSA".to_vec();
        for value in [6, 0, 0, 0, 0, 0, 0] {
            put_u32(&mut bon, value);
        }
        bon.resize(bon.len() + 64, 0);
        (ecm, smd, bon)
    }

    fn read_rig(path: &str, files: &(Vec<u8>, Vec<u8>, Vec<u8>)) -> Result<Vec<u8>, String> {
        match path {
            "models/a.ecm" => Ok(files.0.clone()),
            "models/a.smd" => Ok(files.1.clone()),
            "models/rig.bon" => Ok(files.2.clone()),
            _ => Err(format!("unexpected read {path}")),
        }
    }

    #[test]
    fn skinless_player_rig_ignores_smd_skin_menu_and_combined_aliases() {
        let files = rig_files(1);
        let mut reads = Vec::new();
        let asset = ModelAsset::load_without_skins_with("Models\\a.ecm", |path| {
            reads.push(path.to_owned());
            read_rig(path, &files)
        })
        .unwrap();
        assert!(asset.skins.is_empty());
        assert_eq!(asset.model.skins, ["unselected.ski"]);
        assert_eq!(reads, ["models/a.ecm", "models/a.smd", "models/rig.bon"]);
    }

    #[test]
    fn unique_skin_load_preserves_native_missing_ski_tolerance() {
        let mut files = rig_files(1);
        files.0 = b"MOXTVersion: 40\r\nSkinModelPath: Models\\a.smd\r\n\
                    ComActCount: 0\r\nEventCount: 0\r\n"
            .to_vec();
        let (asset, warnings) =
            ModelAsset::load_unique_with("Models\\a.ecm", |path| read_rig(path, &files)).unwrap();
        assert!(asset.skins.is_empty());
        assert!(asset.model.skins.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("models/unselected.ski"));

        // The ordinary world-model loader remains strict.
        assert!(ModelAsset::load_with("Models\\a.ecm", |path| read_rig(path, &files)).is_err());
    }

    #[test]
    fn ordinary_load_still_rejects_missing_combined_action_base() {
        let files = rig_files(0);
        let error = match ModelAsset::load_with("Models\\a.ecm", |path| read_rig(path, &files)) {
            Ok(_) => panic!("missing combined-action base was accepted"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            "models/a.ecm: combined -> missing SMD action missing"
        );
    }

    #[test]
    fn read_failure_preserves_normalized_dependency_name() {
        let result = ModelAsset::load_with("Models\\a.ecm", |_| Err("not present".into()));
        assert!(matches!(result,Err(ref e) if e=="models/a.ecm: not present"));
    }
}
