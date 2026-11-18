//! Traced ordinary, unscaled A3D bone palette construction.
//! 0x7b4b63 builds a local matrix from sampled quaternion/position;
//! 0x7b4dc9 accumulates it through the parent. 0x79269c / 0x7926d2
//! combine inverse bind, bone transform and model transform before transpose.
//! Reflection flags, scale controllers and constrained joints fail explicitly.
use crate::{animation::BoneTrack, skeleton::Skeleton, skin::Skin};
use glam::{Mat4, Quat, Vec3};
use std::collections::VecDeque;

pub struct Pose {
    pub absolute: Vec<Mat4>,
    pub skin: Vec<Mat4>,
    pub model: Mat4,
}

pub fn evaluate(
    s: &Skeleton,
    tracks: &[BoneTrack],
    seconds: f32,
    model: Mat4,
) -> Result<Pose, String> {
    if !seconds.is_finite() || !model.is_finite() {
        return Err("nonfinite pose input".into());
    }
    // Bone flags: 0 is an ordinary deforming bone. Flag 1 is the helper-joint
    // bit — 533 bones across the shipped skeletons carry it, and they are the
    // 3ds Max helper objects a rig leaves behind (`Box01`, `Cylinder01`,
    // `Line01`, `02_Bip02 Footsteps`), 515 of them unparented roots. They still
    // carry a valid local transform, so they pose exactly like any other bone.
    //
    // This is measured, not assumed: not one flag-1 bone in the archive has a
    // negative-determinant local matrix (533 positive, 0 negative), and neither
    // does any flag-0 bone (5,573 positive). A reflection bit would show up
    // there, so no shipped skeleton asks to mirror geometry.
    //
    // Flags 2 and 3 do not appear in the effect archive at all (25 and 175 bones
    // in the wider model archive do), so they stay refused rather than guessed:
    // the client's scale/constraint controllers are not decoded.
    if s.bones.iter().any(|b| b.flags > 1) || s.joints.iter().any(|j| j.class != 15) {
        return Err(
            "scale/constraint bone controller or non-animation joint is unsupported".into(),
        );
    }
    let count = s.bones.len();
    let mut local: Vec<_> = s
        .bones
        .iter()
        .map(|b| Mat4::from_cols_array(&b.relative))
        .collect();
    let mut assigned = vec![false; count];
    for track in tracks {
        let joint = usize::try_from(track.id)
            .ok()
            .and_then(|i| s.joints.get(i))
            .ok_or("track references an invalid joint")?;
        let bone = usize::try_from(joint.child)
            .ok()
            .filter(|&i| i < count)
            .ok_or("joint references an invalid child bone")?;
        if assigned[bone] {
            return Err("multiple sampled joints target one bone".into());
        }
        assigned[bone] = true;
        let position = track
            .position
            .sample(seconds)
            .ok_or("missing/nonfinite position track")?;
        let rotation = track
            .rotation
            .sample(seconds)
            .ok_or("missing/nonfinite rotation track")?;
        // Data is a complete local transform, not a delta multiplied by bind pose.
        local[bone] =
            Mat4::from_rotation_translation(Quat::from_array(rotation), Vec3::from_array(position));
    }
    let topo = s
        .bones
        .iter()
        .enumerate()
        .all(|(i, b)| b.parent == -1 || (b.parent >= 0 && (b.parent as usize) < i));
    if topo {
        let mut absolute = local;
        let mut skin = Vec::with_capacity(count);
        for (index, bone) in s.bones.iter().enumerate() {
            let initial = Mat4::from_cols_array(&bone.initial);
            if !absolute[index].is_finite() || !initial.is_finite() {
                return Err("nonfinite bone matrix".into());
            }
            absolute[index] = if bone.parent < 0 {
                model * absolute[index]
            } else {
                absolute[bone.parent as usize] * absolute[index]
            };
            skin.push(absolute[index] * initial);
            if !absolute[index].is_finite() || !skin[index].is_finite() {
                return Err("nonfinite composed pose".into());
            }
        }
        return Ok(Pose {
            absolute,
            skin,
            model,
        });
    }
    let mut absolute = vec![Mat4::IDENTITY; count];
    let mut skin = vec![Mat4::IDENTITY; count];
    let mut children = vec![Vec::new(); count];
    let mut queue = VecDeque::new();
    for (index, bone) in s.bones.iter().enumerate() {
        if bone.parent == -1 {
            queue.push_back(index);
        } else {
            let parent = usize::try_from(bone.parent)
                .ok()
                .filter(|&i| i < count && i != index)
                .ok_or("invalid pose hierarchy")?;
            children[parent].push(index);
        }
        if !local[index].is_finite() || !Mat4::from_cols_array(&bone.initial).is_finite() {
            return Err("nonfinite bone matrix".into());
        }
    }
    let mut visited = 0;
    while let Some(i) = queue.pop_front() {
        let parent = s.bones[i].parent;
        absolute[i] = if parent < 0 {
            model * local[i]
        } else {
            absolute[parent as usize] * local[i]
        };
        skin[i] = absolute[i] * Mat4::from_cols_array(&s.bones[i].initial);
        if !absolute[i].is_finite() || !skin[i].is_finite() {
            return Err("nonfinite composed pose".into());
        }
        queue.extend(children[i].iter().copied());
        visited += 1;
    }
    if visited != count {
        return Err("cyclic pose hierarchy".into());
    }
    Ok(Pose {
        absolute,
        skin,
        model,
    })
}

/// Rigid mesh bone IDs address the skeleton blend array directly, including
/// its final virtual/model slot. Unlike weighted v9 vertices they are NOT
/// remapped through SKI bone names (retail 0x7a7158 -> 0x792969).
pub fn rigid_matrix(pose: &Pose, bone: i32) -> Result<Mat4, String> {
    let bone = usize::try_from(bone).map_err(|_| "negative rigid bone index")?;
    let matrix = if bone == pose.skin.len() {
        pose.model
    } else {
        *pose.skin.get(bone).ok_or("rigid bone index out of range")?
    };
    if !matrix.is_finite() {
        return Err("nonfinite rigid matrix".into());
    }
    Ok(matrix)
}

/// v9 uses skin-local bone names. Earlier skins use skeleton indices.
/// The extra model/root slot follows the palette allocation at 0x7927e9.
pub fn palette(skin: &Skin, skeleton: &Skeleton, pose: &Pose) -> Result<Vec<Mat4>, String> {
    if pose.skin.len() != skeleton.bones.len() {
        return Err("pose/skeleton mismatch".into());
    }
    let mut matrices = if skin.version >= 9 {
        let mut out = Vec::with_capacity(skin.bones.len() + 1);
        for name in &skin.bones {
            let index = skeleton
                .bones
                .iter()
                .position(|b| b.name == *name)
                .ok_or_else(|| format!("skin bone not found in skeleton: {name}"))?;
            out.push(pose.skin[index]);
        }
        out
    } else {
        pose.skin.clone()
    };
    matrices.push(pose.model);
    Ok(matrices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        animation::KeyTrack,
        skeleton::{Bone, Joint},
    };
    fn skeleton() -> Skeleton {
        let bone = |name: &str, parent, position: f32, inverse: f32| Bone {
            name: name.into(),
            flags: 0,
            parent,
            first_joint: 0,
            relative: Mat4::from_translation(Vec3::X * position).to_cols_array(),
            initial: Mat4::from_translation(Vec3::X * inverse).to_cols_array(),
            children: Vec::new(),
        };
        Skeleton {
            version: 6,
            start: 0,
            end: 1,
            fps: 30,
            bones: vec![bone("child", 1, 2., -5.), bone("root", -1, 3., -3.)],
            joints: vec![
                Joint {
                    class: 15,
                    name: "root joint".into(),
                    parent: -1,
                    child: 1,
                    sibling: -1,
                },
                Joint {
                    class: 15,
                    name: "child joint".into(),
                    parent: 1,
                    child: 0,
                    sibling: -1,
                },
            ],
            hooks: Vec::new(),
            embedded_tracks: Vec::new(),
        }
    }
    fn track(id: i32, x: f32) -> BoneTrack {
        BoneTrack {
            id,
            position: KeyTrack {
                fps: 30,
                keys: vec![[x, 0., 0.]],
                frames: vec![0.],
                length_ms: 0,
                compression: 0,
            },
            rotation: KeyTrack {
                fps: 30,
                keys: vec![[0., 0., 0., 1.]],
                frames: vec![0.],
                length_ms: 0,
                compression: 0,
            },
        }
    }
    #[test]
    fn joint_to_bone_remapping_parent_order_and_inverse_bind_are_independent() {
        let s = skeleton();
        let pose = evaluate(
            &s,
            &[track(1, 5.), track(0, 3.)],
            0.,
            Mat4::from_translation(Vec3::X * 10.),
        )
        .unwrap();
        assert_eq!(pose.absolute[0].transform_point3(Vec3::ZERO), Vec3::X * 18.);
        assert_eq!(pose.skin[0].transform_point3(Vec3::X), Vec3::X * 14.);
        let rest = evaluate(&s, &[], 0., Mat4::from_translation(Vec3::X * 10.)).unwrap();
        assert_eq!(rest.skin[0].transform_point3(Vec3::X), Vec3::X * 11.);
    }
    #[test]
    fn rigid_indices_are_not_limited_to_weighted_vertex_bytes() {
        let mut pose = Pose {
            absolute: vec![],
            skin: vec![Mat4::IDENTITY; 300],
            model: Mat4::from_translation(Vec3::X * 23.),
        };
        pose.skin[299] = Mat4::from_translation(Vec3::X * 17.);
        assert_eq!(rigid_matrix(&pose, 299).unwrap(), pose.skin[299]);
        assert_eq!(rigid_matrix(&pose, 300).unwrap(), pose.model);
        assert!(rigid_matrix(&pose, 301).is_err());
        assert!(rigid_matrix(&pose, -1).is_err());
        pose.skin[299] = Mat4::from_cols_array(&[f32::NAN; 16]);
        assert!(rigid_matrix(&pose, 299).is_err());
    }
    #[test]
    fn skin_names_reorder_matrices_and_keep_separate_model_slot() {
        let s = skeleton();
        let pose = evaluate(&s, &[], 0., Mat4::from_translation(Vec3::X * 10.)).unwrap();
        let mut skin = Skin {
            version: 9,
            bones: vec!["root".into(), "child".into()],
            textures: Vec::new(),
            materials: Vec::new(),
            meshes: Vec::new(),
        };
        let p = palette(&skin, &s, &pose).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], pose.skin[1]);
        assert_eq!(p[1], pose.skin[0]);
        assert_eq!(p[2], pose.model);
        skin.bones.push("missing".into());
        assert!(palette(&skin, &s, &pose).is_err());
    }
    #[test]
    fn unsupported_or_inconsistent_pose_inputs_fail_not_silently_deform() {
        let mut s = skeleton();
        assert!(evaluate(&s, &[track(99, 0.)], 0., Mat4::IDENTITY).is_err());
        assert!(evaluate(&s, &[track(0, 0.), track(0, 0.)], 0., Mat4::IDENTITY).is_err());
        s.bones[1].parent = 0;
        assert!(evaluate(&s, &[], 0., Mat4::IDENTITY).is_err());
        s = skeleton();
        s.bones[0].flags = 2;
        assert!(evaluate(&s, &[], 0., Mat4::IDENTITY).is_err());
        s = skeleton();
        s.joints[0].class = 12;
        assert!(evaluate(&s, &[], 0., Mat4::IDENTITY).is_err());
    }

    /// The helper-joint bit must pose exactly like an ordinary bone, and the
    /// bits the client uses for scale and constraint controllers must still be
    /// refused.
    ///
    /// Measured over the shipped skeletons: 533 bones carry flag 1 and none of
    /// them has a negative-determinant local matrix, so the bit cannot mean
    /// "reflect". They are the leftover 3ds Max helpers (`Box01`, `Cylinder01`,
    /// `02_Bip02 Footsteps`), 515 of them unparented. Flags 2 and 3 do not occur
    /// in the effect archive, so they stay refused rather than guessed.
    #[test]
    fn helper_joint_bones_pose_but_scale_controllers_stay_refused() {
        let mut s = skeleton();
        s.bones[0].flags = 1;
        let posed = evaluate(&s, &[], 0., Mat4::IDENTITY).expect("a helper joint is still a bone");
        assert_eq!(posed.absolute.len(), s.bones.len());
        for flags in [2u8, 3] {
            s.bones[0].flags = flags;
            assert!(
                evaluate(&s, &[], 0., Mat4::IDENTITY).is_err(),
                "flag {flags} is a scale/constraint controller and must stay refused"
            );
        }
    }
}
