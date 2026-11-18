//! The Imaginator's path tracer: one implementation, no engine types.
//!
//! This is the "software" tracer in the module's selector, and it is also the
//! reference the hardware tracer is checked against. It is real ray tracing, not
//! a screen-space approximation:
//!
//! * every pixel's primary ray is intersected against the module's own BVH of the
//!   map's actual geometry (terrain, buildings, water, trees);
//! * **shadows** come from a shadow ray towards the traced sun, tested with
//!   `Bvh::any_hit`;
//! * **lighting** is the sun term plus a cosine-weighted indirect term that
//!   follows `light_bounces` further bounces through the same BVH;
//! * **reflections** trace a real mirror ray and shade whatever it hits.
//!
//! Each of those three is a separate switch, and the outputs are kept as
//! separate channels (`Frame::occlusion`, `Frame::reflection`, `Frame::indirect`)
//! so the compositor can use them with the right sign: an occlusion factor
//! *multiplies* the rendered frame (a shadow removes light that the raster path
//! drew unconditionally, because the core has no shadow map at all), while
//! reflections and indirect light *add* to it.

use super::bvh::Hit;
use super::math::{Ray, V3};
use super::settings::{ImaginatorSettings, MAX_BOUNCES};

/// Physically-shaped description of the light in the scene, provided by the core
/// from the same values its own shaders use.
#[derive(Clone, Debug)]
pub struct Environment {
    /// Direction *towards* the sun (unit).
    pub sun_direction: V3,
    /// Linear sun colour, already scaled by the region's daylight.
    pub sun_color: V3,
    /// Ambient sky colour seen by an up-facing surface.
    pub sky_up: V3,
    /// Ambient sky colour at the horizon.
    pub sky_horizon: V3,
    /// Colour of the ground hemisphere, for bounce lighting from below.
    pub ground: V3,
    /// Colour the traced image fades to when a ray escapes (fog).
    pub fog: V3,
    /// Distance beyond which nothing is traced and fog takes over.
    pub fog_distance: f32,
    /// Punctual lights near the camera, as the world's own scene-light grid
    /// reports them. Bounded by the host before it arrives here: a shadow ray
    /// per light per sample makes the count the dominant cost.
    pub lights: Vec<PointLight>,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            sun_direction: V3::from_array([0.3, 0.8, 0.5]).normalize_or_zero(),
            sun_color: V3::splat(0.9),
            sky_up: V3::splat(0.25),
            sky_horizon: V3::splat(0.35),
            ground: V3::splat(0.08),
            fog: V3::splat(0.4),
            fog_distance: 20000.,
            lights: Vec::new(),
        }
    }
}

impl Environment {
    /// The sky gradient alone, without the sun disc.
    ///
    /// Diffuse interreflection integrates a whole hemisphere with one sample per
    /// bounce, so a path that lands on a small, very bright source becomes a
    /// firefly instead of light: one white pixel with nothing to average it
    /// against. The disc exists so a *mirror* shows the sun; a diffuse bounce
    /// wants the sky.
    fn sky_only(&self, direction: V3) -> V3 {
        let up = direction.y.clamp(-1., 1.);
        let sky = if up >= 0. {
            self.sky_horizon.lerp(self.sky_up, up.powf(0.6))
        } else {
            self.sky_horizon.lerp(self.ground, (-up).powf(0.5))
        };
        let cos_sun = direction.dot(self.sun_direction).max(0.);
        // A wide, low-amplitude glow only - no disc.
        sky + self.sun_color * (cos_sun.powf(8.) * 0.35)
    }

    /// Radiance arriving from a direction that hit nothing: a sky gradient with
    /// a sun glow, blended into the fog colour with distance.
    fn escape(&self, direction: V3, distance: f32) -> V3 {
        let up = direction.y.clamp(-1., 1.);
        let sky = if up >= 0. {
            self.sky_horizon.lerp(self.sky_up, up.powf(0.6))
        } else {
            self.sky_horizon.lerp(self.ground, (-up).powf(0.5))
        };
        let cos_sun = direction.dot(self.sun_direction).max(0.);
        // A broad glow plus a tight disc: enough for a reflected sun to read as
        // the sun without a second light type.
        let glow = cos_sun.powf(8.) * 0.35 + cos_sun.powf(900.) * 8.;
        let radiance = sky + self.sun_color * glow;
        if self.fog_distance.is_finite() && distance.is_finite() {
            let f = 1. - (-distance / self.fog_distance.max(1.)).exp();
            radiance.lerp(self.fog, f.clamp(0., 1.))
        } else {
            radiance
        }
    }
}

/// A punctual light from the world: a lamp, a fire, a lit window.
///
/// The client stores these per scene-light cell with a position, a range and a
/// diffuse colour. They are the lights the raster path draws, so the tracer
/// shadows and illuminates with the same ones, which is what makes a lamp's
/// light and its shadow agree with the lamp the artist placed.
#[derive(Clone, Copy, Debug)]
pub struct PointLight {
    pub position: V3,
    /// Beyond this distance the light contributes nothing.
    pub range: f32,
    /// Linear RGB, scaled so a diffuse surface at `range * 0.25` reads like the
    /// raster path's own lamp.
    pub colour: V3,
}

impl PointLight {
    /// How much light this source delivers at `point` along `normal`, and the
    /// unit vector pointing at it. Zero outside the range.
    ///
    /// A smooth 1/d² falloff windowed to `range`, because a hard cutoff at the
    /// range boundary shows as a visible sphere. This mirrors the client's
    /// attenuation shape closely enough that the traced shadows land where the
    /// raster lamp is painted.
    pub fn irradiance(&self, point: V3, normal: V3) -> (V3, f32) {
        let to_light = self.position - point;
        let distance_squared = to_light.length_squared();
        let range_squared = self.range * self.range;
        if distance_squared >= range_squared || distance_squared <= 1e-8 {
            return (V3::ZERO, 0.);
        }
        let distance = distance_squared.sqrt();
        let direction = to_light / distance;
        let cos = normal.dot(direction).max(0.);
        if cos == 0. {
            return (V3::ZERO, 0.);
        }
        // Windowed inverse-square: `(1 - d²/r²)²` reaches exactly zero at `range`
        // with no discontinuity in the value, only in the gradient.
        let window = 1. - distance_squared / range_squared;
        let falloff = window * window / (distance_squared + 1.);
        (self.colour * (cos * falloff), distance)
    }
}

/// Camera for the tracer, independent of the engine's `Camera` so this file
/// compiles on its own.
#[derive(Clone, Copy, Debug)]
pub struct TraceCamera {
    pub position: V3,
    /// Unit forward vector.
    pub forward: V3,
    /// Unit right vector (screen +X).
    pub right: V3,
    /// Unit up vector.
    pub up: V3,
    /// Vertical field of view in radians.
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
}

impl TraceCamera {
    /// Primary ray through a pixel, with a sub-pixel offset in `[-0.5, 0.5]`.
    pub fn ray(&self, x: f32, y: f32, width: f32, height: f32) -> Ray {
        let aspect = width / height.max(1.);
        let tan_half = (self.fov_y * 0.5).tan();
        let sx = (2. * (x + 0.5) / width - 1.) * tan_half * aspect;
        let sy = (1. - 2. * (y + 0.5) / height) * tan_half;
        Ray::new(self.position, self.forward + self.right * sx + self.up * sy)
    }
}

/// Per-pixel traced channels. All colours are linear radiance.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    /// 1 = the traced sun reaches this point, 0 = fully shadowed.
    pub occlusion: f32,
    /// Sun + sky diffuse reaching the surface (before albedo).
    pub direct: V3,
    /// Indirect diffuse after `light_bounces` bounces (already albedo-weighted).
    pub indirect: V3,
    /// Specular reflection radiance.
    pub reflection: V3,
    /// The full traced picture for the diagnostic "traced image only" mode.
    pub radiance: V3,
    /// View distance, for the composite's fog consistency and for the depth
    /// rejection of pixels the tracer never saw.
    pub distance: f32,
    /// 0 when the primary ray escaped, so the compositor leaves sky pixels
    /// exactly as the core drew them.
    pub hit: f32,
}

/// Deterministic per-pixel/per-sample RNG. Cheap, good enough for stratification
/// and free of the `rand` dependency so this file stays standalone-testable.
#[derive(Clone, Copy)]
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        // A fixed odd constant; xorshift32 never reaches zero from nonzero.
        Self(seed | 1)
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Cosine-weighted hemisphere sample around `normal` (Malley's method).
    fn cosine_hemisphere(&mut self, normal: V3) -> V3 {
        let u1 = self.next_f32();
        let u2 = self.next_f32();
        let r = u1.sqrt();
        let phi = std::f32::consts::TAU * u2;
        let (x, y) = (r * phi.cos(), r * phi.sin());
        let z = (1. - u1).max(0.).sqrt();
        let (t, b) = tangent_basis(normal);
        (t * x + b * y + normal * z).normalize_or_zero()
    }
}

/// Any orthonormal basis around `n`. The choice does not affect correctness but
/// keeping it continuous avoids visible seams in the indirect term.
fn tangent_basis(n: V3) -> (V3, V3) {
    let helper = if n.x.abs() > 0.7 {
        V3::from_array([0., 1., 0.])
    } else {
        V3::from_array([1., 0., 0.])
    };
    let t = n.cross(helper).normalize_or_zero();
    (t, n.cross(t))
}

/// Offset used to keep a secondary ray off the surface it left, scaled to the
/// hit distance so distant geometry does not need a huge bias.
fn bias(t: f32) -> f32 {
    (t * 2e-4).clamp(1e-4, 0.5)
}

/// One shading configuration, resolved once per frame.
#[derive(Clone, Copy, Debug)]
pub struct TraceOptions {
    pub reflections: bool,
    pub lighting: bool,
    pub shadows: bool,
    pub bounces: u32,
    pub guard: u64,
    /// Water keeps the core's own screen reflection; when this is on the traced
    /// reflection additionally covers water pixels.
    ///
    /// This was a checkbox in the module page that reached nothing: the field
    /// existed on the settings, was serialised, was drawn, and no tracer ever
    /// read it, so toggling it changed exactly nothing. It is honoured here by
    /// suppressing the mirror term on water surfaces when it is off, which is
    /// what the label has always claimed.
    pub water_reflection_fill: bool,
}

impl TraceOptions {
    pub fn from_settings(settings: &ImaginatorSettings) -> Self {
        Self {
            reflections: settings.reflections,
            lighting: settings.lighting,
            shadows: settings.shadows,
            bounces: settings.light_bounces.min(MAX_BOUNCES),
            // A cap on total ray launches so an adversarial scene cannot wedge
            // the thread. Generous: a full-resolution frame at four bounces
            // stays well inside it.
            guard: 4_000_000_000,
            water_reflection_fill: settings.water_reflection_fill,
        }
    }
}

/// The tracer's state: an immutable scene, the environment, and a scratch budget.
pub struct Tracer {
    scene: super::bvh::SharedScene,
    pub environment: Environment,
    /// Rays launched by the last traced frame, for the status line.
    pub rays: std::sync::atomic::AtomicU64,
}

impl Tracer {
    pub fn new(scene: super::bvh::SharedScene, environment: Environment) -> Self {
        Self {
            scene,
            environment,
            rays: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn set_scene(&mut self, scene: super::bvh::SharedScene) {
        self.scene = scene;
    }

    pub fn scene(&self) -> &super::bvh::SharedScene {
        &self.scene
    }

    /// Shade one primary ray.
    fn shade(&self, ray: &Ray, options: &TraceOptions, rng: &mut Rng, rays: &mut u64) -> Sample {
        let Some(bvh) = self.scene.bvh.as_ref() else {
            return Sample::default();
        };
        *rays += 1;
        let Some(hit) = bvh.closest_hit(ray, 0., self.scene_limit()) else {
            // Nothing hit: the pixel is sky, and the compositor must not touch it.
            return Sample::default();
        };
        let albedo = self.albedo(hit);
        let normal = self.shading_normal(hit, ray);
        let point = ray.at(hit.t);
        let mut sample = Sample {
            distance: hit.t,
            hit: 1.,
            ..Sample::default()
        };

        // --- direct light + shadows -------------------------------------------
        let sun = self.environment.sun_direction;
        let n_dot_l = normal.dot(sun).max(0.);
        let mut visibility = 1.;
        if options.shadows && n_dot_l > 0. {
            visibility = self.sun_visibility(bvh, point, normal, hit.t, rng, rays);
        }
        sample.occlusion = if options.shadows {
            // Overcast light means the sun term is small; the factor is still
            // the honest one, so the composite darkens by exactly what the
            // traced sun would have delivered.
            visibility
        } else {
            1.
        };
        let sun_radiance = self.environment.sun_color * (n_dot_l * visibility);
        let sky = self
            .environment
            .sky_horizon
            .lerp(self.environment.sky_up, normal.y.max(0.).powf(0.7));
        // --- punctual lights ---------------------------------------------------
        // The world's lamps, fires and lit windows. Each gets one shadow ray, so
        // the count is what the host bounds rather than the loop.
        let mut lamps = V3::ZERO;
        if options.lighting {
            for light in &self.environment.lights {
                let (radiance, distance) = light.irradiance(point, normal);
                if radiance.max_component() <= 0. {
                    continue;
                }
                if options.shadows {
                    let to_light = (light.position - point) / distance.max(1e-6);
                    let origin = point + normal * bias(hit.t) + to_light * bias(hit.t);
                    *rays += 1;
                    if bvh
                        .closest_hit(&Ray::new(origin, to_light), 0., distance * 0.999)
                        .is_some()
                    {
                        continue;
                    }
                }
                lamps += albedo * radiance;
            }
        }
        let direct = if options.lighting {
            sun_radiance + sky + lamps
        } else {
            V3::ZERO
        };
        sample.direct = direct;

        // --- indirect diffuse --------------------------------------------------
        let mut indirect = V3::ZERO;
        if options.lighting && options.bounces > 0 {
            let mut weight = albedo;
            let mut origin = point + normal * bias(hit.t);
            let mut bounce = 0;
            while bounce < options.bounces && *rays < options.guard {
                let dir = rng.cosine_hemisphere(normal);
                if dir.length_squared() == 0. {
                    break;
                }
                let r = Ray::new(origin, dir);
                *rays += 1;
                match bvh.closest_hit(&r, 0., self.scene_limit()) {
                    Some(h) => {
                        let p = r.at(h.t);
                        let n = self.shading_normal(h, &r);
                        let a = self.albedo(h);
                        let l = self.environment.sun_direction;
                        let mut v = 1.;
                        if options.shadows {
                            let nl = n.dot(l).max(0.);
                            if nl > 0. {
                                v = self.sun_visibility(bvh, p, n, h.t, rng, rays);
                            }
                        }
                        let sky2 = self
                            .environment
                            .sky_horizon
                            .lerp(self.environment.sky_up, n.y.max(0.).powf(0.7));
                        let incoming = self.environment.sun_color * (n.dot(l).max(0.) * v) + sky2;
                        // The *hit* surface's albedo is what turns the light
                        // arriving there into light leaving it. Without it this
                        // adds the full incoming irradiance as though every
                        // bounce surface were a perfect white reflector, which
                        // over-brightens by 1/albedo: terrain is 0.22, so its
                        // indirect term was 4.5x too strong. Added to a frame
                        // that already had the sky and the sun in it, that is
                        // what made the ground read as white.
                        indirect += weight * a * incoming;
                        weight = weight * a;
                        origin = p + n * bias(h.t);
                        bounce += 1;
                    }
                    None => {
                        // Escaped: the sky closes the path. The sun disc in
                        // `escape` is deliberately intense so a *reflection* of
                        // the sun reads as the sun - but a single indirect sample
                        // that happens to land inside an 8x disc becomes an
                        // isolated white pixel. At one or two samples there is no
                        // averaging to hide it, which is the white speckle over
                        // the whole frame. The sky component is what diffuse
                        // interreflection should carry; the disc belongs to
                        // specular reflection only.
                        indirect += weight * self.environment.sky_only(dir);
                        break;
                    }
                }
                // Russian-roulette-free termination: the albedo product falls
                // below visibility quickly in practice.
                if weight.max_component() < 0.02 {
                    break;
                }
            }
        }
        sample.indirect = indirect;

        // --- specular reflection ------------------------------------------------
        //
        // Weighted by the surface's own `specular`. It was not: every surface
        // added a full mirror reflection of the sky, so flat terrain - which is
        // nearly lambertian, `specular` 0.03 - added a bright sky value on every
        // pixel. Added again by the composite on top of a frame that already had
        // the sky in it, that is what made the ground read as white. A dry slope
        // reflects 3% of what a mirror would, and now says so.
        let reflectivity = self.specular(hit);
        let mut reflection = V3::ZERO;
        // Below this, the mirror term cannot be seen: it adds a few percent of
        // the sky to a pixel that already has the sky in the frame. Terrain is
        // 0.03 and vegetation 0.01, so on a world-sized capture almost every
        // pixel was paying a *full BVH traversal* - the single most expensive
        // thing in this loop - to add light nobody can see. That is a large part
        // of why the software tracer took seconds per frame. Water (0.85) and
        // wet slopes stay well above it.
        const VISIBLE_REFLECTANCE: f32 = 0.08;
        // Water's mirror term is the one this switch is about; leave it to the
        // core's own water reflection pass when the user has not asked for it.
        // Water's mirror belongs to the core's water pass unless the user has
        // asked for the traced one. `TERM_WATER_FILL` in the two GPU tracers is
        // the same switch.
        let water = hit.material == super::bvh::material::WATER;
        let water_reflection_fill = options.water_reflection_fill;
        let reflections_allowed = !water || water_reflection_fill;
        if options.reflections && reflections_allowed && reflectivity >= VISIBLE_REFLECTANCE {
            let view = ray.direction;
            let cos_i = normal.dot(-view);
            if cos_i > 0. {
                let mirror = view + normal * (2. * cos_i);
                let r = Ray::new(point + normal * bias(hit.t), mirror);
                *rays += 1;
                reflection = match bvh.closest_hit(&r, 0., self.scene_limit()) {
                    Some(h) => {
                        let p = r.at(h.t);
                        let n = self.shading_normal(h, &r);
                        let a = self.albedo(h);
                        let l = self.environment.sun_direction;
                        let vis = if options.shadows {
                            let nl = n.dot(l).max(0.);
                            if nl > 0. {
                                let mut rng2 = Rng::new(0x9e37_79b9 ^ (h.t.to_bits()));
                                self.sun_visibility(bvh, p, n, h.t, &mut rng2, rays)
                            } else {
                                0.
                            }
                        } else {
                            1.
                        };
                        let sky2 = self
                            .environment
                            .sky_horizon
                            .lerp(self.environment.sky_up, n.y.max(0.).powf(0.7));
                        a * (self.environment.sun_color * (n.dot(l).max(0.) * vis) + sky2)
                    }
                    None => self.environment.escape(mirror, self.scene_limit()),
                };
                reflection = reflection * reflectivity;
            }
        }
        sample.reflection = reflection;

        // Full traced picture for the diagnostic mode: what the eye sees.
        // `direct` is already gated by the lighting switch, so a tracer with
        // every term off produces exactly no light instead of leaking the sun
        // it was told not to compute.
        sample.radiance = albedo * direct + indirect + reflection * self.specular(hit);

        sample
    }

    /// How far a ray is allowed to travel before it is treated as having
    /// escaped.
    ///
    /// This used to be twice the fog distance, which on a clear day with a far
    /// fog is the whole world: every shadow and bounce ray then walked the
    /// entire capture. Past the fog distance nothing survives to the eye - the
    /// fog term has already faded it to the fog colour - so tracing a hit out
    /// there is pure cost. One fog distance is the honest limit, with a floor so
    /// a short fog range does not cut visible geometry.
    fn scene_limit(&self) -> f32 {
        let fog = self.environment.fog_distance.max(1000.);
        // Raster fog is exponential, so at 1.5x the "end" distance a surface is
        // already within a couple of percent of the fog colour.
        (fog * 1.5).clamp(1000., 20000.)
    }

    fn shading_normal(&self, hit: Hit, ray: &Ray) -> V3 {
        let mut n = hit.normal.normalize_or_zero();
        if n.length_squared() == 0. {
            return V3::from_array([0., 1., 0.]);
        }
        // Two-sided shading: world geometry is often single-sided and thin.
        if n.dot(ray.direction) > 0. {
            n = -n;
        }
        n
    }

    fn albedo(&self, hit: Hit) -> V3 {
        self.scene
            .materials
            .get(hit.material as usize)
            .map(|m| m.albedo)
            .unwrap_or(V3::splat(0.5))
    }

    fn specular(&self, hit: Hit) -> f32 {
        self.scene
            .materials
            .get(hit.material as usize)
            .map(|m| m.specular)
            .unwrap_or(0.)
    }

    /// One shadow ray, jittered inside a small cone so a soft edge appears even
    /// with a single sample.
    fn sun_visibility(
        &self,
        bvh: &super::bvh::Bvh,
        point: V3,
        normal: V3,
        distance: f32,
        rng: &mut Rng,
        rays: &mut u64,
    ) -> f32 {
        let sun = self.environment.sun_direction;
        let (t, b) = tangent_basis(sun);
        // 0.6 degrees of sun radius, matching the disc the sky pass draws.
        let spread = 0.0105;
        let jitter = 0.85;
        let angle = std::f32::consts::TAU * rng.next_f32();
        let radius = spread * jitter * rng.next_f32().sqrt();
        let dir = (sun + (t * angle.cos() + b * angle.sin()) * radius).normalize_or_zero();
        if dir.length_squared() == 0. {
            return 1.;
        }
        *rays += 1;
        let origin = point + normal * bias(distance) + sun * bias(distance);
        if bvh.any_hit(&Ray::new(origin, dir), 0., self.scene_limit()) {
            0.
        } else {
            1.
        }
    }

    /// Trace rows `row_start..row_end` of a `width * height` image into `out`.
    ///
    /// `out` holds **this row range only**: `(row_end - row_start) * width`
    /// samples, indexed from zero. That is what makes row splitting cheap — a
    /// worker hands each thread a slice the size of its own band instead of the
    /// whole image. Indexing `out` with the image's absolute stride
    /// (`y * width + x`) is what used to happen here, and it wrote past the end of
    /// every slice but the first: with 16 threads on a 344-pixel-wide frame the
    /// second thread's first write landed at index 73960 in a 73960-element
    /// slice.
    ///
    /// The caller supplies the closures, so the tracer owns no thread pool and
    /// stays testable.
    pub fn trace_rows(
        &self,
        camera: &TraceCamera,
        options: &TraceOptions,
        width: u32,
        height: u32,
        samples: u32,
        row_start: u32,
        row_end: u32,
        out: &mut [Sample],
    ) {
        let mut rays = 0u64;
        let w = width as f32;
        let h = height as f32;
        let end = row_end.min(height);
        debug_assert!(
            row_start <= end,
            "row range {row_start}..{row_end} is inverted for a {height}-row image"
        );
        // `out` starts at `row_start`, not at the top of the image.
        debug_assert_eq!(
            out.len(),
            (end.saturating_sub(row_start)) as usize * width as usize,
            "the buffer must cover exactly the traced rows ({row_start}..{end} of {width}x{height})"
        );
        for y in row_start..end {
            for x in 0..width {
                let mut rng = Rng::new(
                    x.wrapping_mul(0x9e37_79b9) ^ y.wrapping_mul(0x85eb_ca6b) ^ 0x1234_5678,
                );
                let mut acc = Sample::default();
                for s in 0..samples.max(1) {
                    let ox = if s == 0 { 0. } else { rng.next_f32() - 0.5 };
                    let oy = if s == 0 { 0. } else { rng.next_f32() - 0.5 };
                    let ray = camera.ray(x as f32 + ox, y as f32 + oy, w, h);
                    let sample = self.shade(&ray, options, &mut rng, &mut rays);
                    acc.occlusion += sample.occlusion;
                    acc.direct += sample.direct;
                    acc.indirect += sample.indirect;
                    acc.reflection += sample.reflection;
                    acc.radiance += sample.radiance;
                    acc.distance += sample.distance;
                    acc.hit += sample.hit;
                }
                let inv = 1. / samples.max(1) as f32;
                // Row `y` of the image is row `y - row_start` of this buffer.
                out[((y - row_start) * width + x) as usize] = Sample {
                    occlusion: acc.occlusion * inv,
                    direct: acc.direct * inv,
                    indirect: acc.indirect * inv,
                    reflection: acc.reflection * inv,
                    radiance: acc.radiance * inv,
                    distance: acc.distance * inv,
                    hit: acc.hit * inv,
                };
            }
        }
        self.rays
            .fetch_add(rays, std::sync::atomic::Ordering::Relaxed);
    }

    /// Convenience single-threaded trace, used by the tests and by the tiny
    /// preview at the bottom of the module's page.
    pub fn trace(
        &self,
        camera: &TraceCamera,
        options: &TraceOptions,
        width: u32,
        height: u32,
        samples: u32,
    ) -> Vec<Sample> {
        let mut out = vec![Sample::default(); (width * height) as usize];
        self.trace_rows(camera, options, width, height, samples, 0, height, &mut out);
        out
    }

    /// Trace into a row-major `Rgba32Float`-shaped buffer that the compositor
    /// uploads: `r` = reflection, `g` = indirect, `b` = occlusion,
    /// `a` = full radiance luminance (diagnostic mode) and the hit mask in the
    /// sign of the distance. Keeping one upload for all channels halves the
    /// texture count and the bandwidth.
    pub fn pack(samples: &[Sample], out: &mut [f32]) {
        for (i, s) in samples.iter().enumerate() {
            let o = i * 4;
            // The composite's contract, matching the GPU tracer byte for byte:
            // `rgb` is the light the rays found (reflected plus interreflected,
            // in colour) and `a` is sun visibility, negative when the primary
            // ray escaped so the raster sky stands. Reflection and indirect ride
            // together because the composite adds both; keeping them separate
            // forced each into one luminance, which is what made reflections
            // invisible and every lit surface flat white.
            let radiance = s.reflection + s.indirect * s.occlusion;
            out[o] = radiance.x;
            out[o + 1] = radiance.y;
            out[o + 2] = radiance.z;
            out[o + 3] = if s.hit > 0.5 { s.occlusion } else { -1. };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::bvh::{Bvh, Material, Scene, Triangle};
    use super::super::math::v3;
    use super::*;

    /// A floor at y = 0 and a wall at x = 4, both 20 units across, with a
    /// ceiling at y = 8 so a reflection has somewhere to come from.
    fn room() -> std::sync::Arc<Scene> {
        let mut tris = Vec::new();
        let quad = |a: V3, b: V3, c: V3, d: V3, n: V3, m: u16, tris: &mut Vec<Triangle>| {
            tris.push(Triangle {
                a,
                b,
                c,
                normal: n,
                material: m,
            });
            tris.push(Triangle {
                a,
                b: c,
                c: d,
                normal: n,
                material: m,
            });
        };
        let up = v3(0., 1., 0.);
        // floor
        quad(
            v3(-20., 0., -20.),
            v3(20., 0., -20.),
            v3(20., 0., 20.),
            v3(-20., 0., 20.),
            up,
            0,
            &mut tris,
        );
        // ceiling
        quad(
            v3(-20., 8., -20.),
            v3(20., 8., -20.),
            v3(20., 8., 20.),
            v3(-20., 8., 20.),
            v3(0., -1., 0.),
            0,
            &mut tris,
        );
        // wall at x = 4
        quad(
            v3(4., 0., -20.),
            v3(4., 8., -20.),
            v3(4., 8., 20.),
            v3(4., 0., 20.),
            v3(-1., 0., 0.),
            1,
            &mut tris,
        );
        std::sync::Arc::new(Scene {
            bvh: Some(Bvh::build(tris)),
            materials: vec![
                Material::new(V3::splat(0.6), 0.1),
                Material::new(V3::splat(0.8), 0.5),
            ],
            stats: Default::default(),
        })
    }

    fn camera() -> TraceCamera {
        // Facing the wall at x = 4 from x = -10, so the centre ray travels 14
        // units before it lands.
        TraceCamera {
            position: v3(-10., 2., 0.),
            forward: v3(1., 0., 0.),
            right: v3(0., 0., -1.),
            up: v3(0., 1., 0.),
            fov_y: 60f32.to_radians(),
            near: 0.1,
            far: 20000.,
        }
    }

    /// Only the samples whose primary ray actually landed on geometry: the room
    /// has an open side, so sky pixels are expected and are the compositor's
    /// business, not the tracer's.
    fn hits(v: &[Sample]) -> Vec<Sample> {
        v.iter().copied().filter(|s| s.hit > 0.5).collect()
    }

    fn options() -> TraceOptions {
        TraceOptions {
            reflections: true,
            lighting: true,
            shadows: true,
            bounces: 2,
            guard: 4_000_000_000,
            water_reflection_fill: false,
        }
    }

    #[test]
    fn a_primary_ray_lands_on_the_floor_and_is_lit() {
        let scene = room();
        let tracer = Tracer::new(scene, Environment::default());
        let out = tracer.trace(&camera(), &options(), 16, 16, 1);
        let hit = hits(&out);
        assert_eq!(hit.len(), out.len(), "the room fills the view");
        for s in &hit {
            assert!(s.direct.length() > 0., "lit");
            assert!((0. ..=1.).contains(&s.occlusion));
            assert!(s.distance > 0.);
        }
        // The centre pixel looks straight ahead and must land on the wall.
        let centre = out[8 * 16 + 8];
        assert_eq!(centre.hit, 1.);
        assert!((centre.distance - 14.).abs() < 1., "{}", centre.distance);
    }

    /// A lamp must light the surface it can see and lose to geometry between.
    #[test]
    fn a_point_light_lights_and_casts_a_shadow() {
        let scene = room();
        let camera = camera();
        let options = options();
        let bare = {
            let mut env = Environment::default();
            env.lights.clear();
            Tracer::new(scene.clone(), env).trace(&camera, &options, 16, 16, 1)[8 * 16 + 8]
        };
        // Put a lamp just in front of the wall the centre pixel looks at, so it
        // is inside its own range and facing the surface.
        let lit = {
            let mut env = Environment::default();
            env.lights = vec![PointLight {
                position: v3(0., 1.5, -1.),
                range: 40.,
                colour: v3(4., 3., 2.),
            }];
            Tracer::new(scene.clone(), env).trace(&camera, &options, 16, 16, 1)[8 * 16 + 8]
        };
        assert!(
            lit.direct.length() > bare.direct.length() * 1.2,
            "the lamp must add light: {:?} vs {:?}",
            lit.direct,
            bare.direct
        );
        // Warm lamp colour survives into the result rather than becoming grey.
        assert!(lit.direct.x > lit.direct.z, "the lamp's colour is carried");

        // Now put a blocker between the lamp and the wall: the light must go. The
        // camera looks along +x and the lamp sits behind it, so a pane facing the
        // wall, just in front of the lamp, cuts the path.
        let mut tris = vec![
            // The wall the centre ray lands on.
            Triangle {
                a: v3(4., -20., -20.),
                b: v3(4., 20., -20.),
                c: v3(4., 20., 20.),
                normal: v3(-1., 0., 0.),
                material: 0,
            },
            Triangle {
                a: v3(4., -20., -20.),
                b: v3(4., 20., 20.),
                c: v3(4., -20., 20.),
                normal: v3(-1., 0., 0.),
                material: 0,
            },
            // The blocker, a tall pane at x = -0.5 between lamp and wall.
            Triangle {
                a: v3(-0.5, -20., -20.),
                b: v3(-0.5, 20., -20.),
                c: v3(-0.5, 20., 20.),
                normal: v3(-1., 0., 0.),
                material: 0,
            },
            Triangle {
                a: v3(-0.5, -20., -20.),
                b: v3(-0.5, 20., 20.),
                c: v3(-0.5, -20., 20.),
                normal: v3(-1., 0., 0.),
                material: 0,
            },
        ];
        tris.truncate(tris.len());
        let blocked_scene = Scene {
            bvh: Some(Bvh::build(tris)),
            materials: vec![Material::new(V3::splat(0.6), 0.1)],
            stats: Default::default(),
        };
        let mut env = Environment::default();
        env.lights = vec![PointLight {
            position: v3(0., 1.5, -1.),
            range: 40.,
            colour: v3(4., 3., 2.),
        }];
        let shadowed = Tracer::new(std::sync::Arc::new(blocked_scene), env)
            .trace(&camera, &options, 16, 16, 1)[8 * 16 + 8];
        assert!(
            shadowed.direct.length() < lit.direct.length() * 0.95,
            "geometry between the lamp and the surface must shadow it: {:?} vs {:?}",
            shadowed.direct,
            lit.direct
        );
    }

    #[test]
    fn a_sky_ray_reports_no_hit_so_the_compositor_leaves_it_alone() {
        let scene = room();
        let tracer = Tracer::new(scene, Environment::default());
        let mut cam = camera();
        cam.position = v3(0., 100., 0.);
        cam.forward = v3(0., 1., 0.);
        cam.up = v3(0., 0., 1.);
        let out = tracer.trace(&cam, &options(), 4, 4, 1);
        for s in &out {
            assert_eq!(s.hit, 0.);
            assert_eq!(s.radiance, V3::ZERO);
        }
    }

    #[test]
    fn disabling_shadows_removes_the_occlusion_term() {
        let scene = room();
        let tracer = Tracer::new(scene, Environment::default());
        let lit = hits(&tracer.trace(&camera(), &options(), 8, 8, 1));
        let mut no_shadow = options();
        no_shadow.shadows = false;
        let unshadowed = hits(&tracer.trace(&camera(), &no_shadow, 8, 8, 1));
        assert!(
            lit.iter().any(|s| s.occlusion < 0.9),
            "the room shadows itself"
        );
        assert!(unshadowed.iter().all(|s| s.occlusion > 0.99));
        assert!(!lit.is_empty() && !unshadowed.is_empty());
    }

    #[test]
    fn each_term_can_be_switched_off_independently() {
        let scene = room();
        let tracer = Tracer::new(scene, Environment::default());
        let mut o = options();
        o.reflections = false;
        let no_reflect = tracer.trace(&camera(), &o, 8, 8, 1);
        assert!(no_reflect.iter().all(|s| s.reflection == V3::ZERO));
        o = options();
        o.lighting = false;
        let no_light = tracer.trace(&camera(), &o, 8, 8, 1);
        assert!(no_light.iter().all(|s| s.direct == V3::ZERO));
        assert!(no_light.iter().all(|s| s.indirect == V3::ZERO));
        assert!(
            no_light.iter().any(|s| s.reflection.length() > 0.),
            "reflections still work with diffuse lighting off"
        );
        o = options();
        o.lighting = false;
        o.reflections = false;
        let nothing = tracer.trace(&camera(), &o, 8, 8, 1);
        assert!(nothing.iter().all(|s| s.radiance == V3::ZERO));
    }

    #[test]
    fn more_bounces_never_remove_light() {
        let scene = room();
        let tracer = Tracer::new(scene, Environment::default());
        let mut o = options();
        o.bounces = 0;
        let none = tracer.trace(&camera(), &o, 8, 8, 1);
        o.bounces = 4;
        let many = tracer.trace(&camera(), &o, 8, 8, 1);
        for (a, b) in hits(&none).iter().zip(hits(&many).iter()) {
            assert!(b.indirect.length() + 1e-6 >= a.indirect.length());
        }
        assert!(hits(&many).iter().any(|s| s.indirect.length() > 0.001));
    }

    #[test]
    fn the_shadow_term_matches_a_ray_cast_by_hand() {
        // One floor triangle and a blocker directly under the sun above it:
        // the floor point is in shadow only where the blocker sits.
        let sun = v3(0., 1., 0.);
        let tris = vec![
            Triangle {
                a: v3(-10., 0., -10.),
                b: v3(10., 0., -10.),
                c: v3(10., 0., 10.),
                normal: sun,
                material: 0,
            },
            Triangle {
                a: v3(-10., 0., 10.),
                b: v3(10., 0., 10.),
                c: v3(-10., 0., -10.),
                normal: sun,
                material: 0,
            },
            // 2x2 blocker at y = 1 over x in [-1, 1], z in [-1, 1].
            Triangle {
                a: v3(-1., 1., -1.),
                b: v3(1., 1., -1.),
                c: v3(1., 1., 1.),
                normal: sun,
                material: 1,
            },
            Triangle {
                a: v3(-1., 1., -1.),
                b: v3(1., 1., 1.),
                c: v3(-1., 1., 1.),
                normal: sun,
                material: 1,
            },
        ];
        let scene = std::sync::Arc::new(Scene {
            bvh: Some(Bvh::build(tris)),
            materials: vec![
                Material::new(V3::splat(0.5), 0.),
                Material::new(V3::splat(0.5), 0.),
            ],
            stats: Default::default(),
        });
        let tracer = Tracer::new(
            scene,
            Environment {
                sun_direction: sun,
                ..Environment::default()
            },
        );
        let bvh = tracer.scene().bvh.as_ref().unwrap();
        let mut rng = Rng::new(7);
        let mut rays = 0;
        // Under the blocker: shadowed.
        let under = tracer.sun_visibility(bvh, v3(0., 0., 0.), sun, 0., &mut rng, &mut rays);
        assert_eq!(under, 0.);
        // Well outside it: lit.
        let outside = tracer.sun_visibility(bvh, v3(6., 0., 6.), sun, 0., &mut rng, &mut rays);
        assert_eq!(outside, 1.);
    }

    #[test]
    fn packing_follows_the_composite_contract() {
        let samples = vec![
            Sample {
                reflection: v3(1., 2., 3.),
                indirect: v3(0.5, 0., 0.),
                occlusion: 0.25,
                distance: 12.,
                hit: 1.,
                ..Sample::default()
            },
            Sample::default(),
        ];
        let mut out = [0.0f32; 8];
        Tracer::pack(&samples, &mut out);
        // `rgb` is colour, not luminance: a scalar per channel is exactly what
        // made a lit surface flat white and hid every reflection. Indirect is
        // scaled by the visibility it arrives through.
        assert_eq!(out[0], 1. + 0.5 * 0.25, "reflected red plus indirect red");
        assert_eq!(out[1], 2.);
        assert_eq!(out[2], 3.);
        // `a` is sun visibility for a hit, and negative for a miss so the
        // composite leaves the raster sky alone.
        assert_eq!(out[3], 0.25);
        assert_eq!(out[7], -1.);
    }

    #[test]
    fn row_splitting_covers_every_pixel_exactly_once() {
        let scene = room();
        let tracer = Tracer::new(scene, Environment::default());
        // The whole image, and the two halves traced the way the worker splits
        // them: each call gets a buffer the size of its own band.
        let whole = tracer.trace(&camera(), &options(), 8, 8, 1);
        let mut halves = Vec::new();
        for (start, end) in [(0u32, 4u32), (4, 8)] {
            let mut band = vec![Sample::default(); ((end - start) * 8) as usize];
            tracer.trace_rows(&camera(), &options(), 8, 8, 1, start, end, &mut band);
            halves.push(band);
        }
        // Every pixel in the lower rows is floor or wall.
        assert!(whole.iter().all(|s| s.hit > 0.5));
        // Splitting the rows must reproduce the single-threaded image exactly.
        for (index, band) in halves.iter().enumerate() {
            let offset = index * 4 * 8;
            for (pixel, sample) in band.iter().enumerate() {
                assert_eq!(
                    sample.hit,
                    whole[offset + pixel].hit,
                    "row splitting changed pixel {} of band {index}",
                    pixel
                );
                assert_eq!(sample.distance, whole[offset + pixel].distance);
            }
        }
    }

    /// A band-sized buffer is indexed from its own first row.
    ///
    /// The worker hands each thread a slice the size of that thread's rows, so
    /// `trace_rows` must address it with the *band's* stride. Addressing it with
    /// the image's (`y * width + x`) indexes past the end of every band but the
    /// first, which is what produced
    /// "index out of bounds: the len is 73960 but the index is 73960".
    #[test]
    fn a_band_buffer_is_indexed_from_its_own_first_row() {
        let scene = room();
        let tracer = Tracer::new(scene, Environment::default());
        let (width, height) = (16u32, 8u32);
        // The last row on its own: the smallest legal buffer, four rows in from
        // the top of the image.
        let mut band = vec![Sample::default(); width as usize];
        tracer.trace_rows(&camera(), &options(), width, height, 1, 7, 8, &mut band);
        assert!(
            band.iter().all(|s| s.hit > 0.5),
            "the last row of the image was never written into its own band buffer"
        );
        // And it has to be the same image row the full trace produces.
        let whole = tracer.trace(&camera(), &options(), width, height, 1);
        let top_of_last_row = 7 * width as usize;
        for (x, sample) in band.iter().enumerate() {
            assert_eq!(sample.distance, whole[top_of_last_row + x].distance);
        }
    }

    #[test]
    fn the_environment_gets_brighter_towards_the_sun() {
        let env = Environment::default();
        let towards = env.escape(env.sun_direction, f32::INFINITY);
        let away = env.escape(-env.sun_direction, f32::INFINITY);
        assert!(towards.length() > away.length() * 2.);
    }
    /// The banded, multithreaded trace the worker actually runs, on a scene
    /// the size of the real world (2.5 million triangles). This is a
    /// regression test for the scoped-thread panic: it drives the exact
    /// `split_at_mut` + `thread::scope` structure `spawn_software` uses, so a
    /// panic there fails here instead of leaving the page saying "starting".
    #[test]
    fn banded_trace_of_a_world_sized_scene_publishes_a_frame() {
        let count = 2_500_000u32;
        let tris: Vec<Triangle> = (0..count)
            .map(|i| {
                let x = (i % 1580) as f32;
                let z = (i / 1580) as f32;
                Triangle {
                    a: v3(x, 0., z),
                    b: v3(x + 0.5, 0., z),
                    c: v3(x, 0.7, z + 0.5),
                    normal: v3(0., 1., 0.),
                    material: 0,
                }
            })
            .collect();
        let scene = std::sync::Arc::new(Scene {
            bvh: Some(Bvh::build(tris)),
            materials: vec![Material::new(v3(0.6, 0.5, 0.4), 0.1)],
            stats: Default::default(),
        });
        let tracer = Tracer::new(scene, Environment::default());
        let camera = camera();
        let options = options();
        let (width, height) = (590u32, 343u32);
        let mut samples = vec![Sample::default(); (width as usize) * (height as usize)];
        let threads = 16u32;
        let slice = (height + threads - 1) / threads;
        std::thread::scope(|s| {
            let mut rest: &mut [Sample] = &mut samples;
            let mut row = 0u32;
            while row < height {
                let end = (row + slice).min(height);
                let (band, tail) = rest.split_at_mut((end - row) as usize * width as usize);
                rest = tail;
                let tracer_ref = &tracer;
                s.spawn(move || {
                    tracer_ref.trace_rows(&camera, &options, width, height, 1, row, end, band);
                });
                row = end;
            }
        });
        // Every sample must have been written by some band: the sky has
        // `hit == 0`, so a zeroed default is indistinguishable, and the test
        // instead checks that the trace actually ran and covered the image.
        let covered = samples.iter().filter(|s| s.hit > 0.).count();
        eprintln!(
            "banded: {} rays, covered {} of {}",
            tracer.rays.load(std::sync::atomic::Ordering::Relaxed),
            covered,
            samples.len()
        );
        assert!(covered > 0, "the banded trace produced no hits at all");
        assert_eq!(samples.len(), (width as usize) * (height as usize));
    }

    /// Indirect light must scale with the albedo of the surface it bounces off.
    ///
    /// The estimator added the incoming irradiance without multiplying by the
    /// hit surface's albedo, which is the same as assuming every bounce surface
    /// is a perfect white reflector. The error is 1/albedo: terrain is 0.22, so
    /// its interreflected term was 4.5x too bright. On top of a raster frame
    /// that already contained the sky and the sun, that is what made the ground
    /// render as white. Only a numeric check catches this - the code reads fine.
    #[test]
    fn indirect_light_is_weighted_by_the_surface_it_bounces_off() {
        use super::super::bvh::material;
        // The floor the camera looks at keeps a *fixed* albedo; only the surface
        // the bounce lands on changes. That is what isolates the bug: with the
        // missing factor, `indirect` scales with the primary albedo alone and is
        // completely blind to how bright the bounce surface is.
        // A closed box: the camera looks down at a fixed-albedo floor and every
        // cosine-hemisphere bounce lands on the ceiling, whose albedo is the only
        // thing that varies. An open scene would have most bounces escape to the
        // sky and the test would measure nothing, which is exactly what the first
        // attempt at this test did.
        let scene_with_ceiling = |ceiling_albedo: f32| {
            let mut scene = Scene::empty();
            scene.materials[material::TERRAIN as usize] = Material::new(v3(0.5, 0.5, 0.5), 0.);
            scene.materials[material::SCENERY as usize] =
                Material::new(v3(ceiling_albedo, ceiling_albedo, ceiling_albedo), 0.);
            let mut tris = Vec::new();
            let h = 6.;
            let quad = |a: V3, b: V3, c: V3, d: V3, n: V3, m: u16, t: &mut Vec<Triangle>| {
                t.push(Triangle {
                    a,
                    b,
                    c,
                    normal: n,
                    material: m,
                });
                t.push(Triangle {
                    a,
                    b: c,
                    c: d,
                    normal: n,
                    material: m,
                });
            };
            let s = 40.;
            // Floor (fixed) and ceiling (varying).
            quad(
                v3(-s, 0., -s),
                v3(s, 0., -s),
                v3(s, 0., s),
                v3(-s, 0., s),
                v3(0., 1., 0.),
                material::TERRAIN,
                &mut tris,
            );
            quad(
                v3(-s, h, -s),
                v3(-s, h, s),
                v3(s, h, s),
                v3(s, h, -s),
                v3(0., -1., 0.),
                material::SCENERY,
                &mut tris,
            );
            scene.bvh = Some(Bvh::build(tris));
            std::sync::Arc::new(scene)
        };
        let sample = |ceiling_albedo: f32| {
            let tracer = Tracer::new(scene_with_ceiling(ceiling_albedo), Environment::default());
            // Exactly one bounce. With two, the ceiling's albedo still reaches
            // `indirect` through the *second* term even when the first term
            // forgets it, so the averages nearly cancel and the test cannot tell
            // the two behaviours apart. One bounce is the case where the missing
            // factor is the only thing that could make the result depend on the
            // ceiling.
            let mut opts = options();
            opts.bounces = 1;
            let out = tracer.trace(&camera(), &opts, 8, 8, 1);
            let lit: Vec<&Sample> = out.iter().filter(|s| s.hit > 0.5).collect();
            assert!(!lit.is_empty(), "the floor fills the view");
            lit.iter().map(|s| s.indirect.length()).sum::<f32>() / lit.len() as f32
        };
        let dim = sample(0.05);
        let bright = sample(0.5);
        assert!(dim > 0., "dim surfaces still receive indirect light");
        assert!(
            bright > dim * 1.5,
            "indirect must grow with the bounce surface's albedo (dim {dim}, bright {bright})"
        );
    }

    /// The water reflection switch has to change the trace.
    ///
    /// It was a checkbox on the module page: serialised, drawn, and read by no
    /// tracer. Toggling it changed no pixel, which is indistinguishable from
    /// "reflections are broken". This pins that it now does something.
    #[test]
    fn the_water_reflection_switch_changes_the_trace() {
        use super::super::bvh::material;
        let scene = {
            let mut scene = Scene::empty();
            scene.materials[material::WATER as usize] = Material::new(v3(0.02, 0.05, 0.06), 0.85);
            let tris = vec![
                Triangle {
                    a: v3(-50., 0., -50.),
                    b: v3(50., 0., -50.),
                    c: v3(50., 0., 50.),
                    normal: v3(0., 1., 0.),
                    material: material::WATER,
                },
                Triangle {
                    a: v3(-50., 0., -50.),
                    b: v3(50., 0., 50.),
                    c: v3(-50., 0., 50.),
                    normal: v3(0., 1., 0.),
                    material: material::WATER,
                },
            ];
            let mut scene = scene;
            scene.bvh = Some(Bvh::build(tris));
            std::sync::Arc::new(scene)
        };
        let reflection_for = |fill: bool| {
            let tracer = Tracer::new(scene.clone(), Environment::default());
            let mut opts = options();
            opts.water_reflection_fill = fill;
            let out = tracer.trace(&camera(), &opts, 8, 8, 1);
            out.iter().map(|s| s.reflection.length()).sum::<f32>()
        };
        let without = reflection_for(false);
        let with = reflection_for(true);
        assert_eq!(
            without, 0.,
            "water must keep the core's reflection when off"
        );
        assert!(with > 0., "water must get the traced reflection when on");
    }
}
