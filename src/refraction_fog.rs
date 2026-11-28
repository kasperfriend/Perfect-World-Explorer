//! Retail grass refraction vertex fog (`grass_wave_refract`, 0x651319).
//! c91 = (1, water_height, 0.075, 0). The shader uses displaced Y for
//! water depth but the original vertex for the direction toward the eye.
//!
//! The exact sequence, from `shaders/vs/grass_wave_refract.txt` in the client's
//! shaders.pck:
//!
//! ```text
//! sub r1.y, c91.y, r0.y        ; D = waterY - displacedY
//! sub r0, c9, v0               ; eye - original
//! dp3 r0.w, r0, r0
//! rsq r0.w, r0.w               ; 1/|eye-original|
//! mul r0.xyz, r0, r0.w         ; unit direction
//! rcp r0.y, r0.y               ; 1 / unit y
//! mul r1.z, r1.y, r0.y         ; D / unit_y
//! mul r1.z, r1.z, c91.z        ; * density
//! exp r1.z, r1.z
//! rcp r1.z, r1.z               ; exp(-D * density / unit_y)
//! add oFog, r1.z, c91.w        ; + 0.0
//! ```
//!
//! Note the base: D3D shader assembly's `exp` is `2^x`, *not* the natural
//! exponential (`expp` is the same base at partial precision). Writing
//! `e^(-density*D*L/dy)` is close but not equal — for eye `(0,10,0)`, vertex
//! `(0,-10,0)`, water `0` the shader gives `0.59460`, while the natural form
//! gives `0.47237`.
pub const DENSITY: f32 = 0.075;
pub fn grass_transmittance(
    original: [f32; 3],
    displaced: [f32; 3],
    eye: [f32; 3],
    water_height: f32,
) -> f32 {
    let delta: [f32; 3] = std::array::from_fn(|i| eye[i] - original[i]);
    let depth = water_height - displaced[1];
    // Coincident/horizontal rays are undefined on retail rsq/rcp hardware.
    // Choose finite limits rather than propagating NaN through rasterization.
    if delta[1].abs() <= 1e-6 {
        return if depth > 0. { 0. } else { 1. };
    }
    let distance = delta.iter().map(|v| v * v).sum::<f32>().sqrt();
    // rcp of the unit y is the only place the ray direction enters: the shader
    // never multiplies by the full ray length, so `distance` cancels out.
    let unit_y = delta[1] / distance;
    (-DENSITY * depth / unit_y).exp2()
}
/// Lit-model refraction clamps above-plane vertex depth before exponentiation,
/// unlike the grass shader. The returned factor is still vertex-interpolated.
///
/// `shaders/vs/lm_refract_vs.txt` zeroes the depth rather than clamping it:
/// `sub r1.y, c0.y, v0.y` / `sge r1.x, c0.y, v0.y` / `mul r1.y, r1.y, r1.x`.
/// Both forms give `D = 0` above the plane, so the shared evaluator is exact
/// for either reading.
pub fn bmd_transmittance(world: [f32; 3], eye: [f32; 3], water_height: f32) -> f32 {
    let clamped = [world[0], world[1].min(water_height), world[2]];
    grass_transmittance(world, clamped, eye, water_height)
}
/// Terrain refraction (`shaders/vs/terrain_refract_vs.txt`) flattens geometry
/// onto the water plane with `mad r0.y, r1.x, r1.y, v0.y` and emits two values:
/// the fog transmittance and a depth-only darkness stored in the whole diffuse
/// output. Both are base two, because D3D's `exp` instruction is `2^x`:
///
/// ```text
/// mul r1.z, r1.y, c0.z   ; depth * density
/// add r1.z, r1.z, r1.z   ; doubled
/// exp r1.z, r1.z
/// rcp oD0, r1.z          ; darkness = exp(-2 * depth * density)
///
/// mul r1.z, r1.y, r0.y   ; depth / unit_y
/// mul r1.z, r1.z, c0.z
/// exp r1.z, r1.z
/// rcp r1.z, r1.z         ; fog     = exp(-depth * density / unit_y)
/// ```
///
/// Both are base two, because D3D shader assembly's `exp` is `2^x`. For a
/// 10-unit depth the darkness term is `2^(-1.5) = 0.35355`, not the natural
/// `0.22313`.
pub fn terrain_vertex(world: [f32; 3], eye: [f32; 3], water_height: f32) -> ([f32; 3], [f32; 2]) {
    let projected = [world[0], world[1].min(water_height), world[2]];
    let depth = water_height - projected[1];
    (
        projected,
        [
            bmd_transmittance(world, eye, water_height),
            (-2. * DENSITY * depth).exp2(),
        ],
    )
}
pub const WGSL: &str = r#"
fn grass_refraction_transmittance(original:vec3<f32>,displaced:vec3<f32>,eye:vec3<f32>,water_height:f32)->f32 {
 // grass_wave_refract: exp(-(waterY-displacedY)*density/unit_y). D3D's `exp`
 // is 2^x, so this is exp2 in WGSL, not the natural exponential.
 let delta=eye-original;let depth=water_height-displaced.y;
 if abs(delta.y)<=0.000001 {return select(1.,0.,depth>0.);}
 let unit_y=delta.y/length(delta);
 return exp2(-0.075*depth/unit_y);
}
fn grass_pass_fog(linear:vec3<f32>,world:vec3<f32>,transmittance:f32)->vec3<f32> {
 if camera.clip.y>=0. || camera.fog_range.z<0.5 {return fogged(linear,world);}
 let c=max(linear,vec3<f32>(0.));
 let encoded=select(c*12.92,1.055*pow(c,vec3<f32>(1./2.4))-0.055,c>vec3<f32>(0.0031308));
 let v=mix(camera.fog_color.rgb,encoded,clamp(transmittance,0.,1.));
 return select(v/12.92,pow((max(v,vec3<f32>(0.))+0.055)/1.055,vec3<f32>(2.4)),v>vec3<f32>(0.04045));
}
"#;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vertical_ray_uses_base_two_and_the_shaders_unit_y() {
        let f = grass_transmittance([0., -10., 0.], [0., -10., 0.], [0., 10., 0.], 0.);
        // 2^(-0.075 * 10 / 1.0) exactly, because unit_y is 1 for a vertical ray.
        assert!((f - (-0.75f32).exp2()).abs() < 1e-6);
        // The natural-exponential form is a different number, not a rounding
        // variant: `exp` in D3D shader assembly is 2^x, not e^x.
        assert!((f - (-0.75f32).exp()).abs() > 0.1);
    }
    #[test]
    fn oblique_ray_uses_only_the_unit_y_of_the_eye_direction() {
        let f = grass_transmittance([0., -10., 0.], [0., -10., 0.], [10., 10., 0.], 0.);
        // unit = (10,20,0)/sqrt(500), so unit_y = 20/sqrt(500).
        let unit_y = 20. / 500f32.sqrt();
        assert!((f - (-DENSITY * 10. / unit_y).exp2()).abs() < 1e-6);
        // A longer ray to the same height therefore darkens, but never reaches
        // the full eye-to-vertex distance term.
        assert!(f < (-0.75f32).exp2());
        assert!(f > (-DENSITY * 500f32.sqrt()).exp2());
    }
    #[test]
    fn displaced_depth_keeps_original_ray_and_no_vertex_clamp() {
        let f = grass_transmittance([0., -10., 0.], [0., -8., 0.], [0., 10., 0.], 0.);
        assert!((f - (-0.6f32).exp2()).abs() < 1e-6);
        assert!(grass_transmittance([0., 1., 0.], [0., 1., 0.], [0., 10., 0.], 0.) > 1.);
    }
    #[test]
    fn translating_camera_geometry_and_water_does_not_change_fog() {
        let a = grass_transmittance([1., -8., 3.], [1., -7., 3.], [10., 10., 5.], 0.);
        let b = grass_transmittance([21., 92., -17.], [21., 93., -17.], [30., 110., -15.], 100.);
        assert_eq!(a, b);
    }
    #[test]
    fn undefined_horizontal_and_coincident_rays_have_finite_limits() {
        assert_eq!(grass_transmittance([0.; 3], [0., -1., 0.], [0.; 3], 0.), 0.);
        assert_eq!(grass_transmittance([0.; 3], [0.; 3], [1., 0., 0.], 0.), 1.);
    }
    #[test]
    fn bmd_clamps_above_plane_depth_before_interpolation() {
        let p = [0., 1., 0.];
        let eye = [0., 10., 0.];
        assert_eq!(bmd_transmittance(p, eye, 0.), 1.);
        assert!(grass_transmittance(p, p, eye, 0.) > 1.);
        let p = [0., -2., 0.];
        assert_eq!(
            bmd_transmittance(p, eye, 0.),
            grass_transmittance(p, p, eye, 0.)
        );
    }
    #[test]
    fn terrain_projects_geometry_and_darkness_is_separate_from_ray_fog() {
        let (p, f) = terrain_vertex([2., 3., 4.], [0., 10., 0.], 1.);
        assert_eq!(p, [2., 1., 4.]);
        assert_eq!(f, [1., 1.]);
        let world = [2., -9., 4.];
        let (p, f) = terrain_vertex(world, [0., 10., 0.], 1.);
        assert_eq!(p, world);
        // Base two, per the `exp`/`rcp` pair in terrain_refract_vs: D3D9
        // `exp` is 2^x, so the natural exponential would differ.
        assert!((f[1] - (-1.5f32).exp2()).abs() < 1e-6);
        assert!((f[1] - (-1.5f32).exp()).abs() > 0.1);
        let (_, g) = terrain_vertex(world, [10., 10., 0.], 1.);
        assert_eq!(f[1], g[1]);
        assert_ne!(f[0], g[0]);
    }
}
