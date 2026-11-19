//! Native EFACE expressions and the PW 1.4.2 face-bone controller.
//!
//! There are two different matrix conventions in the original engine and in
//! this renderer.  Native code stores row-vector matrices and writes the
//! serialized rows directly to memory.  Existing BON loading passes those
//! rows to `glam::Mat4::from_cols_array`, so a native matrix M is represented
//! here by Mᵀ.  Consequently a native product `A * B` is written `Bᵀ * Aᵀ`
//! here.  EFACE matrices use exactly the same representation.
//!
//! `A3DBoneMatrixController` looks considerably more complicated than a
//! direct controlled matrix, but its decompose/recompose operations cancel:
//! for the rigid/scale transforms used by CFaceBone, the resulting controlled
//! matrix is the blended EFACE TM. Face-bone operation centres are different:
//! native reads the uncontrolled relative TM, so they remain the original
//! relative-bone positions. This module keeps both relationships explicit
//! instead of relying on a visually similar matrix approximation.

use std::array;
use std::collections::HashMap;

use std::fmt::Display;

type Result<T> = std::result::Result<T, String>;

macro_rules! bail {
    ($($arg:tt)*) => { return Err(format!($($arg)*)) };
}

/// Small local equivalent of the context used by the rest of the asset
/// loaders, retaining both the operation and the underlying parser error.
trait Context<T> {
    fn context<C: Display>(self, context: C) -> Result<T>;
    fn with_context<C: Display, F: FnOnce() -> C>(self, context: F) -> Result<T>;
}
impl<T, E: Display> Context<T> for std::result::Result<T, E> {
    fn context<C: Display>(self, context: C) -> Result<T> {
        self.map_err(|error| format!("{context}: {error}"))
    }
    fn with_context<C: Display, F: FnOnce() -> C>(self, context: F) -> Result<T> {
        self.map_err(|error| format!("{}: {error}", context()))
    }
}
impl<T> Context<T> for Option<T> {
    fn context<C: Display>(self, context: C) -> Result<T> {
        self.ok_or_else(|| context.to_string())
    }
    fn with_context<C: Display, F: FnOnce() -> C>(self, context: F) -> Result<T> {
        self.ok_or_else(|| context().to_string())
    }
}
use encoding_rs::GBK;
use glam::{Mat4, Quat, Vec3, Vec4};

use super::assets::Assets;
use crate::skeleton::Skeleton;

pub const TOTAL_BONES: usize = 111;
pub const RIGHT_BONES: usize = 46;
pub const ACTIVE_BONES: usize = 65;
pub const LEFT_EYEBALL: usize = 78;
const NECK: usize = 46;
const RIGHT_EYEBALL: usize = 13;
const BONE_GROUPS: usize = 19;
const BONE_PARTS: usize = 3;

/// An expression loaded from an authentic `.eface` text resource.
#[derive(Debug, Clone)]
pub struct Expression {
    /// Bone index and controlled TM, in file order.  Native lookup returns the
    /// first entry if a malformed file contains a duplicate index.
    entries: Vec<(usize, Mat4)>,
}

impl Expression {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let (text, _, _) = GBK.decode(bytes);
        let mut tokens = text.split_whitespace();
        let first = tokens.next().context("empty EFACE resource")?;
        let count = if first.eq_ignore_ascii_case("MOXT") {
            tokens
                .next()
                .context("EFACE MOXT header has no bone count")?
                .parse::<usize>()
                .context("invalid EFACE bone count")?
        } else {
            first.parse::<usize>().context("invalid EFACE bone count")?
        };
        if count > TOTAL_BONES {
            bail!("EFACE bone count {count} exceeds {TOTAL_BONES}");
        }

        let mut entries = Vec::with_capacity(count);
        for record in 0..count {
            let index = tokens
                .next()
                .with_context(|| format!("EFACE record {record} has no bone index"))?
                .parse::<usize>()
                .with_context(|| format!("invalid EFACE bone index in record {record}"))?;
            if index >= TOTAL_BONES {
                bail!("EFACE bone index {index} exceeds the 111-bone controller");
            }
            // The native parser reads but does not use the bone name.  It is a
            // separate whitespace-delimited token in every shipped resource.
            tokens
                .next()
                .with_context(|| format!("EFACE record {record} has no bone name"))?;
            let mut values = [0.0_f32; 16];
            for (row, value) in values.iter_mut().enumerate() {
                *value = tokens
                    .next()
                    .with_context(|| format!("EFACE record {record} is missing matrix row {row}"))?
                    .parse::<f32>()
                    .with_context(|| format!("invalid EFACE matrix value in record {record}"))?;
                if !value.is_finite() {
                    bail!("non-finite EFACE matrix value in record {record}");
                }
            }
            entries.push((index, Mat4::from_cols_array(&values)));
        }
        if tokens.next().is_some() {
            bail!("trailing tokens after EFACE matrix records");
        }
        Ok(Self { entries })
    }

    fn get(&self, bone: usize) -> Option<Mat4> {
        self.entries
            .iter()
            .find_map(|(index, matrix)| (*index == bone).then_some(*matrix))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Parsed `configs/boneinit.cfg`, `bonegroup.cfg`, and `bonepart.cfg`.
#[derive(Debug, Clone)]
pub struct FaceConfig {
    pub bone_names: [String; TOTAL_BONES],
    pub groups: [Vec<usize>; BONE_GROUPS],
    pub parts: [Vec<usize>; BONE_PARTS],
    name_to_bone: HashMap<String, usize>,
}

impl FaceConfig {
    pub fn load(assets: &mut Assets) -> Result<Self> {
        let init = assets.read("configs/boneinit.cfg")?;
        let groups = assets.read("configs/bonegroup.cfg")?;
        let parts = assets.read("configs/bonepart.cfg")?;
        Self::parse(&init, &groups, &parts)
    }

    pub fn parse(init: &[u8], groups: &[u8], parts: &[u8]) -> Result<Self> {
        let init_tokens = gbk_tokens(init);
        if init_tokens.len() != TOTAL_BONES * 2 {
            bail!(
                "boneinit.cfg contains {} tokens, expected {}",
                init_tokens.len(),
                TOTAL_BONES * 2
            );
        }
        let mut names = Vec::with_capacity(TOTAL_BONES);
        let mut name_to_bone = HashMap::with_capacity(TOTAL_BONES);
        for index in 0..TOTAL_BONES {
            let name = init_tokens[index * 2].clone();
            // The second token is bCanResize.  CFaceBoneController reads it,
            // but none of the Calculate* operations use m_bCanResize.
            init_tokens[index * 2 + 1]
                .parse::<i32>()
                .with_context(|| format!("invalid resize flag for face bone {name}"))?;
            if name_to_bone.insert(name.clone(), index).is_some() {
                bail!("duplicate face bone name {name}");
            }
            names.push(name);
        }
        let bone_names: [String; TOTAL_BONES] = names
            .try_into()
            .map_err(|_| "invalid face bone count".to_owned())?;

        let mut tokens = gbk_tokens(groups).into_iter().peekable();
        let mut parsed_groups: [Vec<usize>; BONE_GROUPS] = array::from_fn(|_| Vec::new());
        for (group_index, group) in parsed_groups.iter_mut().enumerate() {
            let group_name = tokens
                .next()
                .with_context(|| format!("missing face bone group {group_index}"))?;
            let count = parse_count(&mut tokens, &format!("face bone group {group_name}"))?;
            for _ in 0..count {
                let name = tokens
                    .next()
                    .with_context(|| format!("truncated face bone group {group_name}"))?;
                group.push(*name_to_bone.get(&name).with_context(|| {
                    format!("unknown bone {name} in face bone group {group_name}")
                })?);
            }
        }
        if tokens.next().is_some() {
            bail!("trailing tokens in bonegroup.cfg");
        }

        let mut tokens = gbk_tokens(parts).into_iter().peekable();
        let mut parsed_parts: [Vec<usize>; BONE_PARTS] = array::from_fn(|_| Vec::new());
        for (part_index, part) in parsed_parts.iter_mut().enumerate() {
            let count = parse_count(&mut tokens, &format!("face bone part {part_index}"))?;
            let part_name = tokens
                .next()
                .with_context(|| format!("missing face bone part name {part_index}"))?;
            for _ in 0..count {
                let name = tokens
                    .next()
                    .with_context(|| format!("truncated face bone part {part_name}"))?;
                part.push(*name_to_bone.get(&name).with_context(|| {
                    format!("unknown bone {name} in face bone part {part_name}")
                })?);
            }
        }
        if tokens.next().is_some() {
            bail!("trailing tokens in bonepart.cfg");
        }

        Ok(Self {
            bone_names,
            groups: parsed_groups,
            parts: parsed_parts,
            name_to_bone,
        })
    }

    fn bone(&self, name: &str) -> Result<usize> {
        self.name_to_bone
            .get(name)
            .copied()
            .with_context(|| format!("face bone {name} is absent from boneinit.cfg"))
    }
}

fn gbk_tokens(bytes: &[u8]) -> Vec<String> {
    let (text, _, _) = GBK.decode(bytes);
    text.split_whitespace().map(str::to_owned).collect()
}

fn parse_count<I>(tokens: &mut std::iter::Peekable<I>, context: &str) -> Result<usize>
where
    I: Iterator<Item = String>,
{
    tokens
        .next()
        .with_context(|| format!("missing count for {context}"))?
        .parse::<usize>()
        .with_context(|| format!("invalid count for {context}"))
}

/// All shape resources used by one customization state.  Missing IDs/files
/// remain absent, matching the native no-op behavior; assembly reports each
/// omission instead of silently substituting another face.
#[derive(Debug, Default)]
pub struct FaceResources {
    eye: Option<Expression>,
    brow: Option<Expression>,
    nose_tip: Option<Expression>,
    nose_bridge: Option<Expression>,
    mouth_up: Option<Expression>,
    mouth_mid: Option<Expression>,
    mouth_down: Option<Expression>,
    ear: Option<Expression>,
    face_1: Option<Expression>,
    face_2: Option<Expression>,
}

impl FaceResources {
    pub fn load(
        design: &super::customize::CharacterDesign,
        assets: &mut Assets,
        warnings: &mut Vec<String>,
    ) -> Self {
        let face = &design.customize.face;
        Self {
            eye: load_shape(assets, face.id_eye_shape, "eye shape", warnings),
            brow: load_shape(assets, face.id_brow_shape, "brow shape", warnings),
            nose_tip: load_shape(assets, face.id_nose_tip_shape, "nose-tip shape", warnings),
            nose_bridge: load_shape(
                assets,
                face.id_nose_bridge_shape,
                "nose-bridge shape",
                warnings,
            ),
            mouth_up: load_shape(
                assets,
                face.id_mouth_up_lip_line,
                "upper-lip shape",
                warnings,
            ),
            mouth_mid: load_shape(
                assets,
                face.id_mouth_mid_lip_line,
                "lip-line shape",
                warnings,
            ),
            mouth_down: load_shape(
                assets,
                face.id_mouth_down_lip_line,
                "lower-lip shape",
                warnings,
            ),
            ear: load_shape(assets, face.id_ear_shape, "ear shape", warnings),
            face_1: load_shape(assets, face.id_face_shape1, "blend face 1", warnings),
            face_2: load_shape(assets, face.id_face_shape2, "blend face 2", warnings),
        }
    }
}

fn load_shape(
    assets: &mut Assets,
    id: u16,
    label: &str,
    warnings: &mut Vec<String>,
) -> Option<Expression> {
    if id == 0 {
        return None;
    }
    let Some(record) = assets.catalog.face_shape(id as u32) else {
        warnings.push(format!(
            "{label} ID {id} is absent from elements.data; leaving those bones unchanged"
        ));
        return None;
    };
    let path = record.path.clone();
    match assets
        .read(&path)
        .with_context(|| format!("reading {label} {id} ({path})"))
        .and_then(|bytes| Expression::parse(&bytes))
    {
        Ok(expression) => Some(expression),
        Err(error) => {
            warnings.push(format!(
                "{label} ID {id} could not be loaded: {error}; leaving those bones unchanged"
            ));
            None
        }
    }
}

/// Evaluated face-bone state.  `tm` values are controlled matrices consumed
/// by the skeleton's ordinary bone-matrix controller path.
/// Native `MirrorSelectFromRightToLeft` replaces each selected right-side
/// controller with its left-side counterpart. It does not retain the right
/// selection or append a second set of indices.
fn mirror_right_bones(selection: &mut [usize]) {
    for bone in selection {
        if *bone < RIGHT_BONES {
            *bone += ACTIVE_BONES;
        }
    }
}

#[derive(Debug)]
pub struct FaceRig {
    skeleton_bones: [usize; TOTAL_BONES],
    original: [Mat4; TOTAL_BONES],
    tm: [Mat4; TOTAL_BONES],
    positions: [Vec3; TOTAL_BONES],
    initial_positions: [Vec3; TOTAL_BONES],
    selection: Vec<usize>,
    special_center: usize,
    config: FaceConfig,
}

impl FaceRig {
    pub fn new(skeleton: &Skeleton, config: FaceConfig) -> Result<Self> {
        let mut skeleton_bones = [0; TOTAL_BONES];
        for (index, name) in config.bone_names.iter().enumerate() {
            skeleton_bones[index] = skeleton
                .bones
                .iter()
                .position(|bone| &bone.name == name)
                .with_context(|| format!("face skeleton is missing controller bone {name}"))?;
        }
        let original = array::from_fn(|index| {
            Mat4::from_cols_array(&skeleton.bones[skeleton_bones[index]].relative)
        });
        let positions = array::from_fn(|index| original[index].w_axis.truncate());
        Ok(Self {
            skeleton_bones,
            original,
            tm: [Mat4::IDENTITY; TOTAL_BONES],
            positions,
            initial_positions: positions,
            selection: Vec::new(),
            special_center: 0,
            config,
        })
    }

    /// Controlled matrices keyed by the actual skeleton bone index.
    pub fn controlled_bones(&self) -> impl Iterator<Item = (usize, Mat4)> + '_ {
        self.skeleton_bones
            .iter()
            .copied()
            .zip(self.tm.iter().copied())
    }

    fn refresh_positions(&mut self) {
        // CFaceBone::Update/Add call A3DBone::Update and then read
        // GetRelativeTM(), not GetUpToParentTM(). The controller affects only
        // the latter, so native operation centres remain the uncontrolled
        // relative-bone positions. Do not derive them from `tm * original`:
        // that makes each slider change the centre of the next operation.
        self.positions = self.initial_positions;
    }

    fn select_group(&mut self, group: usize) {
        for &bone in &self.config.groups[group] {
            if !self.selection.contains(&bone) {
                self.selection.push(bone);
            }
        }
    }

    fn select_part(&mut self, part: usize) {
        for &bone in &self.config.parts[part] {
            if !self.selection.contains(&bone) {
                self.selection.push(bone);
            }
        }
    }

    fn mirror_selection(&mut self) {
        mirror_right_bones(&mut self.selection);
    }

    fn add_shape(&mut self, expressions: &[(&Expression, f32)]) {
        for bone in 0..TOTAL_BONES {
            let mut matrices = Vec::with_capacity(expressions.len() + 1);
            let mut weights = Vec::with_capacity(expressions.len() + 1);
            for &(expression, weight) in expressions {
                if let Some(matrix) = expression.get(bone) {
                    matrices.push(matrix);
                    weights.push(weight);
                }
            }
            if matrices.is_empty() {
                continue;
            }
            matrices.push(self.tm[bone]);
            weights.push(1.0);
            self.tm[bone] = combine_matrix_list(self.original[bone], &matrices, &weights);
        }
        self.refresh_positions();
    }

    fn reset_selected(&mut self, half_face: bool) {
        let selected = self.selection.clone();
        for bone in selected {
            self.tm[bone] = Mat4::IDENTITY;
            if half_face && bone < RIGHT_BONES {
                self.tm[bone + ACTIVE_BONES] = Mat4::IDENTITY;
            }
        }
        self.refresh_positions();
    }

    fn selected_center(&self) -> Vec3 {
        if self.selection.is_empty() {
            return Vec3::ZERO;
        }
        self.selection
            .iter()
            .map(|&bone| self.positions[bone])
            .sum::<Vec3>()
            / self.selection.len() as f32
    }

    fn translate(&mut self, delta: Vec3, half_face: bool) {
        let selected = self.selection.clone();
        for bone in selected {
            if bone == NECK {
                continue;
            }
            self.tm[bone] = Mat4::from_translation(delta) * self.tm[bone];
            if half_face && bone < RIGHT_BONES {
                let mirrored = Vec3::new(-delta.x, delta.y, delta.z);
                let left = bone + ACTIVE_BONES;
                self.tm[left] = Mat4::from_translation(mirrored) * self.tm[left];
            }
        }
        self.refresh_positions();
    }

    fn rotate(&mut self, angles: Vec3, center_type: CenterType, half_face: bool) {
        let common_center = match center_type {
            CenterType::SelfBone => Vec3::ZERO,
            CenterType::All => self.selected_center(),
            CenterType::Special => self.positions[self.special_center],
        };
        let selected = self.selection.clone();
        for bone in selected {
            if bone == NECK {
                continue;
            }
            let center = if center_type == CenterType::SelfBone {
                self.positions[bone]
            } else {
                common_center
            };
            // CFaceBone::Rotate performs native TM *= Rx; TM *= Ry; TM *= Rz.
            for (axis, angle) in [
                (Vec3::X, angles.x),
                (Vec3::Y, angles.y),
                (Vec3::Z, angles.z),
            ] {
                let transform = transform_about(center, Mat4::from_axis_angle(axis, angle));
                self.tm[bone] = transform * self.tm[bone];
            }
            if half_face && bone < RIGHT_BONES {
                let mirrored_angles = Vec3::new(angles.x, -angles.y, -angles.z);
                let mirrored_center = Vec3::new(-center.x, center.y, center.z);
                let left = bone + ACTIVE_BONES;
                for (axis, angle) in [
                    (Vec3::X, mirrored_angles.x),
                    (Vec3::Y, mirrored_angles.y),
                    (Vec3::Z, mirrored_angles.z),
                ] {
                    let transform =
                        transform_about(mirrored_center, Mat4::from_axis_angle(axis, angle));
                    self.tm[left] = transform * self.tm[left];
                }
            }
            self.refresh_positions();
        }
    }

    fn scale(&mut self, scale: Vec3, center_type: CenterType, half_face: bool) {
        let common_center = match center_type {
            CenterType::SelfBone => Vec3::ZERO,
            CenterType::All => self.selected_center(),
            CenterType::Special => self.positions[self.special_center],
        };
        let selected = self.selection.clone();
        for bone in selected {
            if bone == NECK {
                continue;
            }
            let center = if center_type == CenterType::SelfBone {
                self.positions[bone]
            } else {
                common_center
            };
            self.scale_one(bone, scale, center, center_type);
            if half_face && bone < RIGHT_BONES {
                let left = bone + ACTIVE_BONES;
                let mirrored_center = if center_type == CenterType::SelfBone {
                    self.positions[left]
                } else {
                    Vec3::new(-center.x, center.y, center.z)
                };
                self.scale_one(left, scale, mirrored_center, center_type);
            }
            self.refresh_positions();
        }
    }

    fn scale_one(&mut self, bone: usize, scale: Vec3, center: Vec3, center_type: CenterType) {
        match center_type {
            CenterType::SelfBone => {
                let transform = transform_about(center, Mat4::from_scale(scale));
                self.tm[bone] = transform * self.tm[bone];
            }
            CenterType::All | CenterType::Special => {
                // CT_ALL and CT_SPECIAL in ScaleBone do not multiply a scale
                // into the bone TM.  They transform the bone position around
                // the center and append that displacement as a translation.
                let transform = transform_about(center, Mat4::from_scale(scale));
                let new_position = transform.transform_point3(self.positions[bone]);
                let delta = new_position - self.positions[bone];
                self.tm[bone] = Mat4::from_translation(delta) * self.tm[bone];
            }
        }
    }

    fn move_cheek(&mut self, distance: f32, half_face: bool) {
        let direction = (self.positions[5] - self.positions[9]).normalize_or_zero();
        let direction = Vec3::Z.cross(direction);
        let delta = direction * distance;
        self.selection.clear();
        self.selection.extend(6..9);
        self.translate(delta, half_face);
    }

    fn move_nose_bridge(&mut self, distance: f32, half_face: bool) {
        for (bones, factor) in [(&[51][..], 1.0), (&[50][..], 0.5), (&[52][..], 0.8)] {
            self.selection.clear();
            self.selection.extend_from_slice(bones);
            self.translate(Vec3::new(0.0, 0.0, distance * factor), half_face);
        }
        self.selection.extend_from_slice(&[50, 51]);
    }

    fn scale_eyeball(&mut self, scale: Vec3, side: EyeSide) {
        if side == EyeSide::Both {
            self.scale_eyeball(scale, EyeSide::Right);
            self.scale_eyeball(scale, EyeSide::Left);
            return;
        }
        let (bone, inner, outer) = match side {
            EyeSide::Right => (RIGHT_EYEBALL, 14, 18),
            EyeSide::Left => (LEFT_EYEBALL, 79, 83),
            EyeSide::Both => unreachable!(),
        };
        let position = self.positions[bone];
        let center = Vec3::new(
            position.x,
            position.y,
            (self.positions[inner].z + self.positions[outer].z) / 2.0,
        );
        let transform = transform_about(center, Mat4::from_scale(scale));
        self.tm[bone] = transform * self.tm[bone];
        self.refresh_positions();
    }

    fn move_mouth_corner(&mut self, distance: f32, side: EyeSide) {
        let offset = match side {
            EyeSide::Right => 0,
            EyeSide::Left => ACTIVE_BONES,
            EyeSide::Both => 0,
        };
        let half_face = side == EyeSide::Both;
        for (bones, factor) in [
            (vec![32 + offset], 1.0),
            (
                vec![31 + offset, 33 + offset, 37 + offset, 40 + offset],
                0.5,
            ),
            (
                vec![30 + offset, 34 + offset, 38 + offset, 41 + offset],
                0.25,
            ),
        ] {
            self.selection = bones;
            self.translate(Vec3::new(0.0, distance * factor, 0.0), half_face);
        }
    }

    fn scale_part_down(&mut self, scale: f32) -> Result<()> {
        self.special_center = self.config.bone("鼻型060")?;
        self.selection.clear();
        self.select_part(2);
        self.scale(Vec3::new(1.0, scale, 1.0), CenterType::Special, true);
        Ok(())
    }

    fn scale_part_middle(&mut self, scale: f32) -> Result<()> {
        self.special_center = self.config.bone("鼻型060")?;
        self.selection.clear();
        self.select_part(1);
        self.scale(Vec3::new(1.0, scale, 1.0), CenterType::Special, true);

        self.special_center = self.config.bone("鼻型010")?;
        let center = self.special_center;
        let old = self.initial_positions[center];
        let mut delta = self.tm[center].transform_point3(old) - old;
        delta.x = 0.0;
        delta.z = 0.0;
        self.selection.clear();
        self.select_part(0);
        self.translate(delta, true);
        Ok(())
    }

    fn scale_part_up(&mut self, scale: f32) -> Result<()> {
        self.special_center = self.config.bone("鼻型010")?;
        self.selection.clear();
        self.select_part(0);
        self.scale(Vec3::new(1.0, scale, 1.0), CenterType::Special, true);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CenterType {
    SelfBone,
    All,
    Special,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EyeSide {
    Right,
    Left,
    Both,
}

fn transform_about(center: Vec3, transform: Mat4) -> Mat4 {
    Mat4::from_translation(center) * transform * Mat4::from_translation(-center)
}

/// Port of `A3DBoneMatrixController::CombineMatrixList`, without normalizing
/// by the sum of weights (the original deliberately does not do so).
///
/// Native decomposes `O * F * O⁻¹`, where O is the bone's original relative
/// matrix and F is the EFACE/controller matrix.  In this module's transposed
/// column convention that is `O⁻¹ * F * O`.  Its decomposed translation is
/// multiplied by O's 3x3 before accumulation, and the final controller is
/// `O⁻¹ * M * O * T` in native order.  Keeping these steps matters when two
/// different non-identity expressions are blended; collapsing them to a blend
/// of the un-conjugated F matrices is not equivalent.
fn combine_matrix_list(original: Mat4, matrices: &[Mat4], weights: &[f32]) -> Mat4 {
    debug_assert_eq!(matrices.len(), weights.len());
    let inverse = original.inverse();
    let mut position = Vec3::ZERO;
    let mut scale = Vec3::ZERO;
    // Native starts with the four-float zero quaternion, accumulates
    // (q - A3DQUATERNION(0,0,0)) * weight, then adds the three-angle Euler
    // identity once. Starting at identity and accumulating deltas is exactly
    // the same sum. Glam's four-float constructor is (x,y,z,w).
    let mut rotation = Vec4::W;
    for (&matrix, &weight) in matrices.iter().zip(weights) {
        let conjugated = inverse * matrix * original;
        let (matrix_scale, matrix_rotation, matrix_position) =
            conjugated.to_scale_rotation_translation();
        position += original.transform_vector3(matrix_position) * weight;
        scale += (matrix_scale - Vec3::ONE) * weight;
        rotation += Vec4::new(
            matrix_rotation.x,
            matrix_rotation.y,
            matrix_rotation.z,
            matrix_rotation.w - 1.0,
        ) * weight;
    }
    let rotation = Quat::from_xyzw(rotation.x, rotation.y, rotation.z, rotation.w).normalize();
    let linear = Mat4::from_scale_rotation_translation(scale + Vec3::ONE, rotation, Vec3::ZERO);
    Mat4::from_translation(position) * original * linear * inverse
}

fn scale(value: u8, factor: f32) -> f32 {
    1.0 + (value as f32 - 128.0) / 128.0 * factor
}

fn offset(value: u8, factor: f32) -> f32 {
    (value as f32 - 128.0) / 128.0 * factor
}

/// Evaluate every face operation in the native order.  The returned matrices
/// are suitable for the same bone-controller input used by the body pose.
pub fn evaluate_face(
    skeleton: &Skeleton,
    config: FaceConfig,
    design: &super::customize::CharacterDesign,
    resources: &FaceResources,
) -> Result<FaceRig> {
    let data = &design.customize.face;
    let mut rig = FaceRig::new(skeleton, config)?;

    if let Some(expression) = &resources.eye {
        rig.add_shape(&[(expression, 1.0)]);
        calculate_eye(&mut rig, data);
    }
    if let Some(expression) = &resources.brow {
        rig.add_shape(&[(expression, 1.0)]);
        calculate_brow(&mut rig, data);
    }
    if let (Some(tip), Some(bridge)) = (&resources.nose_tip, &resources.nose_bridge) {
        rig.add_shape(&[(tip, 1.0)]);
        rig.add_shape(&[(bridge, 1.0)]);
        calculate_nose(&mut rig, data);
    }
    if let (Some(up), Some(mid), Some(down)) = (
        &resources.mouth_up,
        &resources.mouth_mid,
        &resources.mouth_down,
    ) {
        rig.add_shape(&[(up, 1.0)]);
        rig.add_shape(&[(mid, 1.0)]);
        rig.add_shape(&[(down, 1.0)]);
        calculate_mouth(&mut rig, data);
    }
    if let Some(expression) = &resources.ear {
        rig.add_shape(&[(expression, 1.0)]);
        rig.selection.clear();
        rig.select_group(18);
        rig.scale(
            Vec3::splat(scale(data.scale_ear, 1.0)),
            CenterType::SelfBone,
            true,
        );
        rig.translate(Vec3::new(0.0, offset(data.offset_ear_v, 0.03), 0.0), true);
    }
    if let (Some(first), Some(second)) = (&resources.face_1, &resources.face_2) {
        let blend = scale(data.blend_face_shape, 1.0);
        rig.add_shape(&[(first, 1.0 - blend), (second, blend)]);
    }

    rig.selection.clear();
    rig.select_group(1);
    rig.scale(
        Vec3::new(scale(data.scale_face_h, 0.3), 1.0, 1.0),
        CenterType::All,
        true,
    );
    rig.special_center = 48;
    rig.scale(
        Vec3::new(1.0, scale(data.scale_face_v, 0.3), 1.0),
        CenterType::Special,
        true,
    );

    rig.selection.clear();
    rig.select_group(3);
    rig.translate(
        Vec3::new(
            offset(data.offset_forehead_h, 0.02),
            offset(data.offset_forehead_v, 0.02),
            offset(data.offset_forehead_z, 0.02),
        ),
        true,
    );
    rig.rotate(
        Vec3::new(offset(data.rotate_forehead, 1.0), 0.0, 0.0),
        CenterType::All,
        true,
    );
    rig.scale(
        Vec3::splat(scale(data.scale_forehead, 0.1)),
        CenterType::SelfBone,
        true,
    );

    rig.selection.clear();
    rig.select_group(4);
    rig.translate(
        Vec3::new(
            offset(data.offset_yoke_bone_h, 0.01),
            offset(data.offset_yoke_bone_v, 0.01),
            offset(data.offset_yoke_bone_z, 0.01),
        ),
        true,
    );
    rig.rotate(
        Vec3::new(0.0, 0.0, offset(data.rotate_yoke_bone, 1.0)),
        CenterType::All,
        true,
    );
    rig.scale(
        Vec3::splat(scale(data.scale_yoke_bone, 0.1)),
        CenterType::SelfBone,
        true,
    );

    rig.selection.clear();
    rig.select_group(5);
    rig.translate(
        Vec3::new(
            offset(data.offset_cheek_h, 0.015),
            offset(data.offset_cheek_v, 0.02),
            offset(data.offset_cheek_z, 0.02),
        ),
        true,
    );
    rig.scale(
        Vec3::splat(scale(data.scale_cheek, 0.5)),
        CenterType::SelfBone,
        true,
    );

    rig.selection.clear();
    rig.select_group(6);
    rig.translate(
        Vec3::new(
            0.0,
            offset(data.offset_chain_v, 0.02),
            offset(data.offset_chain_z, 0.01),
        ),
        true,
    );
    rig.rotate(
        Vec3::new(offset(data.rotate_chain, 1.0), 0.0, 0.0),
        CenterType::All,
        true,
    );
    rig.special_center = 48;
    rig.scale(
        Vec3::new(scale(data.scale_chain_h, 0.99), 1.0, 1.0),
        CenterType::Special,
        true,
    );

    rig.selection.clear();
    rig.select_group(2);
    rig.translate(
        Vec3::new(
            offset(data.offset_jaw_h, 0.01),
            offset(data.offset_jaw_v, 0.01),
            offset(data.offset_jaw_z, 0.01),
        ),
        true,
    );
    rig.scale(
        Vec3::new(
            scale(data.scale_jaw_h, 0.5),
            scale(data.scale_jaw_v, 0.5),
            1.0,
        ),
        CenterType::All,
        true,
    );
    rig.selection.clear();
    rig.move_cheek(offset(data.scale_jaw_special, 0.01), true);

    // CECFace::CalculateAllFaceShape calls Calculate3Parts after CalculateJaw,
    // not immediately after CalculateFace.  The middle bone part contains all
    // four authored brow bones, and all three parts overlap groups transformed
    // by the intervening forehead/yoke/cheek/jaw operations.
    rig.scale_part_down(scale(data.scale_down, 0.4))?;
    rig.scale_part_middle(scale(data.scale_middle, 0.4))?;
    rig.scale_part_up(scale(data.scale_up, 0.4))?;
    rig.refresh_positions();

    Ok(rig)
}

fn calculate_eye(rig: &mut FaceRig, data: &super::customize::FaceCustomize) {
    rig.selection.clear();
    rig.select_group(7);
    rig.mirror_selection();
    rig.scale(
        Vec3::new(
            scale(data.scale_eye_h, 0.7),
            scale(data.scale_eye_v, 1.0),
            1.0,
        ),
        CenterType::All,
        true,
    );
    rig.rotate(
        Vec3::new(0.0, 0.0, offset(data.rotate_eye, 1.0)),
        CenterType::All,
        true,
    );
    let primary_offset = Vec3::new(
        offset(data.offset_eye_h, 0.01),
        offset(data.offset_eye_v, 0.01),
        offset(data.offset_eye_z, 0.01),
    );
    rig.translate(primary_offset, true);

    rig.selection.clear();
    rig.select_group(8);
    rig.mirror_selection();
    rig.reset_selected(true);
    rig.translate(primary_offset, true);
    rig.selection.clear();
    rig.scale_eyeball(Vec3::splat(scale(data.scale_eye_ball, 0.8)), EyeSide::Left);

    rig.selection.clear();
    rig.select_group(7);
    rig.scale(
        Vec3::new(
            scale(data.scale_eye_h2, 0.7),
            scale(data.scale_eye_v2, 1.0),
            1.0,
        ),
        CenterType::All,
        false,
    );
    rig.rotate(
        -Vec3::new(0.0, 0.0, offset(data.rotate_eye2, 1.0)),
        CenterType::All,
        false,
    );
    let secondary_offset = Vec3::new(
        -offset(data.offset_eye_h2, 0.01),
        offset(data.offset_eye_v2, 0.01),
        offset(data.offset_eye_z2, 0.01),
    );
    rig.translate(secondary_offset, false);
    rig.selection.clear();
    rig.select_group(8);
    rig.reset_selected(false);
    rig.translate(secondary_offset, false);
    rig.selection.clear();
    rig.scale_eyeball(
        Vec3::splat(scale(data.scale_eye_ball2, 0.8)),
        EyeSide::Right,
    );
}

fn calculate_brow(rig: &mut FaceRig, data: &super::customize::FaceCustomize) {
    rig.selection.clear();
    rig.select_group(9);
    rig.mirror_selection();
    rig.scale(
        Vec3::new(scale(data.scale_brow_h, 0.4), 1.0, 1.0),
        CenterType::All,
        true,
    );
    rig.scale(
        Vec3::new(1.0, scale(data.scale_brow_v, 1.0), 1.0),
        CenterType::SelfBone,
        true,
    );
    rig.rotate(
        Vec3::new(0.0, 0.0, offset(data.rotate_brow, 0.8)),
        CenterType::All,
        true,
    );
    rig.translate(
        Vec3::new(
            offset(data.offset_brow_h, 0.01),
            offset(data.offset_brow_v, 0.01),
            offset(data.offset_brow_z, 0.01),
        ),
        true,
    );

    rig.selection.clear();
    rig.select_group(9);
    rig.scale(
        Vec3::new(scale(data.scale_brow_h2, 0.4), 1.0, 1.0),
        CenterType::All,
        false,
    );
    rig.scale(
        Vec3::new(1.0, scale(data.scale_brow_v2, 1.0), 1.0),
        CenterType::SelfBone,
        false,
    );
    rig.rotate(
        -Vec3::new(0.0, 0.0, offset(data.rotate_brow2, 0.8)),
        CenterType::All,
        false,
    );
    rig.translate(
        Vec3::new(
            -offset(data.offset_brow_h2, 0.01),
            offset(data.offset_brow_v2, 0.01),
            offset(data.offset_brow_z2, 0.01),
        ),
        false,
    );
}

fn calculate_nose(rig: &mut FaceRig, data: &super::customize::FaceCustomize) {
    rig.selection.clear();
    rig.select_group(11);
    rig.special_center = 53;
    rig.scale(
        Vec3::new(scale(data.scale_nose_tip_h, 0.8), 1.0, 1.0),
        CenterType::Special,
        true,
    );
    rig.scale(
        Vec3::splat(scale(data.scale_nose_tip_v, 0.6)),
        CenterType::SelfBone,
        true,
    );
    rig.special_center = 56;
    rig.scale(
        Vec3::new(1.0, 1.0, scale(data.scale_nose_tip_z, 1.0)),
        CenterType::Special,
        true,
    );
    rig.translate(
        Vec3::new(0.0, offset(data.offset_nose_tip_v, 0.01), 0.0),
        true,
    );

    rig.selection.clear();
    rig.select_group(10);
    rig.special_center = 50;
    rig.scale(
        Vec3::new(scale(data.scale_bridge_tip_h, 0.8), 1.0, 1.0),
        CenterType::Special,
        true,
    );
    rig.selection.clear();
    rig.move_nose_bridge(offset(data.offset_bridge_tip_z, 0.01), true);
    rig.selection.clear();
}

fn calculate_mouth(rig: &mut FaceRig, data: &super::customize::FaceCustomize) {
    rig.selection.clear();
    rig.select_group(12);
    rig.special_center = 32;
    rig.scale(
        Vec3::new(1.0, scale(data.thick_up_lip, 1.0), 1.0),
        CenterType::Special,
        true,
    );
    rig.selection.clear();
    rig.select_group(13);
    rig.special_center = 32;
    rig.scale(
        Vec3::new(1.0, scale(data.thick_down_lip, 1.0), 1.0),
        CenterType::Special,
        true,
    );

    rig.selection.clear();
    for group in 12..=15 {
        rig.select_group(group);
    }
    rig.special_center = 57;
    rig.mirror_selection();
    rig.scale(
        Vec3::new(scale(data.scale_mouth_h, 0.6), 1.0, 1.0),
        CenterType::Special,
        true,
    );
    rig.selection.clear();
    for group in 12..=15 {
        rig.select_group(group);
    }
    rig.special_center = 57;
    rig.scale(
        Vec3::new(scale(data.scale_mouth_h2, 0.6), 1.0, 1.0),
        CenterType::Special,
        false,
    );
    rig.select_group(16);
    rig.selection.push(62);
    rig.translate(
        Vec3::new(
            0.0,
            offset(data.offset_mouth_v, 0.01),
            offset(data.offset_mouth_z, 0.02),
        ),
        true,
    );
    rig.selection.clear();
    rig.move_mouth_corner(
        offset(data.offset_corner_of_mouth_special, 0.02),
        EyeSide::Left,
    );
    rig.move_mouth_corner(
        offset(data.offset_corner_of_mouth_special2, 0.02),
        EyeSide::Right,
    );
    rig.selection.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_selection_replaces_native_eye_and_brow_groups() {
        let mut eye = (14..=21).chain(std::iter::once(13)).collect::<Vec<_>>();
        mirror_right_bones(&mut eye);
        assert_eq!(
            eye,
            (79..=86).chain(std::iter::once(78)).collect::<Vec<_>>()
        );

        let mut brow = vec![22, 23, 24, 25];
        mirror_right_bones(&mut brow);
        assert_eq!(brow, vec![87, 88, 89, 90]);

        let mut centre_and_left = vec![46, 78];
        mirror_right_bones(&mut centre_and_left);
        assert_eq!(centre_and_left, vec![46, 78]);
    }

    #[test]
    fn expression_uses_native_rows_as_glam_columns() {
        let expression =
            Expression::parse(b"MOXT\n1\n\n7 bone\n1 0 0 0\n0 1 0 0\n0 0 1 0\n2 3 4 1\n").unwrap();
        assert_eq!(expression.len(), 1);
        assert_eq!(
            expression.get(7).unwrap().w_axis.truncate(),
            Vec3::new(2.0, 3.0, 4.0)
        );
    }

    #[test]
    fn weighted_matrix_list_preserves_native_unnormalised_translation() {
        let a = Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0));
        let b = Mat4::from_translation(Vec3::new(0.0, 4.0, 0.0));
        let combined =
            combine_matrix_list(Mat4::IDENTITY, &[a, b, Mat4::IDENTITY], &[0.25, 0.5, 1.0]);
        assert_eq!(combined.w_axis.truncate(), Vec3::new(0.5, 2.0, 0.0));
        assert_eq!(combined.x_axis.x, 1.0);
    }

    #[test]
    fn native_conjugated_single_expression_round_trips_through_controller() {
        let original = Mat4::from_translation(Vec3::new(0.03, -0.02, 0.11))
            * Mat4::from_rotation_y(0.37)
            * Mat4::from_rotation_z(-0.21);
        let expression = Mat4::from_translation(Vec3::new(-0.01, 0.02, 0.03))
            * Mat4::from_rotation_x(0.19)
            * Mat4::from_scale(Vec3::splat(1.1));
        let combined = combine_matrix_list(original, &[expression, Mat4::IDENTITY], &[1.0, 1.0]);
        let a = combined.to_cols_array();
        let e = expression.to_cols_array();
        assert!(a.iter().zip(e).all(|(a, e)| (a - e).abs() < 2e-5));
    }

    #[test]
    fn three_zero_quaternion_initializer_is_not_a_zero_quaternion() {
        let combined = combine_matrix_list(Mat4::IDENTITY, &[Mat4::IDENTITY], &[1.0]);
        assert!(combined.is_finite());
        assert!((combined.w_axis.w - 1.0).abs() < 1e-6);
    }

    #[test]
    fn customization_bytes_are_signed_offsets_from_128_not_wrapping_subtraction() {
        assert_eq!(scale(0, 1.0), 0.0);
        assert_eq!(scale(128, 0.6), 1.0);
        assert_eq!(offset(0, 0.02), -0.02);
        assert_eq!(offset(128, 0.02), 0.0);
    }

    #[test]
    fn authentic_brow_sliders_match_independent_native_matrix_sequences() {
        use crate::modules::designer::{
            catalog::{Class, Gender},
            customize::CharacterDesign,
        };

        fn apply_side(
            rig: &mut FaceRig,
            bones: [usize; 4],
            horizontal: Vec3,
            vertical: Vec3,
            angle: f32,
            delta: Vec3,
        ) {
            // Native CFaceBone::Update reads the uncontrolled relative TM,
            // so every operation centre is the original relative position.
            let centre = bones
                .iter()
                .map(|&bone| rig.initial_positions[bone])
                .sum::<Vec3>()
                / 4.0;
            for bone in bones {
                let transform = transform_about(centre, Mat4::from_scale(horizontal));
                let old = rig.initial_positions[bone];
                let displacement = transform.transform_point3(old) - old;
                rig.tm[bone] = Mat4::from_translation(displacement) * rig.tm[bone];
                rig.refresh_positions();
            }
            // CT_SELF scale also uses the uncontrolled relative position.
            for bone in bones {
                let centre = rig.initial_positions[bone];
                rig.tm[bone] = transform_about(centre, Mat4::from_scale(vertical)) * rig.tm[bone];
                rig.refresh_positions();
            }
            let centre = bones
                .iter()
                .map(|&bone| rig.initial_positions[bone])
                .sum::<Vec3>()
                / 4.0;
            for bone in bones {
                rig.tm[bone] = transform_about(centre, Mat4::from_rotation_z(angle)) * rig.tm[bone];
                rig.refresh_positions();
            }
            for bone in bones {
                rig.tm[bone] = Mat4::from_translation(delta) * rig.tm[bone];
                rig.refresh_positions();
            }
        }

        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("facedata.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("facedata.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let class = Class::Blademaster;
        let gender = Gender::Female;
        let bytes = assets.read(&class.default_ini(gender)).unwrap();
        let mut design = CharacterDesign::from_default_ini(class, gender, &bytes).unwrap();
        let data = &mut design.customize.face;
        data.scale_brow_h = 171;
        data.scale_brow_v = 83;
        data.rotate_brow = 146;
        data.offset_brow_h = 159;
        data.offset_brow_v = 112;
        data.offset_brow_z = 137;
        data.scale_brow_h2 = 92;
        data.scale_brow_v2 = 161;
        data.rotate_brow2 = 119;
        data.offset_brow_h2 = 103;
        data.offset_brow_v2 = 148;
        data.offset_brow_z2 = 121;

        let skeleton_path = class.face_asset(gender, design.customize.body_id, "bon");
        let skeleton = Skeleton::parse(&assets.read(&skeleton_path).unwrap()).unwrap();
        let config = FaceConfig::load(&mut assets).unwrap();
        let mut warnings = Vec::new();
        let resources = FaceResources::load(&design, &mut assets, &mut warnings);
        let expression = resources.brow.as_ref().unwrap();
        let data = &design.customize.face;

        let mut actual = FaceRig::new(&skeleton, config.clone()).unwrap();
        actual.add_shape(&[(expression, 1.0)]);
        calculate_brow(&mut actual, data);

        let mut expected = FaceRig::new(&skeleton, config).unwrap();
        // One expression at weight one plus identity reduces exactly to the
        // authored record, even through native conjugated list blending.
        for bone in 0..TOTAL_BONES {
            if let Some(matrix) = expression.get(bone) {
                expected.tm[bone] = matrix;
            }
        }
        expected.refresh_positions();
        apply_side(
            &mut expected,
            [87, 88, 89, 90],
            Vec3::new(scale(data.scale_brow_h, 0.4), 1.0, 1.0),
            Vec3::new(1.0, scale(data.scale_brow_v, 1.0), 1.0),
            offset(data.rotate_brow, 0.8),
            Vec3::new(
                offset(data.offset_brow_h, 0.01),
                offset(data.offset_brow_v, 0.01),
                offset(data.offset_brow_z, 0.01),
            ),
        );
        apply_side(
            &mut expected,
            [22, 23, 24, 25],
            Vec3::new(scale(data.scale_brow_h2, 0.4), 1.0, 1.0),
            Vec3::new(1.0, scale(data.scale_brow_v2, 1.0), 1.0),
            -offset(data.rotate_brow2, 0.8),
            Vec3::new(
                -offset(data.offset_brow_h2, 0.01),
                offset(data.offset_brow_v2, 0.01),
                offset(data.offset_brow_z2, 0.01),
            ),
        );
        for bone in [22, 23, 24, 25, 87, 88, 89, 90] {
            let a = actual.tm[bone].to_cols_array();
            let e = expected.tm[bone].to_cols_array();
            assert!(
                a.iter().zip(e).all(|(a, e)| (a - e).abs() < 2e-5),
                "brow bone {bone}: {a:?} != {e:?}"
            );
        }
    }

    #[test]
    fn authentic_three_part_scaling_is_numerically_last() {
        use crate::modules::designer::{
            catalog::{Class, Gender},
            customize::CharacterDesign,
        };

        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("facedata.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("facedata.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let class = Class::Blademaster;
        let gender = Gender::Female;
        let bytes = assets.read(&class.default_ini(gender)).unwrap();
        let mut adjusted = CharacterDesign::from_default_ini(class, gender, &bytes).unwrap();
        let face = &mut adjusted.customize.face;
        face.scale_up = 172;
        face.scale_middle = 76;
        face.scale_down = 149;
        face.offset_forehead_v = 181;
        face.rotate_forehead = 157;
        face.scale_forehead = 143;
        face.offset_yoke_bone_h = 166;
        face.scale_yoke_bone = 137;
        face.offset_cheek_v = 174;
        face.scale_cheek = 151;
        face.rotate_chain = 146;
        face.scale_chain_h = 139;
        face.offset_jaw_v = 169;
        face.scale_jaw_h = 141;
        face.scale_jaw_special = 158;

        let mut neutral = adjusted.clone();
        neutral.customize.face.scale_up = 128;
        neutral.customize.face.scale_middle = 128;
        neutral.customize.face.scale_down = 128;
        let skeleton_path = class.face_asset(gender, adjusted.customize.body_id, "bon");
        let skeleton = Skeleton::parse(&assets.read(&skeleton_path).unwrap()).unwrap();
        let config = FaceConfig::load(&mut assets).unwrap();
        let mut warnings = Vec::new();
        let resources = FaceResources::load(&adjusted, &mut assets, &mut warnings);
        let actual = evaluate_face(&skeleton, config.clone(), &adjusted, &resources).unwrap();

        // With neutral 3-part sliders those operations are identities, so this
        // is exactly the native state immediately before Calculate3Parts.
        let mut expected = evaluate_face(&skeleton, config, &neutral, &resources).unwrap();
        expected
            .scale_part_down(scale(adjusted.customize.face.scale_down, 0.4))
            .unwrap();
        expected
            .scale_part_middle(scale(adjusted.customize.face.scale_middle, 0.4))
            .unwrap();
        expected
            .scale_part_up(scale(adjusted.customize.face.scale_up, 0.4))
            .unwrap();
        expected.refresh_positions();
        for bone in 0..TOTAL_BONES {
            let a = actual.tm[bone].to_cols_array();
            let e = expected.tm[bone].to_cols_array();
            assert!(
                a.iter().zip(e).all(|(a, e)| (a - e).abs() < 2e-5),
                "bone {bone} differs from native Calculate3Parts placement: {a:?} != {:?}",
                expected.tm[bone].to_cols_array()
            );
        }
    }

    #[test]
    fn authentic_default_face_rigs_produce_finite_controller_matrices() {
        use crate::modules::designer::{
            catalog::{Class, Gender},
            customize::CharacterDesign,
        };

        let element = std::path::Path::new("google_drive/pw142/element");
        if !element.join("facedata.pck").exists() {
            crate::pck::corpus_file_absent(&element.join("facedata.pck"));
            return;
        }
        let mut assets = Assets::open(element).unwrap();
        let config = FaceConfig::load(&mut assets).unwrap();
        let mut evaluated = 0;
        for class in Class::ALL {
            for gender in Gender::ALL {
                if !class.allows(gender) {
                    continue;
                }
                let default_path = class.default_ini(gender);
                let bytes = assets.read(&default_path).unwrap();
                let design = CharacterDesign::from_default_ini(class, gender, &bytes).unwrap();
                let skeleton_path = class.face_asset(gender, design.customize.body_id, "bon");
                let skeleton = Skeleton::parse(&assets.read(&skeleton_path).unwrap()).unwrap();
                let mut warnings = Vec::new();
                let resources = FaceResources::load(&design, &mut assets, &mut warnings);
                let rig = evaluate_face(&skeleton, config.clone(), &design, &resources)
                    .unwrap_or_else(|error| panic!("{} {}: {error}", class.name(), gender.name()));
                let controls = rig.controlled_bones().collect::<Vec<_>>();
                assert!(
                    !controls.is_empty(),
                    "{} {} produced no face controls",
                    class.name(),
                    gender.name()
                );
                for (bone, matrix) in controls {
                    assert!(
                        matrix.is_finite(),
                        "{} {} bone {bone} produced a non-finite matrix",
                        class.name(),
                        gender.name()
                    );
                }
                evaluated += 1;
            }
        }
        assert_eq!(evaluated, 14);
    }
}
