//! A3D skin-mesh four-matrix vertex deformation, isolated from rendering.
//! Retail `skin_mesh_m4_ns_dl` computes weight 4 as 1-w0-w1-w2, then
//! transforms position and normal by each palette matrix and sums them.
//! Rigid deformation uses one already selected skeleton-space matrix.
//! Matrix generation, skeleton/animation loading and drawing live elsewhere.
use glam::{Mat4, Vec3, Vec4};

#[derive(Clone, Copy, Debug)]
pub struct PosedVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

pub fn deform(
    position: [f32; 3],
    normal: [f32; 3],
    first_weights: [f32; 3],
    joints: [u8; 4],
    palette: &[Mat4],
) -> Result<PosedVertex, &'static str> {
    if !position
        .iter()
        .chain(normal.iter())
        .chain(first_weights.iter())
        .all(|v| v.is_finite())
    {
        return Err("nonfinite skin vertex");
    }
    let weights = [
        first_weights[0],
        first_weights[1],
        first_weights[2],
        1. - first_weights.iter().sum::<f32>(),
    ];
    let mut p = Vec3::ZERO;
    let mut n = Vec3::ZERO;
    for i in 0..4 {
        let m = palette
            .get(joints[i] as usize)
            .ok_or("skin palette index out of bounds")?;
        if !m.is_finite() {
            return Err("nonfinite skin matrix");
        }
        if weights[i] == 0.0 {
            continue;
        }
        p += (*m * Vec4::from((Vec3::from_array(position), 1.))).truncate() * weights[i];
        n += (*m * Vec4::from((Vec3::from_array(normal), 0.))).truncate() * weights[i];
    }
    if !p.is_finite() || !n.is_finite() {
        return Err("nonfinite deformed vertex");
    }
    Ok(PosedVertex {
        position: p.to_array(),
        normal: n.normalize_or_zero().to_array(),
    })
}

/// Retail rigid_mesh_* transforms with one skeleton-space matrix and
/// normalizes the resulting normal. No skin-local palette or vertex weights.
pub fn deform_rigid(
    position: [f32; 3],
    normal: [f32; 3],
    matrix: Mat4,
) -> Result<PosedVertex, &'static str> {
    if !position.iter().chain(normal.iter()).all(|v| v.is_finite()) {
        return Err("nonfinite skin vertex");
    }
    if !matrix.is_finite() {
        return Err("nonfinite skin matrix");
    }
    let p = matrix.transform_point3(Vec3::from_array(position));
    let n = matrix.transform_vector3(Vec3::from_array(normal));
    if !p.is_finite() || !n.is_finite() {
        return Err("nonfinite deformed vertex");
    }
    Ok(PosedVertex {
        position: p.to_array(),
        normal: n.normalize_or_zero().to_array(),
    })
}

pub const WGSL: &str = r#"
struct SkinResult {position:vec3<f32>, normal:vec3<f32>};
fn deform_skin(pos:vec3<f32>,normal:vec3<f32>,weights:vec3<f32>,m0:mat4x4<f32>,m1:mat4x4<f32>,m2:mat4x4<f32>,m3:mat4x4<f32>)->SkinResult {
 let w=vec4<f32>(weights,1.-weights.x-weights.y-weights.z);
 let p=(m0*vec4<f32>(pos,1.)).xyz*w.x+(m1*vec4<f32>(pos,1.)).xyz*w.y+(m2*vec4<f32>(pos,1.)).xyz*w.z+(m3*vec4<f32>(pos,1.)).xyz*w.w;
 let n=(m0*vec4<f32>(normal,0.)).xyz*w.x+(m1*vec4<f32>(normal,0.)).xyz*w.y+(m2*vec4<f32>(normal,0.)).xyz*w.z+(m3*vec4<f32>(normal,0.)).xyz*w.w;
 let l=length(n);var result:SkinResult;result.position=p;result.normal=vec3<f32>(0.);if l>0. {result.normal=n/l;}return result;
}
"#;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn weights_and_translation_do_not_translate_normals() {
        let p = [
            Mat4::from_translation(Vec3::new(4., 0., 0.)),
            Mat4::from_translation(Vec3::new(0., 4., 0.)),
            Mat4::IDENTITY,
            Mat4::from_translation(Vec3::new(0., 0., 4.)),
        ];
        let out = deform(
            [1., 2., 3.],
            [0., 2., 0.],
            [0.25, 0.5, 0.],
            [0, 1, 2, 3],
            &p,
        )
        .unwrap();
        assert_eq!(out.position, [2., 4., 4.]);
        assert_eq!(out.normal, [0., 1., 0.]);
    }
    #[test]
    fn rejects_bad_indices_and_nonfinite_values() {
        assert!(deform(
            [0.; 3],
            [0.; 3],
            [1., 0., 0.],
            [0, 0, 0, 1],
            &[Mat4::IDENTITY]
        )
        .is_err());
        assert!(deform(
            [f32::NAN, 0., 0.],
            [0.; 3],
            [1., 0., 0.],
            [0; 4],
            &[Mat4::IDENTITY]
        )
        .is_err());
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    #[test]
    fn tiny_nonzero_normals_are_not_erased_and_overflow_fails() {
        let m = [Mat4::IDENTITY];
        let v = deform([0.; 3], [0., 1e-8, 0.], [1., 0., 0.], [0; 4], &m).unwrap();
        assert_eq!(v.normal, [0., 1., 0.]);
        assert!(deform([1.; 3], [0., 1., 0.], [f32::MAX, f32::MAX, 0.], [0; 4], &m).is_err());
    }
}
