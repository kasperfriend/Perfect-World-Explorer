//! Dynamic point/directional diffuse and ordinary skin/rigid specular variants.
//! This module is not yet wired to world model drawing.
//! The four-input coefficients correspond to VS constants c7/c8/c10..c13.
#[derive(Clone, Copy, Debug)]
pub struct LightInput {
    pub world_position: [f32; 3],
    pub normal: [f32; 3],
    pub direction: [f32; 3],
    pub point_position: [f32; 3],
    pub attenuation: [f32; 3],
    pub other_ambient: [f32; 3],
    pub directional_diffuse: [f32; 3],
    pub point_diffuse: [f32; 3],
    pub point_ambient: [f32; 3],
    pub point_enabled: bool,
}
/// Scalar reference; the client's `_dl` shader has no range cutoff operation.
/// Zero-distance and zero-denominator are undefined on the original shader;
/// here we choose finite conservative values to avoid poisoning a frame.
pub fn vertex_light(p: LightInput) -> [f32; 3] {
    // Keep the finite fallback identical to WGSL. Retail rsq(0) behavior is
    // hardware-dependent; this guard is viewer safety, not recovered semantics.
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let length = dot(p.normal, p.normal).sqrt();
    let n = if length > 1e-6 {
        p.normal.map(|v| v / length)
    } else {
        [0.; 3]
    };
    let lambert = dot(n, p.direction.map(|v| -v)).max(0.);
    let delta = std::array::from_fn(|i| p.point_position[i] - p.world_position[i]);
    let d = dot(delta, delta).sqrt();
    let point_lambert = if d > 1e-6 {
        dot(n, delta.map(|v| v / d)).max(0.)
    } else {
        0.
    };
    let a = p.attenuation;
    let denom = a[0] + a[1] * d + a[2] * d * d;
    let attenuation = if p.point_enabled && denom > 1e-6 {
        1. / denom
    } else {
        0.
    };
    std::array::from_fn(|i| {
        (p.other_ambient[i]
            + lambert * p.directional_diffuse[i]
            + attenuation * (p.point_ambient[i] + point_lambert * p.point_diffuse[i]))
            .min(1.)
    })
}
/// Preserve the distinct alpha instructions in the non-specular shaders.
/// `alpha` contains c7.a, c8.a and c12.a; c11.a is forced to zero by binding.
/// Without a point light c8.a bypasses Lambert entirely. With a point light
/// the `_dl` variant multiplies all c8 channels by the directional Lambert.
pub fn vertex_light_rgba(p: LightInput, alpha: [f32; 3]) -> [f32; 4] {
    let rgb = vertex_light(p);
    let nl = p.normal.iter().map(|v| v * v).sum::<f32>().sqrt();
    let lambert = if nl > 1e-6 {
        -(0..3)
            .map(|i| p.normal[i] / nl * p.direction[i])
            .sum::<f32>()
    } else {
        0.
    }
    .max(0.);
    let d2 = (0..3)
        .map(|i| (p.point_position[i] - p.world_position[i]).powi(2))
        .sum::<f32>();
    let denom = p.attenuation[0] + p.attenuation[1] * d2.sqrt() + p.attenuation[2] * d2;
    let a = if p.point_enabled {
        alpha[0] + lambert * alpha[1] + if denom > 1e-6 { alpha[2] / denom } else { 0. }
    } else {
        alpha[0] + alpha[1]
    };
    [rgb[0], rgb[1], rgb[2], a.min(1.)]
}
/// Ordinary skin/rigid specular variants differ from `_ns_dl`: they clamp the
/// point attenuation denominator to at least one and preserve directional
/// alpha, even with a point light. Specular is a separate oD1 RGB output.
/// This is not the morph-specular shader, which has different instructions.
pub fn specular_variant_diffuse(mut p: LightInput, alpha: [f32; 3]) -> [f32; 4] {
    let d2 = (0..3)
        .map(|i| (p.point_position[i] - p.world_position[i]).powi(2))
        .sum::<f32>();
    let denom = p.attenuation[0] + p.attenuation[1] * d2.sqrt() + p.attenuation[2] * d2;
    if denom < 1. {
        p.attenuation = [1., 0., 0.];
    }
    let rgb = vertex_light(p);
    [rgb[0], rgb[1], rgb[2], (alpha[0] + alpha[1]).min(1.)]
}
/// Directional Blinn term from rigid_mesh_s / rigid_mesh_s_dl. The eye input
/// is the camera's forward direction, not a per-vertex vector to the eye.
pub fn directional_specular(
    p: LightInput,
    eye_direction: [f32; 3],
    power: f32,
    coefficient: [f32; 3],
) -> [f32; 3] {
    let dot = |a: [f32; 3], b: [f32; 3]| (0..3).map(|i| a[i] * b[i]).sum::<f32>();
    let nl = dot(p.normal, p.normal).sqrt();
    let h = std::array::from_fn(|i| -p.direction[i] - eye_direction[i]);
    let hl = dot(h, h).sqrt();
    if nl <= 1e-6 || hl <= 1e-6 || dot(p.normal, p.direction.map(|v| -v)) <= 0. {
        return [0.; 3];
    }
    let nh = (dot(p.normal, h) / (nl * hl)).max(0.);
    let spec = nh.powf(power.clamp(-128., 128.));
    coefficient.map(|c| (c * spec).min(1.))
}
/// Water transmission for geometry below a horizontal water plane.
///
/// This is the same eye-ray law used by the native grass/BMD refraction
/// shaders: exp2(-density * depth * ray_length / ray_vertical). Callers gate
/// it to the above-water clipped refraction pass.
pub fn water_transmittance(world: [f32; 3], eye: [f32; 3], water_height: f32, density: f32) -> f32 {
    let depth = (water_height - world[1]).max(0.);
    if depth <= 0. || density <= 0. {
        return 1.;
    }
    let delta = [eye[0] - world[0], eye[1] - world[1], eye[2] - world[2]];
    let distance = delta.iter().map(|value| value * value).sum::<f32>().sqrt();
    if delta[1] <= 1e-6 || distance <= 1e-6 {
        return 0.;
    }
    (-density * depth * distance / delta[1]).exp2()
}

/// Reusable WGSL equivalent, ready for the animated skin vertex path.
pub const WGSL: &str = r#"
fn model_vertex_light(pos:vec3<f32>, normal:vec3<f32>, direction:vec3<f32>, light_pos:vec3<f32>, att:vec3<f32>, ambient:vec3<f32>, dir_diff:vec3<f32>, point_diff:vec3<f32>, point_ambient:vec3<f32>, enabled:f32)->vec3<f32> {
 let nl=length(normal);let n=select(vec3<f32>(0.),normal/max(nl,0.000001),nl>0.000001);
 let delta=light_pos-pos;let d=length(delta);let l=select(vec3<f32>(0.),delta/max(d,0.000001),d>0.000001);
 let diffuse=max(dot(n,-direction),0.);let point=max(dot(n,l),0.);
 let denom=dot(att,vec3<f32>(1.,d,d*d));let factor=select(0.,1./max(denom,0.000001),enabled>0.5 && denom>0.000001);
 return min(ambient+dir_diff*diffuse+(point_ambient+point_diff*point)*factor,vec3<f32>(1.));
}
fn model_vertex_light_rgba(pos:vec3<f32>,normal:vec3<f32>,direction:vec3<f32>,light_pos:vec3<f32>,att:vec3<f32>,ambient:vec4<f32>,dir_diff:vec4<f32>,point_diff:vec4<f32>,point_ambient:vec4<f32>,enabled:f32)->vec4<f32> {
 let rgb=model_vertex_light(pos,normal,direction,light_pos,att,ambient.rgb,dir_diff.rgb,point_diff.rgb,point_ambient.rgb,enabled);
 let nl=length(normal);let n=select(vec3<f32>(0.),normal/max(nl,0.000001),nl>0.000001);
 let lambert=max(dot(n,-direction),0.);let d=length(light_pos-pos);
 let denom=dot(att,vec3<f32>(1.,d,d*d));
 var a=ambient.a+dir_diff.a;
 if enabled>0.5 {a=ambient.a+lambert*dir_diff.a; if denom>0.000001 {a+=point_ambient.a/denom;}}
 return vec4<f32>(rgb,min(a,1.));
}

fn model_specular_variant_diffuse(pos:vec3<f32>,normal:vec3<f32>,direction:vec3<f32>,light_pos:vec3<f32>,att:vec3<f32>,ambient:vec4<f32>,dir_diff:vec4<f32>,point_diff:vec4<f32>,point_ambient:vec4<f32>,enabled:f32)->vec4<f32> {
 let d=length(light_pos-pos);let denom=dot(att,vec3<f32>(1.,d,d*d));
 let adjusted=select(att,vec3<f32>(1.,0.,0.),denom<1.);
 let rgb=model_vertex_light(pos,normal,direction,light_pos,adjusted,ambient.rgb,dir_diff.rgb,point_diff.rgb,point_ambient.rgb,enabled);
 return vec4<f32>(rgb,min(ambient.a+dir_diff.a,1.));
}
fn model_directional_specular(normal:vec3<f32>,direction:vec3<f32>,eye_direction:vec3<f32>,power:f32,coefficient:vec3<f32>)->vec3<f32> {
 let nl=length(normal);let h=-direction-eye_direction;let hl=length(h);
 if nl<=0.000001 || hl<=0.000001 || dot(normal,-direction)<=0. {return vec3<f32>(0.);}
 let nh=max(dot(normal,h)/(nl*hl),0.);
 return min(coefficient*pow(nh,clamp(power,-128.,128.)),vec3<f32>(1.));
}
fn model_water_transmittance(world:vec3<f32>,eye:vec3<f32>,water_height:f32,density:f32)->f32 {
 let depth=max(water_height-world.y,0.);
 if depth<=0. || density<=0. {return 1.;}
 let delta=eye-world;
 let distance=length(delta);
 if delta.y<=0.000001 || distance<=0.000001 {return 0.;}
 return exp2(-density*depth*distance/delta.y);
}

"#;
#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> LightInput {
        LightInput {
            world_position: [0., 0., 0.],
            normal: [0., 1., 0.],
            direction: [0., -1., 0.],
            point_position: [0., 10., 0.],
            attenuation: [0., 0.4, 0.],
            other_ambient: [0.1, 0.2, 0.3],
            directional_diffuse: [0.2, 0.1, 0.],
            point_diffuse: [0.4, 0.2, 0.1],
            point_ambient: [0.04, 0.08, 0.12],
            point_enabled: true,
        }
    }
    #[test]
    fn point_directional_and_ambient_are_added_before_clamp() {
        let c = vertex_light(input());
        for (a, b) in c.into_iter().zip([0.41, 0.37, 0.355]) {
            assert!((a - b).abs() < 0.00001, "{a} {b}");
        }
    }
    #[test]
    fn no_point_and_reversed_normal() {
        let mut p = input();
        p.point_enabled = false;
        assert!((vertex_light(p)[0] - 0.3).abs() < 1e-6);
        p.normal = [0., -1., 0.];
        assert!((vertex_light(p)[0] - 0.1).abs() < 1e-6);
    }
    #[test]
    fn degenerate_normal_and_point_guards_match_shader_thresholds() {
        let mut p = input();
        p.point_enabled = false;
        for y in [0., 1e-7, 1e-6] {
            p.normal = [0., y, 0.];
            assert_eq!(vertex_light(p), p.other_ambient);
        }
        p = input();
        p.attenuation = [1., 0., 0.];
        let base = [0.34, 0.38, 0.42]; // other + directional + point ambient
        for y in [0., 1e-7, 1e-6] {
            p.point_position = [0., y, 0.];
            for (a, b) in vertex_light(p).into_iter().zip(base) {
                assert!((a - b).abs() < 1e-6);
            }
        }
        p.point_position = [0., 2e-6, 0.];
        assert!(vertex_light(p)[0] > base[0]);
    }
    #[test]
    fn nonpositive_denominator_disables_the_point_term() {
        let mut p = input();
        for constant in [-1., 0., 1e-7, 1e-6] {
            p.attenuation = [constant, 0., 0.];
            let actual = vertex_light(p);
            p.point_enabled = false;
            assert_eq!(actual, vertex_light(p));
            p.point_enabled = true;
        }
    }
    #[test]
    fn zero_distance_is_finite() {
        let mut p = input();
        p.point_position = p.world_position;
        for x in vertex_light(p) {
            assert!(x.is_finite());
        }
    }
    #[test]
    fn alpha_lambert_differs_between_point_and_nonpoint_variants() {
        let mut p = input();
        p.normal = [0., -1., 0.];
        p.point_enabled = false;
        assert!((vertex_light_rgba(p, [0.1, 0.6, 0.2])[3] - 0.7).abs() < 1e-6);
        p.point_enabled = true;
        assert!((vertex_light_rgba(p, [0.1, 0.6, 0.2])[3] - 0.15).abs() < 1e-6);
    }
    #[test]
    fn alpha_has_upper_clamp_but_no_invented_lower_clamp() {
        let mut p = input();
        p.point_enabled = false;
        assert_eq!(vertex_light_rgba(p, [2., 3., 0.])[3], 1.);
        assert_eq!(vertex_light_rgba(p, [-0.5, 0.2, 0.])[3], -0.3);
    }
    #[test]
    fn specular_variant_caps_point_gain_and_preserves_alpha() {
        let mut p = input();
        p.point_position = [0., 0.25, 0.];
        let d = specular_variant_diffuse(p, [0.1, 0.6, 0.2]);
        assert!((d[0] - 0.74).abs() < 1e-6);
        assert!((d[3] - 0.7).abs() < 1e-6);
        assert!(vertex_light(p)[0] > d[0]);
        p.normal = [0., -1., 0.];
        assert!((specular_variant_diffuse(p, [0.1, 0.6, 0.2])[3] - 0.7).abs() < 1e-6);
    }
    #[test]
    fn water_transmission_matches_the_recovered_base_two_eye_ray_law() {
        let transmission = water_transmittance([0., -10., 0.], [0., 10., 0.], 0., 0.075);
        assert!((transmission - (-0.75f32).exp2()).abs() < 1e-6);
        assert_eq!(
            water_transmittance([0., 1., 0.], [0., 10., 0.], 0., 0.075),
            1.
        );
        assert_eq!(
            water_transmittance([0., -1., 0.], [1., -1., 0.], 0., 0.075),
            0.
        );
        assert!(water_transmittance([0., -10., 0.], [10., 10., 0.], 0., 0.075) < transmission);
    }
    #[test]
    fn directional_specular_uses_camera_forward_and_lit_frontface_gate() {
        let mut p = input();
        assert_eq!(
            directional_specular(p, [0., -1., 0.], 10., [0.2, 0.3, 2.]),
            [0.2, 0.3, 1.]
        );
        p.normal = [0., -1., 0.];
        assert_eq!(
            directional_specular(p, [0., -1., 0.], 10., [1.; 3]),
            [0.; 3]
        );
        p.normal = [0., 1., 0.];
        assert_eq!(directional_specular(p, [0., 1., 0.], 10., [1.; 3]), [0.; 3]);
    }
}
