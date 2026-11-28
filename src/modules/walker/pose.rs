//! Character-specific pose adapter. Ordinary models keep using the validated
//! core evaluator. Player skeletons additionally contain fake/flipped bones and
//! controller-only joints; their authored transforms remain in place while
//! class-15 animation joints consume STCK tracks. No core pose rules are changed.

use crate::{animation::BoneTrack, skeleton::Skeleton, skeleton_pose::Pose};
use glam::{Mat4, Quat, Vec3};
use std::collections::VecDeque;

/// A bone-matrix controller in the same transposed-native matrix convention as
/// BON and EFACE data. Native `relative * controlled` is `controlled * relative`
/// here.
#[derive(Clone, Copy, Debug)]
pub struct BoneControl {
    pub bone: usize,
    pub matrix: Mat4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScaleType {
    None,
    Whole,
    Local,
    LocalNoOffset,
}

/// Old Angelica body scaling (`GetFlagNewBoneScale() == false`). `Whole` is
/// inherited by descendants; `Local` additionally scales child offsets;
/// `LocalNoOffset` affects only the bone's own geometry.
#[derive(Clone, Copy, Debug)]
pub struct BoneScale {
    pub bone: usize,
    pub kind: ScaleType,
    pub factor: Vec3,
}

fn validate_hierarchy(skeleton: &Skeleton) -> Result<(), String> {
    let count = skeleton.bones.len();
    let mut children = vec![Vec::new(); count];
    let mut queue = VecDeque::new();
    for (index, bone) in skeleton.bones.iter().enumerate() {
        if bone.parent == -1 {
            queue.push_back(index);
        } else {
            let parent = usize::try_from(bone.parent)
                .ok()
                .filter(|&parent| parent < count && parent != index)
                .ok_or("invalid character pose hierarchy")?;
            children[parent].push(index);
        }
    }
    let mut visited = 0;
    while let Some(index) = queue.pop_front() {
        visited += 1;
        queue.extend(children[index].iter().copied());
    }
    if visited != count {
        return Err("cyclic character pose hierarchy".into());
    }
    Ok(())
}

pub fn evaluate(
    skeleton: &Skeleton,
    tracks: &[BoneTrack],
    seconds: f32,
    model: Mat4,
) -> Result<Pose, String> {
    evaluate_controlled(skeleton, tracks, seconds, model, &[], &[])
}

pub fn evaluate_controlled(
    skeleton: &Skeleton,
    tracks: &[BoneTrack],
    seconds: f32,
    model: Mat4,
    controls: &[BoneControl],
    scales: &[BoneScale],
) -> Result<Pose, String> {
    if controls.is_empty()
        && scales.is_empty()
        && skeleton.bones.iter().all(|b| b.flags == 0)
        && skeleton.joints.iter().all(|j| j.class == 15)
    {
        validate_hierarchy(skeleton)?;
        return crate::skeleton_pose::evaluate(skeleton, tracks, seconds, model);
    }
    if !seconds.is_finite() || !model.is_finite() {
        return Err("nonfinite character pose input".into());
    }
    if skeleton.bones.iter().any(|b| b.flags & !0x03 != 0) {
        return Err("unknown character bone flags".into());
    }
    let count = skeleton.bones.len();
    let mut local: Vec<_> = skeleton
        .bones
        .iter()
        .map(|b| Mat4::from_cols_array(&b.relative))
        .collect();
    let mut assigned = vec![false; count];
    for track in tracks {
        let joint = usize::try_from(track.id)
            .ok()
            .and_then(|i| skeleton.joints.get(i))
            .ok_or("character track references an invalid joint")?;
        if joint.class != 15 {
            // Controlled/spring joints are driven by their controllers in the
            // client, not by an animation track. Walker retains their authored
            // default rather than misreading a controller as a rotation.
            continue;
        }
        let bone = usize::try_from(joint.child)
            .ok()
            .filter(|&i| i < count)
            .ok_or("character joint references an invalid child bone")?;
        if assigned[bone] {
            return Err("multiple character joints target one bone".into());
        }
        assigned[bone] = true;
        let position = track
            .position
            .sample(seconds)
            .ok_or("missing/nonfinite character position track")?;
        let rotation = track
            .rotation
            .sample(seconds)
            .ok_or("missing/nonfinite character rotation track")?;
        local[bone] =
            Mat4::from_rotation_translation(Quat::from_array(rotation), Vec3::from_array(position));
    }

    for control in controls {
        let local = local
            .get_mut(control.bone)
            .ok_or("character controller references an invalid bone")?;
        if !control.matrix.is_finite() {
            return Err("nonfinite character controller matrix".into());
        }
        *local = control.matrix * *local;
    }
    let mut scale_settings = vec![None; count];
    for scale in scales {
        let slot = scale_settings
            .get_mut(scale.bone)
            .ok_or("character scale references an invalid bone")?;
        if slot.is_some() {
            return Err("multiple character scales target one bone".into());
        }
        if !scale.factor.is_finite() {
            return Err("nonfinite character scale factor".into());
        }
        *slot = Some(*scale);
    }

    let mut topo_ordered = true;
    for (index, bone) in skeleton.bones.iter().enumerate() {
        if bone.parent != -1 {
            let parent = usize::try_from(bone.parent)
                .ok()
                .filter(|&i| i < count && i != index)
                .ok_or("invalid character pose hierarchy")?;
            if parent > index {
                topo_ordered = false;
            }
        }
        if !local[index].is_finite() || !Mat4::from_cols_array(&bone.initial).is_finite() {
            return Err("nonfinite character bone matrix".into());
        }
        if bone.flags & 0x02 != 0 {
            // A3DBone::BuildFrameUpToRootMatrix negates _11, _12, _13.
            local[index].x_axis.x = -local[index].x_axis.x;
            local[index].y_axis.x = -local[index].y_axis.x;
            local[index].z_axis.x = -local[index].z_axis.x;
        }
    }

    // Native old-scale evaluation keeps an unscaled up-to-root chain. The
    // scale matrix is applied only when forming each bone's absolute TM; using
    // absolute[parent] for children would incorrectly inherit LOCAL scales.
    let mut up_to_root = vec![Mat4::IDENTITY; count];
    let mut absolute = vec![Mat4::IDENTITY; count];
    let mut skin = vec![Mat4::IDENTITY; count];
    let mut accumulated_whole = vec![Vec3::ONE; count];
    let mut eval_bone = |i: usize| -> Result<(), String> {
        let parent = skeleton.bones[i].parent;
        let setting = scale_settings[i];
        let own_whole = match setting {
            Some(BoneScale {
                kind: ScaleType::Whole,
                factor,
                ..
            }) => factor,
            _ => Vec3::ONE,
        };
        let own_local = match setting {
            Some(BoneScale {
                kind: ScaleType::Local | ScaleType::LocalNoOffset,
                factor,
                ..
            }) => factor,
            _ => Vec3::ONE,
        };
        up_to_root[i] = if parent < 0 {
            accumulated_whole[i] = own_whole;
            local[i]
        } else {
            let parent = parent as usize;
            accumulated_whole[i] = own_whole * accumulated_whole[parent];
            let mut offset_scale = accumulated_whole[parent];
            if let Some(BoneScale {
                kind: ScaleType::Local,
                factor,
                ..
            }) = scale_settings[parent]
            {
                offset_scale *= factor;
            }
            // SCALE_LOCAL_NOOFFSET is deliberately excluded here.
            local[i].w_axis.x *= offset_scale.x;
            local[i].w_axis.y *= offset_scale.y;
            local[i].w_axis.z *= offset_scale.z;
            up_to_root[parent] * local[i]
        };
        let bone_scale = own_local * accumulated_whole[i];
        let abs_m = if bone_scale == Vec3::ONE {
            model * up_to_root[i]
        } else {
            model * up_to_root[i] * Mat4::from_scale(bone_scale)
        };
        absolute[i] = abs_m;
        skin[i] = abs_m * Mat4::from_cols_array(&skeleton.bones[i].initial);
        if !abs_m.is_finite() || !skin[i].is_finite() {
            return Err("nonfinite composed character pose".into());
        }
        Ok(())
    };
    if topo_ordered {
        for i in 0..count {
            eval_bone(i)?;
        }
    } else {
        let mut children = vec![Vec::new(); count];
        let mut queue = VecDeque::with_capacity(count);
        for (index, bone) in skeleton.bones.iter().enumerate() {
            if bone.parent == -1 {
                queue.push_back(index);
            } else {
                children[bone.parent as usize].push(index);
            }
        }
        let mut visited = 0;
        while let Some(i) = queue.pop_front() {
            eval_bone(i)?;
            queue.extend(children[i].iter().copied());
            visited += 1;
        }
        if visited != count {
            return Err("cyclic character pose hierarchy".into());
        }
    }
    Ok(Pose {
        absolute,
        skin,
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        animation::KeyTrack,
        skeleton::{Bone, Joint},
    };

    fn skeleton(class: u32) -> Skeleton {
        Skeleton {
            version: 6,
            start: 0,
            end: 1,
            fps: 30,
            bones: vec![Bone {
                name: "Bip01 Head".into(),
                flags: 0,
                parent: -1,
                first_joint: 0,
                relative: Mat4::from_translation(Vec3::new(0., 1.7, 0.)).to_cols_array(),
                initial: Mat4::IDENTITY.to_cols_array(),
                children: vec![],
            }],
            joints: vec![Joint {
                class,
                name: "head".into(),
                parent: -1,
                child: 0,
                sibling: -1,
            }],
            hooks: vec![],
            embedded_tracks: vec![],
        }
    }

    fn track() -> BoneTrack {
        BoneTrack {
            id: 0,
            position: KeyTrack {
                fps: 30,
                keys: vec![[9., 9., 9.]],
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
    fn absolute_head_matrix_includes_character_world_transform() {
        let pose = evaluate(
            &skeleton(15),
            &[],
            0.,
            Mat4::from_translation(Vec3::new(4., 2., -3.)),
        )
        .unwrap();
        assert_eq!(
            pose.absolute[0].transform_point3(Vec3::ZERO),
            Vec3::new(4., 3.7, -3.)
        );
    }

    #[test]
    fn controller_only_joint_keeps_authored_default_instead_of_track() {
        let pose = evaluate(&skeleton(12), &[track()], 0., Mat4::IDENTITY).unwrap();
        assert_eq!(
            pose.absolute[0].transform_point3(Vec3::ZERO),
            Vec3::new(0., 1.7, 0.)
        );
    }

    fn two_bones() -> Skeleton {
        let mut result = skeleton(12);
        result.bones[0].relative = Mat4::from_translation(Vec3::X).to_cols_array();
        result.bones[0].initial = Mat4::IDENTITY.to_cols_array();
        result.bones.push(crate::skeleton::Bone {
            name: "child".into(),
            flags: 0,
            parent: 0,
            first_joint: -1,
            relative: Mat4::from_translation(Vec3::X * 2.0).to_cols_array(),
            initial: Mat4::IDENTITY.to_cols_array(),
            children: vec![],
        });
        result
    }

    #[test]
    fn local_scale_offsets_children_without_scaling_their_geometry() {
        let pose = evaluate_controlled(
            &two_bones(),
            &[],
            0.0,
            Mat4::IDENTITY,
            &[],
            &[BoneScale {
                bone: 0,
                kind: ScaleType::Local,
                factor: Vec3::splat(2.0),
            }],
        )
        .unwrap();
        assert_eq!(pose.absolute[1].transform_point3(Vec3::ZERO), Vec3::X * 5.0);
        assert_eq!(pose.absolute[1].x_axis.x, 1.0);
    }

    #[test]
    fn local_no_offset_and_whole_scale_have_distinct_child_semantics() {
        let no_offset = evaluate_controlled(
            &two_bones(),
            &[],
            0.0,
            Mat4::IDENTITY,
            &[],
            &[BoneScale {
                bone: 0,
                kind: ScaleType::LocalNoOffset,
                factor: Vec3::splat(2.0),
            }],
        )
        .unwrap();
        assert_eq!(
            no_offset.absolute[1].transform_point3(Vec3::ZERO),
            Vec3::X * 3.0
        );
        assert_eq!(no_offset.absolute[1].x_axis.x, 1.0);

        let whole = evaluate_controlled(
            &two_bones(),
            &[],
            0.0,
            Mat4::IDENTITY,
            &[],
            &[BoneScale {
                bone: 0,
                kind: ScaleType::Whole,
                factor: Vec3::splat(2.0),
            }],
        )
        .unwrap();
        assert_eq!(
            whole.absolute[1].transform_point3(Vec3::ZERO),
            Vec3::X * 5.0
        );
        assert_eq!(whole.absolute[1].x_axis.x, 2.0);
    }

    #[test]
    fn malformed_parent_cycles_are_rejected_before_composition() {
        let mut skeleton = two_bones();
        skeleton.bones[0].parent = 1;
        let error = evaluate(&skeleton, &[], 0.0, Mat4::IDENTITY)
            .err()
            .expect("a parent cycle must be rejected");
        assert!(error.contains("cyclic"));
    }
}
