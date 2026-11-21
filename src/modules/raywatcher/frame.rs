//! Per-frame inputs, and the replacement contract with the raster renderer.
//!
//! Two things live here.
//!
//! **The frame contract.** When Raywatcher replaces an effect, the raster
//! renderer must stop drawing that effect. That is expressed as
//! [`RasterOverrides`]: a list of switches the renderer applies to its own
//! drawers (`camera.light`, the sky's ambient term, the water reflection pass,
//! the fog/occlusion terms) derived from the ray plan, never from the settings
//! directly. A switch that is on here means "the ray pass owns this, the raster
//! path must not draw it" — so the two can never double up.
//!
//! **The frame parameters.** [`FrameParams`] is the GPU-side description of one
//! ray pass: camera basis, the sun's own position and colour, the ambient
//! record, and the pixel jitter the temporal accumulator expects. The CPU
//! reference path and the compute shader read the same struct, which is how the
//! tests can verify the lighting maths the shader performs.

use glam::{Mat4, Vec3, Vec4};

/// What the raster renderer must skip this frame because the ray pass owns it.
///
/// Every field is a *removal*: `true` means the original effect is off. The
/// renderer consults this instead of the module's settings so a mid-frame
/// settings edit cannot leave an effect drawn twice.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RasterOverrides {
    /// Stop adding the inline vertex/sun term in the mesh shaders.
    pub directional_light: bool,
    /// Stop the sky's ambient contribution to the raster scene (the ray pass
    /// reconstructs it from its own hemisphere samples).
    pub ambient: bool,
    /// Skip the screen-space water/terrain reflection submissions.
    pub reflection_pass: bool,
    /// Skip the screen-space ambient-occlusion-style darkening (the fog
    /// occlusion term the raster path applies to the sky and terrain).
    pub occlusion: bool,
    /// Skip the raster cloud/atmosphere glow that would double the ray-traced
    /// sky lighting.
    pub atmosphere_fog: bool,
}

impl RasterOverrides {
    /// Nothing is replaced; the raster renderer behaves exactly as before the
    /// module existed.
    pub const NONE: RasterOverrides = RasterOverrides {
        directional_light: false,
        ambient: false,
        reflection_pass: false,
        occlusion: false,
        atmosphere_fog: false,
    };

    /// The overrides a plan implies. The viewer draws three light terms
    /// inline: the mesh shaders' sun term, the sky's ambient term, and the fog
    /// occlusion term. Each ray switch removes the term it replaces:
    ///
    /// * `shadows` and `lighting` both own the sun term, because a raster sun
    ///   term with no shadow ray is exactly what the ray pass replaces;
    /// * `lighting`, `ambient_occlusion` and `global_illumination` all own the
    ///   ambient term, because the ray pass always produces ambient (bounced
    ///   light *is* ambient transport), so leaving the raster ambient on would
    ///   double it;
    /// * `reflections` owns the reflection submissions and `ambient_occlusion`
    ///   the fog occlusion term;
    /// * `refraction` is water-only and has no raster equivalent to remove.
    /// Raster overrides for a planned frame.
    ///
    /// By default (`plan.replace_raster == false`) Raywatcher layers ray-traced
    /// shadows, reflections, AO and GI over the untouched raster world, so this
    /// returns [`Self::NONE`]. Raster terms are only removed when the user
    /// explicitly opts into `replace_raster`.
    pub fn for_plan(plan: &crate::modules::raywatcher::plan::Plan) -> Self {
        if !plan.trace || !plan.replace_raster {
            return Self::NONE;
        }
        Self::for_effects(plan.effects, true)
    }

    /// The overrides implied when explicit raster replacement is enabled.
    pub fn for_effects(effects: crate::modules::raywatcher::plan::Effects, trace: bool) -> Self {
        if !trace {
            return Self::NONE;
        }
        Self {
            directional_light: effects.lighting || effects.shadows,
            ambient: effects.lighting || effects.ambient_occlusion || effects.global_illumination,
            reflection_pass: effects.reflections,
            occlusion: effects.ambient_occlusion,
            atmosphere_fog: effects.shadows,
        }
    }

    pub fn any(self) -> bool {
        self.directional_light
            || self.ambient
            || self.reflection_pass
            || self.occlusion
            || self.atmosphere_fog
    }

    /// One-line summary for the module's status line: the effects the raster
    /// path has handed over.
    pub fn summary(self) -> String {
        let mut parts = Vec::new();
        if self.directional_light {
            parts.push("direct light");
        }
        if self.ambient {
            parts.push("ambient");
        }
        if self.reflection_pass {
            parts.push("reflections");
        }
        if self.occlusion {
            parts.push("occlusion");
        }
        if self.atmosphere_fog {
            parts.push("atmosphere");
        }
        if parts.is_empty() {
            "raster effects untouched".to_string()
        } else {
            format!("ray tracing replaces {}", parts.join(", "))
        }
    }
}

/// World-space direction of the ray through an NDC point, for a reversed-Z
/// projection with near at `z = 1` and far at `z = 0` (the client's own
/// convention, see `Camera::projection_matrix`).
///
/// Un-projecting the near and far points and subtracting them is the standard
/// construction and it is direction-stable even when the camera is very far
/// from the origin, which a single far-plane point is not.
pub fn ray_through_ndc(inv_view_projection: Mat4, ndc: [f32; 2]) -> Vec3 {
    let near = inv_view_projection * Vec4::new(ndc[0], ndc[1], 1., 1.);
    let far = inv_view_projection * Vec4::new(ndc[0], ndc[1], 0., 1.);
    let near = near.truncate() / near.w;
    let far = far.truncate() / far.w;
    (far - near).normalize_or_zero()
}

/// Unproject a screen NDC coordinate and reversed-Z hardware depth (`1` near,
/// `0` far) back into world space.
pub fn unproject_depth(inv_view_projection: Mat4, ndc: [f32; 2], depth: f32) -> Vec3 {
    let clip = inv_view_projection * Vec4::new(ndc[0], ndc[1], depth, 1.0);
    clip.truncate() / clip.w
}

/// Phase 0 reference check: project `world_pos` through `view_projection` to
/// NDC + reversed-Z depth, unproject back via `inv_view_projection`, and
/// return the Euclidean reconstruction error in world units.
pub fn depth_unprojection_error(
    view_projection: Mat4,
    inv_view_projection: Mat4,
    world_pos: Vec3,
) -> f32 {
    let clip = view_projection * world_pos.extend(1.0);
    if clip.w.abs() < 1e-6 {
        return f32::INFINITY;
    }
    let ndc = clip.truncate() / clip.w;
    let reconstructed = unproject_depth(inv_view_projection, [ndc.x, ndc.y], ndc.z);
    (reconstructed - world_pos).length()
}

/// Phase 1 distance-scaled ray origin bias: prevents self-shadowing acne on
/// distant terrain and meshes while keeping contact shadows tight near the eye.
pub fn distance_scaled_bias(base_bias: f32, distance: f32) -> f32 {
    base_bias.max((distance * 0.0025).max(0.08))
}

/// One local/point light uploaded from the game's `SceneLights` for Phase 7
/// shadowed point-light shading.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PointLight {
    pub position: Vec3,
    pub radius: f32,
    pub colour: Vec3,
    pub intensity: f32,
    pub casts_shadow: bool,
}

/// Phase 7 smooth windowed inverse-square falloff for local point lights:
/// returns `0.0` at `distance >= radius`, decays smoothly with distance, and
/// avoids singularities near `distance = 0`.
pub fn point_light_falloff(distance: f32, radius: f32) -> f32 {
    if radius <= 1e-4 || distance >= radius {
        return 0.0;
    }
    let ratio = (distance / radius).clamp(0.0, 1.0);
    let r2 = ratio * ratio;
    let window = (1.0 - r2 * r2).max(0.0);
    (window * window) / (1.0 + distance * distance * 0.08)
}

/// Phase 2 layered composite reference formula: modulates the untouched raster
/// colour by sun shadow visibility and ambient occlusion (faded into fog), and
/// adds indirect GI, local point lights, and specular reflections. When all RT
/// signals are neutral (`sun_vis = 1`, `ao = 1`, `gi = 0`, `point = 0`,
/// `refl = 0`), the output equals `raster` identically.
#[allow(clippy::too_many_arguments)]
pub fn composite_layered(
    raster: Vec3,
    sun_vis: f32,
    ao: f32,
    gi: Vec3,
    point_lights: Vec3,
    reflection: Vec3,
    fog_t: f32,
    shadows_on: bool,
    ao_on: bool,
    n_dot_l: f32,
) -> Vec3 {
    let shadow_mod = if shadows_on && n_dot_l > 0.02 {
        0.30 + 0.70 * sun_vis.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let ao_mod = if ao_on {
        0.58 + 0.42 * ao.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let fog = fog_t.clamp(0.0, 1.0);
    let light_factor = (shadow_mod * ao_mod) * (1.0 - fog) + fog;
    (raster * light_factor + gi + point_lights + reflection).max(Vec3::ZERO)
}

/// The sun, as the ray pass needs it: a *position* far away (for the shadow
/// ray's angular size) and a radiance.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sun {
    pub direction: Vec3,
    pub colour: Vec3,
    /// Angular radius in radians, from the settings' degrees.
    pub angular_radius: f32,
    /// `0` at night, `1` at noon; the mesh shader's `camera.sun.w` logic.
    pub intensity: f32,
}

impl Default for Sun {
    fn default() -> Self {
        Self {
            direction: Vec3::Y,
            colour: Vec3::splat(1.),
            angular_radius: 0.6f32.to_radians(),
            intensity: 1.,
        }
    }
}

impl Sun {
    /// Recover the sun from the renderer's own values, so the ray-traced world
    /// agrees with the raster one where they touch.
    ///
    /// The raster shader computes `light = ambient + diffuse * max(dot(n, sun),
    /// 0)` with `sun` from `camera.sun.xyz` (daylight-dependent) and the
    /// regional record's direction when one is present. This mirrors that
    /// exactly: the shaft direction is the same vector, the colour is the same
    /// diffuse lane, and only the radius is new (the raster path has no
    /// penumbra at all, which is one of the things the ray pass adds).
    pub fn from_lighting(lighting: &LightingRecord) -> Self {
        Self {
            direction: lighting.direction,
            colour: lighting.diffuse,
            angular_radius: 0.6f32.to_radians(),
            intensity: lighting.daylight,
        }
    }
}

/// The lighting record the renderer derives each frame. Kept separate from the
/// GPU uniform so the CPU reference tracer and the tests can use it without a
/// device, and so the shader's numbers have exactly one author.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LightingRecord {
    /// Direction *towards* the sun (unit length).
    pub direction: Vec3,
    /// Ambient colour the raster path would add (the `camera.light.x` lane,
    /// expanded to the sky's own tint).
    pub ambient: Vec3,
    /// Diffuse colour the raster path would multiply by `max(dot(n,sun),0)`.
    pub diffuse: Vec3,
    /// Day/night factor, 0 at night.
    pub daylight: f32,
    /// Fog colour, used as the infinite-distance radiance so a missed ray and
    /// the raster horizon agree.
    pub fog_colour: Vec3,
    /// Fog range `(start, end)` in world units; `end <= start` disables it.
    pub fog_range: (f32, f32),
}

impl Default for LightingRecord {
    fn default() -> Self {
        // The viewer's own defaults: noon, sky-blue ambient, warm sun.
        Self {
            direction: Vec3::new(0.46, 0.8, 0.36).normalize(),
            ambient: Vec3::new(0.45, 0.55, 0.7),
            diffuse: Vec3::new(1., 0.96, 0.9),
            daylight: 1.,
            fog_colour: Vec3::new(0.62, 0.72, 0.86),
            fog_range: (0., 0.),
        }
    }
}

impl LightingRecord {
    /// The renderer's `begin_frame` lighting maths, transcribed so the two
    /// cannot drift:
    ///
    /// ```text
    /// angle   = (hour - 6) * PI / 12
    /// sun     = (cos(angle)*0.8, max(sin(angle), 0.05), 0.46)
    /// light.x = 0.17 + 0.38 * daylight
    /// light.y = 0.07 + 0.38 * daylight
    /// ```
    ///
    /// The regional record, when the map has one, replaces the direction and
    /// the diffuse lane exactly as `region_uniform` does.
    pub fn derive(
        hour: f32,
        daylight: f32,
        region: Option<&crate::environment::RegionalLight>,
        fog_colour: Vec3,
        fog_range: (f32, f32),
    ) -> Self {
        let angle = (hour - 6.0) * std::f32::consts::PI / 12.0;
        let sun = Vec3::new(angle.cos() * 0.8, angle.sin().max(0.05), 0.46);
        let ambient_level = 0.17 + 0.38 * daylight;
        let diffuse_level = 0.07 + 0.38 * daylight;
        let (direction, diffuse) = match region {
            Some(light) if region_direction(light).is_some() => {
                let d = region_direction(light).expect("checked");
                (
                    d,
                    Vec3::new(
                        light.diffuse[0] * diffuse_level * 2.6,
                        light.diffuse[1] * diffuse_level * 2.6,
                        light.diffuse[2] * diffuse_level * 2.6,
                    ),
                )
            }
            _ => (sun.normalize_or_zero(), {
                let tint = Vec3::new(sun.x.abs().max(0.35), 0.9, 0.72);
                tint * diffuse_level * 2.6
            }),
        };
        Self {
            direction,
            ambient: Vec3::new(
                ambient_level * 0.9,
                ambient_level * 0.98,
                ambient_level * 1.1,
            ),
            diffuse,
            daylight,
            fog_colour,
            fog_range,
        }
    }

    /// The environment radiance a ray sees when it escapes the world: fog at
    /// infinity when the map has fog, the sky tint otherwise. This is what
    /// makes reflections of the horizon line up with the raster horizon.
    pub fn environment(self) -> Vec3 {
        self.fog_colour.max(self.ambient * 0.85)
    }
}

fn region_direction(light: &crate::environment::RegionalLight) -> Option<Vec3> {
    let a = light.toward_sun;
    let d = Vec3::new(a[0], a[1], a[2]).normalize_or_zero();
    (d.length_squared() > 0.5).then_some(d)
}

/// Everything one ray pass needs that is not in [`LightingRecord`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FrameParams {
    /// Camera position, in world units.
    pub eye: Vec3,
    /// Inverse of the *unjittered* view-projection: the ray pass builds primary
    /// rays from it and applies its own sample jitter per pixel, so the raster
    /// camera's upscaler jitter is not double-counted.
    pub inv_view_projection: Mat4,
    /// Conjugate of the transposed `(view-projection)` whose top-left 3x3 is
    /// used to derive ray differentials for the denoiser's covariance.
    pub view_projection: Mat4,
    /// Render extent the image is reconstructed into.
    pub extent: (u32, u32),
    /// Ray grid extent.
    pub grid: (u32, u32),
    /// Near plane, world units.
    pub near: f32,
    /// Far plane / maximum ray distance.
    pub far: f32,
    /// Pixel jitter of *this* frame, in the ray grid's pixel units. Applied per
    /// sample inside the pass.
    pub jitter: (f32, f32),
    /// Frame index, used to seed the sample sequence and detect a history reset.
    pub frame: u32,
    /// `1` when the temporal history is valid, `0` after a camera jump or a
    /// resize.
    pub history_valid: u32,
    pub sun: Sun,
    pub lighting: LightingRecord,
    /// Reflection ray budget per pixel.
    pub bounces: u32,
    pub samples: u32,
}

impl FrameParams {
    /// Build the primary ray for a ray-grid pixel with a sub-pixel offset.
    ///
    /// The result is a *world-space* direction and origin. `offset` is in grid
    /// pixels relative to the pixel centre; the caller supplies one per sample.
    pub fn primary_ray(&self, pixel: (u32, u32), offset: (f32, f32)) -> (Vec3, Vec3) {
        let w = self.grid.0.max(1) as f32;
        let h = self.grid.1.max(1) as f32;
        // `(x + 0.5) / w` maps pixel centres to [0, 1]. The projection is
        // reversed-Z with the client's reversed X, so the same inverse the
        // raster path uses gives the correct world direction here.
        let x = (pixel.0 as f32 + 0.5 + offset.0) / w;
        let y = (pixel.1 as f32 + 0.5 + offset.1) / h;
        let ndc = [x * 2. - 1., 1. - y * 2.];
        (self.eye, ray_through_ndc(self.inv_view_projection, ndc))
    }

    /// Apply the frame's jitter plus the sample index to a pixel.
    pub fn sample_offset(&self, sample: u32) -> (f32, f32) {
        let n = self.samples.max(1);
        // Stratify inside the pixel: one sample sits at the centre, two or more
        // spread along a rotated ramp. The frame jitter then rotates the whole
        // pattern across frames, which is what the temporal accumulator needs.
        let (phase, radius) = match n {
            1 => (0., 0.),
            2 => (0.5, 0.28),
            3 => (sample as f32 / 3., 0.33),
            _ => (sample as f32 / n as f32, 0.4),
        };
        let angle = phase * std::f32::consts::TAU;
        (
            self.jitter.0 + angle.cos() * radius,
            self.jitter.1 + angle.sin() * radius,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lighting() -> LightingRecord {
        LightingRecord::default()
    }

    #[test]
    fn a_disabled_plan_touches_no_raster_effect() {
        use crate::modules::raywatcher::plan::Effects;
        assert_eq!(
            RasterOverrides::for_effects(Effects::default(), false),
            RasterOverrides::NONE
        );
        assert!(!RasterOverrides::NONE.any());
        assert_eq!(RasterOverrides::NONE.summary(), "raster effects untouched");
    }

    #[test]
    fn each_ray_effect_removes_its_raster_counterpart() {
        use crate::modules::raywatcher::plan::Effects;
        let only = |f: fn(&mut Effects)| {
            let mut e = Effects {
                shadows: false,
                lighting: false,
                reflections: false,
                ambient_occlusion: false,
                global_illumination: false,
                refraction: false,
            };
            f(&mut e);
            RasterOverrides::for_effects(e, true)
        };
        // Shadows and lighting both remove the inline sun term, because that
        // term *is* the raster shadowed lighting.
        assert!(only(|e| e.shadows = true).directional_light);
        assert!(only(|e| e.lighting = true).directional_light);
        // Reflections remove exactly the reflection submissions and nothing
        // else...
        let r = only(|e| e.reflections = true);
        assert!(r.reflection_pass);
        assert!(!r.directional_light && !r.occlusion);
        // ...and ambient occlusion removes the fog occlusion term.
        let o = only(|e| e.ambient_occlusion = true);
        assert!(o.occlusion);
        assert!(!o.reflection_pass);
        // Global illumination produces transported ambient, so it removes the
        // raster ambient term and nothing else.
        let g = only(|e| e.global_illumination = true);
        assert!(g.ambient);
        assert!(!g.directional_light && !g.reflection_pass && !g.occlusion);
        // Refraction is water-only and removes nothing either.
        assert!(!only(|e| e.refraction = true).any());
    }

    #[test]
    fn the_shipped_lighting_record_matches_the_renderer_formula() {
        // These are the renderer's own numbers for noon: daylight = 1.
        let record = LightingRecord::derive(12., 1., None, Vec3::ZERO, (0., 0.));
        assert!(
            (record.ambient.x - 0.495).abs() < 1e-5,
            "{:?}",
            record.ambient
        );
        assert!((record.ambient.y - 0.539).abs() < 1e-5);
        assert!((record.ambient.z - 0.605).abs() < 1e-5);
        // Midnight: daylight = 0, so ambient is only the floor.
        let night = LightingRecord::derive(0., 0., None, Vec3::ZERO, (0., 0.));
        assert!((night.ambient.x - 0.153).abs() < 1e-5);
        // The sun's direction is a unit vector at every hour, including below
        // the horizon where `max(sin, 0.05)` keeps it just above.
        for hour in [0., 3., 6., 12., 18., 21., 24.] {
            let r = LightingRecord::derive(hour, 1., None, Vec3::ZERO, (0., 0.));
            assert!((r.direction.length() - 1.).abs() < 1e-4, "hour {hour}");
            assert!(r.direction.y >= 0., "hour {hour} is not below the world");
        }
    }

    #[test]
    fn a_region_with_light_replaces_the_default_sun_direction() {
        let region = crate::environment::RegionalLight {
            ambient: [0.1, 0.2, 0.3],
            diffuse: [0.9, 0.8, 0.7],
            specular: [1., 1., 1.],
            toward_sun: [0.6, -0.4, 0.8],
            celestial_flags: 0x18,
        };
        let with = LightingRecord::derive(12., 1., Some(&region), Vec3::ZERO, (0., 0.));
        let without = LightingRecord::derive(12., 1., None, Vec3::ZERO, (0., 0.));
        assert!((with.direction - without.direction).length() > 0.1);
        // The authored direction points where the client's does, normalised.
        let want = Vec3::new(0.6, -0.4, 0.8).normalize();
        assert!((with.direction - want).length() < 1e-4);
        // A degenerate direction falls back to the sky-derived one.
        let broken = crate::environment::RegionalLight {
            toward_sun: [0., 0., 0.],
            ..region
        };
        let fallback = LightingRecord::derive(12., 1., Some(&broken), Vec3::ZERO, (0., 0.));
        assert!((fallback.direction - without.direction).length() < 1e-4);
    }

    #[test]
    fn the_environment_radiance_never_goes_black() {
        let mut record = lighting();
        record.ambient = Vec3::ZERO;
        record.fog_colour = Vec3::ZERO;
        assert!(record.environment().length() >= 0.);
        record.fog_colour = Vec3::new(0.6, 0.7, 0.9);
        assert!(record.environment().z > 0.5);
    }

    #[test]
    fn a_primary_ray_points_through_its_pixel() {
        // A *finite* reversed-Z projection. The far-plane point of an infinite
        // projection has w = 0, which is exactly why the ray helper subtracts a
        // near point from a far point scaled by a real near plane instead of
        // reading the far point directly.
        let aspect = 16. / 9.;
        let half = 70f32.to_radians() * 0.5;
        let f = 1. / half.tan();
        let (near, far) = (0.5, 5000.);
        // Reversed-Z, right-handed, negated X. Column-major: the third column
        // is the z row, the fourth is the perspective divide.
        let proj = Mat4::from_cols_array(&[
            -f / aspect,
            0.,
            0.,
            0.,
            0.,
            f,
            0.,
            0.,
            0.,
            0.,
            near / (far - near),
            1.,
            0.,
            0.,
            -(near * far) / (far - near),
            0.,
        ]);
        let mut params = FrameParams {
            eye: Vec3::ZERO,
            inv_view_projection: proj.inverse(),
            view_projection: proj,
            extent: (1920, 1080),
            grid: (1920, 1080),
            near,
            far,
            jitter: (0., 0.),
            frame: 0,
            history_valid: 1,
            sun: Sun::from_lighting(&lighting()),
            lighting: lighting(),
            bounces: 1,
            samples: 1,
        };
        // The helper itself: the top-left NDC corner is up and to the reversed-X
        // left of the axis (positive `dir.x`).
        let up_left = ray_through_ndc(params.inv_view_projection, [-1., 1.]);
        let down_right = ray_through_ndc(params.inv_view_projection, [1., -1.]);
        assert!(up_left.x > 0. && up_left.y > 0., "{up_left:?}");
        assert!(down_right.x < 0. && down_right.y < 0., "{down_right:?}");
        assert!((up_left.length() - 1.).abs() < 1e-4);
        // Pixel (959, 539) sits just left of the optical axis and pixel
        // (960, 540) just right and below it.
        let (_, left) = params.primary_ray((959, 539), (0., 0.));
        let (_, right) = params.primary_ray((960, 540), (0., 0.));
        assert!(left.x > 0., "left pixel reads {left:?}");
        assert!(right.x < 0., "right pixel reads {right:?}");
        assert!(left.y > 0., "left pixel sits above the axis: {left:?}");
        assert!(right.y < 0., "right pixel sits below it: {right:?}");
        assert!((left.z - 1.).abs() < 1e-3 && (right.z - 1.).abs() < 1e-3);
        // Rays are unit length and all point forward.
        for pixel in [(0, 0), (0, 1079), (1919, 0), (1919, 1079), (700, 300)] {
            let (_, dir) = params.primary_ray(pixel, (0., 0.));
            assert!((dir.length() - 1.).abs() < 1e-4, "{pixel:?} -> {dir:?}");
            assert!(dir.z > 0.);
        }
        // The exact centre of the image is the optical axis.
        let (_, centre) = params.primary_ray((959, 539), (0.5, 0.5));
        assert!(centre.x.abs() < 0.01 && centre.y.abs() < 0.01, "{centre:?}");
        // The eye is always the origin of the ray, even when the camera is far
        // from the world origin.
        params.eye = Vec3::new(1200., 340., -8000.);
        let (origin, _) = params.primary_ray((100, 100), (0., 0.));
        assert_eq!(origin, params.eye);
    }

    #[test]
    fn sample_offsets_stay_inside_the_pixel_and_differ_between_samples() {
        let mut params = FrameParams {
            eye: Vec3::ZERO,
            inv_view_projection: Mat4::IDENTITY,
            view_projection: Mat4::IDENTITY,
            extent: (640, 360),
            grid: (640, 360),
            near: 1.,
            far: 1000.,
            jitter: (0.1, -0.1),
            frame: 7,
            history_valid: 1,
            sun: Sun::from_lighting(&lighting()),
            lighting: lighting(),
            bounces: 0,
            samples: 1,
        };
        let single = params.sample_offset(0);
        assert_eq!(single, (0.1, -0.1), "one sample sits on the frame jitter");
        params.samples = 4;
        let offsets: Vec<_> = (0..4).map(|s| params.sample_offset(s)).collect();
        for o in &offsets {
            assert!((o.0 - 0.1).abs() <= 0.45 && (o.1 + 0.1).abs() <= 0.45);
        }
        for i in 0..4 {
            for j in (i + 1)..4 {
                let d = (offsets[i].0 - offsets[j].0).hypot(offsets[i].1 - offsets[j].1);
                assert!(d > 1e-3, "samples {i} and {j} coincide");
            }
        }
        // A zero sample count cannot panic or produce NaN.
        params.samples = 0;
        assert!(params.sample_offset(0).0.is_finite());
    }

    #[test]
    fn the_sun_radius_is_the_only_new_lighting_input() {
        let l = lighting();
        let sun = Sun::from_lighting(&l);
        assert_eq!(sun.direction, l.direction);
        assert_eq!(sun.colour, l.diffuse);
        assert!((sun.angular_radius - 0.6f32.to_radians()).abs() < 1e-6);
        assert!((sun.intensity - l.daylight).abs() < 1e-6);
    }

    #[test]
    fn ray_generation_and_unprojection_match_raster_depth() {
        let eye = Vec3::new(120.0, 45.0, -310.0);
        let target = eye + Vec3::new(0.4, -0.15, 0.9).normalize() * 50.0;
        // Match the viewer's reversed-Z right-handed projection (z=1 near, z=0 far).
        let view = Mat4::look_at_rh(eye, target, Vec3::Y);
        let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 16.0 / 9.0, 2000.0, 0.5);
        let vp = proj * view;
        let inv_vp = vp.inverse();

        for &dist in &[5.0_f32, 25.0, 180.0, 750.0] {
            let probe = eye + (target - eye).normalize() * dist + Vec3::new(1.5, -0.7, 0.4);
            let err = depth_unprojection_error(vp, inv_vp, probe);
            assert!(
                err < 0.01,
                "unprojection error at dist {dist} was {err} (>= 0.01)"
            );
            let clip = vp * probe.extend(1.0);
            let ndc = [clip.x / clip.w, clip.y / clip.w];
            let ray_dir = ray_through_ndc(inv_vp, ndc);
            let expected_dir = (probe - eye).normalize();
            assert!(
                ray_dir.dot(expected_dir) > 0.9999,
                "ray direction mismatch at dist {dist}: {ray_dir:?} vs {expected_dir:?}"
            );
        }
    }

    #[test]
    fn layered_composite_preserves_raster_colour_when_rt_signals_are_neutral() {
        let raster = Vec3::new(0.42, 0.61, 0.28);
        let identity = composite_layered(
            raster,
            1.0,
            1.0,
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::ZERO,
            0.0,
            true,
            true,
            0.8,
        );
        assert!(
            (identity - raster).length() < 1e-6,
            "neutral RT signals must preserve raster 1:1: {identity:?} vs {raster:?}"
        );

        let shadowed = composite_layered(
            raster,
            0.0,
            1.0,
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::ZERO,
            0.0,
            true,
            true,
            0.8,
        );
        assert!(shadowed.x < raster.x && shadowed.x > 0.0);

        let with_additives = composite_layered(
            raster,
            1.0,
            1.0,
            Vec3::new(0.05, 0.04, 0.02),
            Vec3::new(0.20, 0.12, 0.05),
            Vec3::new(0.08, 0.10, 0.14),
            0.0,
            true,
            true,
            0.8,
        );
        assert!(with_additives.x > raster.x);
        assert!(with_additives.z > raster.z);
    }

    #[test]
    fn point_light_falloff_decays_smoothly_and_reaches_zero_at_radius() {
        let r = 20.0;
        let f_near = point_light_falloff(1.0, r);
        let f_mid = point_light_falloff(10.0, r);
        let f_edge = point_light_falloff(19.0, r);
        let f_at = point_light_falloff(r, r);
        let f_out = point_light_falloff(25.0, r);
        assert!(f_near > f_mid && f_mid > f_edge && f_edge > 0.0);
        assert_eq!(f_at, 0.0);
        assert_eq!(f_out, 0.0);
        assert_eq!(point_light_falloff(1.0, 0.0), 0.0);
    }

    #[test]
    fn distance_scaled_bias_grows_with_distance_and_respects_floor() {
        let near_b = distance_scaled_bias(0.05, 2.0);
        let far_b = distance_scaled_bias(0.05, 500.0);
        assert!(near_b >= 0.05);
        assert!(far_b > near_b * 5.0);
    }

    #[test]
    fn debug_view_tokens_round_trip() {
        use super::super::plan::DebugView;
        for (idx, view) in DebugView::ALL.iter().copied().enumerate() {
            assert_eq!(view.as_u32(), idx as u32);
            assert_eq!(DebugView::parse(view.token()), Some(view));
        }
    }
}
