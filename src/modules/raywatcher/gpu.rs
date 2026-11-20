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
                0.,
            ],
            fog: [fog.0, fog.1, (params.far * 0.05).clamp(2., 120.), 0.],
            counts: [counts[0] as f32, counts[1] as f32, counts[2] as f32, 0.],
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
    let r = sqrt(next_f32(state));
    let phi = next_f32(state) * 2.0 * PI;
    let x = r * cos(phi);
    let y = r * sin(phi);
    let z = sqrt(max(1.0 - x * x - y * y, 0.0));
    return normalize(orthonormal_basis(n) * vec3<f32>(x, y, z));
}

fn cone_direction(state: ptr<function, u32>, axis: vec3<f32>, radius: f32) -> vec3<f32> {
    let r = sqrt(next_f32(state));
    let phi = next_f32(state) * 2.0 * PI;
    let spread = r * tan(radius);
    return normalize(axis + orthonormal_basis(axis) * vec3<f32>(cos(phi) * spread, sin(phi) * spread, 0.0));
}

fn box_hit(minimum: vec3<f32>, maximum: vec3<f32>, origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32) -> f32 {
    var t0 = t_min;
    var t1 = t_max;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let o = origin[axis];
        let d = dir[axis];
        let lo = minimum[axis];
        let hi = maximum[axis];
        if (abs(d) < 1e-12) {
            if (o < lo || o > hi) {
                return -1.0;
            }
            continue;
        }
        let inv = 1.0 / d;
        var near = (lo - o) * inv;
        var far = (hi - o) * inv;
        if (near > far) {
            let tmp = near;
            near = far;
            far = tmp;
        }
        t0 = max(t0, near);
        t1 = min(t1, far);
        if (t0 > t1) {
            return -1.0;
        }
    }
    return t0;
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
    var stack: array<u32, 64>;
    var sp = 0u;
    stack[0] = 0u;
    sp = 1u;
    loop {
        if (sp == 0u) {
            break;
        }
        sp = sp - 1u;
        let node_index = stack[sp];
        let node = nodes[node_index];
        if (box_hit(node.bounds_min.xyz, node.bounds_max.xyz, origin, dir, t_min, hit.t) < 0.0) {
            continue;
        }
        if (node.bounds_min.w >= 0.0) {
            let count = u32(node.bounds_min.w);
            let first = u32(node.bounds_max.w);
            for (var i = 0u; i < count; i = i + 1u) {
                let tri_index = order[first + i];
                let result = triangle_hit(tris[tri_index], origin, dir, t_min, hit.t);
                if (result.x >= 0.0) {
                    hit.t = result.x;
                    hit.index = tri_index;
                    var geo = normalize(cross(tris[tri_index].b.xyz - tris[tri_index].a.xyz, tris[tri_index].c.xyz - tris[tri_index].a.xyz));
                    hit.normal = select(-geo, geo, dot(geo, dir) <= 0.0);
                }
            }
        } else {
            let left = node_index + 1u;
            let right = u32(node.bounds_max.w);
            let lh = box_hit(nodes[left].bounds_min.xyz, nodes[left].bounds_max.xyz, origin, dir, t_min, hit.t);
            let rh = box_hit(nodes[right].bounds_min.xyz, nodes[right].bounds_max.xyz, origin, dir, t_min, hit.t);
            // Push the far child so the near one pops first.
            var near = 0u;
            var far = 0u;
            var have_near = false;
            var have_far = false;
            if (lh >= 0.0 && rh >= 0.0) {
                if (lh <= rh) {
                    near = left; far = right;
                } else {
                    near = right; far = left;
                }
                have_near = true; have_far = true;
            } else if (lh >= 0.0) {
                near = left; have_near = true;
            } else if (rh >= 0.0) {
                near = right; have_near = true;
            }
            if (have_far && sp < 63u) {
                stack[sp] = far;
                sp = sp + 1u;
            }
            if (have_near && sp < 63u) {
                stack[sp] = near;
                sp = sp + 1u;
            }
        }
    }
    return hit;
}

fn occluded(origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32) -> bool {
    if (params.counts.y < 1.0) {
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
        if (box_hit(node.bounds_min.xyz, node.bounds_max.xyz, origin, dir, t_min, t_max) < 0.0) {
            continue;
        }
        if (node.bounds_min.w >= 0.0) {
            let count = u32(node.bounds_min.w);
            let first = u32(node.bounds_max.w);
            for (var i = 0u; i < count; i = i + 1u) {
                if (triangle_hit(tris[order[first + i]], origin, dir, t_min, t_max).x >= 0.0) {
                    return true;
                }
            }
        } else {
            if (sp + 2u > 64u) {
                continue;
            }
            stack[sp] = node_index + 1u;
            stack[sp + 1u] = u32(node.bounds_max.w);
            sp = sp + 2u;
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
    return materials[u32(tris[index].b.w)];
}

struct Surface {
    colour: vec3<f32>,
    reflection: vec3<f32>,
    visibility: f32,
    depth: f32,
    reflection_distance: f32,
    normal: vec3<f32>,
};

fn shade(origin: vec3<f32>, dir: vec3<f32>, state: ptr<function, u32>) -> Surface {
    var surface: Surface;
    surface.colour = vec3<f32>(0.0);
    surface.reflection = vec3<f32>(0.0);
    surface.visibility = 1.0;
    surface.reflection_distance = params.lens.y;
    surface.depth = params.lens.y;
    surface.normal = vec3<f32>(0.0);
    let hit = closest_hit(origin, dir, params.effects.z, params.lens.y);
    if (hit.index == 0xffffffffu) {
        let fog_start = params.fog.x;
        let fog_end = params.fog.y;
        if (fog_end > fog_start && fog_start >= 0.0) {
            surface.colour = params.fog_colour.xyz;
        } else {
            surface.colour = environment();
        }
        surface.depth = params.lens.y;
        return surface;
    }
    let point = origin + dir * hit.t;
    let material = material_of(hit.index);
    let albedo = material.albedo.rgb;
    let roughness = material.surface.x;
    surface.depth = hit.t;
    surface.normal = hit.normal;

    let effects = u32(round(params.effects.y));
    let bits_shadows = (effects & 1u) != 0u;
    let bits_lighting = (effects & 2u) != 0u;
    let bits_reflections = (effects & 4u) != 0u;
    let bits_ao = (effects & 8u) != 0u;
    let bits_gi = (effects & 16u) != 0u;
    let bias = params.effects.z;

    // Occlusion: a hemisphere fan, exactly the CPU reference's `ao_samples`
    // count, so the two images match.
    var open = 1.0;
    if (bits_ao) {
        var hits = 0u;
        let ao_samples = 8u;
        for (var i = 0u; i < ao_samples; i = i + 1u) {
            let d = cosine_hemisphere(state, hit.normal);
            if (occluded(point + hit.normal * bias, d, bias, params.fog.z)) {
                hits = hits + 1u;
            }
        }
        open = 1.0 - f32(hits) / f32(ao_samples);
    }
    surface.visibility = open;

    // Sun, with a shadow ray per tap. The shadow ray is the only place
    // `bits_shadows` matters: a lighting-only frame still gets the sun term.
    let n_dot_l = max(dot(hit.normal, params.sun_dir.xyz), 0.0);
    var direct = vec3<f32>(0.0);
    if (bits_lighting) {
        var visible = 0u;
        let shadow_samples = 1u;
        for (var i = 0u; i < shadow_samples; i = i + 1u) {
            if (!occluded(point + hit.normal * bias, params.sun_dir.xyz, bias, params.lens.y)) {
                visible = visible + 1u;
            }
        }
        var shadow = f32(visible) / f32(shadow_samples);
        if (!bits_shadows) {
            shadow = 1.0;
        }
        direct = params.sun_colour.xyz * (n_dot_l * params.lens.w * shadow);
    }
    let ambient = params.ambient.xyz * (open * params.tuning.z);
    var diffuse = direct + ambient * albedo;
    if (!bits_lighting) {
        diffuse = ambient * albedo;
    }

    if (bits_reflections && roughness <= params.effects.x) {
        let reflect_dir = normalize(dir - hit.normal * (2.0 * dot(dir, hit.normal)));
        let rhit = closest_hit(point + hit.normal * bias, reflect_dir, bias, params.lens.y);
        if (rhit.index != 0xffffffffu) {
            let rmaterial = material_of(rhit.index);
            let lit = rmaterial.albedo.rgb * (params.ambient.xyz * params.tuning.z + params.sun_colour.xyz * (max(dot(rhit.normal, params.sun_dir.xyz), 0.0) * params.lens.w));
            let f0 = 0.04 + 0.06 * (1.0 - roughness);
            let cos_theta = clamp(dot(-dir, hit.normal), 0.0, 1.0);
            let fresnel = f0 + (1.0 - f0) * pow(1.0 - cos_theta, 5.0);
            surface.reflection = lit * fresnel;
            surface.reflection_distance = rhit.t;
        } else {
            surface.reflection = environment() * 0.08;
            surface.reflection_distance = params.lens.y;
        }
    }

    if (bits_gi && params.frame.z >= 1.0 && params.tuning.w > 0.0) {
        var bounce = vec3<f32>(0.0);
        var throughput = albedo;
        var source = point;
        var normal = hit.normal;
        let bounces = u32(params.frame.z);
        for (var b = 0u; b < bounces; b = b + 1u) {
            let d = cosine_hemisphere(state, normal);
            let bhit = closest_hit(source + normal * bias, d, bias, params.lens.y);
            if (bhit.index != 0xffffffffu) {
                let bpoint = source + d * bhit.t;
                let bmaterial = material_of(bhit.index);
                let incoming = bmaterial.albedo.rgb * (params.ambient.xyz * params.tuning.z + params.sun_colour.xyz * (max(dot(bhit.normal, params.sun_dir.xyz), 0.0) * params.lens.w));
                bounce = bounce + throughput * incoming;
                throughput = throughput * bmaterial.albedo.rgb;
                source = bpoint;
                normal = bhit.normal;
            } else {
                bounce = bounce + throughput * environment();
                break;
            }
        }
        diffuse = diffuse + bounce * params.tuning.w;
    }
    surface.colour = diffuse;
    return surface;
}

@compute @workgroup_size(8, 8, 1)
fn trace_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let extent = vec2<u32>(u32(params.extents.x), u32(params.extents.y));
    if (id.x >= extent.x || id.y >= extent.y) {
        return;
    }
    let grid = vec2<f32>(params.extents.xy);
    let samples = max(u32(params.frame.y), 1u);
    var accum = vec3<f32>(0.0);
    var accum_reflection = vec3<f32>(0.0);
    var accum_visibility = 0.0;
    var accum_depth = 0.0;
    var accum_reflection_distance = 0.0;
    var state = id.x * 0x9e3779b9u ^ id.y * 0x85ebca6bu ^ u32(params.frame.x) * 0xc2b2ae35u;
    for (var s = 0u; s < samples; s = s + 1u) {
        // Jitter rotates across frames; each sample sits at a stratified offset
        // inside the pixel, so a 4-sample frame is a 2x2 grid.
        var offset = vec2<f32>(0.0, 0.0);
        if (samples > 1u) {
            let n = f32(samples);
            offset = vec2<f32>((f32(s) + 0.5) / n - 0.5, (f32(s % 2u) + 0.5) / 2.0);
            offset = offset * 0.4;
        }
        let pixel = vec2<f32>(f32(id.x) + 0.5, f32(id.y) + 0.5) + offset + params.tuning.xy;
        let ndc = vec2<f32>(pixel.x / grid.x * 2.0 - 1.0, 1.0 - pixel.y / grid.y * 2.0);
        let near = params.inv_vp * vec4<f32>(ndc, 1.0, 1.0);
        let far = params.inv_vp * vec4<f32>(ndc, 0.0, 1.0);
        let dir = normalize(far.xyz / far.w - near.xyz / near.w);
        let surface = shade(params.eye.xyz, dir, &state);
        accum = accum + surface.colour;
        accum_reflection = accum_reflection + surface.reflection;
        accum_visibility = accum_visibility + surface.visibility;
        accum_depth = accum_depth + surface.depth;
        accum_reflection_distance = accum_reflection_distance + surface.reflection_distance;
    }
    let inv = 1.0 / f32(samples);
    textureStore(out_colour, vec2<i32>(id.xy), vec4<f32>(accum * inv, 1.0));
    textureStore(out_guide, vec2<i32>(id.xy), vec4<f32>(accum_visibility * inv, (accum_depth * inv) / params.lens.y, 0.0, 0.0));
    textureStore(out_reflection, vec2<i32>(id.xy), vec4<f32>(accum_reflection * inv, (accum_reflection_distance * inv) / params.lens.y));
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
@group(0) @binding(5) var src_depth: texture_2d<f32>;
// History: accumulated colour and moments. The moments carry `depth` in `.z`
// and the history length in `.w`, so the temporal and atrous passes need only
// one history pair each.
@group(0) @binding(6) var history_colour: texture_2d<f32>;
@group(0) @binding(7) var history_moments: texture_2d<f32>;
// Destinations.
@group(0) @binding(9) var out_colour: texture_storage_2d<rgba16float, write>;
@group(0) @binding(10) var out_moments: texture_storage_2d<rgba16float, write>;

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Reproject with the motion vector and validate the depth. Returns the source
// pixel and a flag; the caller resets on a mismatch.
fn reproject(pixel: vec2<i32>, size: vec2<i32>) -> vec3<f32> {
    let motion = textureLoad(src_motion, pixel, 0).xy;
    let source = vec2<f32>(pixel) + vec2<f32>(0.5) - motion;
    if (source.x < 0.0 || source.y < 0.0 || source.x >= f32(size.x) || source.y >= f32(size.y)) {
        return vec3<f32>(0.0, 0.0, 0.0);
    }
    let index = vec2<i32>(source);
    let previous_depth = textureLoad(history_moments, index, 0).w;
    let current_depth = textureLoad(src_depth, pixel, 0).r;
    let both = previous_depth > 0.0 && current_depth > 0.0;
    if (both) {
        if (abs(previous_depth - current_depth) / max(current_depth, 1e-3) > 0.1) {
            return vec3<f32>(0.0, 0.0, 0.0);
        }
    } else if ((previous_depth > 0.0) != (current_depth > 0.0)) {
        return vec3<f32>(0.0, 0.0, 0.0);
    }
    let colour = textureLoad(history_colour, index, 0).rgb;
    let length = textureLoad(history_moments, index, 0).w;
    return vec3<f32>(colour.r, colour.g, length);
}

@compute @workgroup_size(8, 8, 1)
fn temporal_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<i32>(i32(params.extent.x), i32(params.extent.y));
    let pixel = vec2<i32>(i32(id.x), i32(id.y));
    if (id.x >= u32(size.x) || id.y >= u32(size.y)) {
        return;
    }
    let current = textureLoad(src_colour, pixel, 0).rgb + textureLoad(src_reflection, pixel, 0).rgb;
    // Local 3x3 mean and standard deviation, the clamp box.
    var sum = vec3<f32>(0.0);
    var sum_sq = vec3<f32>(0.0);
    var count = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let p = pixel + vec2<i32>(dx, dy);
            if (p.x < 0 || p.y < 0 || p.x >= size.x || p.y >= size.y) {
                continue;
            }
            let c = textureLoad(src_colour, p, 0).rgb + textureLoad(src_reflection, p, 0).rgb;
            sum = sum + c;
            sum_sq = sum_sq + c * c;
            count = count + 1.0;
        }
    }
    var mean = vec3<f32>(0.0);
    var stddev = vec3<f32>(0.0);
    if (count > 0.0) {
        mean = sum / count;
        stddev = sqrt(max(sum_sq / count - mean * mean, vec3<f32>(0.0))) * params.weights.w;
    }
    let previous = reproject(pixel, size);
    let history = clamp(previous.xyz, mean - stddev, mean + stddev);
    let n = min(previous.z, params.misc.z);
    let valid = params.misc.w > 0.5 && n > 0.0;
    let alpha = select(1.0, max(1.0 / (n + 1.0), params.extent.w), valid);
    let blended = mix(history, current, alpha);
    let length = min(n + 1.0, params.misc.z);
    let lum = luminance(blended);
    // The moments are stored in `out_moments.x`/`.y`; `z` carries the current
    // depth so the next frame's reprojection can validate against it.
    let previous_moments = textureLoad(history_moments, pixel, 0);
    var m1 = lum;
    var m2 = lum * lum;
    if (valid) {
        m1 = mix(previous_moments.x, lum, alpha);
        m2 = mix(previous_moments.y, lum * lum, alpha);
    }
    let depth = textureLoad(src_depth, pixel, 0).r;
    textureStore(out_colour, pixel, vec4<f32>(blended, 1.0));
    textureStore(out_moments, pixel, vec4<f32>(m1, m2, depth, length));
}

fn variance_at(pixel: vec2<i32>) -> f32 {
    let moments = textureLoad(history_moments, pixel, 0);
    // The history length rides in the moments' alpha so the atrous pass needs
    // one history texture instead of two.
    let length = moments.w;
    let variance = max(moments.y - moments.x * moments.x, 0.0);
    let bias = select(1.0, 1.0 / max(sqrt(length), 0.25), length > 0.0);
    return max(variance * bias, params.misc.y);
}

fn normal_at(pixel: vec2<i32>) -> vec3<f32> {
    return normalize(textureLoad(src_guide, pixel, 0).xyz * 2.0 - vec3<f32>(1.0));
}

fn depth_at(pixel: vec2<i32>) -> f32 {
    return textureLoad(src_guide, pixel, 0).y;
}

fn edge_weight(centre: vec2<i32>, sample: vec2<i32>, centre_depth: f32) -> f32 {
    let n0 = normal_at(centre);
    let n1 = normal_at(sample);
    let wn = pow(max(dot(n0, n1), 0.0), params.weights.y);
    let d1 = depth_at(sample);
    let wd = exp(-abs(centre_depth - d1) / max(params.weights.z * max(abs(centre_depth), 1e-3), 1e-6));
    return wn * wd;
}

@compute @workgroup_size(8, 8, 1)
fn atrous_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<i32>(i32(params.extent.x), i32(params.extent.y));
    let pixel = vec2<i32>(i32(id.x), i32(id.y));
    if (id.x >= u32(size.x) || id.y >= u32(size.y)) {
        return;
    }
    let centre_depth = depth_at(pixel);
    let centre_variance = variance_at(pixel);
    let sigma = max(params.weights.x * sqrt(centre_variance), 1e-6);
    let centre = textureLoad(history_colour, pixel, 0).rgb;
    let centre_lum = luminance(centre);
    var sum = vec3<f32>(0.0);
    var weight_sum = 0.0;
    var moment1 = 0.0;
    var moment2 = 0.0;
    let step = i32(params.misc.x);
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let p = pixel + vec2<i32>(dx * step, dy * step);
            if (p.x < 0 || p.y < 0 || p.x >= size.x || p.y >= size.y) {
                continue;
            }
            let sample = textureLoad(history_colour, p, 0).rgb;
            let w_edge = edge_weight(pixel, p, centre_depth);
            let w_colour = exp(-abs(centre_lum - luminance(sample)) / sigma);
            let w = w_edge * w_colour;
            if (w <= 0.0) {
                continue;
            }
            sum = sum + sample * w;
            weight_sum = weight_sum + w;
            let moments = textureLoad(history_moments, p, 0);
            moment1 = moment1 + moments.x * w;
            moment2 = moment2 + moments.y * w;
        }
    }
    let centre_moments = textureLoad(history_moments, pixel, 0);
    if (weight_sum > 1e-8) {
        textureStore(out_colour, pixel, vec4<f32>(sum / weight_sum, 1.0));
        textureStore(out_moments, pixel, vec4<f32>(moment1 / weight_sum, moment2 / weight_sum, centre_moments.z, centre_moments.w));
    } else {
        textureStore(out_colour, pixel, vec4<f32>(centre, 1.0));
        textureStore(out_moments, pixel, centre_moments);
    }
}
"#;

/// The compose shader: bilinearly resamples the filtered ray image from the ray
/// grid up to the render extent (which the temporal upscaler then reconstructs
/// to the display), and writes the guide planes the ray reconstruction
/// providers and the debug view read.
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
    // Map the render-extent pixel into the ray grid's texel space. The ray
    // grid is the render extent divided by a power of two, so the centre of a
    // render pixel lands between texels exactly the way a box downsample would.
    let source = (vec2<f32>(pixel) + vec2<f32>(0.5)) / extent.zw * vec2<f32>(textureDimensions(colour));
    let coord = vec2<i32>(clamp(source, vec2<f32>(0.5), vec2<f32>(textureDimensions(colour)) - vec2<f32>(0.5)));
    let c = textureLoad(colour, coord, 0).rgb;
    let m = textureLoad(moments, coord, 0);
    let g = textureLoad(guide, coord, 0);
    let r = textureLoad(reflection, coord, 0);
    textureStore(out_colour, pixel, vec4<f32>(c, 1.0));
    // x: scene depth (0 for sky), y: sky visibility, z: reflection hit
    // distance, w: variance. The ray reconstruction providers want exactly the
    // first three; the debug view reads the fourth.
    let variance = max(m.y - m.x * m.x, 0.0);
    textureStore(out_guide, pixel, vec4<f32>(m.z, g.x, r.w, variance));
}
"#;

/// Intermediate formats. Half-float keeps the guide in a range where
/// normal-space encoding stays precise, and matches what the upscalers accept.
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
    trace: wgpu::ComputePipeline,
    temporal: wgpu::ComputePipeline,
    atrous: wgpu::ComputePipeline,
    trace_layout: wgpu::BindGroupLayout,
    denoise_layout: wgpu::BindGroupLayout,
    compose: wgpu::ComputePipeline,
    compose_layout: wgpu::BindGroupLayout,
}

impl Pipelines {
    fn new(device: &wgpu::Device) -> Self {
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
                texture_entry(5, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(6, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(7, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(8, wgpu::TextureSampleType::Float { filterable: true }),
                storage_texture_entry(9, wgpu::StorageTextureAccess::WriteOnly, COLOUR_FORMAT),
                storage_texture_entry(10, wgpu::StorageTextureAccess::WriteOnly, GUIDE_FORMAT),
                storage_texture_entry(11, wgpu::StorageTextureAccess::WriteOnly, GUIDE_FORMAT),
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
            compilation_options: Default::default(),
            cache: None,
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
            compilation_options: Default::default(),
            cache: None,
        });
        let temporal = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("raywatcher temporal"),
            layout: Some(&denoise_pipeline),
            module: &denoise_module,
            entry_point: "temporal_pass",
            compilation_options: Default::default(),
            cache: None,
        });
        let atrous = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("raywatcher atrous"),
            layout: Some(&denoise_pipeline),
            module: &denoise_module,
            entry_point: "atrous_pass",
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            trace,
            temporal,
            atrous,
            trace_layout,
            denoise_layout,
            compose,
            compose_layout,
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
        let triangles = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher triangles"),
            contents: bytemuck::cast_slice(&scene.triangles),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let nodes = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher bvh"),
            contents: bytemuck::cast_slice(&scene.nodes),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let order = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher order"),
            contents: bytemuck::cast_slice(&scene.order),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let materials = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher materials"),
            contents: bytemuck::cast_slice(&scene.materials),
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
    colour: wgpu::Texture,
    guide: wgpu::Texture,
    reflection: wgpu::Texture,
    colour_view: wgpu::TextureView,
    guide_view: wgpu::TextureView,
    reflection_view: wgpu::TextureView,
}

impl GridTargets {
    fn new(device: &wgpu::Device, w: u32, h: u32) -> Self {
        let colour = storage_texture(device, "raywatcher rays", w, h, COLOUR_FORMAT);
        let guide = storage_texture(device, "raywatcher guide", w, h, GUIDE_FORMAT);
        let reflection = storage_texture(device, "raywatcher reflection", w, h, COLOUR_FORMAT);
        let colour_view = colour.create_view(&Default::default());
        let guide_view = guide.create_view(&Default::default());
        let reflection_view = reflection.create_view(&Default::default());
        Self {
            colour,
            guide,
            reflection,
            colour_view,
            guide_view,
            reflection_view,
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

/// Everything Raywatcher owns on the GPU.
pub struct RaywatcherGpu {
    pipelines: Pipelines,
    uniforms: wgpu::Buffer,
    denoise_uniforms: wgpu::Buffer,
    scene: SceneBuffers,
    grid: GridTargets,
    grid_extent: (u32, u32),
    render_extent: (u32, u32),
    trace_bind_group: wgpu::BindGroup,
    // Ping-pong planes: index 0 and 1 alternate as the filter advances.
    filter_colour: [wgpu::TextureView; 2],
    filter_moments: [wgpu::TextureView; 2],
    temporal_bind_group: wgpu::BindGroup,
    atrous_bind_groups: Vec<wgpu::BindGroup>,
    compose_bind_group: wgpu::BindGroup,
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
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raywatcher params"),
            size: std::mem::size_of::<RayParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let denoise_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raywatcher denoise params"),
            size: std::mem::size_of::<DenoiseParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
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
        let (depth_tex, depth) = plane(
            device,
            "raywatcher depth",
            w,
            h,
            wgpu::TextureFormat::R32Float,
        );
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
        let temporal_bind_group = Self::temporal_binding(
            device,
            &pipelines,
            &denoise_uniforms,
            &grid,
            &motion,
            &depth,
            &filter_colour[0],
            &filter_moments[0],
            &temporal_colour,
            &temporal_moments,
        );
        let atrous_bind_groups = Self::atrous_bindings(
            device,
            &pipelines,
            &denoise_uniforms,
            &grid,
            &depth,
            &motion,
            &filter_colour,
            &filter_moments,
            &temporal_colour,
            &temporal_moments,
        );
        let compose_bind_group = Self::compose_binding(
            device,
            &pipelines,
            &compose_uniforms,
            &grid,
            &filter_colour[0],
            &filter_moments[0],
            &output_view,
            &guide_view,
        );
        Self {
            pipelines,
            uniforms,
            denoise_uniforms,
            scene: scene_buffers,
            grid,
            grid_extent,
            render_extent: (w, h),
            trace_bind_group,
            filter_colour,
            filter_moments,
            temporal_bind_group,
            atrous_bind_groups,
            compose_bind_group,
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
        uniforms: &wgpu::Buffer,
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
                        resource: wgpu::BindingResource::TextureView(read_colour),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(read_moments),
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
        if (w, h) == self.render_extent && grid_extent == self.grid_extent {
            return true;
        }
        let pipelines = &self.pipelines;
        if grid_extent != self.grid_extent {
            self.grid = GridTargets::new(device, grid_extent.0.max(1), grid_extent.1.max(1));
            self.grid_extent = grid_extent;
        }
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
        let (depth_tex, depth) = plane(
            device,
            "raywatcher depth",
            w,
            h,
            wgpu::TextureFormat::R32Float,
        );
        let (output, output_view) = plane(device, "raywatcher output", w, h, COLOUR_FORMAT);
        let (guide, guide_view) = plane(device, "raywatcher output guide", w, h, GUIDE_FORMAT);
        self.trace_bind_group =
            Self::trace_bindings(device, pipelines, &self.uniforms, &self.scene, &self.grid);
        self.temporal_bind_group = Self::temporal_binding(
            device,
            pipelines,
            &self.denoise_uniforms,
            &self.grid,
            &motion,
            &depth,
            &filter_colour[0],
            &filter_moments[0],
            &temporal_colour,
            &temporal_moments,
        );
        self.atrous_bind_groups = Self::atrous_bindings(
            device,
            pipelines,
            &self.denoise_uniforms,
            &self.grid,
            &depth,
            &motion,
            &filter_colour,
            &filter_moments,
            &temporal_colour,
            &temporal_moments,
        );
        self.compose_bind_group = Self::compose_binding(
            device,
            pipelines,
            &self.compose_uniforms,
            &self.grid,
            &filter_colour[0],
            &filter_moments[0],
            &output_view,
            &guide_view,
        );
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
        self.scene = SceneBuffers::upload(device, scene);
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
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        params: &FrameParams,
        plan: &super::plan::Plan,
        transport: &super::shade::TransportConfig,
        config: &DenoiseConfig,
        motion: &wgpu::Texture,
        depth: &wgpu::Texture,
    ) {
        if !plan.trace {
            self.traced_last_frame = false;
            return;
        }
        queue.write_buffer(
            &self.uniforms,
            0,
            bytemuck::bytes_of(&RayParams::pack(
                params,
                plan,
                transport,
                [
                    self.scene.counts.triangles,
                    self.scene.counts.nodes,
                    self.scene.counts.materials,
                ],
            )),
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("raywatcher trace"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.trace);
            pass.set_bind_group(0, &self.trace_bind_group, &[]);
            let (w, h) = plan.grid;
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }
        // The denoiser's guide inputs are the scene's own motion and depth.
        // They are copied rather than aliased because the filter samples them at
        // arbitrary reprojected positions.
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
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: rw.min(depth.width()),
                height: rh.min(depth.height()),
                depth_or_array_layers: 1,
            },
        );
        // Temporal accumulation.
        queue.write_buffer(
            &self.denoise_uniforms,
            0,
            bytemuck::bytes_of(&DenoiseParams::pack(self.render_extent, 0, config, params)),
        );
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("raywatcher temporal"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.temporal);
            pass.set_bind_group(0, &self.temporal_bind_group, &[]);
            pass.dispatch_workgroups(rw.div_ceil(8), rh.div_ceil(8), 1);
        }
        // One pass per iteration; each pass's uniform carries its own tap step.
        let iterations = config.iterations.clamp(1, MAX_PASSES as u32);
        for iteration in 0..iterations {
            queue.write_buffer(
                &self.denoise_uniforms,
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
        // Resample to the render extent.
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
            pass.set_bind_group(0, &self.compose_bind_group, &[]);
            pass.dispatch_workgroups(rw.div_ceil(8), rh.div_ceil(8), 1);
        }
        self.frames = self.frames.saturating_add(1);
        self.traced_last_frame = true;
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
