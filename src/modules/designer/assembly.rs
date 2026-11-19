//! Assemble a complete PW player from the same body, face, equipment, and
//! weapon resources used by CECPlayer/CECFace.
//!
//! Geometry is never substituted silently. Required body/face/item resources
//! fail with their actual path; optional face additions retain the base skin
//! and produce an explicit warning when their authentic resource is absent.

use std::collections::HashMap;

use glam::{Mat4, Vec3};

use super::{
    assets::Assets,
    catalog::{ArmorSlot, Catalog, Class, ClassSpeeds, FaceTexture, FlightMode, Gender},
    customize::CharacterDesign,
    face::{self, FaceConfig, FaceResources},
};
use crate::{
    ecmodel,
    model_asset::{self, FaceShader, ModelAsset, PosedMesh, SkinAsset},
    modules::walker::pose::{self, BoneControl, BoneScale, ScaleType},
    skeleton::Skeleton,
    skeleton_pose::Pose,
    skin,
};

#[derive(Debug)]
pub struct EquipmentPart {
    pub target: ArmorSlot,
    pub item_id: u32,
    pub equip_location: i32,
    pub path: String,
    pub visible: bool,
}

#[derive(Debug, Clone)]
pub struct FaceTextureStage {
    pub skin: usize,
    pub mesh: usize,
    pub base: String,
    /// Native stage 1/2 texture. Brow stages also populate the dedicated
    /// two-texture `FaceShader`; the remaining records preserve data needed by
    /// the other authentic face-shader implementations.
    pub high: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FaceSkinSlots {
    pub face: usize,
    pub hair: Option<usize>,
    pub goatee: Option<usize>,
    pub faling: Option<usize>,
}

pub struct WeaponModel {
    pub asset: ModelAsset,
    pub hook: &'static str,
    /// Native GetRight/LeftWeaponOwnHookPos. Models without CC_weapon bind
    /// directly to the hand; only a right-hand model may fall back to CC_qiu.
    pub child_hook: Option<&'static str>,
    /// Psychic soulspheres receive the player's gendered stand/run/wing
    /// actions. Their animated CC_qiu hook also participates in attachment.
    pub state_actions: bool,
}

/// Native `CECPlayer::GetRightWeaponHookPos`: Psychic soulspheres hang from
/// HH_qiu rather than the ordinary right-hand weapon hook. PROF_MONK is the
/// native Psychic profession and 25333 is its soulsphere major-type record.
fn weapon_parent_hook(
    class: Class,
    major_type: u32,
    right: bool,
    ordinary: &'static str,
) -> &'static str {
    if right && class == Class::Psychic && major_type == 25333 {
        "HH_qiu"
    } else {
        ordinary
    }
}

/// The armor-reflection shader writes diffuse alpha directly; texture alpha is
/// a specular input, not opacity.
fn apply_armor_reflect_shader(asset: &mut ModelAsset) {
    for (skin_index, skin) in asset.skins.iter().enumerate() {
        asset
            .diffuse_alpha_meshes
            .extend((0..skin.data.meshes.len()).map(|mesh| (skin_index, mesh)));
    }
}

/// `glb_ArmorReplaceShader_ReflectPrefix` replaces only textures whose file
/// title starts with `r_` (with `rw_` handled by a different shader). ChangeWing
/// uses that selective callback: an ordinary wing such as 老鹰羽翅.dds keeps its
/// original texture-alpha opacity instead of becoming an opaque armor card.
fn reflect_prefix_texture(path: &str) -> bool {
    ecmodel::normalize(path)
        .rsplit('/')
        .next()
        .is_some_and(|name| name.to_ascii_lowercase().starts_with("r_"))
}

fn apply_reflect_prefix_shader(asset: &mut ModelAsset) {
    for (skin_index, skin) in asset.skins.iter().enumerate() {
        for (mesh_index, mesh) in skin.data.meshes.iter().enumerate() {
            let matches = usize::try_from(mesh.texture)
                .ok()
                .and_then(|texture| skin.data.textures.get(texture))
                .is_some_and(|texture| reflect_prefix_texture(texture));
            if matches {
                asset.diffuse_alpha_meshes.insert((skin_index, mesh_index));
            }
        }
    }
}

/// CECPlayer::_SetWeaponMaterial additionally overrides every hand-weapon
/// material. ChangeWing does not perform this override on flight models.
fn apply_weapon_material(asset: &mut ModelAsset) {
    for skin in &mut asset.skins {
        for material in &mut skin.data.materials {
            material.specular = [0.0, 0.0, 0.0, 1.0];
            material.power = 15.0;
        }
    }
    apply_armor_reflect_shader(asset);
}

pub struct FlightModel {
    pub asset: ModelAsset,
    pub model_path: String,
    pub mode: FlightMode,
    pub parent_hook: &'static str,
    pub child_hook: &'static str,
    pub normal_speed: (f32, f32),
    pub rush_speed: Option<(f32, f32)>,
}

pub struct MountModel {
    pub asset: ModelAsset,
    pub speed: f32,
    pub speed_per_level: f32,
    pub sound_type: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TravelState {
    #[default]
    Ground,
    Mounted,
    Flying,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TravelAnimation {
    pub state: TravelState,
    pub mount: Option<(usize, f32)>,
    pub flight: Option<(usize, f32)>,
    pub weapon: Option<(usize, f32)>,
}

pub struct Character {
    pub design: CharacterDesign,
    pub body: ModelAsset,
    pub face: ModelAsset,
    pub weapons: Vec<WeaponModel>,
    pub flight: Option<FlightModel>,
    pub mount: Option<MountModel>,
    pub class_speeds: ClassSpeeds,
    pub equipment: Vec<EquipmentPart>,
    pub face_skins: FaceSkinSlots,
    pub face_texture_stages: Vec<FaceTextureStage>,
    pub warnings: Vec<String>,
    body_scales: Vec<BoneScale>,
    face_controls: Vec<BoneControl>,
    body_head: usize,
    face_neck: usize,
    original_neck: Mat4,
}

pub struct CharacterPose {
    pub body: Pose,
    pub face: Pose,
    pub weapons: Vec<Pose>,
    pub flight: Option<Pose>,
    pub mount: Option<Pose>,
    pub travel: TravelState,
}

pub struct CharacterMeshes {
    pub body: Vec<PosedMesh>,
    pub face: Vec<PosedMesh>,
    pub weapons: Vec<Vec<PosedMesh>>,
    pub flight: Vec<PosedMesh>,
    pub mount: Vec<PosedMesh>,
}

impl Character {
    pub fn assemble(design: CharacterDesign, assets: &mut Assets) -> Result<Self, String> {
        if !design.class.allows(design.gender) {
            return Err(format!(
                "{} does not support {} characters",
                design.class.name(),
                design.gender.name()
            ));
        }
        if !assets
            .body_variants(design.class, design.gender)
            .contains(&design.customize.body_id)
        {
            return Err(format!(
                "body ID {} does not have a complete authentic body/head/face set",
                design.customize.body_id
            ));
        }
        let mut warnings = Vec::new();
        let mut effective_design = design.clone();
        apply_head_hair_override(&mut effective_design, &design, &assets.catalog)?;

        let equipment = plan_equipment(&design, &assets.catalog)?;
        let body_path = design.class.body_model(design.gender);
        let mut body = ModelAsset::load_without_skins_with(&body_path, |path| assets.read(path))?;

        // SMD owns the skeleton/actions; CECPlayer replaces its skin slots.
        // Keep those native slots isolated rather than modifying shared skin
        // data or inventing a combined equipment skeleton.
        let body_skin_path = design
            .class
            .body_skin(design.gender, design.customize.body_id);
        let head_skin_path = design
            .class
            .head_skin(design.gender, design.customize.body_id);
        let head_skin = load_skin(assets, &head_skin_path)?;
        let head_mesh_count = head_skin.data.meshes.len();
        let mut body_skins = vec![load_skin(assets, &body_skin_path)?, head_skin];
        let mut body_skin_paths = vec![body_skin_path, head_skin_path];
        let mut diffuse_alpha_meshes = std::collections::HashSet::new();
        for part in &equipment {
            match load_skin(assets, &part.path) {
                Ok(skin) if part.visible => {
                    let skin_index = body_skins.len();
                    // Native armor/fashion pixel shaders output v0.a directly.
                    // In particular, armor's base alpha is a specular input,
                    // not opacity; using it in the alpha test cuts away most
                    // legacy starter equipment and exposes/seams the body.
                    diffuse_alpha_meshes.extend(
                        (0..skin.data.meshes.len()).map(|mesh| (skin_index, mesh)),
                    );
                    body_skin_paths.push(part.path.clone());
                    body_skins.push(skin);
                }
                Ok(_) => {}
                Err(error) if part.item_id != 0 => {
                    return Err(format!("equipment {}: {error}", part.item_id));
                }
                Err(error) => warnings.push(format!(
                    "default {} skin {} could not be loaded: {error}; leaving the underlying body visible",
                    part.target.name(),
                    part.path
                )),
            }
        }
        body.skins = body_skins;
        body.model.skins = body_skin_paths;
        body.texture_overrides = HashMap::new();
        body.mesh_tints = HashMap::new();
        body.hidden_meshes = (0..head_mesh_count).map(|mesh| (1, mesh)).collect();
        body.diffuse_alpha_meshes = diffuse_alpha_meshes;
        if let Some(base) = body.skins.first_mut() {
            // CECPlayer::LoadBodySkin's default body material override.
            for material in &mut base.data.materials {
                material.specular = [58. / 255., 58. / 255., 58. / 255., 1.];
                material.power = 10.;
            }
        }
        let body_color = if design.class == Class::Barbarian {
            // CECPlayer::SetBodyColor explicitly disables Orc skin tinting.
            u32::MAX
        } else {
            design.customize.color_body
        };
        if let Some(base) = body.skins.first() {
            for (mesh, data) in base.data.meshes.iter().enumerate() {
                if data.texture == 0 {
                    // CECPlayer::LoadBodySkin replaces texture 0 with the
                    // dedicated Body shader. Its texture alpha is the
                    // body-colour interpolation mask, not transparency.
                    body.face_shaders
                        .insert((0, mesh), FaceShader::Body { color: body_color });
                }
            }
        }

        let face_bon = design
            .class
            .face_asset(design.gender, design.customize.body_id, "bon");
        let face_ski = design
            .class
            .face_asset(design.gender, design.customize.body_id, "ski");
        let mut face_model = ModelAsset::load_static_skins_with(
            &face_bon,
            std::slice::from_ref(&face_ski),
            |path| assets.read(path),
        )?;
        let mut face_skins = FaceSkinSlots::default();
        let customized = &effective_design.customize.face;

        if customized.id_hair_model != 0 {
            match assets
                .catalog
                .hair_item(customized.id_hair_model as u32)
                .cloned()
            {
                Some(item) => {
                    if let Some(skin) = load_optional_face_skin(
                        assets,
                        &item.skin,
                        &format!("hair model {}", item.id),
                        &mut warnings,
                    ) {
                        let index = face_model.skins.len();
                        face_model.model.skins.push(item.skin.clone());
                        face_model.skins.push(skin);
                        face_skins.hair = Some(index);
                    }
                }
                None => warnings.push(format!(
                    "hair model ID {} is absent from elements.data; no hair skin was added",
                    customized.id_hair_model
                )),
            }
        }
        if design.gender == Gender::Male && customized.id_moustache_skin != 0 {
            match assets
                .catalog
                .moustache_item(customized.id_moustache_skin as u32)
                .cloned()
            {
                Some(item) => {
                    if let Some(skin) = load_optional_face_skin(
                        assets,
                        &item.path,
                        &format!("goatee skin {}", item.id),
                        &mut warnings,
                    ) {
                        let index = face_model.skins.len();
                        face_model.model.skins.push(item.path.clone());
                        face_model.skins.push(skin);
                        face_skins.goatee = Some(index);
                    }
                }
                None => warnings.push(format!(
                    "goatee skin ID {} is absent from elements.data; no goatee skin was added",
                    customized.id_moustache_skin
                )),
            }
        }
        if design.gender == Gender::Male
            && design.class != Class::Barbarian
            && customized.id_faling_skin != 0
        {
            match assets
                .catalog
                .faling_item(customized.id_faling_skin as u32)
                .cloned()
            {
                Some(item) => {
                    if let Some(skin) = load_optional_face_skin(
                        assets,
                        &item.path,
                        &format!("faling skin {}", item.id),
                        &mut warnings,
                    ) {
                        let index = face_model.skins.len();
                        face_model.model.skins.push(item.path.clone());
                        face_model.skins.push(skin);
                        face_skins.faling = Some(index);
                    }
                }
                None => warnings.push(format!(
                    "faling skin ID {} is absent from elements.data; no faling skin was added",
                    customized.id_faling_skin
                )),
            }
        }

        let mut texture_stages = Vec::new();
        apply_face_textures_and_colors(
            &effective_design,
            &face_skins,
            &mut face_model,
            assets,
            &mut texture_stages,
            &mut warnings,
        )?;

        let face_config = FaceConfig::load(assets)?;
        let face_resources = FaceResources::load(&effective_design, assets, &mut warnings);
        let rig = face::evaluate_face(
            &face_model.skeleton,
            face_config,
            &effective_design,
            &face_resources,
        )?;
        let face_controls = rig
            .controlled_bones()
            .map(|(bone, matrix)| BoneControl { bone, matrix })
            .collect::<Vec<_>>();

        let body_head = bone_index(&body.skeleton, "Bip01 Head")
            .ok_or("body skeleton has no Bip01 Head attachment bone")?;
        let face_neck = bone_index(&face_model.skeleton, "脖子对齐")
            .ok_or("face skeleton has no 脖子对齐 attachment bone")?;
        let original_neck = Mat4::from_cols_array(&face_model.skeleton.bones[face_neck].relative);
        if !original_neck.is_finite() || !original_neck.inverse().is_finite() {
            return Err("non-invertible/non-finite native neck attachment matrix".into());
        }
        let body_scales = body_scales(&body.skeleton, &design);

        let mut weapons = Vec::new();
        if design.equipment.weapon != 0 {
            let weapon = assets
                .catalog
                .weapon(design.equipment.weapon)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "weapon ID {} is absent from PW 1.4.2 elements.data",
                        design.equipment.weapon
                    )
                })?;
            for (path, ordinary_hook, right) in [
                (weapon.right_model, "HH_righthandweapon", true),
                (weapon.left_model, "HH_lefthandweapon", false),
            ] {
                // elements.data uses "0" for an absent hand model (for
                // example weapon 19's left hand). It is not an archive path.
                if path.is_empty() || path == "0" {
                    continue;
                }
                let mut asset = ModelAsset::load_with(&path, |path| assets.read(path))
                    .map_err(|error| format!("weapon {} ({path}): {error}", weapon.id))?;
                apply_weapon_material(&mut asset);
                let has_hook = |name| asset.skeleton.hooks.iter().any(|hook| hook.name == name);
                let child_hook = if has_hook("CC_weapon") {
                    Some("CC_weapon")
                } else if right && has_hook("CC_qiu") {
                    Some("CC_qiu")
                } else {
                    None
                };
                weapons.push(WeaponModel {
                    asset,
                    hook: weapon_parent_hook(design.class, weapon.subtype, right, ordinary_hook),
                    child_hook,
                    state_actions: right
                        && design.class == Class::Psychic
                        && weapon.subtype == 25333,
                });
            }
        }
        let class_speeds = assets
            .catalog
            .speeds(design.class)
            .ok_or_else(|| format!("missing native class speeds for {}", design.class.name()))?;
        let flight = if design.equipment.flight == 0 {
            None
        } else {
            let item = assets
                .catalog
                .flight(design.equipment.flight)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "flight item {} is absent from PW 1.4.2 elements.data",
                        design.equipment.flight
                    )
                })?;
            if !item.allows(design.class) {
                return Err(format!(
                    "flight item {} does not allow {}",
                    item.id,
                    design.class.name()
                ));
            }
            if item.model.is_empty() || item.model == "0" {
                return Err(format!("flight item {} has no model", item.id));
            }
            let (parent_hook, child_hook) = match item.mode {
                FlightMode::FlySword => ("HH_feijian", "CC_feijian"),
                FlightMode::Wing => ("HH_chibang", "CC_chibang"),
                FlightMode::DoubleWheel => {
                    return Err(format!(
                        "flight item {} uses double-wheel hangers, which have no v27 records",
                        item.id
                    ))
                }
            };
            // ChangeWing loads wings/fly-swords with LSF_UNIQUESKIN. Keep its
            // native missing-SKI tolerance visible as warnings, not a fatal
            // equipment error or an unrelated replacement model.
            let (mut asset, skin_warnings) =
                ModelAsset::load_unique_with(&item.model, |path| assets.read(path)).map_err(
                    |error| format!("flight item {} ({}): {error}", item.id, item.model),
                )?;
            for warning in skin_warnings {
                warnings.push(format!(
                    "flight item {}: {warning}; native unique-skin loading continued without that skin and installed no substitute",
                    item.id
                ));
            }
            if asset.skins.is_empty() {
                warnings.push(format!(
                    "flight item {} has no drawable skin; its rig and flight movement remain available",
                    item.id
                ));
            }
            apply_reflect_prefix_shader(&mut asset);
            Some(FlightModel {
                asset,
                model_path: item.model,
                mode: item.mode,
                parent_hook,
                child_hook,
                normal_speed: item.normal_speed,
                rush_speed: item.rush_speed,
            })
        };
        let mount = if design.equipment.mount == 0 {
            None
        } else {
            let item = assets
                .catalog
                .mount(design.equipment.mount)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "mount {} is absent from PW 1.4.2 Ride-pet records",
                        design.equipment.mount
                    )
                })?;
            if item.class_mask & design.class.bit() == 0 {
                return Err(format!(
                    "mount {} does not allow {}",
                    item.id,
                    design.class.name()
                ));
            }
            if !item.has_loadable_model() {
                return Err(format!("mount {} has no loadable model path", item.id));
            }
            // CECPlayer::LoadPetModel loads the rig with LSF_NOSKIN, then
            // attaches the SKI obtained by changing the ECM extension. It also
            // uses the armor-reflection replacement shader for the pet.
            let mut asset =
                ModelAsset::load_without_skins_with(&item.model, |path| assets.read(path))
                    .map_err(|error| format!("mount {} ({}): {error}", item.id, item.model))?;
            let skin_path = ecmodel::normalize(&item.model)
                .rsplit_once('.')
                .map(|(stem, _)| format!("{stem}.ski"))
                .ok_or_else(|| format!("mount {} has an invalid model path", item.id))?;
            asset.skins.push(
                load_skin(assets, &skin_path)
                    .map_err(|error| format!("mount {} skin ({skin_path}): {error}", item.id))?,
            );
            let mut invalid_vertices = 0;
            let mut discarded_triangles = 0;
            for skin in &asset.skins {
                for mesh in &skin.data.meshes {
                    invalid_vertices += mesh
                        .vertices
                        .iter()
                        .filter(|vertex| model_asset::has_nonfinite_weights(vertex))
                        .count();
                    discarded_triangles += model_asset::nonfinite_weight_triangle_count(mesh);
                }
            }
            if invalid_vertices != 0 {
                warnings.push(format!(
                    "mount {} ({}) has {invalid_vertices} non-finite weighted vertices in {skin_path}; source weights are preserved and {discarded_triangles} referenced triangles are not submitted",
                    item.id, item.name
                ));
            }
            apply_armor_reflect_shader(&mut asset);
            Some(MountModel {
                asset,
                speed: item.speed_a,
                speed_per_level: item.speed_b,
                sound_type: item.sound_type,
            })
        };
        if customized.id_third_eye != 0 {
            warnings.push(format!(
                "third-eye GFX ID {} is retained in the design but is not part of the static character assembly",
                customized.id_third_eye
            ));
        }

        Ok(Self {
            design,
            body,
            face: face_model,
            weapons,
            flight,
            mount,
            class_speeds,
            equipment,
            face_skins,
            face_texture_stages: texture_stages,
            warnings,
            body_scales,
            face_controls,
            body_head,
            face_neck,
            original_neck,
        })
    }

    /// Evaluate the authored bind pose, including body scales, face-bone
    /// controllers, head attachment, and weapon hooks.
    pub fn bind_pose(&self, world: Mat4) -> Result<CharacterPose, String> {
        self.sampled_pose(None, world, TravelAnimation::default())
    }

    /// Evaluate one direct SMD base action while preserving the customized
    /// body scales, face controllers, head attachment, and weapon hooks.
    pub fn action_pose(
        &self,
        action: usize,
        seconds: f32,
        world: Mat4,
    ) -> Result<CharacterPose, String> {
        self.sampled_pose(Some((action, seconds)), world, TravelAnimation::default())
    }

    pub fn sampled_pose(
        &self,
        sample: Option<(usize, f32)>,
        world: Mat4,
        travel: TravelAnimation,
    ) -> Result<CharacterPose, String> {
        let evaluate_body = |world| -> Result<Pose, String> {
            let (tracks, seconds): (&[crate::animation::BoneTrack], f32) = match sample {
                Some((action, seconds)) => {
                    let set = self
                        .body
                        .tracks
                        .get(action)
                        .ok_or("character action index out of range")?;
                    let tracks = match set {
                        Some(tracks) => &tracks.tracks,
                        None if !self.body.skeleton.embedded_tracks.is_empty() => {
                            &self.body.skeleton.embedded_tracks
                        }
                        None => {
                            return Err("character action has no external or embedded tracks".into())
                        }
                    };
                    (tracks, seconds)
                }
                None => (&[], 0.0),
            };
            pose::evaluate_controlled(
                &self.body.skeleton,
                tracks,
                seconds,
                world,
                &[],
                &self.body_scales,
            )
        };
        let mut mount_pose = None;
        let body_world = if travel.state == TravelState::Mounted {
            let model = self
                .mount
                .as_ref()
                .ok_or("mounted state requires an assembled Ride pet")?;
            let mount = evaluate_asset(&model.asset, travel.mount, world)?;
            // Native riding hangs the player on the pet, not the pet on the
            // player. Evaluate the rider's animated CC hook at identity first.
            let rider_reference = evaluate_body(Mat4::IDENTITY)?;
            let rider = model_asset::hook_matrix(&self.body.skeleton, &rider_reference, "CC_ride")?;
            let parent = model_asset::hook_matrix(&model.asset.skeleton, &mount, "HH_ride")?;
            mount_pose = Some(mount);
            hanger_world(parent, rider)?
        } else {
            world
        };
        let body = evaluate_body(body_world)?;
        // CECFace::Tick: InverseTM(originalNeck) * headAbsolute.
        // All matrices here are transposed native matrices, so reverse the
        // product. There is no body-bind/head-inverse term in the native code.
        let face_world = body.absolute[self.body_head] * self.original_neck.inverse();
        if !face_world.is_finite() {
            return Err("non-finite face attachment transform".into());
        }
        let face = pose::evaluate_controlled(
            &self.face.skeleton,
            &[],
            0.0,
            face_world,
            &self.face_controls,
            &[],
        )?;
        let mut weapon_poses = Vec::with_capacity(self.weapons.len());
        for weapon in &self.weapons {
            let parent = model_asset::hook_matrix(&self.body.skeleton, &body, weapon.hook)?;
            let sample = if weapon.state_actions {
                travel.weapon
            } else {
                None
            };
            let attached = if let Some(child_hook) = weapon.child_hook {
                // CECPlayer::AttachWeapon supplies both HH and the weapon's own
                // CC hook. A3DSkinModelHanger evaluates an animation-driven CC
                // at identity, then uses its inverse for the final attachment.
                // This is essential for HH_qiu: it is a body-root hook, while
                // the soulsphere's authored action supplies its hand-side path.
                let reference = evaluate_weapon(&weapon.asset, sample, Mat4::IDENTITY)?;
                let child =
                    model_asset::hook_matrix(&weapon.asset.skeleton, &reference, child_hook)?;
                hanger_world(parent, child)?
            } else {
                parent
            };
            weapon_poses.push(evaluate_weapon(&weapon.asset, sample, attached)?);
        }
        let flight_pose = if travel.state == TravelState::Flying {
            let model = self
                .flight
                .as_ref()
                .ok_or("flying state requires an assembled flight item")?;
            let reference = evaluate_asset(&model.asset, travel.flight, Mat4::IDENTITY)?;
            let child =
                model_asset::hook_matrix(&model.asset.skeleton, &reference, model.child_hook)?;
            let parent = model_asset::hook_matrix(&self.body.skeleton, &body, model.parent_hook)?;
            let attached = hanger_world(parent, child)?;
            Some(evaluate_asset(&model.asset, travel.flight, attached)?)
        } else {
            None
        };
        Ok(CharacterPose {
            body,
            face,
            weapons: weapon_poses,
            flight: flight_pose,
            mount: mount_pose,
            travel: travel.state,
        })
    }

    pub fn meshes_from_pose(&self, pose: &CharacterPose) -> Result<CharacterMeshes, String> {
        if pose.weapons.len() != self.weapons.len() {
            return Err("weapon pose count does not match the assembled character".into());
        }
        let body = self.body.meshes_from_pose(&pose.body)?;
        let face = self.face.meshes_from_pose(&pose.face)?;
        let weapons = if pose.travel == TravelState::Mounted {
            // CECPlayer hides hand weapons while riding.
            Vec::new()
        } else {
            self.weapons
                .iter()
                .zip(&pose.weapons)
                .map(|(weapon, pose)| weapon.asset.meshes_from_pose(pose))
                .collect::<Result<Vec<_>, _>>()?
        };
        let flight = match (&self.flight, &pose.flight) {
            (Some(model), Some(pose)) => model.asset.meshes_from_pose(pose)?,
            (None, None) | (Some(_), None) => Vec::new(),
            (None, Some(_)) => return Err("flight pose has no assembled flight model".into()),
        };
        let mount = match (&self.mount, &pose.mount) {
            (Some(model), Some(pose)) => model.asset.meshes_from_pose(pose)?,
            (None, None) | (Some(_), None) => Vec::new(),
            (None, Some(_)) => return Err("mount pose has no assembled Ride pet".into()),
        };
        Ok(CharacterMeshes {
            body,
            face,
            weapons,
            flight,
            mount,
        })
    }

    pub fn face_neck_bone(&self) -> usize {
        self.face_neck
    }
}

fn evaluate_asset(
    asset: &ModelAsset,
    sample: Option<(usize, f32)>,
    world: Mat4,
) -> Result<Pose, String> {
    let (tracks, seconds): (&[crate::animation::BoneTrack], f32) = match sample {
        Some((action, seconds)) => {
            let set = asset
                .tracks
                .get(action)
                .ok_or("travel model action index out of range")?;
            let tracks = match set {
                Some(tracks) => &tracks.tracks,
                None if !asset.skeleton.embedded_tracks.is_empty() => {
                    &asset.skeleton.embedded_tracks
                }
                None => return Err("travel model action has no external or embedded tracks".into()),
            };
            (tracks, seconds)
        }
        None => (&[], 0.0),
    };
    // Player travel rigs use the same fake/flipped and controller-only joints
    // as bodies and weapons. Non-animation joints retain their authored local
    // transforms instead of being misread as animation tracks.
    pose::evaluate(&asset.skeleton, tracks, seconds, world)
}

fn evaluate_weapon(
    asset: &ModelAsset,
    sample: Option<(usize, f32)>,
    world: Mat4,
) -> Result<Pose, String> {
    let (tracks, seconds): (&[crate::animation::BoneTrack], f32) = match sample {
        Some((action, seconds)) => {
            let set = asset
                .tracks
                .get(action)
                .ok_or("weapon action index out of range")?;
            let tracks = match set {
                Some(tracks) => &tracks.tracks,
                None if !asset.skeleton.embedded_tracks.is_empty() => {
                    &asset.skeleton.embedded_tracks
                }
                None => return Err("weapon action has no external or embedded tracks".into()),
            };
            (tracks, seconds)
        }
        None => (&[], 0.0),
    };
    pose::evaluate_controlled(&asset.skeleton, tracks, seconds, world, &[], &[])
}

/// Column-transform equivalent of the native row-vector hanger update
/// `inverse(CC) * offset * parent_HH`, with the identity offset used by these
/// player/pet models. The (possibly animated) child CC pose is always evaluated
/// at identity first.
fn hanger_world(parent: Mat4, child: Mat4) -> Result<Mat4, String> {
    if !parent.is_finite() || !child.is_finite() {
        return Err("non-finite native hanger input".into());
    }
    let world = parent * child.inverse();
    if !world.is_finite() {
        return Err("non-invertible/non-finite native hanger transform".into());
    }
    Ok(world)
}

fn bone_index(skeleton: &Skeleton, name: &str) -> Option<usize> {
    skeleton.bones.iter().position(|bone| bone.name == name)
}

fn load_skin(assets: &mut Assets, path: &str) -> Result<SkinAsset, String> {
    let path = ecmodel::normalize(path);
    let bytes = assets.read(&path)?;
    let data = skin::Skin::parse(&bytes).map_err(|error| format!("{path}: {error}"))?;
    Ok(SkinAsset { path, data })
}

fn load_optional_face_skin(
    assets: &mut Assets,
    path: &str,
    label: &str,
    warnings: &mut Vec<String>,
) -> Option<SkinAsset> {
    if path.is_empty() {
        warnings.push(format!("{label} has an empty SKI path; no skin was added"));
        return None;
    }
    match load_skin(assets, path) {
        Ok(skin) => Some(skin),
        Err(error) => {
            warnings.push(format!(
                "{label} could not be loaded: {error}; no skin was added"
            ));
            None
        }
    }
}

fn apply_head_hair_override(
    effective: &mut CharacterDesign,
    original: &CharacterDesign,
    catalog: &Catalog,
) -> Result<(), String> {
    // Orc UpdateHairModel returns before reading helmet hair fields.
    if original.class == Class::Barbarian {
        return Ok(());
    }
    let (id, fashion) = if original.equipment.fashion_mode {
        (original.equipment.fashion_head, true)
    } else {
        (original.equipment.head, false)
    };
    if id == 0 {
        return Ok(());
    }
    if fashion {
        let item = catalog
            .fashion_item(id)
            .ok_or_else(|| format!("fashion head ID {id} is absent from elements.data"))?;
        effective.customize.face.id_hair_model = item.id_hair as u16;
        effective.customize.face.id_hair_tex = item.id_hair_texture as u16;
    } else {
        let item = catalog
            .armor_item(id)
            .ok_or_else(|| format!("armor head ID {id} is absent from elements.data"))?;
        if item.id_hair != 0 {
            effective.customize.face.id_hair_model = item.id_hair as u16;
        }
        if item.id_hair_texture != 0 {
            effective.customize.face.id_hair_tex = item.id_hair_texture as u16;
        }
    }
    Ok(())
}

fn target_index(slot: ArmorSlot) -> usize {
    match slot {
        ArmorSlot::Upper => 0,
        ArmorSlot::Wrist => 1,
        ArmorSlot::Lower => 2,
        ArmorSlot::Foot => 3,
        ArmorSlot::Head => unreachable!(),
    }
}

fn location_target(location: i32) -> Option<ArmorSlot> {
    match location {
        1 | 5 | 6 | 8 => Some(ArmorSlot::Upper),
        2 => Some(ArmorSlot::Wrist),
        3 | 7 => Some(ArmorSlot::Lower),
        4 => Some(ArmorSlot::Foot),
        _ => None,
    }
}

fn plan_equipment(
    design: &CharacterDesign,
    catalog: &Catalog,
) -> Result<Vec<EquipmentPart>, String> {
    let fashion = design.equipment.fashion_mode;
    let mut parts: [Option<EquipmentPart>; 4] = std::array::from_fn(|_| None);
    let mut show_mask = 0_u32;
    for slot in [
        ArmorSlot::Upper,
        ArmorSlot::Wrist,
        ArmorSlot::Lower,
        ArmorSlot::Foot,
    ] {
        let id = if fashion {
            design.equipment.fashion(slot)
        } else {
            design.equipment.armor(slot)
        };
        let basic_location = target_index(slot) as i32 + 1;
        if id == 0 {
            // Only explicitly marked starter/reset slots retain CECPlayer's
            // class/gender default-SKI fallback. An ordinary Designer zero is
            // empty and exposes the base body, including in fashion mode.
            if !fashion && design.equipment.native_default_armor(slot) {
                show_mask |= 1 << basic_location;
                if let Some(path) = design.class.default_armor_skin(design.gender, slot) {
                    parts[target_index(slot)] = Some(EquipmentPart {
                        target: slot,
                        item_id: 0,
                        equip_location: basic_location,
                        path,
                        visible: true,
                    });
                }
            }
            continue;
        }

        let (realname, location) = if fashion {
            let item = catalog
                .fashion_item(id)
                .ok_or_else(|| format!("fashion item ID {id} is absent from elements.data"))?;
            (item.realname.clone(), item.equip_location)
        } else {
            let item = catalog
                .armor_item(id)
                .ok_or_else(|| format!("armor item ID {id} is absent from elements.data"))?;
            (item.realname.clone(), item.equip_location)
        };
        let target = location_target(location).ok_or_else(|| {
            format!("item {id} has non-body equip_location {location} in a body slot")
        })?;
        show_mask |= 1 << location;
        parts[target_index(target)] = Some(EquipmentPart {
            target,
            item_id: id,
            equip_location: location,
            // LoadPlayerSkin's close/LOD0 suffix is always 一级. Essence
            // item_level is not an LOD selector.
            path: design.class.equipment_skin(design.gender, &realname, 0),
            visible: true,
        });
    }

    let visible = equipment_visibility(show_mask);
    for (index, part) in parts.iter_mut().enumerate() {
        if let Some(part) = part {
            part.visible = visible[index];
        }
    }
    Ok(parts.into_iter().flatten().collect())
}

/// `_get_skin_show_mask` for one armor or fashion set. Array order is upper,
/// wrist, lower, foot. Composite outfits load their covered default skins but
/// hide those slots, and LowerAndFoot also hides the separately loaded boots.
fn equipment_visibility(show_mask: u32) -> [bool; 4] {
    let mut visible = [true; 4];
    let mut lower_method = 0_i32;
    for location in 1..=8 {
        if show_mask & (1 << location) == 0 {
            continue;
        }
        match location {
            1 => {
                visible[1] = true;
                visible[2] = true;
                visible[3] = lower_method != 7;
            }
            2 => {}
            3 => {
                lower_method = 3;
                visible[3] = true;
            }
            4 => {}
            5 => {
                visible[1] = true;
                visible[2] = false;
                visible[3] = true;
            }
            6 => {
                visible[1] = false;
                visible[2] = true;
                visible[3] = lower_method != 7;
            }
            7 => {
                lower_method = 7;
                visible[3] = false;
            }
            8 => {
                visible[1] = false;
                visible[2] = false;
                visible[3] = true;
            }
            _ => unreachable!(),
        }
    }
    visible
}

fn body_scales(skeleton: &Skeleton, design: &CharacterDesign) -> Vec<BoneScale> {
    fn factor(value: u8) -> f32 {
        1.0 + (value as f32 - 128.0) / 128.0 * 0.99
    }
    let data = &design.customize;
    let head = factor(data.head_scale);
    let up = factor(data.up_scale);
    let waist = factor(data.waist_scale);
    let arm = factor(data.arm_width);
    let leg = factor(data.leg_width);
    let breast = factor(data.breast_scale);
    let mut result = Vec::new();
    let mut add = |name: &str, kind: ScaleType, factor: Vec3| {
        if let Some(bone) = bone_index(skeleton, name) {
            result.push(BoneScale { bone, kind, factor });
        }
    };
    add(
        "Bip01 Neck",
        ScaleType::LocalNoOffset,
        Vec3::new(1.0, head, head),
    );
    add("Bip01 Head", ScaleType::Whole, Vec3::splat(head));
    if design.gender == Gender::Female {
        add("Bone01", ScaleType::Local, Vec3::splat(breast));
    }
    for name in ["Bip01 L Clavicle", "Bip01 R Clavicle"] {
        add(name, ScaleType::Local, Vec3::splat(up));
    }
    for name in ["Bip01 Spine2", "Bip01 Spine1"] {
        add(name, ScaleType::Local, Vec3::new(1.0, up, up));
    }
    add(
        "Bip01 Spine",
        ScaleType::LocalNoOffset,
        Vec3::new(1.0, waist, waist),
    );
    add(
        "Bip01 Pelvis",
        ScaleType::Local,
        Vec3::new(1.0, waist, waist),
    );
    for name in [
        "Bip01 L UpperArm",
        "Bip01 R UpperArm",
        "Bip01 L Forearm",
        "Bip01 R Forearm",
    ] {
        add(name, ScaleType::Local, Vec3::new(1.0, arm, arm));
    }
    for name in [
        "Bip01 L Thigh",
        "Bip01 R Thigh",
        "Bip01 L Calf",
        "Bip01 R Calf",
    ] {
        add(name, ScaleType::Local, Vec3::new(1.0, leg, leg));
    }
    result
}

#[derive(Clone)]
struct TextureChoice {
    base: String,
    high: Option<String>,
}

fn texture_choice(
    assets: &Assets,
    id: u16,
    label: &str,
    warnings: &mut Vec<String>,
) -> Option<FaceTexture> {
    if id == 0 {
        return None;
    }
    match assets.catalog.face_texture(id as u32).cloned() {
        Some(texture) => Some(texture),
        None => {
            warnings.push(format!(
                "{label} texture ID {id} is absent from elements.data; retaining the SKI texture"
            ));
            None
        }
    }
}

fn choice_base(texture: FaceTexture) -> TextureChoice {
    TextureChoice {
        base: texture.base,
        high: None,
    }
}

fn choice_base_high(texture: FaceTexture) -> TextureChoice {
    TextureChoice {
        base: texture.base,
        high: (!texture.high.is_empty()).then_some(texture.high),
    }
}

fn native_face_texture(class: Class, path: &str) -> String {
    let path = ecmodel::normalize(path);
    // CECFace::ChangeFaceTexture selects the `rw_` face texture for Monk and
    // Ghost (and for later classes not present in this PW 1.4.2 catalog).
    if !matches!(class, Class::Psychic | Class::Assassin) {
        return path;
    }
    match path.rsplit_once('/') {
        Some((directory, file)) => format!("{directory}/rw_{file}"),
        None => format!("rw_{path}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_face_textures_and_colors(
    design: &CharacterDesign,
    slots: &FaceSkinSlots,
    model: &mut ModelAsset,
    assets: &Assets,
    stages: &mut Vec<FaceTextureStage>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let data = &design.customize.face;
    let face_choice = texture_choice(assets, data.id_face_tex, "face", warnings)
        .map(choice_base)
        .and_then(|mut choice| {
            // Orc ChangeFaceTexture returns before inspecting or loading the
            // path. Keep any absent-ID warning above, but do not invent a
            // missing-file warning for a texture the native client never uses.
            if design.class == Class::Barbarian {
                return None;
            }
            choice.base = native_face_texture(design.class, &choice.base);
            Some(choice)
        });
    let eye_base = texture_choice(assets, data.id_eye_base_tex, "eye base", warnings);
    let eye_high = texture_choice(assets, data.id_eye_high_tex, "eye highlight", warnings);
    let eye_choice = match (eye_base, eye_high) {
        (Some(base), Some(high)) if data.id_eye_base_tex != 0 && data.id_eye_high_tex != 0 => {
            Some(TextureChoice {
                base: base.base,
                high: (!high.high.is_empty()).then_some(high.high),
            })
        }
        _ => None,
    };
    let brow_choice =
        texture_choice(assets, data.id_brow_tex, "brow", warnings).map(choice_base_high);
    let nose_choice = texture_choice(assets, data.id_nose_tex, "nose", warnings).map(choice_base);
    let mouth_choice =
        texture_choice(assets, data.id_mouth_tex, "mouth", warnings).map(choice_base_high);
    let eyeball_choice =
        texture_choice(assets, data.id_eye_ball_tex, "eyeball", warnings).map(choice_base_high);
    let moustache_choice = if design.gender == Gender::Male {
        texture_choice(assets, data.id_moustache_tex, "moustache", warnings).map(choice_base_high)
    } else {
        None
    };
    let moustache_color = if design.gender == Gender::Male {
        data.color_moustache
    } else {
        u32::MAX
    };
    for (part, color, choice) in [
        (0, data.color_face, face_choice),
        (1, data.color_eye, eye_choice),
        (2, data.color_brow, brow_choice),
        (3, data.color_mouth, mouth_choice),
        (4, data.color_face, nose_choice),
        (5, data.color_eye_ball, eyeball_choice),
        (6, moustache_color, moustache_choice),
    ] {
        apply_face_part(
            model,
            slots.face,
            part,
            color,
            data.color_face,
            choice,
            assets,
            stages,
            warnings,
        )?;
    }

    let hair_choice =
        texture_choice(assets, data.id_hair_tex, "hair", warnings).map(choice_base_high);
    if let Some(skin) = slots.hair {
        apply_extra_texture(
            model,
            skin,
            data.color_hair,
            hair_choice,
            true,
            assets,
            stages,
            warnings,
        )?;
    }
    if let Some(skin) = slots.goatee {
        let choice =
            texture_choice(assets, data.id_goatee_tex, "goatee", warnings).map(choice_base_high);
        apply_extra_texture(
            model,
            skin,
            data.color_moustache,
            choice,
            true,
            assets,
            stages,
            warnings,
        )?;
    }
    if let Some(skin) = slots.faling {
        let choice =
            texture_choice(assets, data.id_faling_tex, "faling", warnings).map(choice_base);
        apply_extra_texture(
            model,
            skin,
            u32::MAX,
            choice,
            false,
            assets,
            stages,
            warnings,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_face_part(
    model: &mut ModelAsset,
    skin: usize,
    part: usize,
    color: u32,
    skin_color: u32,
    choice: Option<TextureChoice>,
    assets: &Assets,
    stages: &mut Vec<FaceTextureStage>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let name = format!("model人脸_{part}");
    let Some((mesh, texture)) =
        model.skins[skin]
            .data
            .meshes
            .iter()
            .enumerate()
            .find_map(|(index, mesh)| {
                mesh.name
                    .eq_ignore_ascii_case(&name)
                    .then_some((index, mesh.texture))
            })
    else {
        // CECFace::SetPartShader treats an absent part as an unavailable
        // shader, not a load failure. Several authentic rigs (for example
        // 巫师男标准脸1.ski) contain only the base face and eyeball meshes.
        return Ok(());
    };
    // CECFace::SetPartShader applies this material override to the complete
    // base face skin whenever any native part shader is installed.
    for material in &mut model.skins[skin].data.materials {
        material.specular = [50. / 255., 50. / 255., 50. / 255., 1.];
        material.power = 10.;
    }
    if part == 4 || (part == 0 && choice.is_some()) {
        // Face and nose use the same native single-texture shader shape. Keep
        // their colour out of ordinary material modulation so vertex light and
        // specular match the adjoining body instead of being tinted again.
        model.mesh_tints.remove(&(skin, mesh));
        model
            .face_shaders
            .insert((skin, mesh), FaceShader::Face { color });
    } else {
        model.mesh_tints.insert((skin, mesh), color);
    }
    if let Some(choice) = choice {
        let stage =
            apply_texture_choice(model, skin, mesh, texture, choice, assets, stages, warnings);
        if matches!(part, 1 | 2 | 3 | 5 | 6) {
            if let Some(highlight) = stage.and_then(|stage| stage.high) {
                model.mesh_tints.remove(&(skin, mesh));
                let shader = match part {
                    1 => FaceShader::Eye {
                        color,
                        skin_color,
                        highlight,
                    },
                    2 => FaceShader::Brow {
                        color,
                        skin_color,
                        highlight,
                    },
                    3 => FaceShader::Mouth {
                        color,
                        skin_color,
                        highlight,
                    },
                    5 => FaceShader::EyeBall { color, highlight },
                    6 => FaceShader::Moustache {
                        color,
                        skin_color,
                        highlight,
                    },
                    _ => unreachable!(),
                };
                model.face_shaders.insert((skin, mesh), shader);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_extra_texture(
    model: &mut ModelAsset,
    skin: usize,
    color: u32,
    choice: Option<TextureChoice>,
    tinted: bool,
    assets: &Assets,
    stages: &mut Vec<FaceTextureStage>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let meshes = model.skins[skin]
        .data
        .meshes
        .iter()
        .enumerate()
        .filter(|(_, mesh)| mesh.texture == 0)
        .map(|(index, mesh)| (index, mesh.texture))
        .collect::<Vec<_>>();
    if meshes.is_empty() && choice.is_some() {
        warnings.push(format!(
            "{} has no texture-0 mesh for its customized texture",
            model.skins[skin].path
        ));
    }
    for (mesh, texture) in meshes {
        if tinted {
            model.mesh_tints.insert((skin, mesh), color);
        }
        if let Some(choice) = choice.clone() {
            let _ =
                apply_texture_choice(model, skin, mesh, texture, choice, assets, stages, warnings);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_texture_choice(
    model: &mut ModelAsset,
    skin: usize,
    mesh: usize,
    texture: i32,
    choice: TextureChoice,
    assets: &Assets,
    stages: &mut Vec<FaceTextureStage>,
    warnings: &mut Vec<String>,
) -> Option<FaceTextureStage> {
    if texture < 0 {
        warnings.push(format!(
            "{} mesh {mesh} is untextured; customized texture was not applied",
            model.skins[skin].path
        ));
        return None;
    }
    let base = ecmodel::normalize(&choice.base);
    if base.is_empty() || !assets.contains(&base) {
        warnings.push(format!(
            "missing customized texture {base}; retaining the SKI texture"
        ));
        return None;
    }
    let high = choice
        .high
        .map(|path| ecmodel::normalize(&path))
        .filter(|path| !path.is_empty());
    let high = high.and_then(|path| {
        if assets.contains(&path) {
            Some(path)
        } else {
            warnings.push(format!(
                "missing customized highlight texture {path}; highlight stage was not applied"
            ));
            None
        }
    });
    model
        .texture_overrides
        .insert((skin, texture as usize), base.clone());
    let stage = FaceTextureStage {
        skin,
        mesh,
        base,
        high,
    };
    stages.push(stage.clone());
    Some(stage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflect_prefix_replacement_does_not_capture_ordinary_wing_alpha() {
        assert!(reflect_prefix_texture("Textures\\Wings\\R_Feather.DDS"));
        assert!(!reflect_prefix_texture("Textures\\Wings\\老鹰羽翅.dds"));
        assert!(!reflect_prefix_texture("Textures\\Wings\\rw_Feather.DDS"));
    }

    #[test]
    fn composite_equipment_matches_native_show_masks() {
        assert_eq!(equipment_visibility(1 << 1), [true, true, true, true]);
        assert_eq!(equipment_visibility(1 << 5), [true, true, false, true]);
        assert_eq!(equipment_visibility(1 << 6), [true, false, true, true]);
        assert_eq!(equipment_visibility(1 << 8), [true, false, false, true]);
        assert_eq!(
            equipment_visibility((1 << 3) | (1 << 7)),
            [true, true, true, false]
        );
        assert_eq!(
            equipment_visibility((1 << 7) | (1 << 1)),
            [true, true, true, false]
        );
    }

    #[test]
    fn native_rewu_face_texture_prefix_is_class_specific() {
        assert_eq!(
            native_face_texture(
                Class::Psychic,
                "Models\\Face\\巫师男标准脸\\Tex_巫师男标准脸5\\头05.DDS"
            ),
            "models/face/巫师男标准脸/tex_巫师男标准脸5/rw_头05.dds"
        );
        assert_eq!(
            native_face_texture(Class::Blademaster, "Textures\\Face\\头05.DDS"),
            "textures/face/头05.dds"
        );
    }

    #[test]
    fn authentic_default_characters_assemble_and_deform() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let mut evaluated = 0;
        for class in Class::ALL {
            for gender in Gender::ALL {
                if !class.allows(gender) {
                    continue;
                }
                let default_path = class.default_ini(gender);
                let bytes = assets.read(&default_path).unwrap();
                let design = CharacterDesign::from_default_ini(class, gender, &bytes).unwrap();
                let character = Character::assemble(design, &mut assets).unwrap_or_else(|error| {
                    panic!("{} {} assembly: {error}", class.name(), gender.name())
                });
                let pose = character.bind_pose(Mat4::IDENTITY).unwrap_or_else(|error| {
                    panic!("{} {} pose: {error}", class.name(), gender.name())
                });
                let meshes = character.meshes_from_pose(&pose).unwrap_or_else(|error| {
                    panic!("{} {} deformation: {error}", class.name(), gender.name())
                });
                let mut vertices = 0;
                for mesh in meshes.body.iter().chain(&meshes.face) {
                    for vertex in &mesh.vertices {
                        assert!(
                            vertex
                                .position
                                .iter()
                                .chain(&vertex.normal)
                                .all(|value| value.is_finite()),
                            "{} {} produced non-finite geometry",
                            class.name(),
                            gender.name()
                        );
                    }
                    vertices += mesh.vertices.len();
                }
                assert!(
                    vertices > 0,
                    "{} {} produced no body/face geometry",
                    class.name(),
                    gender.name()
                );
                let expected =
                    pose.body.absolute[character.body_head] * character.original_neck.inverse();
                let actual = pose.face.model.to_cols_array();
                let expected = expected.to_cols_array();
                assert!(
                    actual
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| (actual - expected).abs() < 1e-5),
                    "incorrect native head attachment for {} {}",
                    class.name(),
                    gender.name()
                );
                evaluated += 1;
            }
        }
        assert_eq!(evaluated, 14);
    }

    #[test]
    fn zero_armor_and_fashion_are_empty_not_native_default_skins() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        for fashion_mode in [false, true] {
            let mut design = assets
                .default_design(Class::Blademaster, Gender::Male)
                .unwrap();
            design.equipment.fashion_mode = fashion_mode;
            for slot in [
                ArmorSlot::Upper,
                ArmorSlot::Wrist,
                ArmorSlot::Lower,
                ArmorSlot::Foot,
            ] {
                design.equipment.set_armor(slot, 0);
                design.equipment.set_fashion(slot, 0);
            }
            let character = Character::assemble(design, &mut assets).unwrap();
            assert!(character.equipment.is_empty());
            assert_eq!(
                character.body.skins.len(),
                2,
                "base body + hidden native head only"
            );
            assert_eq!(character.body.model.skins.len(), 2);
            let pose = character.bind_pose(Mat4::IDENTITY).unwrap();
            assert!(!character.meshes_from_pose(&pose).unwrap().body.is_empty());
        }
    }

    #[test]
    fn authentic_male_human_and_elf_faces_use_native_skin_shaders() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let mut evaluated = 0;
        for class in [Class::Blademaster, Class::Archer, Class::Cleric] {
            let design = assets.default_design(class, Gender::Male).unwrap();
            let skin_color = design.customize.face.color_face;
            let mouth_color = design.customize.face.color_mouth;
            let mouth_highlight = assets
                .catalog
                .face_texture(design.customize.face.id_mouth_tex as u32)
                .unwrap()
                .high
                .clone();
            let moustache_color = design.customize.face.color_moustache;
            let moustache_highlight = assets
                .catalog
                .face_texture(design.customize.face.id_moustache_tex as u32)
                .map(|texture| texture.high.clone());
            let character = Character::assemble(design, &mut assets).unwrap();
            let face = &character.face;
            for part in [0, 4] {
                let name = format!("model人脸_{part}");
                let (mesh, _) = face.skins[character.face_skins.face]
                    .data
                    .meshes
                    .iter()
                    .enumerate()
                    .find(|(_, mesh)| mesh.name.eq_ignore_ascii_case(&name))
                    .unwrap_or_else(|| panic!("{} is missing {name}", class.name()));
                let key = (character.face_skins.face, mesh);
                assert_eq!(
                    face.face_shaders.get(&key),
                    Some(&FaceShader::Face { color: skin_color }),
                    "{} part {part}",
                    class.name()
                );
                assert!(!face.mesh_tints.contains_key(&key));
                evaluated += 1;
            }
            let (mesh, _) = face.skins[character.face_skins.face]
                .data
                .meshes
                .iter()
                .enumerate()
                .find(|(_, mesh)| mesh.name.eq_ignore_ascii_case("model人脸_3"))
                .unwrap_or_else(|| panic!("{} is missing the mouth mesh", class.name()));
            let key = (character.face_skins.face, mesh);
            let mouth = FaceShader::Mouth {
                color: mouth_color,
                skin_color,
                highlight: ecmodel::normalize(&mouth_highlight),
            };
            assert_eq!(face.face_shaders.get(&key), Some(&mouth));
            assert_eq!(mouth.gloss(), Some("textures/face/lipreflect.tga"));
            assert!(!face.mesh_tints.contains_key(&key));
            evaluated += 1;
            if let Some(highlight) = moustache_highlight.filter(|path| !path.is_empty()) {
                let (mesh, _) = face.skins[character.face_skins.face]
                    .data
                    .meshes
                    .iter()
                    .enumerate()
                    .find(|(_, mesh)| mesh.name.eq_ignore_ascii_case("model人脸_6"))
                    .unwrap_or_else(|| panic!("{} is missing the moustache mesh", class.name()));
                let key = (character.face_skins.face, mesh);
                let moustache = FaceShader::Moustache {
                    color: moustache_color,
                    skin_color,
                    highlight: ecmodel::normalize(&highlight),
                };
                assert_eq!(face.face_shaders.get(&key), Some(&moustache));
                assert!(!face.mesh_tints.contains_key(&key));
            }
            for material in &face.skins[character.face_skins.face].data.materials {
                assert_eq!(material.specular, [50. / 255., 50. / 255., 50. / 255., 1.]);
                assert_eq!(material.power, 10.);
            }
        }
        assert_eq!(evaluated, 9);
    }

    #[test]
    fn authentic_brows_keep_highlight_and_separate_skin_color() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let mut evaluated = 0;
        for class in Class::ALL {
            for gender in Gender::ALL {
                if !class.allows(gender) {
                    continue;
                }
                let design = assets.default_design(class, gender).unwrap();
                let brow_texture = design.customize.face.id_brow_tex;
                let brow_color = design.customize.face.color_brow;
                let skin_color = design.customize.face.color_face;
                let character = Character::assemble(design, &mut assets).unwrap();
                let face = &character.face;
                for (mesh_index, mesh) in face.skins[character.face_skins.face]
                    .data
                    .meshes
                    .iter()
                    .enumerate()
                {
                    if !mesh.name.eq_ignore_ascii_case("model人脸_2") {
                        continue;
                    }
                    if brow_texture == 0 {
                        continue;
                    }
                    let key = (character.face_skins.face, mesh_index);
                    let Some(FaceShader::Brow {
                        color,
                        skin_color: actual_skin,
                        highlight,
                    }) = face.face_shaders.get(&key)
                    else {
                        panic!(
                            "{} {} lost its native brow shader",
                            class.name(),
                            gender.name()
                        );
                    };
                    assert_eq!(*color, brow_color);
                    assert_eq!(*actual_skin, skin_color);
                    assert!(assets.contains(highlight));
                    assert!(!face.mesh_tints.contains_key(&key));
                    assert!(character.face_texture_stages.iter().any(|stage| {
                        stage.skin == key.0
                            && stage.mesh == key.1
                            && stage.high.as_ref() == Some(highlight)
                    }));
                    evaluated += 1;
                }
            }
        }
        // The authentic rigs contain nine customizable brow meshes; several
        // classes intentionally share rigs or omit this optional part.
        assert_eq!(evaluated, 9);
    }

    #[test]
    fn authentic_eye_and_eyeball_use_their_dedicated_two_and_three_stage_shaders() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let design = assets
            .default_design(Class::Blademaster, Gender::Male)
            .unwrap();
        let eye_color = design.customize.face.color_eye;
        let eyeball_color = design.customize.face.color_eye_ball;
        let skin_color = design.customize.face.color_face;
        let character = Character::assemble(design, &mut assets).unwrap();
        let face = &character.face;
        let mut eye = false;
        let mut eyeball = false;
        for (mesh_index, mesh) in face.skins[character.face_skins.face]
            .data
            .meshes
            .iter()
            .enumerate()
        {
            let key = (character.face_skins.face, mesh_index);
            if mesh.name.eq_ignore_ascii_case("model人脸_1") {
                let Some(FaceShader::Eye {
                    color,
                    skin_color: actual_skin,
                    highlight,
                }) = face.face_shaders.get(&key)
                else {
                    panic!("authentic eye mesh lost its native Eye shader");
                };
                assert_eq!(*color, eye_color);
                assert_eq!(*actual_skin, skin_color);
                assert!(assets.contains(highlight));
                assert_eq!(face.face_shaders[&key].gloss(), None);
                eye = true;
            } else if mesh.name.eq_ignore_ascii_case("model人脸_5") {
                let Some(FaceShader::EyeBall { color, highlight }) = face.face_shaders.get(&key)
                else {
                    panic!("authentic eyeball mesh lost its native EyeBall shader");
                };
                assert_eq!(*color, eyeball_color);
                assert!(assets.contains(highlight));
                assert_eq!(
                    face.face_shaders[&key].gloss(),
                    Some("textures/face/eyereflect.tga")
                );
                eyeball = true;
            } else {
                continue;
            }
            assert!(!face.mesh_tints.contains_key(&key));
        }
        assert!(
            eye && eyeball,
            "authentic face must contain Eye and EyeBall meshes"
        );
    }

    #[test]
    fn authentic_body_uses_dedicated_color_mask_shader() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let design = assets
            .default_design(Class::Blademaster, Gender::Male)
            .unwrap();
        let body_color = design.customize.color_body;
        let character = Character::assemble(design, &mut assets).unwrap();
        let mut evaluated = 0;
        for (mesh, data) in character.body.skins[0].data.meshes.iter().enumerate() {
            if data.texture != 0 {
                continue;
            }
            let key = (0, mesh);
            assert_eq!(
                character.body.face_shaders.get(&key),
                Some(&FaceShader::Body { color: body_color })
            );
            assert!(!character.body.mesh_tints.contains_key(&key));
            evaluated += 1;
        }
        assert!(evaluated > 0, "authentic base body has no texture-0 mesh");
    }

    #[test]
    fn psychic_soulsphere_uses_native_qiu_parent_hook() {
        assert_eq!(
            weapon_parent_hook(Class::Psychic, 25333, true, "HH_righthandweapon"),
            "HH_qiu"
        );
        assert_eq!(
            weapon_parent_hook(Class::Psychic, 292, true, "HH_righthandweapon"),
            "HH_righthandweapon"
        );
        assert_eq!(
            weapon_parent_hook(Class::Wizard, 25333, true, "HH_righthandweapon"),
            "HH_righthandweapon"
        );
        assert_eq!(
            weapon_parent_hook(Class::Psychic, 25333, false, "HH_lefthandweapon"),
            "HH_lefthandweapon"
        );
    }

    #[test]
    fn authentic_one_handed_weapon_skips_zero_placeholder_and_loads_real_model() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        assert_eq!(assets.catalog.weapon(19).unwrap().left_model, "0");
        let mut design = assets
            .default_design(Class::Blademaster, Gender::Male)
            .unwrap();
        design.equipment.weapon = 19;
        let character = Character::assemble(design, &mut assets).unwrap();
        assert_eq!(character.weapons.len(), 1);
        assert_eq!(character.weapons[0].hook, "HH_righthandweapon");
        assert_eq!(character.weapons[0].child_hook, Some("CC_weapon"));

        let posed = character.bind_pose(Mat4::IDENTITY).unwrap();
        let parent = model_asset::hook_matrix(
            &character.body.skeleton,
            &posed.body,
            character.weapons[0].hook,
        )
        .unwrap();
        let reference = pose::evaluate_controlled(
            &character.weapons[0].asset.skeleton,
            &[],
            0.0,
            Mat4::IDENTITY,
            &[],
            &[],
        )
        .unwrap();
        let child = model_asset::hook_matrix(
            &character.weapons[0].asset.skeleton,
            &reference,
            "CC_weapon",
        )
        .unwrap();
        let expected = hanger_world(parent, child).unwrap().to_cols_array();
        let actual = posed.weapons[0].model.to_cols_array();
        assert!(actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| (actual - expected).abs() < 1e-5));
    }

    #[test]
    fn authentic_weapons_use_native_material_and_diffuse_only_alpha() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        for weapon_id in [6, 11] {
            let mut design = assets
                .default_design(Class::Blademaster, Gender::Male)
                .unwrap();
            design.equipment.weapon = weapon_id;
            let character = Character::assemble(design, &mut assets).unwrap();
            assert!(!character.weapons.is_empty());
            for weapon in &character.weapons {
                for (skin_index, skin) in weapon.asset.skins.iter().enumerate() {
                    for material in &skin.data.materials {
                        assert_eq!(material.specular, [0.0, 0.0, 0.0, 1.0]);
                        assert_eq!(material.power, 15.0);
                    }
                    for mesh in 0..skin.data.meshes.len() {
                        assert!(weapon
                            .asset
                            .diffuse_alpha_meshes
                            .contains(&(skin_index, mesh)));
                    }
                }
            }
        }
    }

    #[test]
    fn native_hanger_uses_parent_times_inverse_child_hook() {
        let parent = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0))
            * Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let child = Mat4::from_translation(Vec3::X);
        let world = hanger_world(parent, child).unwrap();
        let origin = world.transform_point3(Vec3::ZERO);
        assert!((origin - Vec3::new(10.0, -1.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn authentic_mount_parents_rider_and_hides_weapons() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let mut design = assets
            .default_design(Class::Blademaster, Gender::Male)
            .unwrap();
        design.equipment.weapon = 6;
        design.equipment.mount = 9061;
        let character = Character::assemble(design, &mut assets).unwrap();
        assert_eq!(character.mount.as_ref().unwrap().speed, 8.0);

        let ground = character.bind_pose(Mat4::IDENTITY).unwrap();
        assert!(!character
            .meshes_from_pose(&ground)
            .unwrap()
            .weapons
            .is_empty());
        let mounted = character
            .sampled_pose(
                None,
                Mat4::IDENTITY,
                TravelAnimation {
                    state: TravelState::Mounted,
                    ..TravelAnimation::default()
                },
            )
            .unwrap();
        let meshes = character.meshes_from_pose(&mounted).unwrap();
        assert!(!meshes.mount.is_empty());
        assert!(meshes.weapons.is_empty());
        assert!(meshes.flight.is_empty());
        for vertex in meshes
            .body
            .iter()
            .chain(&meshes.face)
            .chain(&meshes.mount)
            .flat_map(|mesh| &mesh.vertices)
        {
            assert!(vertex.position.iter().all(|value| value.is_finite()));
        }
    }

    #[test]
    fn authentic_wing_and_fly_sword_use_their_native_hangers() {
        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("models.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("models.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        for (class, gender, item, mode) in [
            (Class::Cleric, Gender::Male, 2092, FlightMode::Wing),
            (Class::Cleric, Gender::Male, 2767, FlightMode::Wing),
            (
                Class::Blademaster,
                Gender::Male,
                27210,
                FlightMode::FlySword,
            ),
        ] {
            let mut design = assets.default_design(class, gender).unwrap();
            design.equipment.flight = item;
            let character = Character::assemble(design, &mut assets)
                .unwrap_or_else(|error| panic!("flight item {item}: {error}"));
            assert_eq!(character.flight.as_ref().unwrap().mode, mode);
            let model_path = character.flight.as_ref().unwrap().model_path.clone();
            let original = ModelAsset::load_with(&model_path, |path| assets.read(path))
                .unwrap_or_else(|error| panic!("raw flight item {item}: {error}"));
            let flight = character.flight.as_ref().unwrap();
            for (skin_index, (before, after)) in
                original.skins.iter().zip(&flight.asset.skins).enumerate()
            {
                for (before, after) in before.data.materials.iter().zip(&after.data.materials) {
                    assert_eq!(after.specular, before.specular);
                    assert_eq!(after.power, before.power);
                }
                for (mesh_index, before_mesh) in before.data.meshes.iter().enumerate() {
                    let expected = usize::try_from(before_mesh.texture)
                        .ok()
                        .and_then(|texture| before.data.textures.get(texture))
                        .is_some_and(|texture| reflect_prefix_texture(texture));
                    assert_eq!(
                        flight
                            .asset
                            .diffuse_alpha_meshes
                            .contains(&(skin_index, mesh_index)),
                        expected,
                        "flight item {item} mesh {mesh_index} alpha routing"
                    );
                    if item == 2767 {
                        assert!(!expected, "Dance of clouds Wings must retain texture alpha");
                    }
                }
            }
            let flying = character
                .sampled_pose(
                    None,
                    Mat4::IDENTITY,
                    TravelAnimation {
                        state: TravelState::Flying,
                        ..TravelAnimation::default()
                    },
                )
                .unwrap_or_else(|error| panic!("flight item {item} hanger: {error}"));
            let meshes = character.meshes_from_pose(&flying).unwrap();
            assert!(!meshes.flight.is_empty());
            assert!(meshes.mount.is_empty());
            for vertex in meshes
                .body
                .iter()
                .chain(&meshes.face)
                .chain(&meshes.flight)
                .flat_map(|mesh| &mesh.vertices)
            {
                assert!(vertex.position.iter().all(|value| value.is_finite()));
            }
        }
    }

    #[test]
    fn native_head_attachment_uses_inverse_neck_without_a_body_bind_inverse() {
        let neck = Mat4::from_translation(Vec3::new(0.0, 0.2, 0.1));
        let absolute = Mat4::from_translation(Vec3::new(4.0, 3.0, 2.0));
        let face = absolute * neck.inverse();
        let translation = face.w_axis.truncate();
        assert!(
            (translation - Vec3::new(4.0, 2.8, 1.9)).length() < 1e-6,
            "{translation}"
        );
    }
}
