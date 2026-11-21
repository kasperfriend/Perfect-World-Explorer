//! The GPU half of Raywatcher: WGSL for the ray pass and the denoiser, plus the
//! pipelines that run them.
//!
//! The shaders here are transcribed from the CPU reference implementations in
//! [`super::bvh`] and [`super::shade`] — same traversal, same weight functions,
//! same arithmetic order where the order matters — so the two paths cannot
//! disagree about what the world looks like. The tests in this crate run the
//! CPU reference; CI's lavapipe job runs these shaders over a real adapter and
//! compares the two on a synthetic scene (see `tests/raywatcher_gpu.rs`).
//!
//! # Bindings
//!
//! Group 0 is the scene (immutable for a frame, uploaded when the resident set
//! changes): the triangle array, the material array, the BVH nodes, the
//! triangle order, and the per-frame uniform.
//!
//! Group 1 is the previous frame's history, for the temporal pass.
//!
//! Group 2 is the destination: the output colour image, the guide buffer the
//! denoiser consumes and the denoiser's own working planes.

use std::sync::Arc;

use wgpu::util::DeviceExt;

use super::bvh::{BvhNode, RayMaterial, RayTriangle};
use super::denoise::DenoiseConfig;
use super::frame::FrameParams;

/// One-to-one with the `Params` struct in [`RAY_SHADER`]. `#[repr(C)]` and
/// explicit padding keep the Rust and WGSL layouts identical; the size
/// assertion at the bottom of this file is the check.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RayParams {
    pub inv_view_projection: [[f32; 4]; 4],
    pub view_projection: [[f32; 4]; 4],
    pub eye: [f32; 4],
    pub sun_direction: [f32; 4],
    pub sun_colour: [f32; 4],
    pub ambient: [f32; 4],
    pub fog_colour: [f32; 4],
    /// `x` near, `y` far, `z` sun angular radius, `w` sun intensity.
    pub lens: [f32; 4],
    /// `x` grid width, `y` grid height, `z` render width, `w` render height.
    pub extents: [f32; 4],
    /// `x` frame index, `y` samples, `z` bounces, `w` history valid.
    pub frame: [f32; 4],
    /// `x` jitter x, `y` jitter y, `z` ambient strength, `w` bounce strength.
    pub tuning: [f32; 4],
    /// `x` roughness cutoff, `y` effect bits, `z` bias, `w` unused.
    pub effects: [f32; 4],
    /// `x` fog start, `y` fog end, `z` AO radius, `w` unused.
    pub fog: [f32; 4],
    /// `x` triangle count, `y` node count, `z` material count, `w` unused.
    pub counts: [f32; 4],
}

impl RayParams {
    /// Effect bits, mirroring [`super::plan::Effects`]:
    /// 1 shadows, 2 lighting, 4 reflections, 8 AO, 16 GI, 32 refraction.
    pub fn effect_bits(effects: super::plan::Effects) -> u32 {
        let mut bits = 0;
        if effects.shadows {
            bits |= 1;
        }
        if effects.lighting {
            bits |= 2;
        }
        if effects.reflections {
            bits |= 4;
        }
        if effects.ambient_occlusion {
            bits |= 8;
        }
        if effects.global_illumination {
            bits |= 16;
        }
        if effects.refraction {
            bits |= 32;
        }
        bits
    }

    /// Pack a frame's parameters for the shader. `counts` comes from the
    /// uploaded scene so the shader never walks past the arrays.
    pub fn pack(
        params: &FrameParams,
        plan: &super::plan::Plan,
        config: &super::shade::TransportConfig,
        counts: [u32; 3],
    ) -> Self {
        let fog = config.lighting.fog_range;
        Self {
            inv_view_projection: params.inv_view_projection.to_cols_array_2d(),
            view_projection: params.view_projection.to_cols_array_2d(),
            eye: params.eye.extend(1.).to_array(),
            sun_direction: params.sun.direction.extend(0.).to_array(),
            sun_colour: (params.sun.colour * params.sun.intensity)
                .extend(0.)
                .to_array(),
            ambient: config.lighting.ambient.extend(0.).to_array(),
            fog_colour: config.lighting.fog_colour.extend(0.).to_array(),
            lens: [
                params.near,
                params.far,
                params.sun.angular_radius,
                params.sun.intensity,
            ],
            extents: [
                params.grid.0 as f32,
                params.grid.1 as f32,
                plan.render.0 as f32,
                plan.render.1 as f32,
            ],
            frame: [
                params.frame as f32,
                plan.samples.max(1) as f32,
                plan.bounces as f32,
                if params.history_valid > 0 { 1. } else { 0. },
            ],
            tuning: [
                params.jitter.0,
                params.jitter.1,
                config.ambient_strength,
                config.bounce_strength,
            ],
            effects: [
                config.reflection_roughness_cutoff,
                Self::effect_bits(plan.effects) as f32,
                config.bias,
                if plan.replace_raster { 0. } else { 1. },
            ],
            fog: [fog.0, fog.1, 5.0, 0.],
            counts: [
                counts[0] as f32,
                counts[1] as f32,
                counts[2] as f32,
                plan.debug_view.as_u32() as f32,
            ],
        }
    }
}

/// One-to-one with `DenoiseParams` in [`DENOISE_SHADER`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DenoiseParams {
    /// `x` width, `y` height, `z` iteration, `w` temporal floor.
    pub extent: [f32; 4],
    /// `x` phi colour, `y` phi normal, `z` phi depth, `w` variance gamma.
    pub weights: [f32; 4],
    /// `x` step, `y` variance floor, `z` max history, `w` history valid.
    pub misc: [f32; 4],
    /// `x` jitter x, `y` jitter y, `z` frame, `w` unused.
    pub frame: [f32; 4],
}

impl DenoiseParams {
    pub fn pack(
        extent: (u32, u32),
        iteration: u32,
        config: &DenoiseConfig,
        params: &FrameParams,
    ) -> Self {
        Self {
            extent: [
                extent.0 as f32,
                extent.1 as f32,
                iteration as f32,
                config.temporal_floor,
            ],
            weights: [
                config.phi_colour,
                config.phi_normal,
                config.phi_depth,
                config.variance_gamma,
            ],
            misc: [
                DenoiseConfig::step(iteration) as f32,
                config.variance_floor,
                config.max_history,
                if params.history_valid > 0 { 1. } else { 0. },
            ],
            frame: [params.jitter.0, params.jitter.1, params.frame as f32, 0.],
        }
    }
}

/// The scene arrays as the shader sees them.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SceneCounts {
    pub triangles: u32,
    pub nodes: u32,
    pub materials: u32,
    pub _pad: u32,
}

/// Uniform parameters for [`GBUF_PREP_SHADER`]: reconstructs 1:1 world
/// position, depth-derived surface normal, water roughness, and raster colour
/// at ray-grid resolution before tracing.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GbufPrepParams {
    pub inv_view_projection: [[f32; 4]; 4],
    pub eye: [f32; 4],
    /// `x` grid_w, `y` grid_h, `z` render_w, `w` render_h.
    pub extents: [f32; 4],
    /// `x` water_count (0..=16).
    pub water_count: [f32; 4],
    /// Up to 16 active water planes: `[min_x, max_x, min_z, max_z]`.
    pub water_planes: [[f32; 4]; 16],
    /// Packed water heights for the 16 planes (4 per vec4).
    pub water_heights: [[f32; 4]; 4],
}

impl GbufPrepParams {
    pub fn pack(
        params: &FrameParams,
        grid: (u32, u32),
        render: (u32, u32),
        water: &[super::harvest::WaterSurface],
    ) -> Self {
        let mut planes = [[0.0f32; 4]; 16];
        let mut heights = [[0.0f32; 4]; 4];
        let count = water.len().min(16);
        for (i, w) in water.iter().take(count).enumerate() {
            planes[i] = w.bounds;
            heights[i / 4][i % 4] = w.height;
        }
        Self {
            inv_view_projection: params.inv_view_projection.to_cols_array_2d(),
            eye: params.eye.extend(1.0).to_array(),
            extents: [
                grid.0.max(1) as f32,
                grid.1.max(1) as f32,
                render.0.max(1) as f32,
                render.1.max(1) as f32,
            ],
            water_count: [count as f32, 0.0, 0.0, 0.0],
            water_planes: planes,
            water_heights: heights,
        }
    }
}

/// 1:1 raster G-buffer prep shader: reads the full-resolution reversed-Z depth
/// buffer and untouched raster colour buffer, reconstructs exact world-space
/// position and edge-preserving surface normals, and tags water pixels for
/// crisp ray-traced reflections.
pub const GBUF_PREP_SHADER: &str = r#"
struct PrepParams {
    inv_vp: mat4x4<f32>,
    eye: vec4<f32>,
    extents: vec4<f32>,
    water_count: vec4<f32>,
    water_planes: array<vec4<f32>, 16>,
    water_heights: array<vec4<f32>, 4>,
};

@group(0) @binding(0) var<uniform> prep: PrepParams;
@group(0) @binding(1) var src_depth: texture_depth_2d;
@group(0) @binding(2) var src_raster: texture_2d<f32>;
@group(0) @binding(3) var out_pos: texture_storage_2d<rgba32float, write>;
@group(0) @binding(4) var out_norm: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var out_raster: texture_storage_2d<rgba16float, write>;

fn reconstruct_pos(px: vec2<i32>, z: f32, render: vec2<f32>) -> vec3<f32> {
    let uv = (vec2<f32>(px) + vec2<f32>(0.5)) / render;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let clip = prep.inv_vp * vec4<f32>(ndc, z, 1.0);
    return clip.xyz / clip.w;
}

@compute @workgroup_size(8, 8, 1)
fn gbuf_prep(@builtin(global_invocation_id) id: vec3<u32>) {
    let gw = u32(prep.extents.x);
    let gh = u32(prep.extents.y);
    if (id.x >= gw || id.y >= gh) {
        return;
    }
    let grid = prep.extents.xy;
    let render = prep.extents.zw;
    let rmax = vec2<i32>(i32(render.x) - 1, i32(render.y) - 1);
    let px = vec2<i32>(clamp((vec2<f32>(id.xy) + vec2<f32>(0.5)) / grid * render, vec2<f32>(0.0), render - vec2<f32>(1.0)));

    let z = textureLoad(src_depth, px, 0);
    let raster_rgb = textureLoad(src_raster, px, 0).rgb;

    if (z <= 1e-6) {
        textureStore(out_pos, vec2<i32>(id.xy), vec4<f32>(0.0, 0.0, 0.0, 0.0));
        textureStore(out_norm, vec2<i32>(id.xy), vec4<f32>(0.0, 1.0, 0.0, 1.0));
        textureStore(out_raster, vec2<i32>(id.xy), vec4<f32>(raster_rgb, 1.0));
        return;
    }

    let p = reconstruct_pos(px, z, render);
    let dist = length(p - prep.eye.xyz);
    let view_dir = normalize(p - prep.eye.xyz);

    let px_l = clamp(px + vec2<i32>(-1, 0), vec2<i32>(0), rmax);
    let px_r = clamp(px + vec2<i32>(1, 0), vec2<i32>(0), rmax);
    let px_u = clamp(px + vec2<i32>(0, -1), vec2<i32>(0), rmax);
    let px_d = clamp(px + vec2<i32>(0, 1), vec2<i32>(0), rmax);

    let z_l = textureLoad(src_depth, px_l, 0);
    let z_r = textureLoad(src_depth, px_r, 0);
    let z_u = textureLoad(src_depth, px_u, 0);
    let z_d = textureLoad(src_depth, px_d, 0);

    var dpdx = vec3<f32>(1.0, 0.0, 0.0);
    if (abs(z_r - z) < abs(z_l - z) && z_r > 1e-6) {
        dpdx = reconstruct_pos(px_r, z_r, render) - p;
    } else if (z_l > 1e-6) {
        dpdx = p - reconstruct_pos(px_l, z_l, render);
    }

    var dpdy = vec3<f32>(0.0, 0.0, 1.0);
    if (abs(z_d - z) < abs(z_u - z) && z_d > 1e-6) {
        dpdy = reconstruct_pos(px_d, z_d, render) - p;
    } else if (z_u > 1e-6) {
        dpdy = p - reconstruct_pos(px_u, z_u, render);
    }

    let raw_n = cross(dpdy, dpdx);
    var n = select(vec3<f32>(0.0, 1.0, 0.0), normalize(raw_n), length(raw_n) > 1e-6);
    if (dot(n, view_dir) > 0.0) {
        n = -n;
    }

    var roughness = 0.85;
    let wc = min(u32(prep.water_count.x), 16u);
    if (n.y > 0.85) {
        for (var wi = 0u; wi < wc; wi = wi + 1u) {
            let wb = prep.water_planes[wi];
            let wh = prep.water_heights[wi / 4u][wi % 4u];
            if (p.x >= wb.x - 1.0 && p.x <= wb.y + 1.0 &&
                p.z >= wb.z - 1.0 && p.z <= wb.w + 1.0 &&
                abs(p.y - wh) < 0.65) {
                roughness = 0.06;
                n = vec3<f32>(0.0, 1.0, 0.0);
                break;
            }
        }
    }

    textureStore(out_pos, vec2<i32>(id.xy), vec4<f32>(p, dist));
    textureStore(out_norm, vec2<i32>(id.xy), vec4<f32>(n, roughness));
    textureStore(out_raster, vec2<i32>(id.xy), vec4<f32>(raster_rgb, 1.0));
}
"#;

/// The trace shader.
///
/// The traversal is an explicit-stack closest-hit walk; the shading follows
/// `shade::trace` stage by stage. Every stage is gated by an effect bit, so a
/// frame with shadows off does not spend a ray on them.
pub const RAY_SHADER: &str = r#"
struct Params {
    inv_vp: mat4x4<f32>,
    vp: mat4x4<f32>,
    eye: vec4<f32>,
    sun_dir: vec4<f32>,
    sun_colour: vec4<f32>,
    ambient: vec4<f32>,
    fog_colour: vec4<f32>,
    lens: vec4<f32>,
    extents: vec4<f32>,
    frame: vec4<f32>,
    tuning: vec4<f32>,
    effects: vec4<f32>,
    fog: vec4<f32>,
    counts: vec4<f32>,
};

struct Tri {
    a: vec4<f32>,
    b: vec4<f32>,
    c: vec4<f32>,
    n: vec4<f32>,
};

struct Node {
    bounds_min: vec4<f32>,
    bounds_max: vec4<f32>,
};

struct Material {
    albedo: vec4<f32>,
    surface: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> tris: array<Tri>;
@group(0) @binding(2) var<storage, read> nodes: array<Node>;
@group(0) @binding(3) var<storage, read> order: array<u32>;
@group(0) @binding(4) var<storage, read> materials: array<Material>;
// rgb radiance written by whichever shading stage produced it.
@group(0) @binding(5) var out_colour: texture_storage_2d<rgba16float, write>;
// r: sky visibility (AO), g: depth in world units, b: hit distance for
// reflection, a: roughness class (0 diffuse, 1 mirror).
@group(0) @binding(6) var out_guide: texture_storage_2d<rgba16float, write>;
// rgb reflection radiance, a: reflection hit distance normalised.
@group(0) @binding(7) var out_reflection: texture_storage_2d<rgba16float, write>;
// xyz world normal, w roughness (packed for DLSS Ray Reconstruction).
@group(0) @binding(8) var out_normals: texture_storage_2d<rgba16float, write>;
// rgb diffuse albedo, a specular F0.
@group(0) @binding(9) var out_albedo: texture_storage_2d<rgba16float, write>;
// 1:1 raster-derived G-buffer (world position+dist, world normal+roughness, raster rgb).
@group(0) @binding(10) var src_gbuf_pos: texture_2d<f32>;
@group(0) @binding(11) var src_gbuf_norm: texture_2d<f32>;
@group(0) @binding(12) var src_gbuf_raster: texture_2d<f32>;

const PI: f32 = 3.14159265;

fn hash_u32(state: ptr<function, u32>) -> u32 {
    *state = *state * 747796405u + 2891336453u;
    var x = *state;
    x = x ^ (x >> 17u);
    x = x ^ (x << 5u);
    x = x ^ (x >> 13u);
    return x;
}

fn next_f32(state: ptr<function, u32>) -> f32 {
    // 24 bits of mantissa so the result is exactly representable in f32.
    return f32(hash_u32(state) >> 8u) / f32(1u << 24u);
}

fn orthonormal_basis(n: vec3<f32>) -> mat3x3<f32> {
    // Duff et al., same branch as the CPU reference.
    let sign = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (sign + n.z);
    let b = n.x * n.y * a;
    let t = vec3<f32>(1.0 + sign * n.x * n.x * a, sign * b, -sign * n.x);
    let u = vec3<f32>(b, sign + n.y * n.y * a, -n.y);
    return mat3x3<f32>(t, u, n);
}

fn cosine_hemisphere(state: ptr<function, u32>, n: vec3<f32>) -> vec3<f32> {
    let u1 = next_f32(state);
    let r = sqrt(u1);
    let phi = next_f32(state) * 2.0 * PI;
    let x = r * cos(phi);
    let y = r * sin(phi);
    let z = sqrt(max(1.0 - u1, 0.0));
    return orthonormal_basis(n) * vec3<f32>(x, y, z);
}

fn cone_direction(state: ptr<function, u32>, axis: vec3<f32>, radius: f32) -> vec3<f32> {
    let r = sqrt(next_f32(state));
    let phi = next_f32(state) * 2.0 * PI;
    let spread = r * tan(radius);
    return normalize(axis + orthonormal_basis(axis) * vec3<f32>(cos(phi) * spread, sin(phi) * spread, 0.0));
}

fn safe_inv_dir(dir: vec3<f32>) -> vec3<f32> {
    let eps = vec3<f32>(1e-12);
    let clamped = select(dir, select(vec3<f32>(-1e-12), eps, dir >= vec3<f32>(0.0)), abs(dir) < eps);
    return vec3<f32>(1.0) / clamped;
}

fn box_hit(minimum: vec3<f32>, maximum: vec3<f32>, origin: vec3<f32>, inv_dir: vec3<f32>, t_min: f32, t_max: f32) -> f32 {
    let t0s = (minimum - origin) * inv_dir;
    let t1s = (maximum - origin) * inv_dir;
    let tsmaller = min(t0s, t1s);
    let tbigger = max(t0s, t1s);
    let t0 = max(t_min, max(tsmaller.x, max(tsmaller.y, tsmaller.z)));
    let t1 = min(t_max, min(tbigger.x, min(tbigger.y, tbigger.z)));
    return select(-1.0, t0, t0 <= t1);
}

fn triangle_hit(tri: Tri, origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32) -> vec2<f32> {
    // xy = (distance, barycentric u); no-hit is (-1, 0). Two-sided by
    // construction, like the CPU reference.
    let a = tri.a.xyz;
    let b = tri.b.xyz;
    let c = tri.c.xyz;
    let e1 = b - a;
    let e2 = c - a;
    let p = cross(dir, e2);
    let det = dot(e1, p);
    if (abs(det) < 1e-12) {
        return vec2<f32>(-1.0, 0.0);
    }
    let inv = 1.0 / det;
    let tv = origin - a;
    let u = dot(tv, p) * inv;
    if (u < -1e-6 || u > 1.000001) {
        return vec2<f32>(-1.0, 0.0);
    }
    let q = cross(tv, e1);
    let v = dot(dir, q) * inv;
    if (v < -1e-6 || u + v > 1.000001) {
        return vec2<f32>(-1.0, 0.0);
    }
    let t = dot(e2, q) * inv;
    if (t < t_min || t > t_max) {
        return vec2<f32>(-1.0, 0.0);
    }
    return vec2<f32>(t, u);
}

struct Hit {
    t: f32,
    index: u32,
    normal: vec3<f32>,
};

fn closest_hit(origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32) -> Hit {
    var hit: Hit;
    hit.t = t_max;
    hit.index = 0xffffffffu;
    hit.normal = vec3<f32>(0.0);
    if (params.counts.y < 1.0) {
        return hit;
    }
    let inv_dir = safe_inv_dir(dir);
    if (box_hit(nodes[0].bounds_min.xyz, nodes[0].bounds_max.xyz, origin, inv_dir, t_min, hit.t) < 0.0) {
        return hit;
    }
    var stack: array<u32, 64>;
    var sp = 1u;
    stack[0] = 0u;
    loop {
        if (sp == 0u) {
            break;
        }
        sp = sp - 1u;
        let node_index = stack[sp];
        let node = nodes[node_index];
        if (node.bounds_min.w >= 0.0) {
            let count = u32(node.bounds_min.w);
            let first = u32(node.bounds_max.w);
            for (var i = 0u; i < count; i = i + 1u) {
                let tri_index = order[first + i];
                let result = triangle_hit(tris[tri_index], origin, dir, t_min, hit.t);
                if (result.x >= 0.0) {
                    hit.t = result.x;
                    hit.index = tri_index;
                }
            }
        } else {
            let left = node_index + 1u;
            let right = u32(node.bounds_max.w);
            let lh = box_hit(nodes[left].bounds_min.xyz, nodes[left].bounds_max.xyz, origin, inv_dir, t_min, hit.t);
            let rh = box_hit(nodes[right].bounds_min.xyz, nodes[right].bounds_max.xyz, origin, inv_dir, t_min, hit.t);
            if (lh >= 0.0 && rh >= 0.0) {
                let near = select(right, left, lh <= rh);
                let far = select(left, right, lh <= rh);
                if (sp < 63u) { stack[sp] = far; sp = sp + 1u; }
                if (sp < 63u) { stack[sp] = near; sp = sp + 1u; }
            } else if (lh >= 0.0) {
                if (sp < 63u) { stack[sp] = left; sp = sp + 1u; }
            } else if (rh >= 0.0) {
                if (sp < 63u) { stack[sp] = right; sp = sp + 1u; }
            }
        }
    }
    if (hit.index != 0xffffffffu) {
        let geo = normalize(tris[hit.index].n.xyz);
        hit.normal = select(-geo, geo, dot(geo, dir) <= 0.0);
    }
    return hit;
}

fn occluded(origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32) -> bool {
    if (params.counts.y < 1.0) {
        return false;
    }
    let inv_dir = safe_inv_dir(dir);
    if (box_hit(nodes[0].bounds_min.xyz, nodes[0].bounds_max.xyz, origin, inv_dir, t_min, t_max) < 0.0) {
        return false;
    }
    var stack: array<u32, 64>;
    var sp = 1u;
    stack[0] = 0u;
    loop {
        if (sp == 0u) {
            break;
        }
        sp = sp - 1u;
        let node_index = stack[sp];
        let node = nodes[node_index];
        if (node.bounds_min.w >= 0.0) {
            let count = u32(node.bounds_min.w);
            let first = u32(node.bounds_max.w);
            for (var i = 0u; i < count; i = i + 1u) {
                if (triangle_hit(tris[order[first + i]], origin, dir, t_min, t_max).x >= 0.0) {
                    return true;
                }
            }
        } else {
            let left = node_index + 1u;
            let right = u32(node.bounds_max.w);
            if (box_hit(nodes[left].bounds_min.xyz, nodes[left].bounds_max.xyz, origin, inv_dir, t_min, t_max) >= 0.0 && sp < 63u) {
                stack[sp] = left;
                sp = sp + 1u;
            }
            if (box_hit(nodes[right].bounds_min.xyz, nodes[right].bounds_max.xyz, origin, inv_dir, t_min, t_max) >= 0.0 && sp < 63u) {
                stack[sp] = right;
                sp = sp + 1u;
            }
        }
    }
    return false;
}

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn environment() -> vec3<f32> {
    return max(params.fog_colour.xyz, params.ambient.xyz * 0.85);
}

fn material_of(index: u32) -> Material {
    let mat_count = max(u32(params.counts.z), 1u);
    let mat_idx = min(u32(round(tris[index].b.w)), mat_count - 1u);
    return materials[mat_idx];
}

@compute @workgroup_size(8, 8, 1)
fn trace_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let extent = vec2<u32>(u32(params.extents.x), u32(params.extents.y));
    if (id.x >= extent.x || id.y >= extent.y) {
        return;
    }
    let ip = vec2<i32>(id.xy);
    let pos_d = textureLoad(src_gbuf_pos, ip, 0);
    let dist = pos_d.w;
    let layered = params.effects.w > 0.5;
    if (dist <= 0.0) {
        let sky_rgb = textureLoad(src_gbuf_raster, ip, 0).rgb;
        textureStore(out_colour, ip, vec4<f32>(select(sky_rgb, vec3<f32>(0.0), layered), 1.0));
        textureStore(out_guide, ip, vec4<f32>(1.0, 1.0, 0.0, 0.0));
        textureStore(out_reflection, ip, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        textureStore(out_normals, ip, vec4<f32>(0.0, 1.0, 0.0, 1.0));
        textureStore(out_albedo, ip, vec4<f32>(0.0, 0.0, 0.0, 0.04));
        return;
    }

    let point = pos_d.xyz;
    let norm_r = textureLoad(src_gbuf_norm, ip, 0);
    let raw_n = norm_r.xyz;
    let normal = select(vec3<f32>(0.0, 1.0, 0.0), normalize(raw_n), length(raw_n) > 1e-4);
    let roughness = clamp(norm_r.w, 0.04, 1.0);
    let raster_rgb = textureLoad(src_gbuf_raster, ip, 0).rgb;
    let view_dir = normalize(point - params.eye.xyz);

    let effects = u32(round(params.effects.y));
    let bits_shadows = (effects & 1u) != 0u;
    let bits_lighting = (effects & 2u) != 0u;
    let bits_reflections = (effects & 4u) != 0u;
    let bits_ao = (effects & 8u) != 0u;
    let bits_gi = (effects & 16u) != 0u;

    let bias = max(params.effects.z, max(0.15, dist * 0.003));
    let ray_origin = point + normal * bias - view_dir * (bias * 0.35);
    let n_dot_l = max(dot(normal, params.sun_dir.xyz), 0.0);

    var fog_t = 0.0;
    let fog_start = params.fog.x;
    let fog_end = params.fog.y;
    if (fog_end > fog_start && fog_start >= 0.0) {
        fog_t = clamp((dist - fog_start) / (fog_end - fog_start), 0.0, 1.0);
    }

    let samples = max(u32(params.frame.y), 1u);
    var state = id.x * 0x9e3779b9u ^ id.y * 0x85ebca6bu ^ u32(params.frame.x) * 0xc2b2ae35u;
    var accum_shadow = 0.0;
    var accum_open = 0.0;
    var accum_reflection = vec3<f32>(0.0);
    var accum_refl_dist = 0.0;
    var accum_gi = vec3<f32>(0.0);

    let ao_radius = clamp(params.fog.z, 2.0, 6.0);
    let ao_samples = select(2u, 1u, samples > 1u);
    let onb = orthonormal_basis(normal);

    for (var s = 0u; s < samples; s = s + 1u) {
        var shadow = 1.0;
        if (bits_shadows && n_dot_l > 0.02) {
            let shadow_dir = select(
                params.sun_dir.xyz,
                cone_direction(&state, params.sun_dir.xyz, params.lens.z),
                params.lens.z > 1e-4,
            );
            if (occluded(ray_origin, shadow_dir, bias * 0.5, params.lens.y)) {
                shadow = 0.0;
            }
        }
        accum_shadow = accum_shadow + shadow;

        var open = 1.0;
        if (bits_ao) {
            var hits = 0u;
            for (var i = 0u; i < ao_samples; i = i + 1u) {
                let u1 = next_f32(&state);
                let r = sqrt(u1);
                let phi = next_f32(&state) * 2.0 * PI;
                let d = onb * vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(1.0 - u1, 0.0)));
                if (occluded(ray_origin, d, bias * 0.5, ao_radius)) {
                    hits = hits + 1u;
                }
            }
            open = 1.0 - f32(hits) / f32(ao_samples);
        }
        accum_open = accum_open + open;

        var refl_dist = params.lens.y;
        if (bits_reflections && roughness <= params.effects.x) {
            let reflect_dir = reflect(view_dir, normal);
            let rhit = closest_hit(ray_origin, reflect_dir, bias, params.lens.y);
            if (rhit.index != 0xffffffffu) {
                let rmaterial = material_of(rhit.index);
                let r_ndl = max(dot(rhit.normal, params.sun_dir.xyz), 0.0);
                let lit = rmaterial.albedo.rgb * (params.ambient.xyz * params.tuning.z + params.sun_colour.xyz * r_ndl);
                let f0 = 0.04 + 0.16 * (1.0 - roughness);
                let m = 1.0 - clamp(dot(-view_dir, normal), 0.0, 1.0);
                let m2 = m * m;
                let fresnel = f0 + (1.0 - f0) * (m2 * m2 * m);
                accum_reflection = accum_reflection + lit * fresnel * (1.0 - fog_t);
                refl_dist = rhit.t;
            }
        }
        accum_refl_dist = accum_refl_dist + refl_dist;

        if (bits_gi && params.frame.z >= 1.0 && params.tuning.w > 0.0) {
            let d = cosine_hemisphere(&state, normal);
            let bhit = closest_hit(ray_origin, d, bias, min(params.lens.y, 48.0));
            if (bhit.index != 0xffffffffu) {
                let bmaterial = material_of(bhit.index);
                let b_ndl = max(dot(bhit.normal, params.sun_dir.xyz), 0.0);
                if (b_ndl > 0.01) {
                    let incoming = bmaterial.albedo.rgb * params.sun_colour.xyz * b_ndl;
                    accum_gi = accum_gi + raster_rgb * incoming * (params.tuning.w * 0.30 * (1.0 - fog_t));
                }
            }
        }
    }

    let inv = 1.0 / f32(samples);
    let avg_shadow = accum_shadow * inv;
    let avg_open = accum_open * inv;
    let avg_refl = accum_reflection * inv;
    let avg_refl_dist = accum_refl_dist * inv;
    let avg_gi = accum_gi * inv;

    var out_rgb = vec3<f32>(0.0);
    var out_lf = 1.0;
    if (layered) {
        let shadow_mod = select(1.0, mix(0.30, 1.0, avg_shadow), bits_shadows && n_dot_l > 0.02);
        let ao_mod = select(1.0, mix(0.58, 1.0, avg_open), bits_ao);
        out_lf = mix(shadow_mod * ao_mod, 1.0, fog_t);
        out_rgb = avg_gi;
    } else {
        let direct = select(vec3<f32>(0.0), params.sun_colour.xyz * (n_dot_l * avg_shadow), bits_lighting);
        let ambient = params.ambient.xyz * (avg_open * params.tuning.z);
        let relit = raster_rgb * (direct + ambient) + avg_gi;
        out_rgb = mix(relit, params.fog_colour.xyz, fog_t);
        out_lf = 1.0;
    }

    let debug_mode = u32(round(params.counts.w));
    var store_refl = avg_refl;
    if (debug_mode == 1u) {
        out_rgb = raster_rgb;
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    } else if (debug_mode == 2u) {
        out_rgb = vec3<f32>(avg_shadow);
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    } else if (debug_mode == 3u) {
        out_rgb = vec3<f32>(avg_open);
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    } else if (debug_mode == 4u) {
        out_rgb = avg_gi;
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    } else if (debug_mode == 5u) {
        out_rgb = avg_refl;
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    } else if (debug_mode == 6u) {
        out_rgb = vec3<f32>(0.0);
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    }

    let spec_f0 = 0.04 + 0.16 * (1.0 - roughness);
    textureStore(out_colour, ip, vec4<f32>(out_rgb, out_lf));
    textureStore(out_guide, ip, vec4<f32>(avg_open, dist / params.lens.y, avg_shadow, roughness));
    textureStore(out_reflection, ip, vec4<f32>(store_refl, avg_refl_dist / params.lens.y));
    textureStore(out_normals, ip, vec4<f32>(normal, roughness));
    textureStore(out_albedo, ip, vec4<f32>(raster_rgb, spec_f0));
}
"#;

/// The denoiser shader: a temporal accumulation pass and one à-trous iteration.
pub const DENOISE_SHADER: &str = r#"
struct Params {
    extent: vec4<f32>,
    weights: vec4<f32>,
    misc: vec4<f32>,
    frame: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
// Colour, guide, reflection, motion, depth of the current frame.
@group(0) @binding(1) var src_colour: texture_2d<f32>;
@group(0) @binding(2) var src_guide: texture_2d<f32>;
@group(0) @binding(3) var src_reflection: texture_2d<f32>;
@group(0) @binding(4) var src_motion: texture_2d<f32>;
@group(0) @binding(5) var src_depth: texture_depth_2d;
// History: accumulated colour and moments. The moments carry `depth` in `.z`
// and the history length in `.w`, so the temporal and atrous passes need only
// one history pair each.
@group(0) @binding(6) var history_colour: texture_2d<f32>;
@group(0) @binding(7) var history_moments: texture_2d<f32>;
// World-space normal (.xyz) + roughness (.w) from the trace pass.
@group(0) @binding(8) var src_normals: texture_2d<f32>;
// Destinations.
@group(0) @binding(9) var out_colour: texture_storage_2d<rgba16float, write>;
@group(0) @binding(10) var out_moments: texture_storage_2d<rgba16float, write>;

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Map a render-extent pixel to the ray-grid texel coordinate.
fn grid_coord(pixel: vec2<i32>, render_size: vec2<i32>, grid_dim: vec2<u32>) -> vec2<i32> {
    let g = vec2<f32>(grid_dim);
    let mapped = (vec2<f32>(pixel) + vec2<f32>(0.5)) / vec2<f32>(render_size) * g;
    return vec2<i32>(clamp(mapped, vec2<f32>(0.0), g - vec2<f32>(1.0)));
}

struct ReprojectResult {
    colour: vec3<f32>,
    light_factor: f32,
    moments: vec2<f32>,
    length: f32,
};

// Reproject with the motion vector and validate against the previous frame's
// depth stored in `history_moments.z`.
fn reproject(pixel: vec2<i32>, size: vec2<i32>) -> ReprojectResult {
    var empty: ReprojectResult;
    empty.colour = vec3<f32>(0.0);
    empty.light_factor = 1.0;
    empty.moments = vec2<f32>(0.0);
    empty.length = 0.0;
    let motion = textureLoad(src_motion, pixel, 0).xy;
    let source = vec2<f32>(pixel) + vec2<f32>(0.5) - motion;
    if (source.x < 0.0 || source.y < 0.0 || source.x >= f32(size.x) || source.y >= f32(size.y)) {
        return empty;
    }
    let index = vec2<i32>(source);
    let prev_m = textureLoad(history_moments, index, 0);
    let previous_depth = prev_m.z;
    let current_depth = textureLoad(src_depth, pixel, 0);
    let both = previous_depth > 0.0 && current_depth > 0.0;
    if (both) {
        if (abs(previous_depth - current_depth) / max(current_depth, 1e-3) > 0.1) {
            return empty;
        }
    } else if ((previous_depth > 0.0) != (current_depth > 0.0)) {
        return empty;
    }
    let prev_c = textureLoad(history_colour, index, 0);
    var out: ReprojectResult;
    out.colour = prev_c.rgb;
    out.light_factor = prev_c.a;
    out.moments = prev_m.xy;
    out.length = prev_m.w;
    return out;
}

@compute @workgroup_size(8, 8, 1)
fn temporal_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<i32>(i32(params.extent.x), i32(params.extent.y));
    let pixel = vec2<i32>(i32(id.x), i32(id.y));
    if (id.x >= u32(size.x) || id.y >= u32(size.y)) {
        return;
    }
    let gdim = textureDimensions(src_colour);
    let gmax = vec2<i32>(i32(gdim.x) - 1, i32(gdim.y) - 1);
    let gp = grid_coord(pixel, size, gdim);
    let cur_raw = textureLoad(src_colour, gp, 0);
    let current = cur_raw.rgb + textureLoad(src_reflection, gp, 0).rgb;
    let current_lf = cur_raw.a;
    let depth = textureLoad(src_depth, pixel, 0);
    let prev = reproject(pixel, size);
    let n = min(prev.length, params.misc.z);
    let valid = params.misc.w > 0.5 && n > 0.0;
    var blended = current;
    var blended_lf = current_lf;
    var length = 1.0;
    var m1 = luminance(current);
    var m2 = m1 * m1;
    if (valid) {
        // Local 3x3 mean and standard deviation in ray-grid space for variance clamping.
        var sum = current;
        var sum_sq = current * current;
        var sum_lf = current_lf;
        var sum_lf_sq = current_lf * current_lf;
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                if (dx == 0 && dy == 0) {
                    continue;
                }
                let p = clamp(gp + vec2<i32>(dx, dy), vec2<i32>(0), gmax);
                let raw = textureLoad(src_colour, p, 0);
                let c = raw.rgb + textureLoad(src_reflection, p, 0).rgb;
                sum = sum + c;
                sum_sq = sum_sq + c * c;
                sum_lf = sum_lf + raw.a;
                sum_lf_sq = sum_lf_sq + raw.a * raw.a;
            }
        }
        let inv9 = 1.0 / 9.0;
        let mean = sum * inv9;
        let stddev = sqrt(max(sum_sq * inv9 - mean * mean, vec3<f32>(0.0))) * params.weights.w;
        let history = clamp(prev.colour, mean - stddev, mean + stddev);
        let mean_lf = sum_lf * inv9;
        let std_lf = sqrt(max(sum_lf_sq * inv9 - mean_lf * mean_lf, 0.0)) * params.weights.w;
        let history_lf = clamp(prev.light_factor, mean_lf - std_lf, mean_lf + std_lf);
        let alpha = max(1.0 / (n + 1.0), params.extent.w);
        blended = mix(history, current, alpha);
        blended_lf = clamp(mix(history_lf, current_lf, alpha), 0.0, 1.0);
        length = min(n + 1.0, params.misc.z);
        let lum = luminance(blended);
        m1 = mix(prev.moments.x, lum, alpha);
        m2 = mix(prev.moments.y, lum * lum, alpha);
    }
    textureStore(out_colour, pixel, vec4<f32>(blended, blended_lf));
    textureStore(out_moments, pixel, vec4<f32>(m1, m2, depth, length));
}

fn variance_of(moments: vec4<f32>) -> f32 {
    let length = moments.w;
    let variance = max(moments.y - moments.x * moments.x, 0.0);
    let bias = select(1.0, 1.0 / max(sqrt(length), 0.25), length > 0.0);
    return max(variance * bias, params.misc.y);
}

fn normal_at_scaled(pixel: vec2<i32>, grid_scale: vec2<f32>, gmax: vec2<f32>) -> vec3<f32> {
    let gp = vec2<i32>(clamp((vec2<f32>(pixel) + vec2<f32>(0.5)) * grid_scale, vec2<f32>(0.0), gmax));
    let n = textureLoad(src_normals, gp, 0).xyz;
    let len = length(n);
    return select(vec3<f32>(0.0, 1.0, 0.0), n / len, len > 1e-4);
}

fn kernel_weight(d: i32) -> f32 {
    let a = abs(d);
    if (a == 0) { return 0.375; }
    if (a == 1) { return 0.25; }
    return 0.0625;
}

@compute @workgroup_size(8, 8, 1)
fn atrous_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<i32>(i32(params.extent.x), i32(params.extent.y));
    let pixel = vec2<i32>(i32(id.x), i32(id.y));
    if (id.x >= u32(size.x) || id.y >= u32(size.y)) {
        return;
    }
    let ndim = vec2<f32>(textureDimensions(src_normals));
    let grid_scale = ndim / vec2<f32>(size);
    let gmax = ndim - vec2<f32>(1.0);
    let centre_depth = textureLoad(src_depth, pixel, 0);
    let centre_normal = normal_at_scaled(pixel, grid_scale, gmax);
    let centre_moments = textureLoad(history_moments, pixel, 0);
    let centre_variance = variance_of(centre_moments);
    let inv_sigma = 1.0 / max(params.weights.x * sqrt(centre_variance), 1e-6);
    let inv_depth_denom = 1.0 / max(params.weights.z * max(abs(centre_depth), 1e-3), 1e-6);
    let centre_raw = textureLoad(history_colour, pixel, 0);
    let centre = centre_raw.rgb;
    let centre_lum = luminance(centre);

    // Seed accumulation with the centre tap (skipping redundant exp(0)/pow(1)).
    let w0 = 0.375 * 0.375;
    var sum = centre * w0;
    var sum_lf = centre_raw.a * w0;
    var weight_sum = w0;
    var moment1 = centre_moments.x * w0;
    var moment2 = centre_moments.y * w0;
    let step = i32(params.misc.x);
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        let wy = kernel_weight(dy);
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            if (dx == 0 && dy == 0) {
                continue;
            }
            let p = pixel + vec2<i32>(dx * step, dy * step);
            if (p.x < 0 || p.y < 0 || p.x >= size.x || p.y >= size.y) {
                continue;
            }
            let d1 = textureLoad(src_depth, p, 0);
            let wd = exp(-abs(centre_depth - d1) * inv_depth_denom);
            if (wd < 1e-4) {
                continue;
            }
            let n1 = normal_at_scaled(p, grid_scale, gmax);
            let ndot = dot(centre_normal, n1);
            if (ndot <= 0.0) {
                continue;
            }
            let wn = pow(ndot, params.weights.y);
            if (wn < 1e-4) {
                continue;
            }
            let sample_raw = textureLoad(history_colour, p, 0);
            let sample = sample_raw.rgb;
            let w_colour = exp(-abs(centre_lum - luminance(sample)) * inv_sigma);
            let w_lf = exp(-abs(centre_raw.a - sample_raw.a) * 5.0);
            let w = wy * kernel_weight(dx) * wn * wd * w_colour * w_lf;
            if (w <= 1e-6) {
                continue;
            }
            sum = sum + sample * w;
            sum_lf = sum_lf + sample_raw.a * w;
            weight_sum = weight_sum + w;
            let moments = textureLoad(history_moments, p, 0);
            moment1 = moment1 + moments.x * w;
            moment2 = moment2 + moments.y * w;
        }
    }
    let inv_w = 1.0 / weight_sum;
    textureStore(out_colour, pixel, vec4<f32>(sum * inv_w, sum_lf * inv_w));
    textureStore(out_moments, pixel, vec4<f32>(moment1 * inv_w, moment2 * inv_w, centre_moments.z, centre_moments.w));
}
"#;

/// The compose shader: writes the final render-extent colour and guide planes
/// (resampling the ray-grid guides cleanly onto render-extent pixels).
pub const COMPOSE_SHADER: &str = r#"
@group(0) @binding(0) var colour: texture_2d<f32>;
@group(0) @binding(1) var moments: texture_2d<f32>;
@group(0) @binding(2) var guide: texture_2d<f32>;
@group(0) @binding(3) var reflection: texture_2d<f32>;
@group(0) @binding(4) var out_colour: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var out_guide: texture_storage_2d<rgba16float, write>;
@group(0) @binding(6) var<uniform> extent: vec4<f32>;

@compute @workgroup_size(8, 8, 1)
fn compose_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<i32>(i32(extent.x), i32(extent.y));
    let pixel = vec2<i32>(i32(id.x), i32(id.y));
    if (id.x >= u32(size.x) || id.y >= u32(size.y)) { return; }
    let c = textureLoad(colour, pixel, 0);
    let m = textureLoad(moments, pixel, 0);
    let gdim = vec2<f32>(textureDimensions(guide));
    let gcoord = vec2<i32>(clamp((vec2<f32>(pixel) + vec2<f32>(0.5)) / extent.xy * gdim, vec2<f32>(0.0), gdim - vec2<f32>(1.0)));
    let g = textureLoad(guide, gcoord, 0);
    let r = textureLoad(reflection, gcoord, 0);
    textureStore(out_colour, pixel, c);
    let variance = max(m.y - m.x * m.x, 0.0);
    textureStore(out_guide, pixel, vec4<f32>(m.z, g.x, r.w, variance));
}
"#;

const BLIT_SHADER: &str = r#"
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    var out: VOut;
    let xy = p[idx];
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5);
    return out;
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSampleLevel(src_tex, src_samp, i.uv, 0.0).rgb, 1.0);
}
"#;

/// Fullscreen layered composite shader: modulates the untouched full raster
/// world render by Raywatcher's denoised shadow*AO light factor (`src_rt.a`)
/// and adds denoised RT reflections + indirect GI bounce (`src_rt.rgb`).
const COMPOSITE_SHADER: &str = r#"
@group(0) @binding(0) var src_raster: texture_2d<f32>;
@group(0) @binding(1) var src_rt: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    var out: VOut;
    let xy = p[idx];
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5);
    return out;
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let raster = textureSampleLevel(src_raster, src_samp, i.uv, 0.0);
    let rt = textureSampleLevel(src_rt, src_samp, i.uv, 0.0);
    let light_factor = clamp(rt.a, 0.0, 1.0);
    let layered = max(raster.rgb * light_factor + rt.rgb, vec3<f32>(0.0));
    return vec4<f32>(layered, raster.a);
}
"#;

pub const COLOUR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// `Rg16Float` would be smaller, but storage textures cannot be 16-bit
/// single/dual channel on every backend, so the guide is RGBA like the rest.
pub const GUIDE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

fn storage_texture(
    device: &wgpu::Device,
    label: &str,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Shader modules and pipeline layouts, created once.
struct Pipelines {
    gbuf_prep: wgpu::ComputePipeline,
    gbuf_prep_layout: wgpu::BindGroupLayout,
    trace: wgpu::ComputePipeline,
    temporal: wgpu::ComputePipeline,
    atrous: wgpu::ComputePipeline,
    trace_layout: wgpu::BindGroupLayout,
    denoise_layout: wgpu::BindGroupLayout,
    compose: wgpu::ComputePipeline,
    compose_layout: wgpu::BindGroupLayout,
    blit_module: wgpu::ShaderModule,
    blit_layout: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    composite_module: wgpu::ShaderModule,
    composite_layout: wgpu::BindGroupLayout,
}

impl Pipelines {
    fn new(device: &wgpu::Device) -> Self {
        let gbuf_prep_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("raywatcher gbuf prep"),
            source: wgpu::ShaderSource::Wgsl(GBUF_PREP_SHADER.into()),
        });
        let gbuf_prep_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("raywatcher gbuf prep"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1, wgpu::TextureSampleType::Depth),
                texture_entry(2, wgpu::TextureSampleType::Float { filterable: true }),
                storage_texture_entry(
                    3,
                    wgpu::StorageTextureAccess::WriteOnly,
                    wgpu::TextureFormat::Rgba32Float,
                ),
                storage_texture_entry(4, wgpu::StorageTextureAccess::WriteOnly, GUIDE_FORMAT),
                storage_texture_entry(5, wgpu::StorageTextureAccess::WriteOnly, COLOUR_FORMAT),
            ],
        });
        let gbuf_prep_pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("raywatcher gbuf prep"),
            bind_group_layouts: &[&gbuf_prep_layout],
            push_constant_ranges: &[],
        });
        let gbuf_prep = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("raywatcher gbuf prep"),
            layout: Some(&gbuf_prep_pipeline),
            module: &gbuf_prep_module,
            entry_point: "gbuf_prep",
        });
        let trace_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("raywatcher trace"),
            source: wgpu::ShaderSource::Wgsl(RAY_SHADER.into()),
        });
        let denoise_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("raywatcher denoise"),
            source: wgpu::ShaderSource::Wgsl(DENOISE_SHADER.into()),
        });
        let compose_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("raywatcher compose"),
            source: wgpu::ShaderSource::Wgsl(COMPOSE_SHADER.into()),
        });
        let trace_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("raywatcher scene"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage_texture_entry(5, wgpu::StorageTextureAccess::WriteOnly, COLOUR_FORMAT),
                storage_texture_entry(6, wgpu::StorageTextureAccess::WriteOnly, GUIDE_FORMAT),
                storage_texture_entry(7, wgpu::StorageTextureAccess::WriteOnly, COLOUR_FORMAT),
                storage_texture_entry(8, wgpu::StorageTextureAccess::WriteOnly, GUIDE_FORMAT),
                storage_texture_entry(9, wgpu::StorageTextureAccess::WriteOnly, COLOUR_FORMAT),
                texture_entry(10, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(11, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(12, wgpu::TextureSampleType::Float { filterable: true }),
            ],
        });
        let denoise_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("raywatcher denoise"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(2, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(4, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(5, wgpu::TextureSampleType::Depth),
                texture_entry(6, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(7, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(8, wgpu::TextureSampleType::Float { filterable: true }),
                storage_texture_entry(9, wgpu::StorageTextureAccess::WriteOnly, COLOUR_FORMAT),
                storage_texture_entry(10, wgpu::StorageTextureAccess::WriteOnly, GUIDE_FORMAT),
            ],
        });
        let trace_pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("raywatcher trace"),
            bind_group_layouts: &[&trace_layout],
            push_constant_ranges: &[],
        });
        let denoise_pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("raywatcher denoise"),
            bind_group_layouts: &[&denoise_layout],
            push_constant_ranges: &[],
        });
        let trace = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("raywatcher trace"),
            layout: Some(&trace_pipeline),
            module: &trace_module,
            entry_point: "trace_pass",
        });
        let compose_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("raywatcher compose"),
            entries: &[
                texture_entry(0, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(1, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(2, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: true }),
                storage_texture_entry(4, wgpu::StorageTextureAccess::WriteOnly, COLOUR_FORMAT),
                storage_texture_entry(5, wgpu::StorageTextureAccess::WriteOnly, GUIDE_FORMAT),
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let compose_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("raywatcher compose"),
                bind_group_layouts: &[&compose_layout],
                push_constant_ranges: &[],
            });
        let compose = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("raywatcher compose"),
            layout: Some(&compose_pipeline_layout),
            module: &compose_module,
            entry_point: "compose_pass",
        });
        let temporal = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("raywatcher temporal"),
            layout: Some(&denoise_pipeline),
            module: &denoise_module,
            entry_point: "temporal_pass",
        });
        let atrous = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("raywatcher atrous"),
            layout: Some(&denoise_pipeline),
            module: &denoise_module,
            entry_point: "atrous_pass",
        });
        Self {
            gbuf_prep,
            gbuf_prep_layout,
            trace,
            temporal,
            atrous,
            trace_layout,
            denoise_layout,
            compose,
            compose_layout,
            blit_module: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("raywatcher blit"),
                source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
            }),
            blit_layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("raywatcher blit"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            }),
            blit_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("raywatcher blit sampler"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            composite_module: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("raywatcher composite"),
                source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
            }),
            composite_layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("raywatcher composite"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            }),
        }
    }
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_texture_entry(
    binding: u32,
    access: wgpu::StorageTextureAccess,
    format: wgpu::TextureFormat,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access,
            view_dimension: wgpu::TextureViewDimension::D2,
            format,
        },
        count: None,
    }
}

/// The uploaded scene buffers. Rebuilt only when the resident set changes.
pub struct SceneBuffers {
    pub triangles: wgpu::Buffer,
    pub nodes: wgpu::Buffer,
    pub order: wgpu::Buffer,
    pub materials: wgpu::Buffer,
    pub counts: SceneCounts,
    /// The CPU copy, kept for the software path and for the tests.
    pub source: Arc<super::bvh::SceneGeometry>,
}

impl SceneBuffers {
    fn upload(device: &wgpu::Device, scene: Arc<super::bvh::SceneGeometry>) -> Self {
        // wgpu 0.19 rejects 0-byte storage buffers; substitute a 1-element dummy
        // buffer when a slice is empty while keeping `counts` at the real length.
        let dummy_tri = [<RayTriangle as bytemuck::Zeroable>::zeroed()];
        let dummy_node = [<BvhNode as bytemuck::Zeroable>::zeroed()];
        let dummy_ord = [0u32];
        let dummy_mat = [<RayMaterial as bytemuck::Zeroable>::zeroed()];
        let tri_slice = if scene.triangles.is_empty() {
            &dummy_tri[..]
        } else {
            &scene.triangles[..]
        };
        let node_slice = if scene.nodes.is_empty() {
            &dummy_node[..]
        } else {
            &scene.nodes[..]
        };
        let ord_slice = if scene.order.is_empty() {
            &dummy_ord[..]
        } else {
            &scene.order[..]
        };
        let mat_slice = if scene.materials.is_empty() {
            &dummy_mat[..]
        } else {
            &scene.materials[..]
        };
        let triangles = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher triangles"),
            contents: bytemuck::cast_slice(tri_slice),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let nodes = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher bvh"),
            contents: bytemuck::cast_slice(node_slice),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let order = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher order"),
            contents: bytemuck::cast_slice(ord_slice),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let materials = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher materials"),
            contents: bytemuck::cast_slice(mat_slice),
            usage: wgpu::BufferUsages::STORAGE,
        });
        Self {
            triangles,
            nodes,
            order,
            materials,
            counts: SceneCounts {
                triangles: scene.triangles.len() as u32,
                nodes: scene.nodes.len() as u32,
                materials: scene.materials.len() as u32,
                _pad: 0,
            },
            source: scene,
        }
    }
}

/// The ray pass's own render targets, at the ray grid's extent.
struct GridTargets {
    gbuf_pos: wgpu::Texture,
    gbuf_norm: wgpu::Texture,
    gbuf_raster: wgpu::Texture,
    gbuf_pos_view: wgpu::TextureView,
    gbuf_norm_view: wgpu::TextureView,
    gbuf_raster_view: wgpu::TextureView,
    colour: wgpu::Texture,
    guide: wgpu::Texture,
    reflection: wgpu::Texture,
    normals: wgpu::Texture,
    albedo: wgpu::Texture,
    colour_view: wgpu::TextureView,
    guide_view: wgpu::TextureView,
    reflection_view: wgpu::TextureView,
    normals_view: wgpu::TextureView,
    albedo_view: wgpu::TextureView,
}

impl GridTargets {
    fn new(device: &wgpu::Device, w: u32, h: u32) -> Self {
        let gbuf_pos = storage_texture(
            device,
            "raywatcher gbuf pos",
            w,
            h,
            wgpu::TextureFormat::Rgba32Float,
        );
        let gbuf_norm = storage_texture(device, "raywatcher gbuf norm", w, h, GUIDE_FORMAT);
        let gbuf_raster = storage_texture(device, "raywatcher gbuf raster", w, h, COLOUR_FORMAT);
        let gbuf_pos_view = gbuf_pos.create_view(&Default::default());
        let gbuf_norm_view = gbuf_norm.create_view(&Default::default());
        let gbuf_raster_view = gbuf_raster.create_view(&Default::default());
        let colour = storage_texture(device, "raywatcher rays", w, h, COLOUR_FORMAT);
        let guide = storage_texture(device, "raywatcher guide", w, h, GUIDE_FORMAT);
        let reflection = storage_texture(device, "raywatcher reflection", w, h, COLOUR_FORMAT);
        let normals = storage_texture(device, "raywatcher normals", w, h, GUIDE_FORMAT);
        let albedo = storage_texture(device, "raywatcher albedo", w, h, COLOUR_FORMAT);
        let colour_view = colour.create_view(&Default::default());
        let guide_view = guide.create_view(&Default::default());
        let reflection_view = reflection.create_view(&Default::default());
        let normals_view = normals.create_view(&Default::default());
        let albedo_view = albedo.create_view(&Default::default());
        Self {
            gbuf_pos,
            gbuf_norm,
            gbuf_raster,
            gbuf_pos_view,
            gbuf_norm_view,
            gbuf_raster_view,
            colour,
            guide,
            reflection,
            normals,
            albedo,
            colour_view,
            guide_view,
            reflection_view,
            normals_view,
            albedo_view,
        }
    }
}

/// Number of temporal-plus-spatial filter passes the denoiser records. The
/// settings' iteration count only changes how many of them run; the bind groups
/// always exist, so changing a slider mid-frame cannot invalidate a pass.
pub const MAX_PASSES: usize = 6;

fn plane(
    device: &wgpu::Device,
    label: &str,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = storage_texture(device, label, w, h, format);
    let view = texture.create_view(&Default::default());
    (texture, view)
}

fn depth_plane(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("raywatcher depth"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        aspect: wgpu::TextureAspect::DepthOnly,
        ..Default::default()
    });
    (texture, view)
}

/// Everything Raywatcher owns on the GPU.
pub struct RaywatcherGpu {
    pipelines: Pipelines,
    gbuf_prep_uniforms: wgpu::Buffer,
    gbuf_prep_bind_group: Option<wgpu::BindGroup>,
    uniforms: wgpu::Buffer,
    temporal_uniforms: wgpu::Buffer,
    atrous_uniforms: Vec<wgpu::Buffer>,
    scene: SceneBuffers,
    scene_geom: Arc<super::bvh::SceneGeometry>,
    dxr: Option<super::dxr::DxrTracer>,
    dxr_failed: bool,
    dlss_rr: Option<super::reconstruct::DlssRayReconstructor>,
    dlss_rr_failed: bool,
    blit_pipeline: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    blit_bind_group: wgpu::BindGroup,
    composite_pipeline: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    raster_copy: Option<(
        wgpu::TextureFormat,
        (u32, u32),
        wgpu::Texture,
        wgpu::TextureView,
        wgpu::BindGroup,
    )>,
    grid: GridTargets,
    grid_extent: (u32, u32),
    render_extent: (u32, u32),
    trace_bind_group: wgpu::BindGroup,
    // Ping-pong planes: index 0 and 1 alternate as the filter advances.
    filter_colour: [wgpu::TextureView; 2],
    filter_moments: [wgpu::TextureView; 2],
    temporal_bind_groups: [wgpu::BindGroup; 2],
    last_output_plane: usize,
    atrous_bind_groups: Vec<wgpu::BindGroup>,
    compose_bind_groups: [wgpu::BindGroup; 2],
    compose_uniforms: wgpu::Buffer,
    filter_colour_tex: [wgpu::Texture; 2],
    filter_moments_tex: [wgpu::Texture; 2],
    temporal_colour: wgpu::TextureView,
    temporal_moments: wgpu::TextureView,
    _temporal_colour_tex: wgpu::Texture,
    _temporal_moments_tex: wgpu::Texture,
    motion: wgpu::TextureView,
    _motion_tex: wgpu::Texture,
    depth: wgpu::TextureView,
    _depth_tex: wgpu::Texture,
    /// Interpolated result, at the render extent, for the upscaler to consume.
    pub output: wgpu::Texture,
    pub output_view: wgpu::TextureView,
    /// Guide planes at render extent: `x` the sky visibility, `y` the reflection
    /// hit distance, `z` the filtered variance, `w` the depth. Handed to the ray
    /// reconstruction providers and to the debug view.
    pub guide: wgpu::Texture,
    pub guide_view: wgpu::TextureView,
    /// Frames rendered since the last reset, saturating, for the history bit.
    frames: u32,
    /// Whether the last frame dispatched the trace pass.
    pub traced_last_frame: bool,
}

impl RaywatcherGpu {
    pub fn new(
        device: &wgpu::Device,
        scene: Arc<super::bvh::SceneGeometry>,
        render_extent: (u32, u32),
        grid_extent: (u32, u32),
    ) -> Self {
        let pipelines = Pipelines::new(device);
        let gbuf_prep_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raywatcher gbuf prep params"),
            size: std::mem::size_of::<GbufPrepParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raywatcher params"),
            size: std::mem::size_of::<RayParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mk_denoise_buf = |label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<DenoiseParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let temporal_uniforms = mk_denoise_buf("raywatcher temporal params");
        let atrous_uniforms: Vec<wgpu::Buffer> = (0..MAX_PASSES)
            .map(|_| mk_denoise_buf("raywatcher atrous params"))
            .collect();
        let scene_geom = scene.clone();
        let scene_buffers = SceneBuffers::upload(device, scene);
        let grid = GridTargets::new(device, grid_extent.0, grid_extent.1);
        let (w, h) = (render_extent.0.max(1), render_extent.1.max(1));
        let (filter_colour_tex, filter_colour) = {
            let (a, av) = plane(device, "raywatcher filter colour a", w, h, COLOUR_FORMAT);
            let (b, bv) = plane(device, "raywatcher filter colour b", w, h, COLOUR_FORMAT);
            ([a, b], [av, bv])
        };
        let (filter_moments_tex, filter_moments) = {
            let (a, av) = plane(device, "raywatcher filter moments a", w, h, GUIDE_FORMAT);
            let (b, bv) = plane(device, "raywatcher filter moments b", w, h, GUIDE_FORMAT);
            ([a, b], [av, bv])
        };
        let (temporal_colour_tex, temporal_colour) =
            plane(device, "raywatcher temporal colour", w, h, COLOUR_FORMAT);
        let (temporal_moments_tex, temporal_moments) =
            plane(device, "raywatcher temporal moments", w, h, GUIDE_FORMAT);
        let (motion_tex, motion) = plane(device, "raywatcher motion", w, h, COLOUR_FORMAT);
        let (depth_tex, depth) = depth_plane(device, w, h);
        let (output, output_view) = plane(device, "raywatcher output", w, h, COLOUR_FORMAT);
        let (guide, guide_view) = plane(device, "raywatcher output guide", w, h, GUIDE_FORMAT);
        let compose_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raywatcher compose params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let trace_bind_group =
            Self::trace_bindings(device, &pipelines, &uniforms, &scene_buffers, &grid);
        let temporal_bind_groups = [
            Self::temporal_binding(
                device,
                &pipelines,
                &temporal_uniforms,
                &grid,
                &motion,
                &depth,
                &filter_colour[0],
                &filter_moments[0],
                &temporal_colour,
                &temporal_moments,
            ),
            Self::temporal_binding(
                device,
                &pipelines,
                &temporal_uniforms,
                &grid,
                &motion,
                &depth,
                &filter_colour[1],
                &filter_moments[1],
                &temporal_colour,
                &temporal_moments,
            ),
        ];
        let atrous_bind_groups = Self::atrous_bindings(
            device,
            &pipelines,
            &atrous_uniforms,
            &grid,
            &depth,
            &motion,
            &filter_colour,
            &filter_moments,
            &temporal_colour,
            &temporal_moments,
        );
        let compose_bind_groups = [
            Self::compose_binding(
                device,
                &pipelines,
                &compose_uniforms,
                &grid,
                &filter_colour[0],
                &filter_moments[0],
                &output_view,
                &guide_view,
            ),
            Self::compose_binding(
                device,
                &pipelines,
                &compose_uniforms,
                &grid,
                &filter_colour[1],
                &filter_moments[1],
                &output_view,
                &guide_view,
            ),
        ];
        let blit_bind_group = Self::blit_binding(device, &pipelines, &output_view);
        Self {
            pipelines,
            gbuf_prep_uniforms,
            gbuf_prep_bind_group: None,
            uniforms,
            temporal_uniforms,
            atrous_uniforms,
            scene: scene_buffers,
            scene_geom,
            dxr: None,
            dxr_failed: false,
            dlss_rr: None,
            dlss_rr_failed: false,
            blit_pipeline: None,
            blit_bind_group,
            composite_pipeline: None,
            raster_copy: None,
            grid,
            grid_extent,
            render_extent: (w, h),
            trace_bind_group,
            filter_colour,
            filter_moments,
            temporal_bind_groups,
            last_output_plane: 0,
            atrous_bind_groups,
            compose_bind_groups,
            compose_uniforms,
            filter_colour_tex,
            filter_moments_tex,
            temporal_colour,
            temporal_moments,
            _temporal_colour_tex: temporal_colour_tex,
            _temporal_moments_tex: temporal_moments_tex,
            motion,
            _motion_tex: motion_tex,
            depth,
            _depth_tex: depth_tex,
            output,
            output_view,
            guide,
            guide_view,
            frames: 0,
            traced_last_frame: false,
        }
    }

    fn trace_bindings(
        device: &wgpu::Device,
        pipelines: &Pipelines,
        uniforms: &wgpu::Buffer,
        scene: &SceneBuffers,
        grid: &GridTargets,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raywatcher scene"),
            layout: &pipelines.trace_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: scene.triangles.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: scene.nodes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: scene.order.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: scene.materials.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&grid.colour_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&grid.guide_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&grid.reflection_view),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::TextureView(&grid.normals_view),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(&grid.albedo_view),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(&grid.gbuf_pos_view),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::TextureView(&grid.gbuf_norm_view),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::TextureView(&grid.gbuf_raster_view),
                },
            ],
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn temporal_binding(
        device: &wgpu::Device,
        pipelines: &Pipelines,
        uniforms: &wgpu::Buffer,
        grid: &GridTargets,
        motion: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        history_colour: &wgpu::TextureView,
        history_moments: &wgpu::TextureView,
        out_colour: &wgpu::TextureView,
        out_moments: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raywatcher temporal"),
            layout: &pipelines.denoise_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&grid.colour_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&grid.guide_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&grid.reflection_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(motion),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(depth),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(history_colour),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(history_moments),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::TextureView(&grid.normals_view),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(out_colour),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(out_moments),
                },
            ],
        })
    }

    /// One bind group per filter pass, pre-wired to alternate between the two
    /// ping-pong planes. Pass `i` reads post-temporal (i == 0) or pass `i - 1`'s
    /// output, and writes the other plane.
    #[allow(clippy::too_many_arguments)]
    fn atrous_bindings(
        device: &wgpu::Device,
        pipelines: &Pipelines,
        uniforms: &[wgpu::Buffer],
        grid: &GridTargets,
        depth: &wgpu::TextureView,
        motion: &wgpu::TextureView,
        colour: &[wgpu::TextureView; 2],
        moments: &[wgpu::TextureView; 2],
        temporal_colour: &wgpu::TextureView,
        temporal_moments: &wgpu::TextureView,
    ) -> Vec<wgpu::BindGroup> {
        let mut groups = Vec::new();
        for i in 0..MAX_PASSES {
            // Post-temporal output lives in the "temporal" pair; pass 0 reads
            // it and writes plane 0, pass 1 reads plane 0 and writes plane 1,
            // pass 2 reads plane 1 and writes plane 0, and so on.
            let (read_colour, read_moments) = if i == 0 {
                (temporal_colour, temporal_moments)
            } else {
                let index = (i - 1) % 2;
                (&colour[index], &moments[index])
            };
            let write = i % 2;
            groups.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("raywatcher atrous"),
                layout: &pipelines.denoise_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms[i].as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&grid.colour_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&grid.guide_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&grid.reflection_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(motion),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(depth),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(read_colour),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(read_moments),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: wgpu::BindingResource::TextureView(&grid.normals_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 9,
                        resource: wgpu::BindingResource::TextureView(&colour[write]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 10,
                        resource: wgpu::BindingResource::TextureView(&moments[write]),
                    },
                ],
            }));
        }
        groups
    }

    fn compose_binding(
        device: &wgpu::Device,
        pipelines: &Pipelines,
        uniforms: &wgpu::Buffer,
        grid: &GridTargets,
        colour: &wgpu::TextureView,
        moments: &wgpu::TextureView,
        output: &wgpu::TextureView,
        guide: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raywatcher compose"),
            layout: &pipelines.compose_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(colour),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(moments),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&grid.guide_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&grid.reflection_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(output),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(guide),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: uniforms.as_entire_binding(),
                },
            ],
        })
    }

    fn blit_binding(
        device: &wgpu::Device,
        pipelines: &Pipelines,
        output_view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raywatcher blit"),
            layout: &pipelines.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.blit_sampler),
                },
            ],
        })
    }

    /// Resize the render-extent targets. Returns `false` when the device refused
    /// the size, in which case the caller keeps the module disabled rather than
    /// binding stale targets.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        render_extent: (u32, u32),
        grid_extent: (u32, u32),
    ) -> bool {
        let (w, h) = (render_extent.0.max(1), render_extent.1.max(1));
        let grid_extent = (grid_extent.0.max(1), grid_extent.1.max(1));
        if (w, h) == self.render_extent && grid_extent == self.grid_extent {
            return true;
        }
        let grid_changed = grid_extent != self.grid_extent;
        let render_changed = (w, h) != self.render_extent;
        if grid_changed {
            self.grid = GridTargets::new(device, grid_extent.0, grid_extent.1);
            self.grid_extent = grid_extent;
            if let Some(dxr) = self.dxr.as_mut() {
                if let Err(err) = dxr.resize(device, self.grid_extent) {
                    eprintln!("[Raywatcher] DXR resize failed ({err}); falling back to Software");
                    self.dxr = None;
                    self.dxr_failed = true;
                }
            }
        }
        if !render_changed {
            let pipelines = &self.pipelines;
            self.trace_bind_group =
                Self::trace_bindings(device, pipelines, &self.uniforms, &self.scene, &self.grid);
            self.temporal_bind_groups = [
                Self::temporal_binding(
                    device,
                    pipelines,
                    &self.temporal_uniforms,
                    &self.grid,
                    &self.motion,
                    &self.depth,
                    &self.filter_colour[0],
                    &self.filter_moments[0],
                    &self.temporal_colour,
                    &self.temporal_moments,
                ),
                Self::temporal_binding(
                    device,
                    pipelines,
                    &self.temporal_uniforms,
                    &self.grid,
                    &self.motion,
                    &self.depth,
                    &self.filter_colour[1],
                    &self.filter_moments[1],
                    &self.temporal_colour,
                    &self.temporal_moments,
                ),
            ];
            self.atrous_bind_groups = Self::atrous_bindings(
                device,
                pipelines,
                &self.atrous_uniforms,
                &self.grid,
                &self.depth,
                &self.motion,
                &self.filter_colour,
                &self.filter_moments,
                &self.temporal_colour,
                &self.temporal_moments,
            );
            self.compose_bind_groups = [
                Self::compose_binding(
                    device,
                    pipelines,
                    &self.compose_uniforms,
                    &self.grid,
                    &self.filter_colour[0],
                    &self.filter_moments[0],
                    &self.output_view,
                    &self.guide_view,
                ),
                Self::compose_binding(
                    device,
                    pipelines,
                    &self.compose_uniforms,
                    &self.grid,
                    &self.filter_colour[1],
                    &self.filter_moments[1],
                    &self.output_view,
                    &self.guide_view,
                ),
            ];
            if let Some(rr) = self.dlss_rr.as_mut() {
                rr.invalidate_prep();
            }
            self.gbuf_prep_bind_group = None;
            self.frames = 0;
            return true;
        }
        let pipelines = &self.pipelines;
        self.render_extent = (w, h);
        let (filter_colour_tex, filter_colour) = {
            let (a, av) = plane(device, "raywatcher filter colour a", w, h, COLOUR_FORMAT);
            let (b, bv) = plane(device, "raywatcher filter colour b", w, h, COLOUR_FORMAT);
            ([a, b], [av, bv])
        };
        let (filter_moments_tex, filter_moments) = {
            let (a, av) = plane(device, "raywatcher filter moments a", w, h, GUIDE_FORMAT);
            let (b, bv) = plane(device, "raywatcher filter moments b", w, h, GUIDE_FORMAT);
            ([a, b], [av, bv])
        };
        let (temporal_colour_tex, temporal_colour) =
            plane(device, "raywatcher temporal colour", w, h, COLOUR_FORMAT);
        let (temporal_moments_tex, temporal_moments) =
            plane(device, "raywatcher temporal moments", w, h, GUIDE_FORMAT);
        let (motion_tex, motion) = plane(device, "raywatcher motion", w, h, COLOUR_FORMAT);
        let (depth_tex, depth) = depth_plane(device, w, h);
        let (output, output_view) = plane(device, "raywatcher output", w, h, COLOUR_FORMAT);
        let (guide, guide_view) = plane(device, "raywatcher output guide", w, h, GUIDE_FORMAT);
        self.trace_bind_group =
            Self::trace_bindings(device, pipelines, &self.uniforms, &self.scene, &self.grid);
        self.temporal_bind_groups = [
            Self::temporal_binding(
                device,
                pipelines,
                &self.temporal_uniforms,
                &self.grid,
                &motion,
                &depth,
                &filter_colour[0],
                &filter_moments[0],
                &temporal_colour,
                &temporal_moments,
            ),
            Self::temporal_binding(
                device,
                pipelines,
                &self.temporal_uniforms,
                &self.grid,
                &motion,
                &depth,
                &filter_colour[1],
                &filter_moments[1],
                &temporal_colour,
                &temporal_moments,
            ),
        ];
        self.last_output_plane = 0;
        self.atrous_bind_groups = Self::atrous_bindings(
            device,
            pipelines,
            &self.atrous_uniforms,
            &self.grid,
            &depth,
            &motion,
            &filter_colour,
            &filter_moments,
            &temporal_colour,
            &temporal_moments,
        );
        self.compose_bind_groups = [
            Self::compose_binding(
                device,
                pipelines,
                &self.compose_uniforms,
                &self.grid,
                &filter_colour[0],
                &filter_moments[0],
                &output_view,
                &guide_view,
            ),
            Self::compose_binding(
                device,
                pipelines,
                &self.compose_uniforms,
                &self.grid,
                &filter_colour[1],
                &filter_moments[1],
                &output_view,
                &guide_view,
            ),
        ];
        if let Some(rr) = self.dlss_rr.as_mut() {
            if rr.extent() != (w, h) {
                self.dlss_rr = None;
            } else {
                // Grid or depth views were recreated; rebuild cached prep bindings.
                rr.invalidate_prep();
            }
        }
        self.filter_colour = filter_colour;
        self.filter_moments = filter_moments;
        self.filter_colour_tex = filter_colour_tex;
        self.filter_moments_tex = filter_moments_tex;
        self.temporal_colour = temporal_colour;
        self.temporal_moments = temporal_moments;
        self._temporal_colour_tex = temporal_colour_tex;
        self._temporal_moments_tex = temporal_moments_tex;
        self.motion = motion;
        self._motion_tex = motion_tex;
        self.depth = depth;
        self._depth_tex = depth_tex;
        self.blit_bind_group = Self::blit_binding(device, &self.pipelines, &output_view);
        self.raster_copy = None;
        self.gbuf_prep_bind_group = None;
        self.output = output;
        self.output_view = output_view;
        self.guide = guide;
        self.guide_view = guide_view;
        // A resize invalidates every reprojection.
        self.frames = 0;
        true
    }

    /// Replace the scene. Called when the resident terrain set, tree set or
    /// water areas change.
    pub fn set_scene(&mut self, device: &wgpu::Device, scene: Arc<super::bvh::SceneGeometry>) {
        self.scene_geom = scene.clone();
        self.scene = SceneBuffers::upload(device, scene);
        if let Some(dxr) = self.dxr.as_mut() {
            if let Err(err) = dxr.set_scene(&self.scene_geom) {
                eprintln!("[Raywatcher] DXR acceleration structure rebuild failed ({err}); falling back to Software");
                self.dxr = None;
                self.dxr_failed = true;
            }
        }
        self.trace_bind_group = Self::trace_bindings(
            device,
            &self.pipelines,
            &self.uniforms,
            &self.scene,
            &self.grid,
        );
    }

    /// Record one frame's ray pass and denoiser into `encoder`.
    ///
    /// `motion` and `depth` are the scene's own motion vectors and depth, so the
    /// denoiser reprojects with the same data the upscaler will — that is what
    /// keeps the denoised image and the reconstructed image aligned.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        params: &FrameParams,
        plan: &super::plan::Plan,
        hardware_mode: bool,
        transport: &super::shade::TransportConfig,
        config: &DenoiseConfig,
        motion: &wgpu::Texture,
        depth: &wgpu::Texture,
        water: &[super::harvest::WaterSurface],
    ) {
        if !plan.trace {
            self.traced_last_frame = false;
            return;
        }
        // Copy hardware depth FIRST so both the 1:1 G-buffer prep pass and the
        // denoiser/DLSS RR consume the exact raster depth from the world pass.
        let (rw, rh) = self.render_extent;
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: depth,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::ImageCopyTexture {
                texture: &self._depth_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::Extent3d {
                width: rw.min(depth.width()),
                height: rh.min(depth.height()),
                depth_or_array_layers: 1,
            },
        );

        // Build the 1:1 depth-anchored G-buffer (world pos, normal, water
        // roughness, and untouched raster colour) at ray-grid resolution.
        if self.gbuf_prep_bind_group.is_none() {
            let raster_view = self
                .raster_copy
                .as_ref()
                .map(|(_, _, _, v, _)| v)
                .unwrap_or(&self.output_view);
            self.gbuf_prep_bind_group =
                Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("raywatcher gbuf prep"),
                    layout: &self.pipelines.gbuf_prep_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.gbuf_prep_uniforms.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&self.depth),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(raster_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&self.grid.gbuf_pos_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(&self.grid.gbuf_norm_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: wgpu::BindingResource::TextureView(
                                &self.grid.gbuf_raster_view,
                            ),
                        },
                    ],
                }));
        }
        let prep_packed = GbufPrepParams::pack(params, plan.grid, self.render_extent, water);
        queue.write_buffer(
            &self.gbuf_prep_uniforms,
            0,
            bytemuck::bytes_of(&prep_packed),
        );
        if let Some(ref bg) = self.gbuf_prep_bind_group {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("raywatcher gbuf prep"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.gbuf_prep);
            pass.set_bind_group(0, bg, &[]);
            let (gw, gh) = plan.grid;
            pass.dispatch_workgroups(gw.div_ceil(8), gh.div_ceil(8), 1);
        }

        let packed = RayParams::pack(
            params,
            plan,
            transport,
            [
                self.scene.counts.triangles,
                self.scene.counts.nodes,
                self.scene.counts.materials,
            ],
        );

        let skip_inhouse = plan.reconstruction
            == super::plan::RayReconstruction::DlssRayReconstruction
            && !self.dlss_rr_failed;

        let mut traced_hw = false;
        if hardware_mode && !self.dxr_failed {
            if self.dxr.is_none() {
                match super::dxr::DxrTracer::new(device, &self.scene_geom, self.grid_extent) {
                    Ok(tracer) => self.dxr = Some(tracer),
                    Err(err) => {
                        eprintln!(
                            "[Raywatcher] DXR 1.1 init failed ({err}); falling back to Software"
                        );
                        self.dxr_failed = true;
                    }
                }
            }
            if let Some(dxr) = self.dxr.as_mut() {
                // Copy the freshly built G-buffer planes into the shared DX12
                // inputs and submit the wgpu encoder so the world pass +
                // G-buffer prep complete on the GPU before DXR dispatches.
                dxr.copy_gbuf_inputs(
                    encoder,
                    &self.grid.gbuf_pos,
                    &self.grid.gbuf_norm,
                    &self.grid.gbuf_raster,
                );
                let pending = std::mem::replace(
                    encoder,
                    device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("raywatcher post-dxr"),
                    }),
                );
                queue.submit(Some(pending.finish()));
                let dst_guide =
                    (!skip_inhouse || self.dlss_rr.is_none()).then_some(&self.grid.guide);
                let dst_albedo = skip_inhouse.then_some(&self.grid.albedo);
                match dxr.trace(
                    encoder,
                    &packed,
                    plan.grid,
                    &self.grid.colour,
                    dst_guide,
                    &self.grid.reflection,
                    &self.grid.normals,
                    dst_albedo,
                ) {
                    Ok(()) => traced_hw = true,
                    Err(err) => {
                        eprintln!(
                            "[Raywatcher] DXR 1.1 trace failed ({err}); falling back to Software"
                        );
                        self.dxr = None;
                        self.dxr_failed = true;
                    }
                }
            }
        }

        if !traced_hw {
            queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&packed));
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("raywatcher trace"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.trace);
            pass.set_bind_group(0, &self.trace_bind_group, &[]);
            let (w, h) = plan.grid;
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }

        if !skip_inhouse {
            self.record_denoise(queue, encoder, params, config, motion);
        }
        self.frames = self.frames.saturating_add(1);
        self.traced_last_frame = true;
    }

    /// Record the in-house temporal + à-trous + compose denoiser passes.
    pub fn record_denoise(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        params: &FrameParams,
        config: &DenoiseConfig,
        motion: &wgpu::Texture,
    ) {
        let (rw, rh) = self.render_extent;
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: motion,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &self._motion_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: rw.min(motion.width()),
                height: rh.min(motion.height()),
                depth_or_array_layers: 1,
            },
        );
        queue.write_buffer(
            &self.temporal_uniforms,
            0,
            bytemuck::bytes_of(&DenoiseParams::pack(self.render_extent, 0, config, params)),
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("raywatcher temporal"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.temporal);
            pass.set_bind_group(0, &self.temporal_bind_groups[self.last_output_plane], &[]);
            pass.dispatch_workgroups(rw.div_ceil(8), rh.div_ceil(8), 1);
        }
        let iterations = config.iterations.clamp(1, MAX_PASSES as u32);
        for iteration in 0..iterations {
            queue.write_buffer(
                &self.atrous_uniforms[iteration as usize],
                0,
                bytemuck::bytes_of(&DenoiseParams::pack(
                    self.render_extent,
                    iteration,
                    config,
                    params,
                )),
            );
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("raywatcher atrous"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.atrous);
            pass.set_bind_group(0, &self.atrous_bind_groups[iteration as usize], &[]);
            pass.dispatch_workgroups(rw.div_ceil(8), rh.div_ceil(8), 1);
        }
        let final_plane = ((iterations - 1) % 2) as usize;
        self.last_output_plane = final_plane;
        queue.write_buffer(
            &self.compose_uniforms,
            0,
            bytemuck::bytes_of(&[
                rw as f32,
                rh as f32,
                self.grid_extent.0.max(1) as f32,
                self.grid_extent.1.max(1) as f32,
            ]),
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("raywatcher compose"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.compose);
            pass.set_bind_group(0, &self.compose_bind_groups[final_plane], &[]);
            pass.dispatch_workgroups(rw.div_ceil(8), rh.div_ceil(8), 1);
        }
    }

    /// Run NVIDIA DLSS Ray Reconstruction (`nvngx_dlssd.dll`) over the freshly
    /// traced ray grid, overwriting `self.output` with the reconstructed image.
    /// Returns `true` when DLSS RR ran; falls back silently to the in-house
    /// denoiser output already in `self.output` on failure.
    #[allow(clippy::too_many_arguments)]
    pub fn reconstruct_dlss_rr(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        post_encoder: &mut wgpu::CommandEncoder,
        sdk_dir: &std::path::Path,
        params: &FrameParams,
        motion: &wgpu::Texture,
        reset: bool,
        layered: bool,
        world_to_view: [[f32; 4]; 4],
        view_to_clip: [[f32; 4]; 4],
    ) -> bool {
        if self.dlss_rr_failed {
            return false;
        }
        if self.dlss_rr.is_none() {
            match super::reconstruct::DlssRayReconstructor::new(device, sdk_dir, self.render_extent)
            {
                Ok(rr) => self.dlss_rr = Some(rr),
                Err(err) => {
                    eprintln!("[Raywatcher] DLSS Ray Reconstruction init failed ({err}); keeping in-house denoiser");
                    self.dlss_rr_failed = true;
                    return false;
                }
            }
        }
        let Some(rr) = self.dlss_rr.as_mut() else {
            return false;
        };
        let inputs = super::reconstruct::DlssRrInputs {
            grid_colour: &self.grid.colour_view,
            grid_reflection: &self.grid.reflection_view,
            grid_normals: &self.grid.normals_view,
            grid_albedo: &self.grid.albedo_view,
            depth: &self.depth,
            raster: self.raster_copy.as_ref().map(|r| &r.3),
            layered,
            motion,
            jitter: params.jitter,
            reset,
            world_to_view,
            view_to_clip,
        };
        match rr.evaluate(device, queue, post_encoder, &inputs, &self.output) {
            Ok(()) => true,
            Err(err) => {
                eprintln!("[Raywatcher] DLSS Ray Reconstruction evaluate failed ({err}); falling back to in-house denoiser");
                self.dlss_rr = None;
                self.dlss_rr_failed = true;
                false
            }
        }
    }

    /// Snapshot the untouched full raster world target into an internal copy so
    /// Raywatcher can composite shadows, reflections, AO and GI on top of it
    /// (and feed it into DLSS Ray Reconstruction) without overwriting it.
    pub fn capture_raster(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::Texture,
    ) {
        let fmt = source.format();
        let ext = (source.width().max(1), source.height().max(1));
        let needs_alloc = self
            .raster_copy
            .as_ref()
            .is_none_or(|(f, e, _, _, _)| *f != fmt || *e != ext);
        if needs_alloc {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("raywatcher raster snapshot"),
                size: wgpu::Extent3d {
                    width: ext.0,
                    height: ext.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("raywatcher composite"),
                layout: &self.pipelines.composite_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.output_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.pipelines.blit_sampler),
                    },
                ],
            });
            self.raster_copy = Some((fmt, ext, tex, view, bg));
            self.gbuf_prep_bind_group = None;
            if let Some(rr) = self.dlss_rr.as_mut() {
                rr.invalidate_prep();
            }
        }
        if let Some((_, _, ref tex, _, _)) = self.raster_copy {
            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: source,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: ext.0,
                    height: ext.1,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    /// Composite Raywatcher's denoised lighting layer on top of the untouched
    /// raster world render (`layered && !dlss_rr_ran`), or blit `self.output`
    /// directly when DLSS RR already produced the composited image or when the
    /// user explicitly opted into `replace_raster`.
    pub fn composite_to(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        destination: &wgpu::Texture,
        layered: bool,
        dlss_rr_ran: bool,
    ) {
        if !layered || dlss_rr_ran || self.raster_copy.is_none() {
            self.blit_to(device, encoder, destination);
            return;
        }
        let target_format = destination.format();
        if self
            .composite_pipeline
            .as_ref()
            .is_none_or(|(fmt, _)| *fmt != target_format)
        {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("raywatcher composite"),
                bind_group_layouts: &[&self.pipelines.composite_layout],
                push_constant_ranges: &[],
            });
            let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("raywatcher composite"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &self.pipelines.composite_module,
                    entry_point: "vs",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.pipelines.composite_module,
                    entry_point: "fs",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });
            self.composite_pipeline = Some((target_format, pipe));
        }
        let (Some((_, ref pipe)), Some((_, _, _, _, ref bg))) =
            (&self.composite_pipeline, &self.raster_copy)
        else {
            return;
        };
        let dst_view = destination.create_view(&Default::default());
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("raywatcher composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &dst_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        rp.set_pipeline(pipe);
        rp.set_bind_group(0, bg, &[]);
        rp.draw(0..3, 0..1);
    }

    /// Copy or blit `self.output` (`Rgba16Float`) into `destination`, converting
    /// pixel formats cleanly when `destination` uses a swapchain format such as
    /// `Bgra8UnormSrgb`.
    pub fn blit_to(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        destination: &wgpu::Texture,
    ) {
        let (w, h) = (
            destination.width().min(self.output.width()),
            destination.height().min(self.output.height()),
        );
        if w == 0 || h == 0 {
            return;
        }
        if destination.format() == COLOUR_FORMAT {
            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.output,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: destination,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            return;
        }
        let target_format = destination.format();
        if self
            .blit_pipeline
            .as_ref()
            .is_none_or(|(fmt, _)| *fmt != target_format)
        {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("raywatcher blit"),
                bind_group_layouts: &[&self.pipelines.blit_layout],
                push_constant_ranges: &[],
            });
            let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("raywatcher blit"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &self.pipelines.blit_module,
                    entry_point: "vs",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.pipelines.blit_module,
                    entry_point: "fs",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });
            self.blit_pipeline = Some((target_format, pipe));
        }
        let Some((_, ref pipe)) = self.blit_pipeline else {
            return;
        };
        let dst_view = destination.create_view(&Default::default());
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("raywatcher blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &dst_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        rp.set_pipeline(pipe);
        rp.set_bind_group(0, &self.blit_bind_group, &[]);
        rp.draw(0..3, 0..1);
    }

    /// Number of history frames accumulated. Zero after [`Self::reset`].
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Forget the temporal history: a camera cut, a resize, or the module being
    /// switched off and on again.
    pub fn reset(&mut self) {
        self.frames = 0;
    }

    /// The render extent this instance filters into.
    pub fn render_extent(&self) -> (u32, u32) {
        self.render_extent
    }

    /// The ray grid the trace pass runs at.
    pub fn grid_extent(&self) -> (u32, u32) {
        self.grid_extent
    }
}
