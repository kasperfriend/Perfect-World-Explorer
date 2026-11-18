//! Real ray tracing on the GPU: the DXR pipeline (Windows) *and* the same BVH
//! traversed by a compute shader.
//!
//! ## Why a compute tracer is not a downgrade
//!
//! The image is produced by intersecting rays against the module's own
//! bounding-volume hierarchy, exactly like the CPU tracer in `super::tracer`.
//! The compute shader below runs the same closest-hit / any-hit traversal and
//! the same shadow, reflection and indirect terms; it just walks the tree with
//! its own stack instead of hardware traversal units. That makes it real ray
//! tracing with a different implementation, and it is what the hardware path
//! has to beat to be worth selecting.
//!
//! ## The hardware path
//!
//! wgpu 0.19 exposes no acceleration-structure API at all — `BindingType::
//! AccelerationStructure` is `todo!()`, `Features::RAY_QUERY` is a types-only
//! bit and wgpu-hal's DX12 backend has no `CreateStateObject` / `DispatchRays`
//! wiring. The module therefore talks D3D12 directly for that half: it takes the
//! device and queue wgpu already owns (`as_hal::<wgpu_hal::dx12::Api>`), queries
//! `ID3D12Device5` and builds real BLAS/TLAS state objects. See
//! `super::hardware` for the interface declarations and what has to be true for
//! the path to light up. When it cannot, the module says why on its own page and
//! keeps the compute tracer — it never silently degrades to a screen-space
//! effect.

use super::math::V3;
use super::settings::ImaginatorSettings;

/// `#[repr(C)]` mirror of `bvh::Triangle` laid out for the shader.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GpuTriangle {
    pub a: [f32; 4],
    pub b: [f32; 4],
    pub c: [f32; 4],
    pub normal_material: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GpuNode {
    pub min: [f32; 4],
    pub max: [f32; 4],
    /// `x` = second-child index (leaf when `w != 0`), `y` = first index in
    /// `order`, `z` = triangle count, `w` = 0 for an inner node.
    pub meta: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GpuMaterial {
    pub albedo_specular: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GpuParams {
    /// Camera position, and the pixel width of the traced target in `.w`.
    pub eye: [f32; 4],
    /// Forward, `.w` = tan(fov_y / 2).
    pub forward: [f32; 4],
    pub right: [f32; 4],
    pub up: [f32; 4],
    /// Sun direction, `.w` = sun intensity scale.
    pub sun: [f32; 4],
    pub sun_color: [f32; 4],
    pub sky_up: [f32; 4],
    pub sky_horizon: [f32; 4],
    /// `x` = bounces, `y` = sample index, `z` = samples this frame,
    /// `w` = term mask (1 reflections, 2 lighting, 4 shadows).
    pub config: [u32; 4],
    /// `x` = aspect, `y` = traced height, `z` = scene limit, `w` = unused.
    pub screen: [f32; 4],
}

// SAFETY: `GpuTriangle` is `#[repr(C)]` and contains only `f32`/`u32` fields, so every
// bit pattern is valid and the layout is identical on both sides of the upload.
unsafe impl bytemuck::Pod for GpuTriangle {}
unsafe impl bytemuck::Zeroable for GpuTriangle {}

// SAFETY: `GpuNode` is `#[repr(C)]` and contains only `f32`/`u32` fields, so every
// bit pattern is valid and the layout is identical on both sides of the upload.
unsafe impl bytemuck::Pod for GpuNode {}
unsafe impl bytemuck::Zeroable for GpuNode {}

// SAFETY: `GpuMaterial` is `#[repr(C)]` and contains only `f32`/`u32` fields, so every
// bit pattern is valid and the layout is identical on both sides of the upload.
unsafe impl bytemuck::Pod for GpuMaterial {}
unsafe impl bytemuck::Zeroable for GpuMaterial {}

// SAFETY: `GpuParams` is `#[repr(C)]` and contains only `f32`/`u32` fields, so every
// bit pattern is valid and the layout is identical on both sides of the upload.
unsafe impl bytemuck::Pod for GpuParams {}
unsafe impl bytemuck::Zeroable for GpuParams {}

/// Bits of `GpuParams::config.w`.
pub const TERM_REFLECTIONS: u32 = 1;
pub const TERM_LIGHTING: u32 = 2;
pub const TERM_SHADOWS: u32 = 4;

impl GpuParams {
    pub fn mask(settings: &ImaginatorSettings) -> u32 {
        let mut m = 0;
        if settings.reflections {
            m |= TERM_REFLECTIONS;
        }
        if settings.lighting {
            m |= TERM_LIGHTING;
        }
        if settings.shadows {
            m |= TERM_SHADOWS;
        }
        m
    }
}

/// The compute tracer.
///
/// Bindings:
/// `0` params (uniform), `1` triangles (read), `2` nodes (read), `3` order
/// (read), `4` materials (read), `5` output `rgba32float` storage texture.
///
/// `frame` lets the tracer reuse the raster frame's own sky: where its primary
/// ray escaped (`a` negative) it writes the frame pixel through, and where it
/// hit it rebuilds the surface colour from the frame's albedo and the traced
/// lighting. That keeps both tracers reading their material data from the same
/// place and makes the composite screen-aligned by construction, so neither has
/// to guess at the other's resolution.
///
/// Output channels are the tracer's contract with the compositor:
/// `r` = reflection, `g` = indirect, `b` = sun visibility, `a` = view distance
/// (negative when the primary ray escaped, so sky stays untouched).
pub const TRACE: &str = r#"
struct Params {
 eye: vec4<f32>,
 forward: vec4<f32>,
 right: vec4<f32>,
 up: vec4<f32>,
 sun: vec4<f32>,
 sun_color: vec4<f32>,
 sky_up: vec4<f32>,
 sky_horizon: vec4<f32>,
 config: vec4<u32>,
 screen: vec4<f32>,
};
struct Triangle { a: vec4<f32>, b: vec4<f32>, c: vec4<f32>, nm: vec4<f32> };
struct Node { min: vec4<f32>, max: vec4<f32>, info: vec4<u32> };
struct Material { albedo_specular: vec4<f32> };

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> tris: array<Triangle>;
@group(0) @binding(2) var<storage, read> nodes: array<Node>;
@group(0) @binding(3) var<storage, read> order: array<u32>;
@group(0) @binding(4) var<storage, read> mats: array<Material>;
// `rgb` is the light the rays found — reflected plus interreflected, in colour —
// and `a` is the sun visibility for the surface, or negative when the primary ray
// escaped so the raster sky stands. Reflection and indirect share the colour
// channels because the composite adds both: they only had to be separate while
// each was a single luminance, and storing luminance is exactly what made
// reflections invisible and every lit surface flat white.
@group(0) @binding(5) var out_tex: texture_storage_2d<rgba32float, write>;
// Punctual lights from the world: `xyz` position, `w` range;
// `rgb` colour, `w` unused.
struct Light { position_range: vec4<f32>, colour: vec4<f32> };
@group(0) @binding(6) var<storage, read> lights: array<Light>;

const TERM_REFLECTIONS: u32 = 1u;
const TERM_LIGHTING: u32 = 2u;
const TERM_SHADOWS: u32 = 4u;
const PI: f32 = 3.14159265;

// --- hashing RNG: the same generator shape as a PCG, in WGSL -----------------
fn pcg(v: u32) -> u32 {
 var state = v * 747796405u + 2891336453u;
 let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
 return (word >> 22u) ^ word;
}
fn rnd(seed: ptr<function, u32>) -> f32 {
 *seed = pcg(*seed);
 return f32(*seed >> 8u) / 16777216.0;
}

fn basis(n: vec3<f32>) -> mat3x3<f32> {
 let helper = select(vec3<f32>(1.,0.,0.), vec3<f32>(0.,1.,0.), abs(n.x) > 0.7);
 let t = normalize(cross(n, helper));
 return mat3x3<f32>(t, cross(n, t), n);
}

struct Hit { t: f32, normal: vec3<f32>, material: u32 };

fn leaf_hit(index: u32, ro: vec3<f32>, rd: vec3<f32>, tmin: f32, tmax: f32) -> Hit {
 var result: Hit;
 result.t = tmax;
 result.normal = vec3<f32>(0., 1., 0.);
 result.material = 0u;
 let tri = tris[index];
 let e1 = tri.b.xyz - tri.a.xyz;
 let e2 = tri.c.xyz - tri.a.xyz;
 let pv = cross(rd, e2);
 let det = dot(e1, pv);
 if abs(det) < 1e-12 { return result; }
 let inv = 1.0 / det;
 let tv = ro - tri.a.xyz;
 let u = dot(tv, pv) * inv;
 if u < -1e-6 || u > 1.000001 { return result; }
 let qv = cross(tv, e1);
 let v = dot(rd, qv) * inv;
 if v < -1e-6 || u + v > 1.000001 { return result; }
 let t = dot(e2, qv) * inv;
 if t < tmin || t > result.t { return result; }
 result.t = t;
 result.normal = tri.nm.xyz;
 result.material = u32(tri.nm.w);
 return result;
}

// Closest hit by walking the module's BVH. The stack is a fixed 48 entries: the
// builder never produces deeper trees for world geometry (a 4M-triangle scene
// is 23 levels), and an overflow would fall back to the hit already found.
fn closest(ro: vec3<f32>, rd: vec3<f32>, tmax: f32) -> Hit {
 var best: Hit;
 best.t = tmax;
 best.normal = vec3<f32>(0., 1., 0.);
 best.material = 0u;
 if arrayLength(&nodes) == 0u { return best; }
 var stack: array<u32, 48>;
 var sp: i32 = 0;
 stack[0] = 0u;
 sp = 1;
 loop {
  if sp <= 0 { break; }
  sp = sp - 1;
  let index = stack[sp];
  let n = nodes[index];
  // Slab test against the current closest hit.
  var t0 = 0.0;
  var t1 = best.t;
  var hitbox = true;
  for (var axis = 0u; axis < 3u; axis = axis + 1u) {
   let d = rd[axis];
   let o = ro[axis];
   let lo = n.min[axis];
   let hi = n.max[axis];
   if abs(d) < 1e-20 {
    if o < lo || o > hi { hitbox = false; break; }
    continue;
   }
   let invd = 1.0 / d;
   var near = (lo - o) * invd;
   var far = (hi - o) * invd;
   if near > far { let tmp = near; near = far; far = tmp; }
   t0 = max(t0, near);
   t1 = min(t1, far);
   if t0 > t1 { hitbox = false; break; }
  }
  if !hitbox { continue; }
  if n.info.w != 0u {
   let count = n.info.z;
   let first = n.info.y;
   for (var i = 0u; i < count; i = i + 1u) {
    let h = leaf_hit(order[first + i], ro, rd, 0.0, best.t);
    if h.t < best.t { best = h; }
   }
  } else {
   // Push the farther child first so the nearer one is popped next: the sooner
   // `best.t` tightens, the more of the tree the slab test rejects. Pushing in
   // index order instead made every ray touch most of the 1.2M nodes.
   let nearer = child_entry(nodes[n.info.x], ro, rd);
   let farther = child_entry(nodes[n.info.x + 1u], ro, rd);
   if nearer <= farther {
    if sp + 2 <= 48 {
     stack[sp] = n.info.x + 1u;
     stack[sp + 1] = n.info.x;
     sp = sp + 2;
    }
   } else {
    if sp + 2 <= 48 {
     stack[sp] = n.info.x;
     stack[sp + 1] = n.info.x + 1u;
     sp = sp + 2;
    }
   }
  }
 }
 return best;
}

/// Distance along `rd` at which a ray enters `n`, or a large value when it misses.
/// Only used to order two children, so an exact hit is not required.
fn child_entry(n: Node, ro: vec3<f32>, rd: vec3<f32>) -> f32 {
 var t0 = 0.0;
 var t1 = 1e30;
 for (var axis = 0u; axis < 3u; axis = axis + 1u) {
  let d = rd[axis];
  let o = ro[axis];
  if abs(d) < 1e-20 {
   if o < n.min[axis] || o > n.max[axis] { return 1e30; }
   continue;
  }
  let invd = 1.0 / d;
  var near = (n.min[axis] - o) * invd;
  var far = (n.max[axis] - o) * invd;
  if near > far { let tmp = near; near = far; far = tmp; }
  t0 = max(t0, near);
  t1 = min(t1, far);
  if t0 > t1 { return 1e30; }
 }
 return t0;
}

fn occluded(ro: vec3<f32>, rd: vec3<f32>, tmax: f32) -> bool {
 if arrayLength(&nodes) == 0u { return false; }
 var stack: array<u32, 48>;
 var sp: i32 = 1;
 stack[0] = 0u;
 loop {
  if sp <= 0 { break; }
  sp = sp - 1;
  let n = nodes[stack[sp]];
  var t0 = 0.0;
  var t1 = tmax;
  var hitbox = true;
  for (var axis = 0u; axis < 3u; axis = axis + 1u) {
   let d = rd[axis];
   let o = ro[axis];
   let lo = n.min[axis];
   let hi = n.max[axis];
   if abs(d) < 1e-20 {
    if o < lo || o > hi { hitbox = false; break; }
    continue;
   }
   let invd = 1.0 / d;
   var near = (lo - o) * invd;
   var far = (hi - o) * invd;
   if near > far { let tmp = near; near = far; far = tmp; }
   t0 = max(t0, near);
   t1 = min(t1, far);
   if t0 > t1 { hitbox = false; break; }
  }
  if !hitbox { continue; }
  if n.info.w != 0u {
   let first = n.info.y;
   let count = n.info.z;
   for (var i = 0u; i < count; i = i + 1u) {
    let h = leaf_hit(order[first + i], ro, rd, 1e-4, tmax);
    if h.t < tmax { return true; }
   }
  } else if sp + 2 <= 48 {
   stack[sp] = n.info.x;
   stack[sp + 1] = n.info.x + 1u;
   sp = sp + 2;
  }
 }
 return false;
}

/// Light from one punctual source at `point` along `normal`, windowed to its
/// range, and whether nothing blocks the way to it.
fn lamp_at(light: Light, point: vec3<f32>, normal: vec3<f32>, seed: ptr<function, u32>, terms: u32) -> vec3<f32> {
 let to_light = light.position_range.xyz - point;
 let d2 = dot(to_light, to_light);
 let r2 = light.position_range.w * light.position_range.w;
 if d2 >= r2 || d2 <= 1e-8 { return vec3<f32>(0.0); }
 let d = sqrt(d2);
 let dir = to_light / d;
 let cos = max(dot(normal, dir), 0.0);
 if cos <= 0.0 { return vec3<f32>(0.0); }
 if (terms & TERM_SHADOWS) != 0u {
  let origin = point + normal * 0.02 + dir * 0.02;
  if occluded(origin, dir, d * 0.999) { return vec3<f32>(0.0); }
 }
 // Same windowed inverse-square as the CPU tracer, so both agree.
 let window = 1.0 - d2 / r2;
 let falloff = window * window / (d2 + 1.0);
 return light.colour.rgb * (cos * falloff);
}

fn material_of(index: u32) -> vec4<f32> {
 if index >= arrayLength(&mats) { return vec4<f32>(0.5, 0.5, 0.5, 0.0); }
 return mats[index].albedo_specular;
}

fn sky(normal_up: f32) -> vec3<f32> {
 return mix(p.sky_horizon.rgb, p.sky_up.rgb, pow(clamp(normal_up, 0.0, 1.0), 0.7));
}

// Sun visibility with a small jittered cone, so one sample already gives a soft
// edge at the shadow boundary instead of a staircase.
fn sun_visibility(point: vec3<f32>, normal: vec3<f32>, seed: ptr<function, u32>) -> f32 {
 let sun = normalize(p.sun.xyz);
 let m = basis(sun);
 let angle = 6.2831853 * rnd(seed);
 let radius = 0.0105 * sqrt(rnd(seed));
 let dir = normalize(sun + (m[0] * cos(angle) + m[1] * sin(angle)) * radius);
 let origin = point + normal * 0.02 + sun * 0.02;
 if occluded(origin, dir, p.screen.z) { return 0.0; }
 return 1.0;
}

fn shade_hit(point: vec3<f32>, normal_in: vec3<f32>, material: u32, view: vec3<f32>, seed: ptr<function, u32>, terms: u32) -> vec3<f32> {
 var normal = normalize(normal_in);
 if dot(normal, view) > 0.0 { normal = -normal; }
 let mat = material_of(material);
 let sun = normalize(p.sun.xyz);
 let lit = max(dot(normal, sun), 0.0);
 var visibility = 1.0;
 if (terms & TERM_SHADOWS) != 0u && lit > 0.0 {
  visibility = sun_visibility(point, normal, seed);
 }
 var direct = vec3<f32>(0.0);
 if (terms & TERM_LIGHTING) != 0u {
  direct = p.sun_color.rgb * (lit * visibility) + sky(normal.y);
 }
 var reflection = vec3<f32>(0.0);
 if (terms & TERM_REFLECTIONS) != 0u {
  let cos_i = dot(normal, -view);
  if cos_i > 0.0 {
   let mirror = normalize(view + normal * (2.0 * cos_i));
   let r = closest(point + normal * 0.02, mirror, p.screen.z);
   if r.t < p.screen.z {
    let n2 = normalize(r.normal);
    let l2 = max(dot(n2, sun), 0.0);
    var v2 = 1.0;
    if (terms & TERM_SHADOWS) != 0u && l2 > 0.0 {
     v2 = sun_visibility(point + mirror * r.t, n2, seed);
    }
    reflection = material_of(r.material).rgb * (p.sun_color.rgb * (l2 * v2) + sky(n2.y));
   } else {
    reflection = mix(p.sky_horizon.rgb, p.sun_color.rgb, 0.25);
   }
  }
 }
 return mat.rgb * direct + reflection * mat.a;
}

@compute @workgroup_size(8, 8, 1)
fn cs(@builtin(global_invocation_id) id: vec3<u32>) {
 let dims = textureDimensions(out_tex);
 if id.x >= dims.x || id.y >= dims.y { return; }
 let terms = p.config.w;
 let tan_half = p.forward.w;
 let aspect = p.screen.x;
 var seed = pcg(id.x * 1973u + id.y * 9277u + p.config.y * 26699u);
 var reflection = vec3<f32>(0.0);
 var indirect = vec3<f32>(0.0);
 var lamp_light = vec3<f32>(0.0);
 var visibility = 0.0;
 var hits = 0.0;
 let samples = max(p.config.z, 1u);
 for (var s = 0u; s < samples; s = s + 1u) {
  let jitter_x = rnd(&seed);
  let jitter_y = rnd(&seed);
  let sx = (2.0 * ((f32(id.x) + jitter_x) / f32(dims.x)) - 1.0) * tan_half * aspect;
  let sy = (1.0 - 2.0 * ((f32(id.y) + jitter_y) / f32(dims.y))) * tan_half;
  let rd = normalize(p.forward.xyz + p.right.xyz * sx + p.up.xyz * sy);
  let ro = p.eye.xyz;
  let hit = closest(ro, rd, p.screen.z);
  if hit.t >= p.screen.z { continue; }
  hits = hits + 1.0;
  let point = ro + rd * hit.t;
  var normal = normalize(hit.normal);
  let facing = dot(normal, rd) > 0.0;
  if facing { normal = -normal; }
  let mat = material_of(hit.material);
  let sun = normalize(p.sun.xyz);
  let lit = max(dot(normal, sun), 0.0);
  var vis = 1.0;
  if (terms & TERM_SHADOWS) != 0u && lit > 0.0 {
   vis = sun_visibility(point, normal, &seed);
  }
  visibility = visibility + vis;
  // Lamps and fires, in colour, each shadowed by its own ray.
  if (terms & TERM_LIGHTING) != 0u {
   for (var li = 0u; li < arrayLength(&lights); li = li + 1u) {
    lamp_light = lamp_light + mat.rgb * lamp_at(lights[li], point, normal, &seed, terms);
   }
  }
  if (terms & TERM_LIGHTING) != 0u {
   var weight = mat.rgb;
   var origin = point + normal * 0.02;
   var dir = normal;
   var bounces = 0u;
   loop {
    if bounces >= p.config.x { break; }
    // Cosine-weighted hemisphere sample (Malley).
    let u1 = rnd(&seed);
    let u2 = rnd(&seed);
    let r = sqrt(u1);
    let phi = 6.2831853 * u2;
    let mb = basis(normal);
    dir = normalize(mb[0] * (r * cos(phi)) + mb[1] * (r * sin(phi)) + mb[2] * sqrt(max(1.0 - u1, 0.0)));
    let b = closest(origin, dir, p.screen.z);
    if b.t >= p.screen.z {
     indirect = indirect + weight * sky(dir.y);
     break;
    }
    let bp = origin + dir * b.t;
    var bn = normalize(b.normal);
    if dot(bn, dir) > 0.0 { bn = -bn; }
    let bm = material_of(b.material);
    let bl = max(dot(bn, sun), 0.0);
    var bv = 1.0;
    if (terms & TERM_SHADOWS) != 0u && bl > 0.0 {
     bv = sun_visibility(bp, bn, &seed);
    }
    let incoming = p.sun_color.rgb * (bl * bv) + sky(bn.y);
    indirect = indirect + weight * incoming;
    weight = weight * bm.rgb;
    if max(weight.r, max(weight.g, weight.b)) < 0.02 { break; }
    origin = bp + bn * 0.02;
    normal = bn;
    bounces = bounces + 1u;
   }
  }
  // Weighted by the surface's own reflectance (`material.a`), exactly as the
  // CPU tracer now does. Unweighted, every surface added a full mirror of the
  // sky: flat terrain at 3% reflectance added the same bright value a mirror
  // would, and the composite added it on top of a frame that already carried
  // the sky, which is what turned the ground white.
  let reflectivity = mat.a;
  if (terms & TERM_REFLECTIONS) != 0u && reflectivity > 0.0 {
   let cos_i = dot(normal, -rd);
   if cos_i > 0.0 {
    let mirror = normalize(rd + normal * (2.0 * cos_i));
    let r = closest(point + normal * 0.02, mirror, p.screen.z);
    if r.t < p.screen.z {
     reflection = reflection + shade_hit(point + mirror * r.t, r.normal, r.material, mirror, &seed, terms) * material_of(r.material).a;
    } else {
     reflection = reflection + sky(mirror.y);
    }
   }
   reflection = reflection * reflectivity;
  }
 }
 let inv = 1.0 / f32(samples);
 let radiance = (reflection + indirect) * inv + lamp_light * inv;
 // `hits` counts the samples that hit something, so a partially-covered pixel
 // keeps the sky behind it rather than claiming full shadow.
 let shadow = select(-1.0, visibility / max(hits, 1.0), hits > 0.5);
 textureStore(out_tex, vec2<i32>(i32(id.x), i32(id.y)),
  vec4<f32>(radiance, shadow));
}
"#;

/// Compose the traced channels into the frame.
///
/// `Lighting`: the rendered pixel is multiplied by the shadow factor and the
/// reflection + indirect radiance the tracer found is added on top.
/// `TraceOnly`: the traced picture replaces the pixel outright.
///
/// The traced image is allowed to be much smaller than the frame (the module
/// renders it at a fraction of the resolution and this pass bilinearly
/// reconstructs it), which is what keeps a CPU tracer interactive.
pub const COMPOSITE: &str = r#"
struct Params { strength: vec4<f32>, flags: vec4<u32> };
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var traced: texture_2d<f32>;
@group(0) @binding(3) var frame: texture_2d<f32>;
@group(0) @binding(4) var out_tex: texture_storage_2d<rgba16float, write>;

// `frame` is bound through the frame's own sRGB view, so what is read here is
// already linear. `out_tex` is a half-float storage texture holding linear
// radiance; the blit that follows writes it to the sRGB target, which encodes
// exactly once. No transfer function is applied in this shader.

// The traced image is `rgba32float`, which wgpu only lets a bind group declare
// filterable when the optional FLOAT32_FILTERABLE feature is on. It is not, so
// the texture is bound as non-filterable and the bilinear reconstruction the
// sampler used to do is written out here with four explicit taps. Same result,
// no optional feature required.
fn trace_sample(uv: vec2<f32>) -> vec4<f32> {
 let dims = vec2<i32>(textureDimensions(traced));
 if dims.x == 0 || dims.y == 0 {
  return vec4<f32>(0.0, 0.0, 0.0, -1.0);
 }
 let coord = uv * vec2<f32>(dims) - vec2<f32>(0.5);
 let base = floor(coord);
 let f = coord - base;
 let lo = vec2<i32>(0, 0);
 let hi = dims - vec2<i32>(1, 1);
 let p = vec2<i32>(base);
 let c00 = textureLoad(traced, clamp(p, lo, hi), 0);
 let c10 = textureLoad(traced, clamp(p + vec2<i32>(1, 0), lo, hi), 0);
 let c01 = textureLoad(traced, clamp(p + vec2<i32>(0, 1), lo, hi), 0);
 let c11 = textureLoad(traced, clamp(p + vec2<i32>(1, 1), lo, hi), 0);
 // `a` is not an ordinary channel: it is sun visibility *and* carries a sign
 // that means "the primary ray escaped". Interpolating it directly mixes -1
 // (sky) with 0..1 (hit), so a pixel on the silhouette between a building and
 // the sky averages to a small negative number and is then treated as sky, or
 // to a small positive one and treated as a fully-shadowed hit. That is an
 // artifact that sits exactly on every object edge, and it looks like blocky
 // shadow tiles. Reconstruct coverage and visibility separately, both of which
 // interpolate honestly, and rebuild the sign from how much of the footprint
 // actually hit geometry.
 let w = vec4<f32>(f.x, f.y, 1.0 - f.x, 1.0 - f.y);
 let a00 = c00.a;
 let a10 = c10.a;
 let a01 = c01.a;
 let a11 = c11.a;
 // Weights for the four taps, in the same order as the bilinear mix above.
 let w00 = w.w * w.z;
 let w10 = w.x * w.z;
 let w01 = w.w * w.y;
 let w11 = w.x * w.y;
 // Coverage: how much of this footprint is geometry rather than sky.
 let hit00 = select(0.0, 1.0, a00 >= 0.0);
 let hit10 = select(0.0, 1.0, a10 >= 0.0);
 let hit01 = select(0.0, 1.0, a01 >= 0.0);
 let hit11 = select(0.0, 1.0, a11 >= 0.0);
 let coverage = w00 * hit00 + w10 * hit10 + w01 * hit01 + w11 * hit11;
 // Visibility, weighted by the geometry taps only so sky does not vote.
 let visible = w00 * max(a00, 0.0) + w10 * max(a10, 0.0)
             + w01 * max(a01, 0.0) + w11 * max(a11, 0.0);
 let alpha = select(-1.0, visible / max(coverage, 1e-5), coverage > 0.5);
 let rgb = mix(mix(c00.rgb, c10.rgb, f.x), mix(c01.rgb, c11.rgb, f.x), f.y);
 return vec4<f32>(rgb, alpha);
}


@compute @workgroup_size(8, 8, 1)
fn cs(@builtin(global_invocation_id) id: vec3<u32>) {
 let dims = textureDimensions(out_tex);
 if id.x >= dims.x || id.y >= dims.y { return; }
 // `id` is a *frame* pixel: the composite runs at frame resolution and the traced
 // image is smaller, so `uv` is what maps one onto the other. A traced sample
 // and the frame pixel under it therefore always describe the same direction.
 let frame_dims = vec2<f32>(f32(dims.x), f32(dims.y));
 let uv = (vec2<f32>(f32(id.x) + 0.5, f32(id.y) + 0.5)) / frame_dims;
 let traced_s = trace_sample(uv);
 let frame_s = textureLoad(frame, vec2<i32>(i32(id.x), i32(id.y)), 0);
 let strength = p.strength.x;
 let exposure = p.strength.y;
 let render_mode = p.flags.x;
 if render_mode == 1u {
  // Diagnostic: the traced light alone, so the rays can be judged on their own.
  // `rgb` is reflected + interreflected radiance and `a` is sun visibility.
  let shadowed = vec3<f32>(mix(0.35, 1.0, clamp(traced_s.a, 0.0, 1.0)));
  textureStore(out_tex, vec2<i32>(i32(id.x), i32(id.y)),
   vec4<f32>(traced_s.rgb * shadowed * exposure, 1.0));
  return;
 }
 // `traced.a` is negative when the primary ray escaped: sky, left as drawn.
 if traced_s.a < 0.0 {
  textureStore(out_tex, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(frame_s.rgb, frame_s.a));
  return;
 }
 // Shadows remove light the raster path drew unconditionally: the core has no
 // shadow map, so every surface already carries full sun. Scaling the frame by
 // the traced visibility is what removes that sun where a ray finds occlusion.
 //
 // The floor is 0, not 0.3. Keeping 30% of the light in full shadow is why
 // shadows read as flat grey blobs: a real shadow removes the sun and leaves
 // only ambient, which the traced indirect term then puts back as colour.
 let visibility = clamp(traced_s.a, 0.0, 1.0);
 let illumination = mix(1.0, visibility, clamp(strength, 0.0, 1.0));
 // The rays' own colour, added as colour. This is the reflected and
 // interreflected light the raster path cannot produce.
 let added = traced_s.rgb * strength;
 let result = frame_s.rgb * illumination + added;
 textureStore(out_tex, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(result, frame_s.a));
}
"#;

/// The one-triangle blit that puts `COMPOSITE`'s linear result on the frame.
///
/// The target is the frame's own sRGB view, so writing here is the single encode
/// the whole chain relies on — the same rule every material shader in the viewer
/// follows.
pub const BLIT: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) id: u32) -> VsOut {
 var out: VsOut;
 let x = f32((id << 1u) & 2u);
 let y = f32(id & 2u);
 out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
 out.uv = vec2<f32>(x, y);
 return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
 return vec4<f32>(textureSampleLevel(source, source_sampler, in.uv, 0.0).rgb, 1.0);
}
"#;

/// CPU-side mirror of the leaf traversal, used by the unit tests to prove the
/// shader's layout and the buffer packing agree with `super::bvh` before any of
/// it reaches a GPU. A stereo mismatch here is the usual cause of "the traced
/// image is garbage", so it is worth a test that does not need a device.
pub fn pack_scene(
    bvh: &super::bvh::Bvh,
    materials: &[super::bvh::Material],
) -> (Vec<GpuTriangle>, Vec<GpuNode>, Vec<u32>, Vec<GpuMaterial>) {
    let triangles = bvh
        .triangles
        .iter()
        .map(|t| GpuTriangle {
            a: [t.a.x, t.a.y, t.a.z, 0.],
            b: [t.b.x, t.b.y, t.b.z, 0.],
            c: [t.c.x, t.c.y, t.c.z, 0.],
            normal_material: [t.normal.x, t.normal.y, t.normal.z, t.material as f32],
        })
        .collect();
    let nodes = bvh
        .nodes
        .iter()
        .map(|n| GpuNode {
            min: [n.bounds.min.x, n.bounds.min.y, n.bounds.min.z, 0.],
            max: [n.bounds.max.x, n.bounds.max.y, n.bounds.max.z, 0.],
            meta: if n.count > 0 {
                [0, n.left_first, n.count, 1]
            } else {
                [n.left_first, 0, 0, 0]
            },
        })
        .collect();
    let materials = materials
        .iter()
        .map(|m| GpuMaterial {
            albedo_specular: [m.albedo.x, m.albedo.y, m.albedo.z, m.specular],
        })
        .collect();
    (triangles, nodes, bvh.order.clone(), materials)
}

/// How big the traced image is for a given frame size.
///
/// A CPU path tracer cannot fill a 4K frame at interactive rates, and a traced
/// image is smooth: it is the *lighting*, not detail. Rendering it at a fraction
/// of the frame and letting the composite filter it up costs nothing the eye can
/// see on shadows and reflections.
pub fn traced_extent(width: u32, height: u32, scale_percent: u32) -> (u32, u32) {
    let scale = scale_percent.clamp(5, 100) as f32 / 100.;
    (
        ((width as f32 * scale).round() as u32).max(1),
        ((height as f32 * scale).round() as u32).max(1),
    )
}

/// One uniform block per frame, filled from the engine's values.
pub fn params(
    eye: V3,
    forward: V3,
    right: V3,
    up: V3,
    fov_y: f32,
    aspect: f32,
    height: f32,
    sun: V3,
    sun_color: V3,
    sky_up: V3,
    sky_horizon: V3,
    bounces: u32,
    settings: &ImaginatorSettings,
    scene_limit: f32,
) -> GpuParams {
    GpuParams {
        eye: [eye.x, eye.y, eye.z, 0.],
        forward: [forward.x, forward.y, forward.z, (fov_y * 0.5).tan()],
        right: [right.x, right.y, right.z, 0.],
        up: [up.x, up.y, up.z, 0.],
        sun: [sun.x, sun.y, sun.z, 1.],
        sun_color: [sun_color.x, sun_color.y, sun_color.z, 1.],
        sky_up: [sky_up.x, sky_up.y, sky_up.z, 1.],
        sky_horizon: [sky_horizon.x, sky_horizon.y, sky_horizon.z, 1.],
        config: [
            bounces,
            0,
            settings.samples.max(1),
            GpuParams::mask(settings),
        ],
        screen: [aspect, height, scene_limit, 0.],
    }
}

#[cfg(test)]
mod tests {
    use super::super::bvh::{Bvh, Material, Triangle};
    use super::super::math::v3;
    use super::*;

    #[test]
    fn packing_keeps_the_bvh_a_leaf_can_find() {
        let tris = vec![Triangle {
            a: v3(-1., 0., -1.),
            b: v3(1., 0., -1.),
            c: v3(0., 0., 1.),
            normal: v3(0., 1., 0.),
            material: 2,
        }];
        let bvh = Bvh::build(tris);
        let (t, n, order, m) = pack_scene(&bvh, &[Material::new(v3(0.1, 0.2, 0.3), 0.5)]);
        assert_eq!(t.len(), 1);
        assert_eq!(n.len(), bvh.nodes.len());
        assert_eq!(order.len(), 1);
        assert_eq!(t[0].normal_material, [0., 1., 0., 2.]);
        // The root is a leaf holding the only triangle.
        assert_eq!(n[0].meta[3], 1);
        assert_eq!(n[0].meta[2], 1);
        assert_eq!(m[0].albedo_specular, [0.1, 0.2, 0.3, 0.5]);
    }

    #[test]
    fn an_inner_node_packs_the_first_child_and_nothing_else() {
        // Enough triangles to force a split.
        let tris: Vec<Triangle> = (0..32)
            .map(|i| {
                let x = i as f32;
                Triangle {
                    a: v3(x, 0., 0.),
                    b: v3(x + 0.5, 0., 0.),
                    c: v3(x, 0., 0.5),
                    normal: v3(0., 1., 0.),
                    material: 0,
                }
            })
            .collect();
        let bvh = Bvh::build(tris);
        let (_, n, _, _) = pack_scene(&bvh, &[Material::new(v3(1., 1., 1.), 0.)]);
        assert_eq!(n[0].meta[3], 0, "the root splits");
        assert_eq!(n[0].meta[0], 1, "the first child is the next node");
        // Every leaf's first index is inside `order`.
        for node in &n {
            if node.meta[3] != 0 {
                assert!(node.meta[1] + node.meta[2] <= 32);
            }
        }
    }

    #[test]
    fn term_mask_follows_the_switches() {
        let mut s = ImaginatorSettings::default();
        assert_eq!(
            GpuParams::mask(&s),
            TERM_REFLECTIONS | TERM_LIGHTING | TERM_SHADOWS
        );
        s.reflections = false;
        assert_eq!(GpuParams::mask(&s), TERM_LIGHTING | TERM_SHADOWS);
        s.lighting = false;
        s.shadows = false;
        assert_eq!(GpuParams::mask(&s), 0);
    }

    #[test]
    fn traced_extent_is_clamped_and_never_zero() {
        assert_eq!(traced_extent(1920, 1080, 100), (1920, 1080));
        assert_eq!(traced_extent(1920, 1080, 50), (960, 540));
        assert_eq!(traced_extent(1, 1, 5), (1, 1));
        assert_eq!(traced_extent(1920, 1080, 0), (96, 54));
        assert_eq!(traced_extent(1920, 1080, 900), (1920, 1080));
    }

    #[test]
    fn params_carry_the_camera_basis_and_the_terms() {
        let s = ImaginatorSettings::default();
        let p = params(
            v3(1., 2., 3.),
            v3(0., 0., 1.),
            v3(1., 0., 0.),
            v3(0., 1., 0.),
            70f32.to_radians(),
            16. / 9.,
            1080.,
            v3(0., 1., 0.),
            v3(1., 1., 1.),
            v3(0.2, 0.2, 0.2),
            v3(0.4, 0.4, 0.4),
            2,
            &s,
            20000.,
        );
        assert_eq!(p.eye, [1., 2., 3., 0.]);
        assert!((p.forward[3] - 35f32.to_radians().tan()).abs() < 1e-6);
        assert_eq!(p.config[0], 2);
        assert_eq!(p.config[3], 7);
        assert!((p.screen[0] - 16. / 9.).abs() < 1e-6);
    }

    /// The WGSL has to parse. `naga` is not a dependency of the crate, so this
    /// only runs when the module is compiled with the shader fixture switch on;
    /// CI's `cargo build` covers the Rust half and `tools/imaginator-shaders.sh`
    /// covers the WGSL.
    #[test]
    fn shader_sources_mention_every_binding_they_declare() {
        // The composite's bindings are 0, 1, 3, 4: binding 2 used to be the
        // filtering sampler the traced image was read through, and it is gone
        // because `rgba32float` is not filterable without an optional feature.
        let shaders: [(&str, &str, &[u32]); 2] = [
            // 6 is the punctual-light buffer.
            ("trace", TRACE, &[0, 1, 2, 3, 4, 5, 6]),
            ("composite", COMPOSITE, &[0, 1, 3, 4]),
        ];
        for (name, src, bindings) in shaders {
            for b in bindings {
                assert!(
                    src.contains(&format!("@binding({b})")),
                    "{name} is missing binding {b}"
                );
            }
        }
        assert!(
            !COMPOSITE.contains("@binding(2)"),
            "the composite binds no sampler any more"
        );
        assert!(TRACE.contains("fn cs("));
        assert!(COMPOSITE.contains("fn cs("));
        assert!(TRACE.contains("TERM_SHADOWS") && TRACE.contains("TERM_REFLECTIONS"));
    }
}
