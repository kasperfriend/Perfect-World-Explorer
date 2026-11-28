//! Validated retained-asset -> ordinary drawing contract. Animation time and
//! action selection are caller inputs, never inferred from the first SMD action.
use crate::{
    model_asset::{FaceShader, ModelAsset, PosedMesh},
    skin,
};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}
pub struct PreparedMesh<'a> {
    pub skin: usize,
    pub mesh: usize,
    pub vertices: Vec<Vertex>,
    pub indices: Cow<'a, [u16]>,
    pub material: &'a skin::Material,
    pub color: u32,
    pub texture_paths: [String; 2],
    pub diffuse_alpha_only: bool,
    pub face_shader: Option<&'a FaceShader>,
}
/// Validate the complete set before any GPU uploads. Missing/duplicate poses,
/// changed topology and unsupported render overrides are errors, not permission
/// to draw a partial or incorrectly lit model.
pub fn prepare<'a>(
    asset: &'a ModelAsset,
    poses: &[PosedMesh],
) -> Result<Vec<PreparedMesh<'a>>, String> {
    let settings = asset.descriptor.render_settings();
    // Authored blend factors are applied by the alpha queue (0x7a76a0), so a
    // non-default pair is not itself unsupported. Modes with no pipeline
    // equivalent still fail here instead of being approximated.
    crate::model_queue::blend_factor(settings.source_blend)?;
    crate::model_queue::blend_factor(settings.destination_blend)?;
    if settings.original_color >> 24 != 255
        || asset.mesh_tints.values().any(|color| color >> 24 != 255)
    {
        return Err("ordinary model draw requires opaque ARGB color overrides".into());
    }
    let expected: usize = asset.skins.iter().map(|s| s.data.meshes.len()).sum();
    if poses.len() != expected {
        return Err("model pose does not cover every retained mesh".into());
    }
    let mut seen = std::collections::HashSet::with_capacity(expected);
    let mut result = Vec::with_capacity(expected);
    for pose in poses {
        if !seen.insert((pose.skin, pose.mesh)) {
            return Err("duplicate model mesh pose".into());
        }
        let skin = asset
            .skins
            .get(pose.skin)
            .ok_or("posed skin index out of range")?;
        let mesh = skin
            .data
            .meshes
            .get(pose.mesh)
            .ok_or("posed mesh index out of range")?;
        if let Some(bone) = mesh.rigid_bone {
            if usize::try_from(bone)
                .ok()
                .filter(|&i| i <= asset.skeleton.bones.len())
                .is_none()
            {
                return Err("rigid bone index out of range".into());
            }
        }
        if mesh.vertices.len() != pose.vertices.len()
            || mesh.vertices.is_empty()
            || mesh.indices.is_empty()
            || mesh.indices.len() % 3 != 0
            || mesh
                .indices
                .iter()
                .any(|&i| i as usize >= mesh.vertices.len())
        {
            return Err("model pose/topology mismatch".into());
        }
        let material = usize::try_from(mesh.material)
            .ok()
            .and_then(|i| skin.data.materials.get(i))
            .ok_or("model mesh material index out of range")?;
        if !(material.power.is_finite()
            && material.ambient.iter().all(|v| v.is_finite())
            && material.diffuse.iter().all(|v| v.is_finite())
            && material.emissive.iter().all(|v| v.is_finite())
            && material.specular.iter().all(|v| v.is_finite()))
        {
            return Err("nonfinite model material".into());
        }
        let texture = usize::try_from(mesh.texture)
            .map_err(|_| "untextured ordinary model draw is not implemented")?;
        let texture_paths = asset.texture_paths(pose.skin, texture)?;
        let face_shader = asset.face_shaders.get(&(pose.skin, pose.mesh));
        // Dedicated native pixel shaders consume their own constants. Native
        // does not also modulate the material by the part/body colour.
        let color = if face_shader.is_some() {
            u32::MAX
        } else {
            asset.mesh_color(pose.skin, pose.mesh, settings.original_color)
        };
        if color >> 24 != 255 {
            return Err("ordinary model draw requires an opaque mesh color".into());
        }
        let mut vertices = Vec::with_capacity(pose.vertices.len());
        for (source, posed) in mesh.vertices.iter().zip(&pose.vertices) {
            let p = posed.position;
            let n = posed.normal;
            let uv = source.uv;
            if !(p[0].is_finite()
                && p[1].is_finite()
                && p[2].is_finite()
                && n[0].is_finite()
                && n[1].is_finite()
                && n[2].is_finite()
                && uv[0].is_finite()
                && uv[1].is_finite())
            {
                return Err("nonfinite posed model geometry/UV".into());
            }
            vertices.push(Vertex {
                position: posed.position,
                normal: posed.normal,
                uv: source.uv,
            });
        }
        if asset.hidden_meshes.contains(&(pose.skin, pose.mesh)) {
            continue;
        }
        let indices: Cow<'_, [u16]> = crate::model_asset::indices_without_nonfinite_weights(mesh)
            .map(Cow::Owned)
            .unwrap_or_else(|| Cow::Borrowed(mesh.indices.as_slice()));
        // A fault can remove the last triangle without invalidating the rest
        // of the model. Do not submit an empty GPU mesh for that native no-op.
        if indices.is_empty() {
            continue;
        }
        result.push(PreparedMesh {
            skin: pose.skin,
            mesh: pose.mesh,
            vertices,
            indices,
            material,
            color,
            texture_paths,
            diffuse_alpha_only: asset.diffuse_alpha_meshes.contains(&(pose.skin, pose.mesh)),
            face_shader,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_draw_fixture::fixture;
    #[test]
    fn no_active_action_uses_authored_bind_pose_and_world_transform() {
        let (asset, _) = fixture();
        let poses = asset
            .pose_bind(glam::Mat4::from_translation(glam::Vec3::new(10., 4., -2.)))
            .unwrap();
        let prepared = prepare(&asset, &poses).unwrap();
        assert_eq!(prepared[0].vertices[2].position, [12., 4., -2.]);
    }
    #[test]
    fn rigid_uses_direct_skeleton_inverse_bind_not_skin_name_map() {
        let (mut asset, _) = fixture();
        let t = |x| glam::Mat4::from_translation(glam::Vec3::X * x);
        asset.skeleton.bones.push(crate::skeleton::Bone {
            name: "root".into(),
            flags: 0,
            parent: -1,
            first_joint: -1,
            relative: t(5.).to_cols_array(),
            initial: t(-3.).to_cols_array(),
            children: vec![],
        });
        asset.skins[0].data.meshes[0].rigid_bone = Some(0);
        asset.skeleton.bones.push(crate::skeleton::Bone {
            name: "other".into(),
            flags: 0,
            parent: -1,
            first_joint: -1,
            relative: t(99.).to_cols_array(),
            initial: t(-5.).to_cols_array(),
            children: vec![],
        });
        // Neither this valid reordered mapping nor weighted joint bytes may
        // alter rigid ID 0. Vertices already live in skeleton bind space.
        asset.skins[0].data.bones = vec!["other".into()];
        for v in &mut asset.skins[0].data.meshes[0].vertices {
            v.joints = [255; 4];
            v.weights = [0.; 4];
            v.normal = [0., 3., 0.];
        }
        let p = asset.pose_bind(t(10.)).unwrap();
        assert_eq!(p[0].vertices[2].position, [14., 0., 0.]);
        assert_eq!(p[0].vertices[2].normal, [0., 1., 0.]);
        assert!(prepare(&asset, &p).is_ok());
        // Final skeleton slot is the model transform, not the last real bone.
        asset.skins[0].data.meshes[0].rigid_bone = Some(2);
        let p = asset.pose_bind(t(10.)).unwrap();
        assert_eq!(p[0].vertices[2].position, [12., 0., 0.]);
        asset.skins[0].data.bones = vec!["missing".into()];
        assert!(asset.pose_bind(t(10.)).is_err());
        asset.skins[0].data.bones.clear();
        for bone in [-1, 3] {
            asset.skins[0].data.meshes[0].rigid_bone = Some(bone);
            assert!(asset.pose_bind(t(10.)).is_err());
            assert!(prepare(&asset, &p).is_err());
        }
    }
    #[test]
    fn joins_posed_geometry_to_authored_uv_material_and_texture() {
        let (asset, poses) = fixture();
        let p = prepare(&asset, &poses).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].vertices[2].position, [2., 2., 3.]);
        assert_eq!(p[0].vertices[2].normal, [0., 0., 1.]);
        assert_eq!(p[0].vertices[2].uv, [1., 0.]);
        assert_eq!(
            p[0].texture_paths,
            ["models/a/tex_a/a.dds", "models/a/textures/a.dds"]
        );
        assert!(p[0].material.two_sided);
        assert_eq!(p[0].indices.as_ref(), [0, 1, 2]);
    }
    #[test]
    fn native_face_shader_owns_skin_color_without_generic_material_tint() {
        let (mut asset, poses) = fixture();
        let color = 0xfff6f0e9;
        asset.mesh_tints.insert((0, 0), color);
        asset
            .face_shaders
            .insert((0, 0), FaceShader::Face { color });
        let prepared = prepare(&asset, &poses).unwrap();
        assert_eq!(prepared[0].color, u32::MAX);
        assert_eq!(prepared[0].face_shader, Some(&FaceShader::Face { color }));
    }

    #[test]
    fn native_mouth_shader_keeps_skin_lip_highlight_and_gloss_separate() {
        let (mut asset, poses) = fixture();
        let mouth = FaceShader::Mouth {
            color: 0xffe19486,
            skin_color: 0xfff6f0e9,
            highlight: "facedata/mouth-a.dds".into(),
        };
        assert_eq!(mouth.highlight(), Some("facedata/mouth-a.dds"));
        assert_eq!(mouth.gloss(), Some("textures/face/lipreflect.tga"));
        asset.face_shaders.insert((0, 0), mouth.clone());
        let prepared = prepare(&asset, &poses).unwrap();
        assert_eq!(prepared[0].color, u32::MAX);
        assert_eq!(prepared[0].face_shader, Some(&mouth));
    }

    #[test]
    fn native_moustache_shader_preserves_skin_color_and_has_no_gloss() {
        let (mut asset, poses) = fixture();
        let moustache = FaceShader::Moustache {
            color: 0xffe8c6ae,
            skin_color: 0xfff6f0e9,
            highlight: "facedata/moustache-a.dds".into(),
        };
        assert_eq!(moustache.highlight(), Some("facedata/moustache-a.dds"));
        assert_eq!(moustache.gloss(), None);
        asset.face_shaders.insert((0, 0), moustache.clone());
        let prepared = prepare(&asset, &poses).unwrap();
        assert_eq!(prepared[0].color, u32::MAX);
        assert_eq!(prepared[0].face_shader, Some(&moustache));
    }

    #[test]
    fn native_equipment_alpha_mode_is_per_mesh() {
        let (mut asset, poses) = fixture();
        assert!(!prepare(&asset, &poses).unwrap()[0].diffuse_alpha_only);
        asset.diffuse_alpha_meshes.insert((0, 0));
        assert!(prepare(&asset, &poses).unwrap()[0].diffuse_alpha_only);
        asset.diffuse_alpha_meshes.insert((1, 0));
        assert!(prepare(&asset, &poses).unwrap()[0].diffuse_alpha_only);
        asset.diffuse_alpha_meshes.remove(&(0, 0));
        assert!(!prepare(&asset, &poses).unwrap()[0].diffuse_alpha_only);
    }
    #[test]
    fn nonfinite_weight_recovery_preserves_unrelated_triangles_and_source_data() {
        let (mut asset, mut poses) = fixture();
        let mesh = &mut asset.skins[0].data.meshes[0];
        mesh.vertices = (0..6)
            .map(|i| skin::Vertex {
                position: [i as f32, 0., 0.],
                normal: [0., 1., 0.],
                uv: [i as f32 / 5., 0.],
                weights: [1., 0., 0., 0.],
                joints: [0; 4],
            })
            .collect();
        mesh.indices = vec![0, 1, 2, 3, 4, 5];
        mesh.vertices[2].weights = [f32::NAN; 4];
        poses[0].vertices = (0..6)
            .map(|i| crate::skin_pose::PosedVertex {
                position: [i as f32, 2., 3.],
                normal: [0., 0., 1.],
            })
            .collect();
        let prepared = prepare(&asset, &poses).unwrap();
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].indices.as_ref(), [3, 4, 5]);
        assert_eq!(asset.skins[0].data.meshes[0].indices, [0, 1, 2, 3, 4, 5]);
        assert!(asset.skins[0].data.meshes[0].vertices[2].weights[0].is_nan());
    }

    #[test]
    fn hidden_native_skin_is_validated_but_not_submitted() {
        let (mut asset, mut poses) = fixture();
        asset.hidden_meshes.insert((0, 0));
        assert!(prepare(&asset, &poses).unwrap().is_empty());
        poses[0].vertices.pop();
        assert!(prepare(&asset, &poses).is_err());
    }
    #[test]
    fn rejects_incomplete_duplicate_and_changed_poses() {
        let (mut asset, mut poses) = fixture();
        assert!(prepare(&asset, &[]).is_err());
        poses[0].vertices.pop();
        assert!(prepare(&asset, &poses).is_err());
        let (a, p) = fixture();
        asset.skins[0]
            .data
            .meshes
            .push(a.skins[0].data.meshes[0].clone());
        let (_, p2) = fixture();
        let duplicates = vec![
            p.into_iter().next().unwrap(),
            p2.into_iter().next().unwrap(),
        ];
        assert!(prepare(&asset, &duplicates).is_err());
    }
    #[test]
    fn rejects_unverified_overrides_and_invalid_indices_or_floats() {
        for kind in 0..7 {
            let (mut a, mut p) = fixture();
            match kind {
                0 => a.descriptor.original_color = Some(0x80808080),
                1 => a.descriptor.destination_blend = Some(11),
                2 => a.skins[0].data.meshes[0].material = -1,
                3 => a.skins[0].data.meshes[0].texture = -1,
                4 => a.skins[0].data.meshes[0].indices[2] = 99,
                5 => p[0].vertices[0].position[0] = f32::NAN,
                _ => a.skins[0].data.meshes[0].vertices[0].uv[1] = f32::INFINITY,
            }
            assert!(prepare(&a, &p).is_err(), "invalid draw {kind}");
        }
    }
}
