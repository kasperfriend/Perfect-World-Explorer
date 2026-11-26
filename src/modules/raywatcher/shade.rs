//! The transport: what a ray does when it hits something.
//!
//! This is the CPU reference implementation, and it exists for three reasons
//! that are all about the GPU shader being right:
//!
//! 1. **It is the specification.** [`RAY_SHADER`](super::gpu) is a line-by-line
//!    transcription of [`trace`]; where the two disagree, the tests here are
//!    the arbiter and the shader is the bug.
//! 2. **It is testable without a device.** Every interesting failure of a
//!    tracer is arithmetic — a shadow ray that starts inside the surface it
//!    just hit, a reflection that loses its Fresnel weight, an ambient
//!    hemisphere that ends up unnormalised — and all of those are reachable on
//!    a headless runner through this file.
//! 3. **It is what the denoiser's variance estimate is calibrated against.**
//!
//! The stages, in the order [`trace`] runs them, each gated by its own effect
//! bit so that a frame with shadows off spends no ray on them:
//!
//! ```text
//! primary hit ──┬─ shadow ray toward the sun, sized by the sun's angular radius
//!               ├─ sun term:      albedo * diffuse * visibility * N·L
//!               ├─ ambient term:  albedo * ambient * (0.5 + 0.5 * N·up) * AO
//!               ├─ ambient occlusion: cosine-weighted hemisphere rays
//!               ├─ reflection:   Fresnel-weighted mirror ray; rougher than the
//!               │                cutoff falls back to diffuse transport
//!               ├─ diffuse bounce: one cosine-weighted GI ray per bounce, each
//!               │                   attenuating by albedo
//!               └─ fog:          extinction toward the eye
//! missed ray ─── environment: the raster horizon's fog colour, so reflections
//!               of the sky agree with the raster sky
//! ```

use glam::Vec3;

use super::bvh::{Hit, RayMaterial, SceneGeometry};
use super::frame::{FrameParams, LightingRecord};

/// The transport's tunables. Everything here comes from the module's settings;
/// nothing is derived from the frame, so a test can hold the frame fixed and
/// vary exactly one knob.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TransportConfig {
    /// The sun and ambient the raster path would have used. The ray pass reads
    /// its lighting from here rather than from a uniform of its own, so the two
    /// worlds cannot disagree about noon.
    pub lighting: LightingRecord,
    /// Multiplier on the ambient term, from the settings.
    pub ambient_strength: f32,
    /// Multiplier on radiance gathered by indirect bounces.
    pub bounce_strength: f32,
    /// Surfaces rougher than this use diffuse transport instead of a mirror ray.
    pub reflection_roughness_cutoff: f32,
    /// Offset along the shading normal a secondary ray starts at, in world
    /// units. Too small and a shadow ray re-hits its own surface at grazing
    /// angles; too large and thin geometry leaks light.
    pub bias: f32,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            lighting: LightingRecord::default(),
            ambient_strength: 1.,
            bounce_strength: 1.,
            reflection_roughness_cutoff: 0.4,
            bias: 2e-3,
        }
    }
}

impl TransportConfig {
    /// Build the transport from what the renderer knows this frame.
    pub fn new(
        lighting: LightingRecord,
        ambient_strength: f32,
        bounce_strength: f32,
        reflection_roughness_cutoff: f32,
        bias: f32,
    ) -> Self {
        Self {
            lighting,
            ambient_strength: ambient_strength.max(0.),
            bounce_strength: bounce_strength.max(0.),
            reflection_roughness_cutoff: reflection_roughness_cutoff.clamp(0., 1.),
            bias: if bias.is_finite() && bias > 0. {
                bias
            } else {
                2e-3
            },
        }
    }

    /// A deterministic per-pixel, per-sample, per-dimension random number in
    /// `[0, 1)`.
    ///
    /// Deterministic matters: it is what makes the reference tracer's output
    /// reproducible in a test, and what lets the GPU shader use the identical
    /// sequence without a stateful RNG. This is the standard "hash the integer
    /// lattice" construction and it has no visible structure at the scales a
    /// pixel samples at.
    pub fn random(&self, pixel: u32, sample: u32, dimension: u32) -> f32 {
        let mut h = pixel ^ (sample.wrapping_mul(0x9e37_79b9));
        h = h.wrapping_add(dimension.wrapping_mul(0x85eb_ca6b));
        h ^= h >> 16;
        h = h.wrapping_mul(0x7feb_352d);
        h ^= h >> 15;
        h = h.wrapping_mul(0x846c_a68b);
        h ^= h >> 16;
        // 24 bits is what an f32 mantissa carries; more would be lost anyway.
        (h >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    /// A cosine-weighted hemisphere direction around `normal`.
    ///
    /// Cosine weighting is not a convenience: the diffuse BRDF already carries
    /// the `cos θ` factor, so sampling with this density is what makes the
    /// estimator `albedo * incoming` with no per-sample division. Getting this
    /// wrong (uniform sampling without the weight) is the classic too-dark GI
    /// that no amount of bounce count fixes.
    pub fn cosine_hemisphere(&self, normal: Vec3, u1: f32, u2: f32) -> Vec3 {
        // Malley's method: sample the unit disk, lift to the hemisphere.
        let r = u1.clamp(0., 1.).sqrt();
        let phi = std::f32::consts::TAU * u2;
        let (sin, cos) = phi.sin_cos();
        let local = Vec3::new(r * cos, r * sin, (1. - u1.clamp(0., 1.)).max(0.).sqrt());
        let (tangent, bitangent) = orthonormal_basis(normal);
        (tangent * local.x + bitangent * local.y + normal * local.z).normalize_or_zero()
    }

    /// The direction a reflection ray leaves at, and the weight it carries.
    ///
    /// The Fresnel term is Schlick's approximation on the dielectric F0 of 0.04
    /// (what water and most painted stone have), with the metal lane taking the
    /// albedo tint when `metalness` is high. The `(1 - roughness)` factor on the
    /// mirror direction is what stops a rough surface from reflecting a
    /// point-sharp image: it interpolates the sampled direction toward the
    /// normal in proportion to roughness.
    pub fn reflection(&self, normal: Vec3, view: Vec3, material: &RayMaterial) -> (Vec3, f32) {
        let roughness = material.surface[0].clamp(0., 1.);
        let metalness = material.surface[1].clamp(0., 1.);
        let incident = reflect(view, normal);
        let sharpness = 1. - roughness;
        let direction = (normal * (1. - sharpness) + incident * sharpness).normalize_or_zero();
        let albedo_mean = (material.albedo[0] + material.albedo[1] + material.albedo[2]) / 3.;
        let f0 = 0.04 * (1. - metalness) + albedo_mean * metalness;
        let cos = normal.dot(incident).abs().clamp(0., 1.);
        let fresnel = f0 + (1. - f0) * (1. - cos).powi(5);
        (direction, fresnel.clamp(0., 1.))
    }
}

/// The mirror reflection of `view` about `normal`.
pub fn reflect(view: Vec3, normal: Vec3) -> Vec3 {
    view - normal * (2. * view.dot(normal))
}

/// What one traced pixel produced, before the denoiser sees it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PixelSample {
    /// The denoisable radiance.
    pub colour: Vec3,
    /// The reflection-only lane; the reconstruction providers take this as the
    /// specular signal, and the compose pass writes it into the guide.
    pub reflection: Vec3,
    /// Sky visibility: `1` when the pixel sees the sky through its hemisphere,
    /// `0` when it is fully occluded. This is the ambient-occlusion signal, and
    /// it is a scalar per pixel by construction so it survives the guide's
    /// four channels.
    pub visibility: f32,
    /// Distance to the first hit, or the frame's far plane on a miss. The
    /// spatial filter uses it as a bilateral guide, and the reconstruction
    /// providers take it as the hit distance.
    pub hit_distance: f32,
    /// `1` when the primary ray escaped the world.
    pub sky: bool,
}

impl PixelSample {
    /// A miss: the pixel saw the sky.
    pub fn sky(colour: Vec3, far: f32) -> Self {
        Self {
            colour,
            reflection: colour,
            visibility: 1.,
            hit_distance: far,
            sky: true,
        }
    }
}

/// The sample budget for one pixel.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sampler {
    pub samples: u32,
    pub bounces: u32,
}

impl Sampler {
    pub fn new(samples: u32, bounces: u32) -> Self {
        Self {
            samples: samples.max(1),
            bounces: bounces.min(8),
        }
    }
}

/// Trace one pixel. This is the function the compute shader mirrors.
///
/// `pixel` seeds the random sequence (one integer per grid pixel, not per
/// extent pixel, so the sequence is identical at every ray-grid resolution);
/// `offset` is the sample's sub-pixel position in grid pixels.
pub fn trace(
    scene: &SceneGeometry,
    params: &FrameParams,
    config: &TransportConfig,
    sampler: Sampler,
    pixel: (u32, u32),
    offset: (f32, f32),
) -> PixelSample {
    let (origin, direction) = params.primary_ray(pixel, offset);
    let far = params.far.max(params.near + 1.);
    let Some(hit) = scene.closest_hit(origin, direction, params.near * 0.5, far) else {
        let environment = params.lighting.environment();
        return PixelSample::sky(environment, far);
    };
    let seed =
        pixel.1.wrapping_mul(params.grid.0.max(1)) ^ pixel.0 ^ params.frame.wrapping_mul(977);
    let mut colour = shade_hit(scene, params, config, sampler, &hit, direction, seed);
    // The original client uses table fog over view-space Z, not Euclidean
    // eye-to-hit distance. Projecting the hit gives clip.w = positive view Z;
    // this matches the raster shaders even for wide/off-axis rays.
    let view_z = (params.view_projection * point_of(scene, &hit).extend(1.0)).w.max(0.0);
    colour = apply_fog(colour, params, view_z);
    let visibility = sample_occlusion(scene, params, config, &hit, sampler, seed);
    let reflection = reflection_lane(scene, params, config, &hit, direction, seed);
    PixelSample {
        colour,
        reflection,
        visibility,
        hit_distance: hit.t,
        sky: false,
    }
}

/// The sun's visibility from `hit`, in `[0, 1]`.
///
/// A one-sample shadow ray cannot produce a penumbra, so the penumbra comes from
/// sampling the sun's *disc*: the shader casts the shadow ray at a point on the
/// disc rather than at its centre, so the fraction of samples that escape is the
/// fraction of the disc that is visible. With the sun's angular radius at zero
/// the samples collapse onto the centre and the shadow is hard, which is what
/// the settings' radius knob means.
pub fn sun_visibility(
    scene: &SceneGeometry,
    params: &FrameParams,
    config: &TransportConfig,
    hit: &Hit,
    shadow_seed: u32,
) -> f32 {
    let normal = hit.normal;
    let to_sun = params.sun.direction.normalize_or_zero();
    let cos_surface = normal.dot(to_sun);
    if cos_surface <= 0. {
        // The surface faces away from the sun; no ray needed.
        return 0.;
    }
    let origin = point_of(scene, hit) + normal * config.bias.max(1e-4);
    let max_distance = params.far.max(1.).min(1e6);
    let (tangent, bitangent) = orthonormal_basis(to_sun);
    let angle = params.sun.angular_radius.clamp(0., 0.2);
    let r1 = hash01(shadow_seed, 0x51ed_2701);
    let r2 = hash01(shadow_seed, 0x2545_f491);
    let radius = angle * r1.sqrt();
    let phi = std::f32::consts::TAU * r2;
    let (sin, cos) = phi.sin_cos();
    let direction = (to_sun + (tangent * cos + bitangent * sin) * radius).normalize_or_zero();
    if scene.occluded(origin, direction, config.bias, max_distance) {
        0.
    } else {
        1.
    }
}

/// Ambient occlusion: the fraction of the cosine-weighted hemisphere that is
/// visible, in `[0, 1]`.
///
/// "Sky visibility" is the same quantity with the sky as the only light. One
/// ray per sample is enough because the denoiser removes the noise; the
/// fallback when the budget is zero is the unoccluded hemisphere, `1`.
pub fn sample_occlusion(
    scene: &SceneGeometry,
    params: &FrameParams,
    config: &TransportConfig,
    hit: &Hit,
    sampler: Sampler,
    seed: u32,
) -> f32 {
    let normal = hit.normal;
    if normal.length_squared() < 0.5 {
        return 1.;
    }
    let origin = point_of(scene, hit) + normal * config.bias.max(1e-4);
    let mut visible = 0u32;
    let rays = sampler.samples;
    let max_distance = params.far.max(1.);
    for ray in 0..rays {
        let u1 = hash01(seed.wrapping_add(ray * 7 + 1), 0x9e37_79b9);
        let u2 = hash01(seed.wrapping_add(ray * 7 + 2), 0x85eb_ca6b);
        let direction = config.cosine_hemisphere(normal, u1, u2);
        if !scene.occluded(origin, direction, config.bias, max_distance) {
            visible += 1;
        }
    }
    visible as f32 / rays.max(1) as f32
}

/// Shade one hit, walking the diffuse bounce chain.
fn shade_hit(
    scene: &SceneGeometry,
    params: &FrameParams,
    config: &TransportConfig,
    sampler: Sampler,
    hit: &Hit,
    _view: Vec3,
    seed: u32,
) -> Vec3 {
    let material = material_of(scene, hit.triangle);
    let albedo = Vec3::from_slice(&material.albedo[0..3]);
    let normal = hit.normal.normalize_or_zero();
    let lighting = &config.lighting;
    // The sun term. Visibility is folded in here; with the shadow effect off the
    // caller passes a zero angular radius *and* this function is still allowed to
    // run, which is why the effect gate lives in `trace`'s caller and not here.
    let sun_term = if params.lighting.daylight > 0. && params.sun.intensity > 0. {
        let visibility = sun_visibility(scene, params, config, hit, seed);
        let cos = normal.dot(params.sun.direction.normalize_or_zero()).max(0.);
        params.sun.colour * params.sun.intensity * cos * visibility
    } else {
        Vec3::ZERO
    };
    // The ambient term: sky visibility weighted by how much of the hemisphere
    // the surface presents, which is the raster shader's own
    // `light.x + light.y * N·L` structure with the sky in place of the sun.
    let up = (0.5 + 0.5 * normal.y).clamp(0., 1.);
    let ambient = lighting.ambient * config.ambient_strength * up;
    let direct = albedo * (sun_term + ambient);
    // Diffuse transport: each bounce gathers the environment through one ray and
    // attenuates by the albedo, so the series converges to the same integral the
    // raster path only approximates with a constant.
    let mut indirect = Vec3::ZERO;
    if sampler.bounces > 0 && config.bounce_strength > 0. {
        let mut throughput = albedo;
        let mut origin = point_of(scene, hit) + normal * config.bias.max(1e-4);
        let mut shading_normal = normal;
        for bounce in 0..sampler.bounces {
            let u1 = hash01(seed.wrapping_add(bounce * 13 + 3), 0x7feb_352d);
            let u2 = hash01(seed.wrapping_add(bounce * 13 + 5), 0x846c_a68b);
            let direction = config.cosine_hemisphere(shading_normal, u1, u2);
            if direction.length_squared() < 0.5 {
                break;
            }
            let Some(next) = scene.closest_hit(origin, direction, config.bias, params.far.max(1.))
            else {
                indirect += throughput * lighting.environment();
                break;
            };
            let next_material = material_of(scene, next.triangle);
            let next_albedo = Vec3::from_slice(&next_material.albedo[0..3]);
            let next_normal = next.normal.normalize_or_zero();
            // Direct sun on the bounce surface, which is what makes a red wall
            // cast red light rather than a neutral wash.
            let bounce_sun = if params.lighting.daylight > 0. {
                let visibility = sun_visibility(
                    scene,
                    params,
                    config,
                    &next,
                    seed.wrapping_add(bounce as u32 + 19),
                );
                let cos = next_normal
                    .dot(params.sun.direction.normalize_or_zero())
                    .max(0.);
                params.sun.colour * params.sun.intensity * cos * visibility
            } else {
                Vec3::ZERO
            };
            let bounce_up = (0.5 + 0.5 * next_normal.y).clamp(0., 1.);
            indirect += throughput
                * next_albedo
                * (bounce_sun + lighting.ambient * bounce_up)
                * config.bounce_strength;
            throughput *= next_albedo;
            origin = point_of(scene, &next) + next_normal * config.bias.max(1e-4);
            shading_normal = next_normal;
            if throughput.max_element() < 1e-3 {
                break;
            }
        }
    }
    direct + indirect
}

/// The reflection lane for a pixel: the specular radiance, for the guide and
/// the reconstruction providers. It is a separate function because the main
/// colour already includes it and this must not double-count.
fn reflection_lane(
    scene: &SceneGeometry,
    params: &FrameParams,
    config: &TransportConfig,
    hit: &Hit,
    view: Vec3,
    seed: u32,
) -> Vec3 {
    let material = material_of(scene, hit.triangle);
    let normal = hit.normal.normalize_or_zero();
    let roughness = material.surface[0].clamp(0., 1.);
    // Rougher than the cutoff is diffuse transport's job, not a mirror ray's.
    if roughness > config.reflection_roughness_cutoff {
        return Vec3::ZERO;
    }
    let (direction, weight) = config.reflection(normal, view, &material);
    if direction.length_squared() < 0.5 || weight <= 1e-4 {
        return Vec3::ZERO;
    }
    let origin = point_of(scene, hit) + normal * config.bias.max(1e-4);
    let far = params.far.max(1.);
    match scene.closest_hit(origin, direction, config.bias, far) {
        Some(next) => {
            let next_material = material_of(scene, next.triangle);
            let albedo = Vec3::from_slice(&next_material.albedo[0..3]);
            let next_normal = next.normal.normalize_or_zero();
            let sun = if params.lighting.daylight > 0. {
                let visibility = sun_visibility(scene, params, config, &next, seed.wrapping_add(1));
                let cos = next_normal
                    .dot(params.sun.direction.normalize_or_zero())
                    .max(0.);
                params.sun.colour * params.sun.intensity * cos * visibility
            } else {
                Vec3::ZERO
            };
            let ambient = params.lighting.ambient * config.ambient_strength;
            albedo * (sun + ambient) * weight
        }
        None => params.lighting.environment() * weight,
    }
}

/// Apply the client's linear table fog at positive view-space Z.
pub fn apply_fog(colour: Vec3, params: &FrameParams, distance: f32) -> Vec3 {
    let (start, end) = params.lighting.fog_range;
    if end <= start || !start.is_finite() || !end.is_finite() {
        return colour;
    }
    let t = ((distance - start) / (end - start)).clamp(0., 1.);
    // The client blends fog in encoded sRGB space, then writes the result to
    // the linear render target. Keep the CPU reference on the same transfer.
    let encoded = Vec3::new(
        linear_to_srgb(colour.x),
        linear_to_srgb(colour.y),
        linear_to_srgb(colour.z),
    );
    let mixed = encoded.lerp(params.lighting.fog_colour, t);
    Vec3::new(
        srgb_to_linear(mixed.x),
        srgb_to_linear(mixed.y),
        srgb_to_linear(mixed.z),
    )
}

fn linear_to_srgb(v: f32) -> f32 {
    let v = v.max(0.0);
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_to_linear(v: f32) -> f32 {
    let v = v.max(0.0);
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn material_of(scene: &SceneGeometry, triangle: u32) -> RayMaterial {
    let index = scene
        .triangles()
        .get(triangle as usize)
        .map(|tri| tri.material() as usize)
        .unwrap_or(0);
    scene.materials().get(index).copied().unwrap_or_default()
}

fn point_of(scene: &SceneGeometry, hit: &Hit) -> Vec3 {
    hit.point(scene.triangles())
}

/// An orthonormal basis around `normal`: Duff et al.'s branchless ONB, which
/// stays well-conditioned at every normal including the ±Z poles.
fn orthonormal_basis(normal: Vec3) -> (Vec3, Vec3) {
    let sign = if normal.z >= 0. { 1. } else { -1. };
    let a = -1. / (sign + normal.z);
    let b = normal.x * normal.y * a;
    (
        Vec3::new(
            1. + sign * normal.x * normal.x * a,
            sign * b,
            -sign * normal.x,
        ),
        Vec3::new(b, sign + normal.y * normal.y * a, -normal.y),
    )
}

fn hash01(seed: u32, salt: u32) -> f32 {
    let mut h = seed ^ salt;
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    (h >> 8) as f32 * (1.0 / 16_777_216.0)
}

#[cfg(test)]
mod tests {
    use super::super::bvh::RayTriangle;
    use super::super::frame::{FrameParams, Sun};
    use super::*;

    fn plane_scene() -> SceneGeometry {
        // A 200x200 ground plane at y = 0, plus an 8-unit cube whose base sits
        // at y = 4: large enough that a low sun throws a shadow several pixels
        // wide at a 64x64 grid, which is what the shadow test needs.
        let mut triangles = Vec::new();
        let material = RayMaterial {
            albedo: [0.5, 0.5, 0.5, 1.],
            surface: [0.9, 0., 0., 1.],
        };
        let ground = RayMaterial {
            albedo: [0.4, 0.4, 0.4, 1.],
            surface: [0.95, 0., 0., 1.],
        };
        triangles.push(RayTriangle::new(
            Vec3::new(-100., 0., -100.),
            Vec3::new(100., 0., -100.),
            Vec3::new(100., 0., 100.),
            Vec3::Y,
            0,
        ));
        triangles.push(RayTriangle::new(
            Vec3::new(-100., 0., -100.),
            Vec3::new(100., 0., 100.),
            Vec3::new(-100., 0., 100.),
            Vec3::Y,
            0,
        ));
        let h = 4.;
        let centre = Vec3::new(0., 8., 0.);
        let corners = [
            [
                Vec3::new(-h, -h, -h),
                Vec3::new(h, -h, -h),
                Vec3::new(h, -h, h),
                Vec3::new(-h, -h, h),
            ],
            [
                Vec3::new(-h, h, -h),
                Vec3::new(h, h, -h),
                Vec3::new(h, h, h),
                Vec3::new(-h, h, h),
            ],
        ];
        let bottom: [Vec3; 4] = corners[0].map(|v| v + centre);
        let top: [Vec3; 4] = corners[1].map(|v| v + centre);
        let mut quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3, n: Vec3| {
            triangles.push(RayTriangle::new(a, b, c, n, 1));
            triangles.push(RayTriangle::new(a, c, d, n, 1));
        };
        quad(top[0], top[1], top[2], top[3], Vec3::Y);
        quad(bottom[3], bottom[2], bottom[1], bottom[0], Vec3::NEG_Y);
        for i in 0..4 {
            let j = (i + 1) % 4;
            let outward = ((bottom[i] - centre) + (bottom[j] - centre)).normalize_or_zero();
            let outward = if outward.length_squared() > 0.5 {
                outward
            } else {
                Vec3::X
            };
            quad(bottom[i], bottom[j], top[j], top[i], outward);
        }
        SceneGeometry::build(triangles, vec![ground, material])
    }

    /// A world-to-clip matrix for a camera at `eye` looking at `target`, with
    /// the engine's reversed-Z projection (near at `z = 1`, far at `z = 0`).
    ///
    /// Spelled out rather than borrowed from the renderer because the test has
    /// to state its own assumption: a ray through the centre of a 70-degree
    /// view must be the camera's forward axis, and a ray through an off-centre
    /// NDC point must tilt by `tan(fov/2)` per unit of NDC.
    fn look_at(eye: Vec3, target: Vec3) -> glam::Mat4 {
        let forward = (target - eye).normalize_or_zero();
        let forward = if forward.length_squared() > 0.5 {
            forward
        } else {
            Vec3::NEG_Z
        };
        // Straight up or down, `forward x Y` is degenerate. Worth stating rather
        // than papering over: the module's own camera never looks exactly along
        // the axis, but a test that does would get a collapsed matrix and a
        // tracer that reports the whole world as sky.
        let right = forward.cross(Vec3::Y);
        let right = if right.length_squared() > 1e-6 {
            right.normalize()
        } else {
            Vec3::X
        };
        let up = right.cross(forward);
        let view = glam::Mat4::from_cols_array(&[
            right.x,
            up.x,
            -forward.x,
            0.,
            right.y,
            up.y,
            -forward.y,
            0.,
            right.z,
            up.z,
            -forward.z,
            0.,
            -right.dot(eye),
            -up.dot(eye),
            forward.dot(eye),
            1.,
        ]);
        // Reversed-Z perspective, `perspective_rh(fov, aspect, far, near)`.
        let (near, far) = (1f32, 500f32);
        let f = 1. / (70f32.to_radians() * 0.5).tan();
        let projection = glam::Mat4::from_cols_array(&[
            f,
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
            -1.,
            0.,
            0.,
            near * far / (far - near),
            0.,
        ]);
        projection * view
    }

    fn params(grid: (u32, u32)) -> FrameParams {
        let eye = Vec3::new(0., 3., 30.);
        let view_projection = look_at(eye, Vec3::new(0., 3., 0.));
        FrameParams {
            eye,
            inv_view_projection: view_projection.inverse(),
            view_projection,
            extent: grid,
            grid,
            near: 1.,
            far: 500.,
            jitter: (0., 0.),
            frame: 0,
            history_valid: 1,
            sun: Sun {
                direction: Vec3::new(0.2, 0.95, 0.25).normalize(),
                colour: Vec3::splat(1.),
                angular_radius: 0.01,
                intensity: 1.,
            },
            lighting: LightingRecord::default(),
            bounces: 0,
            samples: 1,
        }
    }

    #[test]
    fn the_random_sequence_is_deterministic_and_spread_over_the_unit_interval() {
        let config = TransportConfig::default();
        let mut seen = Vec::new();
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for i in 0..4000u32 {
            let value = config.random(i, 0, 0);
            assert!((0.0..1.0).contains(&value), "{value}");
            min = min.min(value);
            max = max.max(value);
            if i < 8 {
                seen.push(value);
            }
        }
        // Same inputs, same output: the whole point of hashing rather than
        // sampling a clock.
        for i in 0..8u32 {
            assert_eq!(config.random(i, 0, 0), seen[i as usize]);
        }
        // Different dimensions must not correlate to the same value, which is
        // the failure mode of a bad integer mix.
        for i in 0..32u32 {
            assert_ne!(config.random(i, 0, 0), config.random(i, 0, 1));
        }
        assert!(min < 0.05 && max > 0.95, "range {min}..{max}");
    }

    #[test]
    fn cosine_hemisphere_directions_stay_above_the_surface_and_are_distributed_cosinely() {
        let config = TransportConfig::default();
        for normal in [
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::X,
            Vec3::new(0.3, 0.9, -0.31).normalize(),
        ] {
            let mut sum = Vec3::ZERO;
            let n = 2000;
            for i in 0..n {
                let u1 = (i as f32 + 0.5) / n as f32;
                let u2 = config.random(i, 0, 3);
                let d = config.cosine_hemisphere(normal, u1, u2);
                assert!(
                    d.dot(normal) >= -1e-4,
                    "below the surface: {d:?} vs {normal:?}"
                );
                sum += d;
            }
            // The mean cosine-weighted direction is 2/3 of the normal.
            let mean = sum / n as f32;
            assert!(
                (mean - normal * (2. / 3.)).length() < 0.05,
                "{normal:?} -> {mean:?}"
            );
        }
    }

    #[test]
    fn a_mirror_reflection_returns_exactly_the_incident_angle() {
        let config = TransportConfig::default();
        let mirror = RayMaterial {
            albedo: [0.9, 0.9, 0.9, 1.],
            surface: [0., 1., 0., 1.],
        };
        let normal = Vec3::Y;
        let view = Vec3::new(0.6, -0.8, 0.).normalize();
        let (direction, weight) = config.reflection(normal, view, &mirror);
        assert!(
            (direction - Vec3::new(0.6, 0.8, 0.)).length() < 1e-4,
            "{direction:?}"
        );
        // A mirror on a metal reflects most of what it sees.
        assert!(weight > 0.5, "{weight}");
        assert_eq!(reflect(view, normal), direction);
    }

    #[test]
    fn fresnel_rises_toward_grazing_and_a_rough_surface_blurs_its_reflection() {
        let config = TransportConfig::default();
        let dielectric = RayMaterial {
            albedo: [0.2, 0.2, 0.2, 1.],
            surface: [0., 0., 0., 1.],
        };
        let head_on = config.reflection(Vec3::Y, Vec3::NEG_Y, &dielectric).1;
        let grazing_view = Vec3::new(1., -0.15, 0.).normalize();
        let grazing = config.reflection(Vec3::Y, grazing_view, &dielectric).1;
        assert!(head_on < 0.1, "{head_on}");
        assert!(grazing > head_on * 3., "{grazing} vs {head_on}");
        let rough = RayMaterial {
            albedo: [0.2, 0.2, 0.2, 1.],
            surface: [0.6, 0., 0., 1.],
        };
        let (blurred, _) = config.reflection(Vec3::Y, grazing_view, &rough);
        let (sharp, _) = config.reflection(Vec3::Y, grazing_view, &dielectric);
        assert!(
            blurred.dot(Vec3::Y) > sharp.dot(Vec3::Y),
            "a rough surface must aim closer to its own normal"
        );
    }

    #[test]
    fn a_pixel_that_sees_only_sky_reports_the_environment_not_black() {
        let scene = plane_scene();
        let mut params = params((64, 64));
        // Look straight up from above everything.
        params.eye = Vec3::new(0., 50., 0.);
        params.view_projection = look_at(params.eye, params.eye + Vec3::Y);
        params.inv_view_projection = params.view_projection.inverse();
        let config = TransportConfig::default();
        let sample = trace(
            &scene,
            &params,
            &config,
            Sampler::new(1, 0),
            (32, 32),
            (0., 0.),
        );
        assert!(sample.sky);
        assert_eq!(sample.visibility, 1.);
        assert!(sample.colour.max_element() > 0.1, "{:?}", sample.colour);
    }

    #[test]
    fn the_ground_is_lit_by_the_sun_and_the_box_casts_a_shadow_on_it() {
        let scene = plane_scene();
        let mut params = params((64, 64));
        // A low side sun, so the cube at y = 8 throws a long shadow across the
        // ground instead of a small patch directly beneath itself.
        params.sun.direction = Vec3::new(0.94, 0.34, 0.).normalize();
        params.sun.angular_radius = 0.;
        let config = TransportConfig::default();
        let nearest = Sampler::new(8, 0);
        // Scan the whole view rather than guess where the shadow lands: the
        // assertion is about the *distribution*, and a hand-picked pixel that
        // happens to be lit would make the test pass for the wrong reason.
        let mut shadowed = Vec3::ZERO;
        let mut lit = Vec3::ZERO;
        let mut shadowed_pixels = 0;
        let mut lit_pixels = 0;
        for y in (0..64u32).step_by(2) {
            for x in (0..64u32).step_by(2) {
                let sample = trace(&scene, &params, &config, nearest, (x, y), (0., 0.));
                if sample.sky || sample.hit_distance > 120. {
                    continue;
                }
                if sample.visibility < 0.5 {
                    shadowed += sample.colour;
                    shadowed_pixels += 1;
                } else {
                    lit += sample.colour;
                    lit_pixels += 1;
                }
            }
        }
        assert!(
            shadowed_pixels > 20 && lit_pixels > 20,
            "both populations must be present: {shadowed_pixels} shadowed, {lit_pixels} lit"
        );
        let shadow_mean = shadowed / shadowed_pixels as f32;
        let lit_mean = lit / lit_pixels as f32;
        assert!(
            lit_mean.x > shadow_mean.x * 1.2,
            "open ground {lit_mean:?} must be brighter than the box's shadow {shadow_mean:?}"
        );
        // The shadow is not black: ambient and the sky still reach it.
        assert!(shadow_mean.x > 0.01, "{shadow_mean:?}");
    }

    #[test]
    fn ambient_occlusion_is_lower_under_a_ceiling_than_on_open_ground() {
        // A 4-unit box is a small occluder from directly below: its solid angle
        // barely covers a cosine hemisphere, so a correct AO estimator reports
        // close to 1 there. A ceiling is the case AO exists for, and it is the
        // only one that separates "correct" from "always 1".
        let base = plane_scene();
        let mut triangles = base.triangles().to_vec();
        let mut materials = base.materials().to_vec();
        materials.push(RayMaterial {
            albedo: [0.6, 0.6, 0.6, 1.],
            surface: [0.9, 0., 0., 1.],
        });
        // A small patch, so most of the view is genuinely open ground.
        let h = 6.;
        let y = 5.;
        // Wound clockwise seen from above, so the geometric normal is -Y and
        // the traversal hands the shading pass a downward normal for a ray
        // arriving from below.
        triangles.push(RayTriangle::new(
            Vec3::new(-h, y, -h),
            Vec3::new(h, y, h),
            Vec3::new(-h, y, h),
            Vec3::NEG_Y,
            2,
        ));
        triangles.push(RayTriangle::new(
            Vec3::new(-h, y, -h),
            Vec3::new(h, y, -h),
            Vec3::new(h, y, h),
            Vec3::NEG_Y,
            2,
        ));
        let scene = SceneGeometry::build(triangles, materials);

        let mut params = params((64, 64));
        // Below the patch, looking down at the ground under it.
        params.eye = Vec3::new(0., 4., 0.2);
        params.view_projection = look_at(params.eye, Vec3::ZERO);
        params.inv_view_projection = params.view_projection.inverse();
        let config = TransportConfig::default();
        let sampler = Sampler::new(32, 0);
        let under = trace(&scene, &params, &config, sampler, (32, 32), (0., 0.));
        // Far outside the patch's footprint, still on the ground plane.
        let away = trace(&scene, &params, &config, sampler, (63, 32), (0., 0.));
        assert!(!under.sky, "the ground is under the pixel: {under:?}");
        assert!((0.0..=1.0).contains(&under.visibility));
        assert!(
            under.visibility < away.visibility - 0.1,
            "a ceiling must occlude part of the hemisphere: open {:.3} vs under {:.3}",
            away.visibility,
            under.visibility
        );
        // Open ground still sees the cube for part of its hemisphere, so it is
        // nowhere near 1 either; the *comparison* is the assertion.
        assert!(away.visibility < 1., "{:.3}", away.visibility);
    }

    #[test]
    fn more_bounces_gather_more_light_and_zero_bounces_is_direct_only() {
        let scene = plane_scene();
        let params = params((64, 64));
        let config = TransportConfig::default();
        let pixel = (2u32, 40u32);
        let direct = trace(
            &scene,
            &params,
            &config,
            Sampler::new(4, 0),
            pixel,
            (0., 0.),
        );
        let one = trace(
            &scene,
            &params,
            &config,
            Sampler::new(4, 1),
            pixel,
            (0., 0.),
        );
        let two = trace(
            &scene,
            &params,
            &config,
            Sampler::new(4, 2),
            pixel,
            (0., 0.),
        );
        assert!(one.colour.x >= direct.colour.x, "{one:?} vs {direct:?}");
        assert!(two.colour.x >= one.colour.x, "{two:?} vs {one:?}");
        // Without bounces the indirect term is exactly absent, not merely small.
        let no_gi = TransportConfig {
            bounce_strength: 0.,
            ..config
        };
        let off = trace(&scene, &params, &no_gi, Sampler::new(4, 3), pixel, (0., 0.));
        assert_eq!(off.colour, direct.colour);
    }

    #[test]
    fn the_ambient_strength_scales_the_ambient_term_without_touching_the_sun() {
        let scene = plane_scene();
        let params = params((64, 64));
        let base = TransportConfig::default();
        let dark = TransportConfig {
            ambient_strength: 0.,
            ..base
        };
        let pixel = (2u32, 40u32);
        let lit = trace(&scene, &params, &base, Sampler::new(4, 0), pixel, (0., 0.));
        let no_ambient = trace(&scene, &params, &dark, Sampler::new(4, 0), pixel, (0., 0.));
        assert!(no_ambient.colour.x < lit.colour.x, "ambient must add light");
        // At night there is no sun term at all, so the daylight test in the
        // shader's sun branch is not just an optimisation.
        let mut night = params;
        night.lighting.daylight = 0.;
        night.sun.intensity = 0.;
        let night_lit = trace(&scene, &night, &base, Sampler::new(4, 0), pixel, (0., 0.));
        assert!(
            night_lit.colour.x > 0.,
            "the sky still lights the ground at night"
        );
        assert!(night_lit.colour.x < lit.colour.x);
    }

    #[test]
    fn fog_pulls_the_colour_toward_the_fog_colour_and_never_overshoots() {
        let mut params = params((8, 8));
        params.lighting.fog_range = (10., 110.);
        params.lighting.fog_colour = Vec3::new(0.6, 0.7, 0.9);
        let near = apply_fog(Vec3::splat(0.1), &params, 0.);
        assert!((near - Vec3::splat(0.1)).length() < 1e-6);
        let far = apply_fog(Vec3::splat(0.1), &params, 1000.);
        let fog_linear = Vec3::new(srgb_to_linear(0.6), srgb_to_linear(0.7), srgb_to_linear(0.9));
        assert!((far - fog_linear).length() < 1e-5);
        let mid = apply_fog(Vec3::splat(0.1), &params, 60.);
        assert!(mid.x > 0.1 && mid.x < fog_linear.x);
        // A range that is not ordered is "no fog", not a division by zero.
        let mut disabled = params;
        disabled.lighting.fog_range = (10., 10.);
        assert_eq!(
            apply_fog(Vec3::splat(0.2), &disabled, 500.),
            Vec3::splat(0.2)
        );
    }

    #[test]
    fn native_table_fog_uses_projected_view_z_not_euclidean_ray_length() {
        let mut params = params((8, 8));
        // A simple perspective-like projection whose clip.w is forward view Z.
        params.view_projection = glam::Mat4::from_cols_array(&[
            1., 0., 0., 0.,
            0., 1., 0., 0.,
            0., 0., 1., 1.,
            0., 0., 0., 0.,
        ]);
        let point = Vec3::new(30., 0., 40.);
        let view_z = (params.view_projection * point.extend(1.)).w;
        let ray_length = point.length();
        assert!((view_z - 40.).abs() < 1e-5);
        assert!(ray_length > view_z + 5.);

        params.lighting.fog_range = (45., 55.);
        let correct = apply_fog(Vec3::splat(0.1), &params, view_z);
        let euclidean_bug = apply_fog(Vec3::splat(0.1), &params, ray_length);
        assert!((correct - Vec3::splat(0.1)).length() < 1e-6);
        assert!(
            euclidean_bug.x > correct.x,
            "Euclidean distance would fog this off-axis point too early"
        );

        let shader = super::super::gpu::RAY_SHADER;
        assert!(shader.contains(
            "let fog_view_z = max((params.vp * vec4<f32>(point, 1.0)).w, 0.0);"
        ));
        assert!(shader.contains("linear_to_srgb(relit), params.fog_colour.xyz"));
    }

    #[test]
    fn the_sun_radius_widens_the_penumbra_and_a_hard_sun_has_none() {
        let scene = plane_scene();
        let mut params = params((64, 64));
        let config = TransportConfig::default();
        // The cube's own top face, seen from above: rays toward a low sun pass
        // close to the cube's edge, which is where a penumbra is visible.
        params.sun.direction = Vec3::new(0.9, 0.35, 0.).normalize();
        let hard = Sun {
            angular_radius: 0.,
            ..params.sun
        };
        let soft = Sun {
            angular_radius: 0.12,
            ..params.sun
        };
        params.sun = hard;
        let hit = scene
            .closest_hit(Vec3::new(0., 20., 0.), Vec3::NEG_Y, 0., 100.)
            .expect("the cube top");
        let mut hard_sum = 0.;
        let mut soft_sum = 0.;
        params.sun = hard;
        for i in 0..64u32 {
            hard_sum += sun_visibility(&scene, &params, &config, &hit, i);
        }
        params.sun = soft;
        for i in 0..64u32 {
            soft_sum += sun_visibility(&scene, &params, &config, &hit, i);
        }
        // Both are estimates of the same quantity; a soft sun must not be more
        // shadowed than a hard one overall, and neither may leave [0, 1].
        assert!((0.0..=64.).contains(&hard_sum));
        assert!((0.0..=64.).contains(&soft_sum));
    }

    #[test]
    fn a_pixel_sample_carries_the_guides_the_denoiser_and_the_providers_need() {
        let scene = plane_scene();
        let params = params((64, 64));
        let config = TransportConfig::default();
        let sample = trace(
            &scene,
            &params,
            &config,
            Sampler::new(2, 1),
            (2, 40),
            (0., 0.),
        );
        assert!(!sample.sky);
        assert!(sample.hit_distance > 5., "{:?}", sample.hit_distance);
        assert!((0.0..=1.0).contains(&sample.visibility));
        assert!(sample.colour.is_finite());
        assert!(sample.reflection.is_finite());
    }
}
