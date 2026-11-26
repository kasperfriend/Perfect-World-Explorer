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
    /// `x` triangle count, `y` node count, `z` material count, `w` debug view mode.
    pub counts: [f32; 4],
    /// `x` active light count (<= 128), `y` point_light_shadows, `z` write_albedo, `w` write_guide.
    pub point_light_meta: [f32; 4],
    /// Up to 128 nearest scene point + world-effect lights: `xyz` world position, `w` radius.
    pub point_lights_pos_rad: [[f32; 4]; 128],
    /// Matching light colours: `rgb` colour, `w` intensity.
    pub point_lights_col_int: [[f32; 4]; 128],
    /// Top-down real textured terrain albedo map bounds: `[min_x, max_z, inv_diameter, valid]`.
    pub terrain_map_bounds: [f32; 4],
    /// Previous frame's view-projection matrix, used to reproject GI reservoirs.
    pub prev_view_projection: [[f32; 4]; 4],
    /// x=ReSTIR GI enabled, y=max adaptive samples, z=spatial reuse count, w=history ping-pong parity.
    pub restir: [f32; 4],
}

/// ReSTIR history is only reusable when the host explicitly says this frame
/// continues the same camera history. Checking only that a previous VP exists
/// lets reservoirs survive camera cuts even though the denoiser correctly resets.
fn restir_history_valid_for_frame(
    frame_history_valid: u32,
    frames: u32,
    has_previous_view_projection: bool,
) -> bool {
    frame_history_valid > 0 && frames > 0 && has_previous_view_projection
}

/// Maximum active point lights + luminous world effects packed for ReSTIR DI.
pub const MAX_RAY_LIGHTS: usize = 128;

/// Legacy cache allocation retained for descriptor compatibility (65,536 16-byte cells = 1 MiB); shader reads/writes are disabled because hash slots are untagged.
pub const RADIANCE_CACHE_CELLS: usize = 65_536;

/// Uniform per-frame scene-illumination albedo recovery gain (`1 / max(scene_illum, 0.08)`).
/// Preserves true dark stone/wood albedo by day while allowing night point lights to
/// reveal crisp surface textures without ever turning dark walls chalky grey.
pub fn compute_scene_albedo_gain(
    sun_colour: glam::Vec3,
    sun_intensity: f32,
    ambient: glam::Vec3,
    ambient_strength: f32,
) -> f32 {
    let luma_w = glam::Vec3::new(0.2126, 0.7152, 0.0722);
    let sun_luma = (sun_colour * sun_intensity).dot(luma_w).max(0.0);
    let amb_luma = (ambient * ambient_strength).dot(luma_w).max(0.0);
    let scene_illum = (sun_luma * 0.65 + amb_luma).max(0.08);
    let raw_gain = (1.0 / scene_illum).clamp(1.0, 9.0);
    // Keep daytime albedo gain at 1.0 so layered GI never washes daytime surfaces white,
    // while preserving full night texture recovery under point lights.
    let day_t = ((sun_luma - 0.18) / 0.22).clamp(0.0, 1.0);
    raw_gain * (1.0 - day_t) + 1.0 * day_t
}

impl RayParams {
    /// Effect bits, mirroring [`super::plan::Effects`]:
    /// 1 shadows, 2 lighting, 4 reflections, 8 AO, 16 GI
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
        if effects.contact_shadows {
            bits |= 256;
        }
        if effects.moonlit_night {
            bits |= 512;
        }
        if effects.twilight_sky {
            bits |= 4096;
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
        point_lights: &[super::frame::PointLight],
    ) -> Self {
        let fog = config.lighting.fog_range;
        let mut pl_pos_rad = [[0.0f32; 4]; MAX_RAY_LIGHTS];
        let mut pl_col_int = [[0.0f32; 4]; MAX_RAY_LIGHTS];
        // A light can affect a visible receiver only when its influence sphere intersects
        // the camera's far-distance sphere. This conservative test removes no contributing light.
        let far = params.far.max(0.0);
        let mut pl_count = 0usize;
        for l in point_lights {
            if pl_count == MAX_RAY_LIGHTS {
                break;
            }
            if !l.position.is_finite() || !l.radius.is_finite() {
                continue;
            }
            let radius = l.radius.max(0.0);
            let reach = far + radius;
            if (l.position - params.eye).length_squared() > reach * reach {
                continue;
            }
            pl_pos_rad[pl_count] = [l.position.x, l.position.y, l.position.z, radius];
            pl_col_int[pl_count] = [l.colour.x, l.colour.y, l.colour.z, l.intensity.max(0.0)];
            pl_count += 1;
        }
        Self {
            inv_view_projection: params.inv_view_projection.to_cols_array_2d(),
            view_projection: params.view_projection.to_cols_array_2d(),
            eye: params.eye.extend(-1.0e6).to_array(),
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
            point_light_meta: [
                pl_count as f32,
                if plan.point_light_shadows { 1.0 } else { 0.0 },
                1.0,
                1.0,
            ],
            point_lights_pos_rad: pl_pos_rad,
            point_lights_col_int: pl_col_int,
            terrain_map_bounds: [0.0; 4],
            prev_view_projection: [[0.0; 4]; 4],
            restir: [
                if plan.effects.global_illumination {
                    1.0
                } else {
                    0.0
                },
                4.0,
                4.0,
                0.0,
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
    /// Inverse of the current frame's view-projection matrix (for world-space
    /// camera reprojection when `object_mv` has zero coverage on static world).
    pub inv_view_projection: [[f32; 4]; 4],
    /// Previous frame's view-projection matrix for static camera reprojection.
    pub prev_view_projection: [[f32; 4]; 4],
}

impl DenoiseParams {
    pub fn pack(
        extent: (u32, u32),
        iteration: u32,
        config: &DenoiseConfig,
        params: &FrameParams,
        unjittered_vp: glam::Mat4,
        prev_view_projection: Option<glam::Mat4>,
        has_object_mv: bool,
    ) -> Self {
        let history_valid = params.history_valid > 0 && prev_view_projection.is_some();
        let prev_vp = prev_view_projection.unwrap_or(unjittered_vp);
        let unjittered_inv = unjittered_vp.inverse();
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
                if history_valid {
                    if has_object_mv {
                        1.
                    } else {
                        2.
                    }
                } else {
                    0.
                },
            ],
            frame: [params.jitter.0, params.jitter.1, params.frame as f32, 0.],
            inv_view_projection: unjittered_inv.to_cols_array_2d(),
            prev_view_projection: (prev_vp * unjittered_inv).to_cols_array_2d(),
        }
    }
}

/// ReSTIR GI history record. Keep this 4-vec4 / 64-byte layout identical in WGSL and DXR HLSL.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RestirReservoir {
    /// xyz = sampled secondary position (or sky direction), w = accumulated importance weight.
    pub sample_pos_weight: [f32; 4],
    /// rgb = selected multi-bounce radiance, w = represented candidate count (M).
    pub radiance_m: [f32; 4],
    /// xyz = previous receiver position, w = selected target / proposal-PDF at that receiver.
    pub receiver_pos_target: [f32; 4],
    /// xyz = receiver normal, w = sign encodes sky sample (<0) vs surface sample (>0).
    pub receiver_normal_flags: [f32; 4],
}

fn create_restir_reservoir_buffer(
    device: &wgpu::Device,
    extent: (u32, u32),
) -> (wgpu::Buffer, bool) {
    let pixels = u64::from(extent.0.max(1)) * u64::from(extent.1.max(1));
    let required = pixels
        .saturating_mul(2)
        .saturating_mul(std::mem::size_of::<RestirReservoir>() as u64);
    let limits = device.limits();
    let enabled = required <= u64::from(limits.max_storage_buffer_binding_size)
        && required <= limits.max_buffer_size;
    // Keep a harmless non-zero descriptor bound when this adapter cannot fit
    // the full two-frame history. The shader guards all reservoir accesses.
    let size = if enabled { required } else { 64 };
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("raywatcher ReSTIR GI reservoirs"),
        size: size.max(64),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (buffer, enabled)
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

/// CPU mirror of [`GBUF_PREP_SHADER`]'s plane-and-height water classification
/// (`0.06` on native water, `0.92` on matte land).
pub fn classify_surface_roughness(
    normal: glam::Vec3,
    world_pos: glam::Vec3,
    water: &[super::harvest::WaterSurface],
) -> f32 {
    if water.is_empty() || normal.y.abs() <= 0.985 {
        return 0.92;
    }
    for w in water.iter().take(16) {
        let [x0, x1, z0, z1] = w.bounds;
        if world_pos.x >= x0
            && world_pos.x <= x1
            && world_pos.z >= z0
            && world_pos.z <= z1
            && (world_pos.y - w.height).abs() < 0.15
        {
            return 0.06;
        }
    }
    0.92
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
    // Boost #17: Reuse `dist` to compute `view_dir` (eliminates 1 sqrt per pixel).
    let to_p = p - prep.eye.xyz;
    let dist = length(to_p);
    let view_dir = to_p / max(dist, 1e-6);

    let px_l = clamp(px + vec2<i32>(-1, 0), vec2<i32>(0), rmax);
    let px_r = clamp(px + vec2<i32>(1, 0), vec2<i32>(0), rmax);
    let px_u = clamp(px + vec2<i32>(0, -1), vec2<i32>(0), rmax);
    let px_d = clamp(px + vec2<i32>(0, 1), vec2<i32>(0), rmax);

    let z_l = textureLoad(src_depth, px_l, 0);
    let z_r = textureLoad(src_depth, px_r, 0);
    let z_u = textureLoad(src_depth, px_u, 0);
    let z_d = textureLoad(src_depth, px_d, 0);

    // Phase 8: Relative depth-jump guard prevents thin geometry (fences,
    // branches, grass blades) from differentiating across background depth
    // discontinuities and producing grazing/inverted normals.
    let max_rel_dz = 0.18;
    let inv_z = 1.0 / max(z, 1e-4);
    let dz_l = abs(z_l - z);
    let dz_r = abs(z_r - z);
    let dz_u = abs(z_u - z);
    let dz_d = abs(z_d - z);
    let valid_l = z_l > 1e-6 && (dz_l * inv_z) <= max_rel_dz;
    let valid_r = z_r > 1e-6 && (dz_r * inv_z) <= max_rel_dz;
    let valid_u = z_u > 1e-6 && (dz_u * inv_z) <= max_rel_dz;
    let valid_d = z_d > 1e-6 && (dz_d * inv_z) <= max_rel_dz;

    var dpdx = vec3<f32>(0.0);
    var has_x = false;
    if (valid_r && (!valid_l || dz_r <= dz_l)) {
        dpdx = reconstruct_pos(px_r, z_r, render) - p;
        has_x = true;
    } else if (valid_l) {
        dpdx = p - reconstruct_pos(px_l, z_l, render);
        has_x = true;
    }

    var dpdy = vec3<f32>(0.0);
    var has_y = false;
    if (valid_d && (!valid_u || dz_d <= dz_u)) {
        dpdy = reconstruct_pos(px_d, z_d, render) - p;
        has_y = true;
    } else if (valid_u) {
        dpdy = p - reconstruct_pos(px_u, z_u, render);
        has_y = true;
    }

    let fallback_n = normalize(vec3<f32>(0.0, 0.6, 0.0) - view_dir * 0.8);
    var n = fallback_n;
    if (has_x && has_y) {
        // Opt #20: Squared length check + inverseSqrt eliminates redundant sqrt in GBUF normal derivation.
        let raw_n = cross(dpdy, dpdx);
        let len2 = dot(raw_n, raw_n);
        if (len2 > 1e-12) {
            n = raw_n * inverseSqrt(len2);
            if (dot(n, view_dir) > 0.0) {
                n = -n;
            }
        }
    }

    // Native water remains a flat raster surface. Identify it using the
    // active water plane footprint and height, without a second depth pass.
    var roughness = 0.92;
    let water_count = min(u32(prep.water_count.x), 16u);
    // Surface normals are oriented toward the camera above, so the underside
    // of a horizontal water plane has a negative Y normal. Classify both faces.
    if (abs(n.y) > 0.985) {
        for (var wi = 0u; wi < water_count; wi = wi + 1u) {
            let bounds = prep.water_planes[wi];
            let height = prep.water_heights[wi / 4u][wi % 4u];
            if (p.x >= bounds.x && p.x <= bounds.y
                && p.z >= bounds.z && p.z <= bounds.w
                && abs(p.y - height) < 0.15)
            {
                roughness = 0.06;
                // Keep the tagged water normal facing the camera on either side
                // of the plane; reflection/Fresnel code relies on that convention.
                n = select(
                    vec3<f32>(0.0, -1.0, 0.0),
                    vec3<f32>(0.0, 1.0, 0.0),
                    view_dir.y < 0.0,
                );
                break;
            }
        }
    }

    // Feature #3: Ultra-conservative calibrated glossy reflection mask.
    // Matte stone, terrain, plaster, wood, and foliage ALWAYS stay matte (0.92).
    // Only crisp close-range specular/metallic/glazed architectural trims qualify
    // for a subtle satin sheen (roughness 0.26..=0.34).
    let is_water = roughness <= 0.12;
    if (!is_water && dist < 95.0) {
        let max_c = max(raster_rgb.r, max(raster_rgb.g, raster_rgb.b));
        let is_green_foliage = raster_rgb.g > raster_rgb.r * 1.12 && raster_rgb.g > raster_rgb.b * 1.12;
        if (max_c > 0.68 && !is_green_foliage && (abs(n.y) < 0.94 || max_c > 0.82)) {
            let lum_w = vec3<f32>(0.2126, 0.7152, 0.0722);
            let l_c = dot(raster_rgb, lum_w);
            let l_nb = 0.25 * (
                dot(textureLoad(src_raster, px_l, 0).rgb, lum_w)
                + dot(textureLoad(src_raster, px_r, 0).rgb, lum_w)
                + dot(textureLoad(src_raster, px_u, 0).rgb, lum_w)
                + dot(textureLoad(src_raster, px_d, 0).rgb, lum_w)
            );
            let spec_contrast = l_c - l_nb;
            if (spec_contrast > 0.14) {
                roughness = clamp(0.34 - (spec_contrast - 0.14) * 0.35, 0.26, 0.34);
            }
        }
    }

    textureStore(out_pos, vec2<i32>(id.xy), vec4<f32>(p, dist));
    textureStore(out_norm, vec2<i32>(id.xy), vec4<f32>(n, roughness));
    textureStore(out_raster, vec2<i32>(id.xy), vec4<f32>(raster_rgb, 0.0));
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
    point_light_meta: vec4<f32>,
    point_lights_pos_rad: array<vec4<f32>, 128>,
    point_lights_col_int: array<vec4<f32>, 128>,
    terrain_map_bounds: vec4<f32>,
    prev_vp: mat4x4<f32>,
    restir: vec4<f32>,
};

struct RestirReservoir {
    sample_pos_weight: vec4<f32>,
    radiance_m: vec4<f32>,
    receiver_pos_target: vec4<f32>,
    receiver_normal_flags: vec4<f32>,
};

// Separate integer sums and count use only atomicAdd/atomicLoad, which are
// supported by wgpu 0.19's HLSL translator. One vec4-sized cache cell.
struct RadianceCacheCell {
    r: atomic<u32>,
    g: atomic<u32>,
    b: atomic<u32>,
    count: atomic<u32>,
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
@group(0) @binding(13) var src_sky_env: texture_2d<f32>;
@group(0) @binding(14) var src_terrain_map: texture_2d<f32>;
@group(0) @binding(15) var<storage, read_write> radiance_cache: array<RadianceCacheCell>;
@group(0) @binding(16) var<storage, read_write> restir_reservoirs: array<RestirReservoir>;
// Full render-resolution raster colour: keep reflected trees/buildings crisp even
// when the tracing G-buffer is deliberately downsampled for performance.
@group(1) @binding(0) var src_screen_raster: texture_2d<f32>;

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

fn triangle_hit(tri: Tri, origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32) -> vec3<f32> {
    // xyz = (distance, barycentric u, barycentric v); no-hit is (-1, 0, 0).
    let a = tri.a.xyz;
    let b = tri.b.xyz;
    let c = tri.c.xyz;
    let e1 = b - a;
    let e2 = c - a;
    let p = cross(dir, e2);
    let det = dot(e1, p);
    if (abs(det) < 1e-12) {
        return vec3<f32>(-1.0, 0.0, 0.0);
    }
    let inv = 1.0 / det;
    let tv = origin - a;
    let u = dot(tv, p) * inv;
    if (u < -1e-6 || u > 1.000001) {
        return vec3<f32>(-1.0, 0.0, 0.0);
    }
    let q = cross(tv, e1);
    let v = dot(dir, q) * inv;
    if (v < -1e-6 || u + v > 1.000001) {
        return vec3<f32>(-1.0, 0.0, 0.0);
    }
    let t = dot(e2, q) * inv;
    if (t < t_min || t > t_max) {
        return vec3<f32>(-1.0, 0.0, 0.0);
    }
    return vec3<f32>(t, u, v);
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
                if (result.x >= 0.0 && accept_opacity(tri_index, vec2<f32>(result.y, result.z), 0u)) {
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

fn ign(px: vec2<u32>, frame: u32) -> f32 {
    let p = vec2<f32>(px) + vec2<f32>(f32(frame & 63u), f32((frame * 7u) & 63u)) * 5.588238;
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

fn unpack_alpha_mask25(f: f32) -> u32 {
    let r = bitcast<u32>(f);
    return (r & 0x00ffffffu) | ((r >> 7u) & 0x01000000u);
}

fn accept_opacity(tri_index: u32, bary: vec2<f32>, seed: u32) -> bool {
    let raw_lo = bitcast<u32>(tris[tri_index].c.w);
    let raw_hi = bitcast<u32>(tris[tri_index].n.w);
    if ((raw_lo | raw_hi) != 0u) {
        let lo25 = unpack_alpha_mask25(tris[tri_index].c.w);
        let hi24 = unpack_alpha_mask25(tris[tri_index].n.w);
        let s = clamp(bary.x, 0.0, 1.0);
        let t = clamp(bary.y / max(1.0 - s, 1e-4), 0.0, 1.0);
        let ix = min(u32(s * 7.0), 6u);
        let iy = min(u32(t * 7.0), 6u);
        let bit = iy * 7u + ix;
        let hit_bit = select((hi24 >> (bit - 25u)) & 1u, (lo25 >> bit) & 1u, bit < 25u);
        return hit_bit != 0u;
    }
    let mat_count = max(u32(params.counts.z), 1u);
    let mat_idx = min(u32(round(tris[tri_index].b.w)), mat_count - 1u);
    let opacity = materials[mat_idx].surface.w;
    if (opacity >= 0.99) {
        return true;
    }
    var h = tri_index * 747796405u ^ u32(bary.x * 4096.0) * 2891336453u ^ seed;
    h = h ^ (h >> 16u);
    return f32(h & 0xffffu) / 65536.0 < opacity;
}

fn occluded(origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32, seed: u32) -> bool {
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
                let tid = order[first + i];
                let hit_res = triangle_hit(tris[tid], origin, dir, t_min, t_max);
                if (hit_res.x >= 0.0 && accept_opacity(tid, vec2<f32>(hit_res.y, hit_res.z), seed)) {
                    return true;
                }
            }
        } else {
            // Opt #14: Near-first child ordering in software BVH occluded() hits blockers ~30% sooner.
            let left = node_index + 1u;
            let right = u32(node.bounds_max.w);
            let lh = box_hit(nodes[left].bounds_min.xyz, nodes[left].bounds_max.xyz, origin, inv_dir, t_min, t_max);
            let rh = box_hit(nodes[right].bounds_min.xyz, nodes[right].bounds_max.xyz, origin, inv_dir, t_min, t_max);
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

fn occluded_point_light(origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32, terrain_skip: f32, seed: u32) -> bool {
    let h = closest_hit(origin, dir, t_min, t_max);
    if (h.index == 0xffffffffu) {
        return false;
    }
    if (terrain_skip > 0.0 && h.t < terrain_skip) {
        let k = tris[h.index].a.w;
        if (k > 1.5 && k < 2.5) {
            let next_t = h.t + 0.85;
            if (next_t < t_max) {
                return occluded(origin, dir, next_t, t_max, seed);
            }
            return false;
        }
    }
    return true;
}

fn point_light_falloff(dist: f32, radius: f32) -> f32 {
    if (radius <= 1e-4 || dist >= radius) {
        return 0.0;
    }
    let ratio = dist / radius;
    let s = max(1.0 - ratio, 0.0);
    let sw = s * s * (3.0 - 2.0 * s);
    let fade = sw * sw;
    let norm_d = dist / max(radius * 0.28, 3.0);
    return fade / (1.0 + norm_d * norm_d);
}

// Phase 4: Reproject a world-space hit point into the current frame's 1:1
// raster G-buffer to fetch real textured raster colour when on-screen.
// Returns `vec4(rgb, 1.0)` on a valid on-screen match, or `vec4(0.0)` off-screen
// (off-screen hits fall back to harvested per-material baked albedo).
fn sample_screen_radiance(world_pos: vec3<f32>, ray_t: f32) -> vec4<f32> {
    let clip = params.vp * vec4<f32>(world_pos, 1.0);
    if (clip.w <= 1e-4) {
        return vec4<f32>(0.0);
    }
    let ndc = clip.xyz / clip.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.01 || uv.x > 0.99 || uv.y < 0.01 || uv.y > 0.99) {
        return vec4<f32>(0.0);
    }
    let gdim = vec2<i32>(i32(params.extents.x), i32(params.extents.y));
    let hp = clamp(vec2<i32>(uv * params.extents.xy), vec2<i32>(0), gdim - vec2<i32>(1));
    let gpos = textureLoad(src_gbuf_pos, hp, 0);
    let separation = gpos.xyz - world_pos;
    let tolerance = max(2.5, ray_t * 0.08);
    if (gpos.w > 0.0 && dot(separation, separation) < tolerance * tolerance) {
        let screen_dim = vec2<i32>(textureDimensions(src_screen_raster));
        let screen_px = clamp(
            vec2<i32>(uv * vec2<f32>(screen_dim)),
            vec2<i32>(0),
            screen_dim - vec2<i32>(1),
        );
        return vec4<f32>(textureLoad(src_screen_raster, screen_px, 0).rgb, 1.0);
    }
    return vec4<f32>(0.0);
}

// Phase 3: Sample the real rendered SkyGpu + CloudGpu 3x2 cubemap atlas.
fn sample_sky_env(dir: vec3<f32>) -> vec3<f32> {
    let d = normalize(dir);
    let ad = abs(d);
    var face = 0u;
    var uc = 0.0;
    var vc = 0.0;
    var ma = 1.0;
    if (ad.x >= ad.y && ad.x >= ad.z) {
        ma = ad.x;
        if (d.x >= 0.0) { face = 0u; uc = -d.z; vc = -d.y; }
        else            { face = 1u; uc =  d.z; vc = -d.y; }
    } else if (ad.y >= ad.x && ad.y >= ad.z) {
        ma = ad.y;
        if (d.y >= 0.0) { face = 2u; uc =  d.x; vc =  d.z; }
        else            { face = 3u; uc =  d.x; vc = -d.z; }
    } else {
        ma = ad.z;
        if (d.z >= 0.0) { face = 4u; uc =  d.x; vc = -d.y; }
        else            { face = 5u; uc = -d.x; vc = -d.y; }
    }
    let fuv = clamp(0.5 * (vec2<f32>(uc, vc) / max(ma, 1e-5) + vec2<f32>(1.0)), vec2<f32>(0.002), vec2<f32>(0.998));
    let dim = vec2<f32>(textureDimensions(src_sky_env));
    let fw = dim.x / 3.0;
    let fh = dim.y / 2.0;
    let col = face % 3u;
    let row = face / 3u;
    let fx = f32(col) * fw + fuv.x * (fw - 1.0);
    let fy = f32(row) * fh + fuv.y * (fh - 1.0);
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let x1 = min(x0 + 1, i32((col + 1u) * u32(fw)) - 1);
    let y1 = min(y0 + 1, i32((row + 1u) * u32(fh)) - 1);
    let frac_xy = vec2<f32>(fx - f32(x0), fy - f32(y0));
    let c00 = textureLoad(src_sky_env, vec2<i32>(x0, y0), 0).rgb;
    let c10 = textureLoad(src_sky_env, vec2<i32>(x1, y0), 0).rgb;
    let c01 = textureLoad(src_sky_env, vec2<i32>(x0, y1), 0).rgb;
    let c11 = textureLoad(src_sky_env, vec2<i32>(x1, y1), 0).rgb;
    return max(mix(mix(c00, c10, frac_xy.x), mix(c01, c11, frac_xy.x), frac_xy.y), vec3<f32>(0.0));
}

// Phase 3: Reflection miss ALWAYS samples the real rendered SkyGpu + CloudGpu
// environment target (analytic gradient removed from the miss path).
fn sample_sky_reflection(dir: vec3<f32>) -> vec3<f32> {
    return sample_sky_env(dir);
}

fn sample_terrain_albedo(world_pos: vec3<f32>, fallback: vec3<f32>) -> vec3<f32> {
    if (params.terrain_map_bounds.w <= 0.5) {
        return fallback;
    }
    let u = (world_pos.x - params.terrain_map_bounds.x) * params.terrain_map_bounds.z;
    let v = (params.terrain_map_bounds.y - world_pos.z) * params.terrain_map_bounds.z;
    if (u < 0.005 || u > 0.995 || v < 0.005 || v > 0.995) {
        return fallback;
    }
    let dim = textureDimensions(src_terrain_map);
    let fx = u * f32(dim.x - 1u);
    let fy = v * f32(dim.y - 1u);
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let x1 = min(x0 + 1, i32(dim.x) - 1);
    let y1 = min(y0 + 1, i32(dim.y) - 1);
    let fr = vec2<f32>(fx - f32(x0), fy - f32(y0));
    let c00 = textureLoad(src_terrain_map, vec2<i32>(x0, y0), 0).rgb;
    let c10 = textureLoad(src_terrain_map, vec2<i32>(x1, y0), 0).rgb;
    let c01 = textureLoad(src_terrain_map, vec2<i32>(x0, y1), 0).rgb;
    let c11 = textureLoad(src_terrain_map, vec2<i32>(x1, y1), 0).rgb;
    let col = mix(mix(c00, c10, fr.x), mix(c01, c11, fr.x), fr.y);
    if (dot(col, vec3<f32>(1.0)) < 1e-4) {
        return fallback;
    }
    return clamp(col, vec3<f32>(0.03), vec3<f32>(0.95));
}

fn ggx_sample(normal: vec3<f32>, roughness: f32, u1: f32, u2: f32) -> vec3<f32> {
    if (roughness < 0.06) {
        return normal;
    }
    let a = roughness * roughness;
    let a2 = a * a;
    let phi = 2.0 * PI * u1;
    let cos_theta = sqrt(max((1.0 - u2) / (1.0 + (a2 - 1.0) * u2), 0.0));
    let sin_theta = sqrt(max(1.0 - cos_theta * cos_theta, 0.0));
    let local_h = vec3<f32>(cos(phi) * sin_theta, sin(phi) * sin_theta, cos_theta);
    return normalize(orthonormal_basis(normal) * local_h);
}

const RC_MASK: u32 = 65535u;

// Stable spatial hash shared with the DXR shader: world voxel, dominant
// normal direction, and near/far cache scale all contribute to the key.
fn rc_slot(world_pos: vec3<f32>, normal: vec3<f32>, cam_dist: f32) -> u32 {
    let cell_size = select(12.0, 4.0, cam_dist < 64.0);
    let cell = vec3<i32>(floor(world_pos / cell_size));
    let an = abs(normal);
    var ndir = 0u;
    if (an.x >= an.y && an.x >= an.z) {
        ndir = select(1u, 0u, normal.x >= 0.0);
    } else if (an.y >= an.x && an.y >= an.z) {
        ndir = select(3u, 2u, normal.y >= 0.0);
    } else {
        ndir = select(5u, 4u, normal.z >= 0.0);
    }
    let scale_bit = select(1u, 0u, cam_dist < 64.0);
    var h = bitcast<u32>(cell.x) * 73856093u
        ^ bitcast<u32>(cell.y) * 19349663u
        ^ bitcast<u32>(cell.z) * 83492791u
        ^ (ndir * 2654435761u)
        ^ (scale_bit * 374761393u);
    h = h ^ (h >> 16u);
    h = h * 0x85ebca6bu;
    h = h ^ (h >> 13u);
    return h & RC_MASK;
}

fn rc_query(world_pos: vec3<f32>, normal: vec3<f32>, cam_dist: f32) -> vec4<f32> {
    // Disabled: this 16-bit spatial hash stores no key/tag, so unrelated
    // world cells can alias and feed one another's radiance into GI.
    return vec4<f32>(0.0);
}

fn rc_update(world_pos: vec3<f32>, normal: vec3<f32>, cam_dist: f32, rad: vec3<f32>) {
    // Disabled until cache slots can validate the full world-cell key.
}

fn detail_offscreen_albedo(hit_pos: vec3<f32>, hit_normal: vec3<f32>, base_albedo: vec3<f32>, opacity: f32) -> vec3<f32> {
    let an = abs(hit_normal);
    if (opacity < 0.96) {
        let fc = vec3<i32>(floor(hit_pos * 1.4));
        var h = bitcast<u32>(fc.x) * 374761393u ^ bitcast<u32>(fc.y) * 668265263u ^ bitcast<u32>(fc.z) * 2147483647u;
        h = h ^ (h >> 15u);
        let v = 0.85 + 0.30 * (f32(h & 0xffu) / 255.0);
        return clamp(base_albedo * v, vec3<f32>(0.02), vec3<f32>(0.95));
    }
    if (an.y < 0.55) {
        let horiz = select(hit_pos.x, hit_pos.z, an.x > an.z);
        let row = floor(hit_pos.y * 1.25);
        let col = floor(horiz * 0.75 + fract(row * 0.5) * 0.5);
        let local_uv = vec2<f32>(fract(horiz * 0.75 + fract(row * 0.5) * 0.5), fract(hit_pos.y * 1.25));
        let mortar = select(1.0, 0.82, local_uv.x < 0.06 || local_uv.y < 0.08);
        var bh = bitcast<u32>(i32(col)) * 73856093u ^ bitcast<u32>(i32(row)) * 19349663u;
        bh = bh ^ (bh >> 13u);
        let tone = 0.88 + 0.24 * (f32(bh & 0xffu) / 255.0);
        return clamp(base_albedo * (mortar * tone), vec3<f32>(0.02), vec3<f32>(0.95));
    }
    if (an.y < 0.94) {
        let ridge = 0.90 + 0.14 * sin(hit_pos.x * 4.2 + hit_pos.z * 4.2);
        return clamp(base_albedo * ridge, vec3<f32>(0.02), vec3<f32>(0.95));
    }
    let sc = vec2<i32>(floor(hit_pos.xz * 0.5));
    var sh = bitcast<u32>(sc.x) * 73856093u ^ bitcast<u32>(sc.y) * 83492791u;
    sh = sh ^ (sh >> 14u);
    let slab = 0.91 + 0.18 * (f32(sh & 0xffu) / 255.0);
    return clamp(base_albedo * slab, vec3<f32>(0.02), vec3<f32>(0.95));
}

fn hg_phase(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = max(1.0 + g2 - 2.0 * g * cos_theta, 0.05);
    return (1.0 - g2) / (4.0 * PI * denom * sqrt(denom));
}

fn trace_effect_light_reflections(origin: vec3<f32>, dir: vec3<f32>, max_t: f32, pl_count: u32) -> vec3<f32> {
    var glow_sum = vec3<f32>(0.0);
    for (var li = 0u; li < pl_count; li = li + 1u) {
        let pr = params.point_lights_pos_rad[li];
        if (pr.w <= 0.1) {
            continue;
        }
        let d_eye = pr.xyz - params.eye.xyz;
        if (dot(d_eye, d_eye) < 12.25) {
            continue;
        }
        let to_l = pr.xyz - origin;
        let t_proj = dot(to_l, dir);
        if (t_proj <= 0.8 || t_proj >= max_t) {
            continue;
        }
        let d2 = max(dot(to_l, to_l) - t_proj * t_proj, 0.0);
        let r_core = clamp(pr.w * 0.11, 0.50, 3.2);
        let r2_out = r_core * r_core * 6.0;
        if (d2 < r2_out) {
            let ci = params.point_lights_col_int[li];
            if (ci.w < 0.05) {
                continue;
            }
            let u = clamp(d2 / max(r2_out, 0.1), 0.0, 1.0);
            let win = 1.0 - u;
            let orb = (win * win * win) * exp(-u * 3.5) / (1.0 + t_proj * 0.014);
            glow_sum = glow_sum + ci.rgb * (ci.w * orb * 0.28);
        }
    }
    return min(glow_sum, vec3<f32>(2.2));
}

// Enhancement #4: High-Precision Micro-Contact Shadows (4-step depth-anchored screen-space ray).
fn trace_contact_shadow(world_pos: vec3<f32>, sun_dir: vec3<f32>, cam_dist: f32, jitter: f32) -> f32 {
    if (cam_dist > 42.0) {
        return 1.0;
    }
    let max_reach = mix(1.35, 0.55, clamp(cam_dist / 42.0, 0.0, 1.0));
    var occ = 0.0;
    let gdim = vec2<i32>(i32(params.extents.x), i32(params.extents.y));
    for (var ci = 0u; ci < 4u; ci = ci + 1u) {
        let t_c = ((f32(ci) + 0.35 + 0.55 * jitter) / 4.0) * max_reach;
        let p_c = world_pos + sun_dir * t_c;
        let clip = params.vp * vec4<f32>(p_c, 1.0);
        if (clip.w <= 1e-4) {
            continue;
        }
        let ndc = clip.xy / clip.w;
        let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        if (uv.x < 0.005 || uv.x > 0.995 || uv.y < 0.005 || uv.y > 0.995) {
            continue;
        }
        let hp = clamp(vec2<i32>(uv * params.extents.xy), vec2<i32>(0), gdim - vec2<i32>(1));
        let gpos = textureLoad(src_gbuf_pos, hp, 0);
        if (gpos.w <= 0.0) {
            continue;
        }
        let ray_cam_d = length(p_c - params.eye.xyz);
        let dz = ray_cam_d - gpos.w;
        if (dz > 0.045 && dz < 0.58) {
            occ = max(occ, 1.0 - smoothstep(0.32, 0.58, dz));
        }
    }
    let dist_fade = 1.0 - smoothstep(26.0, 42.0, cam_dist);
    return 1.0 - occ * 0.72 * dist_fade;
}

fn kind_colour(kind: f32) -> vec3<f32> {
    if (kind < 0.5) { return vec3<f32>(0.04, 0.04, 0.04); }
    if (kind < 1.5) { return vec3<f32>(0.0, 0.85, 1.0); }   // cyan: water quad
    if (kind < 2.5) { return vec3<f32>(1.0, 0.55, 0.0); }   // orange: LOD terrain
    if (kind < 3.5) { return vec3<f32>(0.15, 0.90, 0.25); } // green: real mesh
    return vec3<f32>(0.95, 0.15, 0.85);                     // magenta: instance proxy
}

fn adaptive_ray_samples(pixel: vec2<i32>, dist: f32, normal: vec3<f32>, roughness: f32, is_water: bool, base: u32) -> u32 {
    let maximum = clamp(u32(params.restir.y), base, 8u);
    if (maximum <= base || is_water) { return base; }
    let dimensions = vec2<i32>(textureDimensions(src_gbuf_pos));
    let centre_luma = luminance(textureLoad(src_gbuf_raster, pixel, 0).rgb);
    var normal_edge = 0.0;
    var depth_edge = 0.0;
    var colour_edge = 0.0;
    for (var i = 0u; i < 4u; i = i + 1u) {
        var offset = vec2<i32>(1, 0);
        if (i == 1u) { offset = vec2<i32>(-1, 0); }
        if (i == 2u) { offset = vec2<i32>(0, 1); }
        if (i == 3u) { offset = vec2<i32>(0, -1); }
        let q = clamp(pixel + offset, vec2<i32>(0), dimensions - vec2<i32>(1));
        let neighbour_pos = textureLoad(src_gbuf_pos, q, 0);
        if (neighbour_pos.w <= 0.0) {
            depth_edge = max(depth_edge, 1.0);
            continue;
        }
        let neighbour_raw_n = textureLoad(src_gbuf_norm, q, 0).xyz;
        let neighbour_n = select(vec3<f32>(0.0, 1.0, 0.0), normalize(neighbour_raw_n), length(neighbour_raw_n) > 1e-4);
        normal_edge = max(normal_edge, 1.0 - clamp(dot(normal, neighbour_n), -1.0, 1.0));
        depth_edge = max(depth_edge, abs(neighbour_pos.w - dist) / max(dist, 1.0));
        let neighbour_luma = luminance(textureLoad(src_gbuf_raster, q, 0).rgb);
        colour_edge = max(colour_edge, abs(neighbour_luma - centre_luma));
    }
    var complexity = max(normal_edge * 2.0, max(depth_edge * 5.0, colour_edge * 2.0));
    complexity = max(complexity, (1.0 - smoothstep(0.0, 320.0, dist)) * 0.20);
    complexity = max(complexity, clamp((0.36 - roughness) / 0.36, 0.0, 1.0) * 0.20);
    let gi_enabled = (u32(round(params.effects.y)) & 16u) != 0u
        && params.frame.z >= 1.0
        && params.tuning.w > 0.0;
    if (gi_enabled) {
        complexity = max(complexity, (1.0 - smoothstep(40.0, 220.0, dist)) * 0.50);
    }
    let extra = u32(ceil(clamp(complexity, 0.0, 1.0) * f32(maximum - base)));
    return clamp(base + extra, base, maximum);
}

fn restir_empty() -> RestirReservoir {
    return RestirReservoir(
        vec4<f32>(0.0), vec4<f32>(0.0),
        vec4<f32>(0.0), vec4<f32>(0.0)
    );
}

fn restir_clear(index: u32) {
    if (params.restir.x > 0.5) {
        restir_reservoirs[index] = restir_empty();
    }
}

fn restir_update(
    reservoir: ptr<function, RestirReservoir>,
    sample_pos: vec3<f32>,
    radiance: vec3<f32>,
    target_over_pdf: f32,
    weight: f32,
    represented: f32,
    is_sky: bool,
    receiver_pos: vec3<f32>,
    receiver_normal: vec3<f32>,
    state: ptr<function, u32>
) {
    if (weight <= 0.0 || represented <= 0.0) { return; }
    var value = *reservoir;
    let sum = value.sample_pos_weight.w + weight;
    if (next_f32(state) * sum < weight) {
        value.sample_pos_weight = vec4<f32>(sample_pos, 0.0);
        value.radiance_m = vec4<f32>(radiance, value.radiance_m.w);
        // Reuse needs the old importance weight (target / proposal PDF), not
        // the un-divided target. The proposal is cosine-weighted, so storing
        // target alone would reweight history by the cosine twice.
        value.receiver_pos_target = vec4<f32>(receiver_pos, max(target_over_pdf, 1e-5));
        value.receiver_normal_flags = vec4<f32>(receiver_normal, select(1.0, -1.0, is_sky));
    }
    value.sample_pos_weight.w = sum;
    value.radiance_m.w = value.radiance_m.w + represented;
    *reservoir = value;
}

fn restir_reuse_one(
    reservoir: ptr<function, RestirReservoir>,
    source: RestirReservoir,
    receiver_pos: vec3<f32>,
    receiver_normal: vec3<f32>,
    ray_origin: vec3<f32>,
    bias: f32,
    camera_distance: f32,
    spatial: bool,
    state: ptr<function, u32>
) {
    if (source.sample_pos_weight.w <= 0.0 || source.radiance_m.w <= 0.0 || source.receiver_pos_target.w <= 1e-5) { return; }
    let receiver_delta = source.receiver_pos_target.xyz - receiver_pos;
    let receiver_plane = dot(receiver_delta, receiver_normal);
    let plane_error = abs(receiver_plane);
    let lateral = length(receiver_delta - receiver_normal * receiver_plane);
    let max_lateral = select(max(0.25, camera_distance * 0.003), max(1.25, camera_distance * 0.012), spatial);
    let max_plane = select(max(0.08, camera_distance * 0.001), max(0.16, camera_distance * 0.003), spatial);
    let normal_match = dot(normalize(source.receiver_normal_flags.xyz), receiver_normal);
    if (normal_match < 0.90 || plane_error > max_plane || lateral > max_lateral) { return; }

    let is_sky = source.receiver_normal_flags.w < 0.0;
    var direction = vec3<f32>(0.0, 1.0, 0.0);
    var sample_distance = 0.0;
    if (is_sky) {
        direction = normalize(source.sample_pos_weight.xyz);
    } else {
        let to_sample = source.sample_pos_weight.xyz - receiver_pos;
        sample_distance = length(to_sample);
        if (sample_distance <= bias) { return; }
        direction = to_sample / sample_distance;
    }
    let cosine = max(dot(receiver_normal, direction), 0.0);
    if (cosine <= 0.01) { return; }
    if (!is_sky) {
        let tolerance = max(0.35, sample_distance * 0.02);
        let visible = closest_hit(ray_origin, direction, max(0.05, bias * 0.25), sample_distance + tolerance);
        if (visible.index == 0xffffffffu || abs(visible.t - sample_distance) > tolerance) { return; }
    }
    let target_density = max(luminance(source.radiance_m.xyz) * cosine, 1e-5);
    let target_over_pdf = target_density / max(cosine, 1e-5);
    let represented = clamp(source.radiance_m.w, 1.0, 32.0);
    let age_scale = represented / max(source.radiance_m.w, 1.0);
    let weight = source.sample_pos_weight.w * age_scale * target_over_pdf
        / max(source.receiver_pos_target.w, 1e-5);
    restir_update(
        reservoir, source.sample_pos_weight.xyz, source.radiance_m.xyz,
        target_over_pdf, weight, represented, is_sky,
        receiver_pos, receiver_normal, state
    );
}

fn restir_reuse_history(
    reservoir: ptr<function, RestirReservoir>,
    point: vec3<f32>,
    normal: vec3<f32>,
    ray_origin: vec3<f32>,
    bias: f32,
    distance: f32,
    pixel: vec2<u32>,
    extent: vec2<u32>,
    previous_base: u32,
    state: ptr<function, u32>
) {
    if (params.frame.w <= 0.5) { return; }
    let clip = params.prev_vp * vec4<f32>(point, 1.0);
    if (clip.w <= 1e-5) { return; }
    let ndc = clip.xy / clip.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.0 || uv.x >= 1.0 || uv.y < 0.0 || uv.y >= 1.0) { return; }
    let previous_pixel = vec2<i32>(uv * vec2<f32>(extent));
    let linear = u32(previous_pixel.y) * extent.x + u32(previous_pixel.x);
    restir_reuse_one(
        reservoir, restir_reservoirs[previous_base + linear],
        point, normal, ray_origin, bias, distance, false, state
    );

    let reuse_count = min(u32(params.restir.z), 4u);
    for (var i = 0u; i < reuse_count; i = i + 1u) {
        var offset = vec2<i32>(1, 0);
        if (i == 1u) { offset = vec2<i32>(-1, 0); }
        if (i == 2u) { offset = vec2<i32>(0, 1); }
        if (i == 3u) { offset = vec2<i32>(0, -1); }
        let neighbour = previous_pixel + offset;
        if (neighbour.x < 0 || neighbour.y < 0 || neighbour.x >= i32(extent.x) || neighbour.y >= i32(extent.y)) {
            continue;
        }
        let neighbour_index = previous_base + u32(neighbour.y) * extent.x + u32(neighbour.x);
        restir_reuse_one(
            reservoir, restir_reservoirs[neighbour_index],
            point, normal, ray_origin, bias, distance, true, state
        );
    }
}

@compute @workgroup_size(8, 8, 1)
fn trace_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let extent = vec2<u32>(u32(params.extents.x), u32(params.extents.y));
    if (id.x >= extent.x || id.y >= extent.y) {
        return;
    }
    let ip = vec2<i32>(id.xy);
    let pixel_count = extent.x * extent.y;
    let parity = u32(round(params.restir.w)) & 1u;
    let reservoir_index = parity * pixel_count + id.y * extent.x + id.x;
    let previous_base = (1u - parity) * pixel_count;
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
        restir_clear(reservoir_index);
        return;
    }

    let point = pos_d.xyz;
    let norm_r = textureLoad(src_gbuf_norm, ip, 0);
    let raw_n = norm_r.xyz;
    let base_normal = select(vec3<f32>(0.0, 1.0, 0.0), normalize(raw_n), length(raw_n) > 1e-4);
    let roughness = clamp(norm_r.w, 0.04, 1.0);
    let raster_rgba = textureLoad(src_gbuf_raster, ip, 0);
    let raster_rgb = raster_rgba.rgb;
    let view_dir = normalize(point - params.eye.xyz);
    let is_water = roughness <= 0.12;
    // Orient the flat water plane toward the camera; below the surface the
    // legacy G-buffer normal can still point +Y, placing the RT ray origin
    // above the interface and giving Schlick Fresnel the wrong incident angle.
    let normal = select(base_normal, -base_normal, is_water && dot(base_normal, view_dir) > 0.0);
    let is_glossy_trim = !is_water && roughness <= 0.36;

    let effects = u32(round(params.effects.y));
    let bits_shadows = (effects & 1u) != 0u;
    let bits_lighting = (effects & 2u) != 0u;
    let bits_reflections = (effects & 4u) != 0u;
    let bits_ao = (effects & 8u) != 0u;
    let bits_gi = (effects & 16u) != 0u;
    let bits_contact_shadows = (effects & 256u) != 0u;
    let bits_moonlit_night = (effects & 512u) != 0u;
    let bits_weather = (effects & 2048u) != 0u;
    let trace_water_reflection = bits_reflections
        && u32(round(params.counts.w)) == 0u;
    // Keep native water only when RT reflections are disabled or a debug view owns the output.
    if (is_water && !trace_water_reflection) {
        textureStore(out_colour, ip, vec4<f32>(select(raster_rgb, vec3<f32>(0.0), layered), 1.0));
        textureStore(out_guide, ip, vec4<f32>(1.0, dist / max(params.lens.y, 1e-4), 1.0, roughness));
        textureStore(out_reflection, ip, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        textureStore(out_normals, ip, vec4<f32>(normal, roughness));
        textureStore(out_albedo, ip, vec4<f32>(raster_rgb, 0.0));
        restir_clear(reservoir_index);
        return;
    }

    let steepness = select(1.0 - clamp(normal.y, 0.0, 1.0), 0.0, is_water);
    let slope_bias = steepness * (0.75 + dist * 0.0065);
    let bias = max(params.effects.z, max(0.18, dist * 0.0035)) + slope_bias;
    let ray_origin = point + normal * bias + vec3<f32>(0.0, 1.0, 0.0) * (steepness * 0.55) - view_dir * (bias * 0.35);
    let shadow_t_min = max(bias * 1.15, 0.60 + steepness * 0.85);
    let ao_t_min = max(bias * 0.80, 0.40);
    let n_dot_l = max(dot(normal, params.sun_dir.xyz), 0.0);

    var fog_t = 0.0;
    let fog_start = params.fog.x;
    let fog_end = params.fog.y;
    if (fog_end > fog_start && fog_start >= 0.0) {
        fog_t = clamp((dist - fog_start) / (fog_end - fog_start), 0.0, 1.0);
    }

    let debug_mode_early = u32(round(params.counts.w));
    if (fog_t >= 0.99 && layered && debug_mode_early == 0u) {
        textureStore(out_colour, ip, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        textureStore(out_guide, ip, vec4<f32>(1.0, 1.0, dist, 1.0));
        textureStore(out_reflection, ip, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        textureStore(out_normals, ip, vec4<f32>(normal, roughness));
        textureStore(out_albedo, ip, vec4<f32>(raster_rgb, roughness));
        restir_clear(reservoir_index);
        return;
    }

    let base_samples = max(u32(params.frame.y), 1u);
    let samples = adaptive_ray_samples(ip, dist, normal, roughness, is_water, base_samples);
    let max_bounces = min(u32(round(params.frame.z)), 6u);
    let pl_count = min(u32(round(params.point_light_meta.x)), 128u);
    var state = id.x * 0x9e3779b9u ^ id.y * 0x85ebca6bu ^ u32(params.frame.x) * 0xc2b2ae35u;
    var accum_shadow = 0.0;
    var accum_trans = vec3<f32>(0.0);
    var accum_open = 0.0;
    var accum_bent_n = vec3<f32>(0.0);
    var gi_sky_hits = 0.0;
    var gi_total_rays = 0.0;
    var accum_reflection = vec3<f32>(0.0);
    var accum_refl_dist = 0.0;
    var accum_gi = vec3<f32>(0.0);
    var restir_current = restir_empty();

    let ao_radius = clamp(params.fog.z, 2.0, 6.0);
    let ao_samples = select(select(1u, 2u, dist < 140.0), 3u, dist < 48.0);
    let ao_fade = 1.0 - clamp((dist - 200.0) / 40.0, 0.0, 1.0);
    let ign_v = ign(id.xy, u32(params.frame.x));
    let rot_angle = ign_v * (2.0 * PI);
    let onb = orthonormal_basis(normal);
    let sun_onb = orthonormal_basis(params.sun_dir.xyz);
    let spread_tan = select(0.0, tan(params.lens.z), params.lens.z > 1e-4);
    let sun_luma = luminance(params.sun_colour.xyz);
    let sun_strength = smoothstep(0.28, 0.48, sun_luma);
    let gi_fade = 1.0 - clamp((dist - 220.0) / 180.0, 0.0, 1.0);

    for (var s = 0u; s < samples; s = s + 1u) {
        // Feature #7 (Contact-Hardening Soft Shadows / PCSS) + Feature #4 (Foliage Transmission)
        var shadow = 1.0;
        var trans_tint = vec3<f32>(0.0);
        if (bits_shadows && !is_water && n_dot_l > 0.02 && fog_t < 0.96) {
            let sun_taps = select(1u, 2u, spread_tan > 0.0 && dist < 140.0);
            var h0 = closest_hit(ray_origin, params.sun_dir.xyz, shadow_t_min, params.lens.y);
            let terrain_self_skip = select(2.5 + steepness * 14.0 + dist * 0.025, 0.0, is_water);
            if (h0.index != 0xffffffffu && terrain_self_skip > 0.0 && h0.t < terrain_self_skip) {
                let hk = tris[h0.index].a.w;
                if (hk > 1.5 && hk < 2.5 && h0.t + 0.65 < params.lens.y) {
                    h0 = closest_hit(ray_origin, params.sun_dir.xyz, h0.t + 0.65, params.lens.y);
                }
            }
            var hit0 = h0.index != 0xffffffffu;
            if (hit0 && !accept_opacity(h0.index, vec2<f32>(0.33, 0.33), state)) {
                hit0 = occluded(ray_origin, params.sun_dir.xyz, shadow_t_min, params.lens.y, state);
            }
            var vis_sum = select(1.0, 0.0, hit0);
            if (hit0 && h0.index != 0xffffffffu) {
                let m0 = material_of(h0.index);
                if (m0.surface.w < 0.96) {
                    let tr = clamp((1.0 - m0.surface.w) * 0.42 + 0.10, 0.0, 1.0);
                    trans_tint = trans_tint + clamp(m0.albedo.rgb, vec3<f32>(0.10), vec3<f32>(0.95)) * tr;
                }
            }
            if (sun_taps > 1u) {
                let pcss_scale = select(0.75, clamp(h0.t / 18.0, 0.14, 1.45), hit0);
                let r = sqrt(0.75) * spread_tan * pcss_scale;
                let theta = 2.39996323 + rot_angle + f32(s) * 1.6180339;
                let shadow_dir1 = normalize(params.sun_dir.xyz + sun_onb * vec3<f32>(cos(theta) * r, sin(theta) * r, 0.0));
                if (!occluded(ray_origin, shadow_dir1, shadow_t_min, params.lens.y, state ^ 1u)) {
                    vis_sum = vis_sum + 1.0;
                }
            }
            shadow = vis_sum / f32(sun_taps);
            trans_tint = trans_tint / f32(sun_taps);
            // Enhancement #10: Foliage canopy depth & warm sun-dapple contrast.
            if (dot(trans_tint, vec3<f32>(1.0)) > 1e-4) {
                trans_tint = trans_tint * vec3<f32>(1.06, 1.10, 0.90);
            }
            // Enhancement #17: Distant terrain self-shadowing beyond near BLAS bubble (460..1400u).
            if (shadow > 0.5 && dist > 260.0 && params.lens.y > 500.0) {
                let far_max_t = min(params.lens.y, 1400.0);
                if (occluded(ray_origin, params.sun_dir.xyz, 460.0, far_max_t, state ^ 7u)) {
                    shadow = 0.0;
                }
            }
            // Enhancement #4: High-precision micro-contact shadows for fine grounding.
            if (bits_contact_shadows && !is_water && shadow > 0.05 && dist < 42.0) {
                shadow = shadow * trace_contact_shadow(point, params.sun_dir.xyz, dist, ign_v);
            }
        }
        accum_shadow = accum_shadow + shadow;
        accum_trans = accum_trans + trans_tint;

        var open = 1.0;
        if (bits_ao && ao_fade > 0.01 && !is_water) {
            var occ_sum = 0.0;
            var bent = vec3<f32>(0.0);
            let inv_ao_r = 1.0 / max(ao_radius, 0.5);
            for (var i = 0u; i < ao_samples; i = i + 1u) {
                let u1 = (f32(i) + next_f32(&state)) / f32(ao_samples);
                let phi = f32(i) * 2.39996323 + rot_angle + f32(s) * 0.7548776;
                let r = sqrt(u1);
                let d = onb * vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(1.0 - u1, 0.0)));
                let ao_hit = closest_hit(ray_origin, d, ao_t_min, ao_radius);
                if (ao_hit.index != 0xffffffffu) {
                    let t_norm = clamp(ao_hit.t * inv_ao_r, 0.0, 1.0);
                    let w_close = 1.0 - t_norm;
                    let contact_boost = select(1.0, 1.18, ao_hit.t < 1.8);
                    occ_sum = occ_sum + clamp(w_close * sqrt(w_close) * contact_boost, 0.0, 1.0);
                } else {
                    bent = bent + d;
                }
            }
            open = mix(1.0, 1.0 - clamp(occ_sum / f32(ao_samples), 0.0, 1.0), ao_fade);
            accum_bent_n = accum_bent_n + select(normal, normalize(bent), dot(bent, bent) > 1e-4);
        } else {
            accum_bent_n = accum_bent_n + normal;
        }
        accum_open = accum_open + open;

        var refl_dist = params.lens.y;
        if (bits_reflections && roughness <= params.effects.x && (!is_water || s == 0u)) {
            // Native game water is a flat glossy dielectric: no animated wave
            // normals. Use the raster normal for both water and non-water surfaces.
            let refl_base_n = normal;
            // Keep non-water glossy trims on GGX, but use the exact
            // planar normal for native water so the reflected scene is stable.
            var h = refl_base_n;
            if (!is_water) {
                h = ggx_sample(refl_base_n, roughness, next_f32(&state), next_f32(&state));
            }
            var reflect_dir = reflect(view_dir, h);
            if (dot(reflect_dir, refl_base_n) <= 0.0) {
                reflect_dir = reflect(view_dir, refl_base_n);
            }
            // Air/water dielectric normal-incidence reflectance (IOR 1.333).
            let f0 = select(0.04, 0.0204, is_water);
            let m = 1.0 - clamp(dot(-view_dir, h), 0.0, 1.0);
            let m2 = m * m;
            let fresnel = f0 + (1.0 - f0) * (m2 * m2 * m);
            let trim_scale = select(0.28 * (1.0 - smoothstep(0.24, 0.37, roughness)), 1.0, is_water);
            let refl_max_t = select(min(params.lens.y, 180.0), min(params.lens.y, 900.0), is_water);
            let rhit = closest_hit(ray_origin, reflect_dir, bias, refl_max_t);
            let eff_max_t = select(refl_max_t, rhit.t, rhit.index != 0xffffffffu);
            var refl_col = vec3<f32>(0.0);
            if (rhit.index != 0xffffffffu) {
                let hit_pos = ray_origin + reflect_dir * rhit.t;
                let screen_col = sample_screen_radiance(hit_pos, rhit.t);
                let screen_w = clamp(screen_col.a, 0.0, 1.0);
                var lit = screen_col.rgb;
                if (screen_w < 0.99) {
                    let rmaterial = material_of(rhit.index);
                    let r_ndl = max(dot(rhit.normal, params.sun_dir.xyz), 0.0);
                    var r_shadow = 1.0;
                    if (bits_shadows && r_ndl > 0.02) {
                        let rhit_pos = hit_pos + rhit.normal * bias;
                        if (occluded(rhit_pos, params.sun_dir.xyz, bias * 0.5, params.lens.y, state ^ 0x517cc1b7u)) {
                            r_shadow = 0.0;
                        }
                    }
                    let rkind = tris[rhit.index].a.w;
                    let r_albedo = select(
                        detail_offscreen_albedo(hit_pos, rhit.normal, rmaterial.albedo.rgb, rmaterial.surface.w),
                        sample_terrain_albedo(hit_pos, rmaterial.albedo.rgb),
                        rkind > 1.5 && rkind < 2.5,
                    );
                    let offscreen_lit = r_albedo * (params.ambient.xyz * params.tuning.z + params.sun_colour.xyz * (r_ndl * r_shadow));
                    lit = mix(offscreen_lit, screen_col.rgb, screen_w);
                }
                refl_col = lit * (fresnel * trim_scale * (1.0 - fog_t));
                refl_dist = rhit.t;
            } else {
                let sky_col = sample_sky_reflection(reflect_dir);
                refl_col = sky_col * (fresnel * trim_scale * 0.65 * (1.0 - fog_t));
            }
            if (bits_lighting && pl_count > 0u && fog_t < 0.92) {
                let fx_refl = trace_effect_light_reflections(ray_origin, reflect_dir, eff_max_t, pl_count);
                refl_col = refl_col + fx_refl * (fresnel * trim_scale * (1.0 - fog_t));
            }
            // One deterministic mirror ray is enough for a flat water plane.
            // Scale it to preserve the loop's later 1/samples averaging.
            accum_reflection = accum_reflection + refl_col * select(1.0, f32(samples), is_water);
        }
        if (!is_water || s == 0u) {
            accum_refl_dist = accum_refl_dist + refl_dist * select(1.0, f32(samples), is_water);
        }

        if (bits_gi && !is_water && max_bounces >= 1u && params.tuning.w > 0.0 && gi_fade > 0.01) {
            let bounce_max_t = min(params.lens.y, 70.0);
            let day_gi_scale = 0.04 + 0.96 * sun_strength;
            var cur_origin = ray_origin;
            var cur_onb = onb;
            var cur_bias = bias;
            var path_dist = dist;
            var cur_hit_world = point;
            var cur_normal = normal;
            var throughput = vec3<f32>(1.0);
            var gi_sample = vec3<f32>(0.0);
            var gi_first_direction = vec3<f32>(0.0, 1.0, 0.0);
            var gi_first_position = vec3<f32>(0.0);
            var gi_first_is_sky = false;
            gi_total_rays = gi_total_rays + 1.0;

            for (var bounce = 0u; bounce < max_bounces; bounce = bounce + 1u) {
                // Confidence-gated radiance-cache tail: at the last secondary
                // bounce, a converged entry replaces the ray entirely.
                if (bounce > 0u && bounce + 1u == max_bounces) {
                    let cached = rc_query(cur_hit_world, cur_normal, path_dist);
                    if (cached.w >= 2.0) {
                        gi_sample = gi_sample + throughput * min(cached.rgb, vec3<f32>(2.4));
                        break;
                    }
                }
                let u1 = next_f32(&state);
                let phi = rot_angle + 1.0471975 + f32(s) * 2.39996323
                    + f32(bounce) * 1.9132 + next_f32(&state) * 0.65;
                let r = sqrt(u1);
                let d = cur_onb * vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(1.0 - u1, 0.0)));
                if (bounce == 0u) { gi_first_direction = d; }
                let bhit = closest_hit(cur_origin, d, cur_bias, bounce_max_t);
                if (bhit.index != 0xffffffffu) {
                    if (bounce == 0u) {
                        gi_first_position = cur_origin + d * bhit.t;
                        gi_first_is_sky = false;
                    }
                    let b_ndl = max(dot(bhit.normal, params.sun_dir.xyz), 0.0);
                    let bhit_world = cur_origin + d * bhit.t;
                    path_dist = path_dist + bhit.t;
                    let b_bias = max(bias, path_dist * 0.004);
                    var b_shadow = 1.0;
                    if (bits_shadows && b_ndl > 0.06) {
                        let bhit_pos = bhit_world + bhit.normal * b_bias;
                        if (occluded(bhit_pos, params.sun_dir.xyz, b_bias * 0.5, params.lens.y, state ^ (11u + bounce * 37u))) {
                            b_shadow = 0.0;
                        }
                    }
                    let bmaterial = material_of(bhit.index);
                    let bkind = tris[bhit.index].a.w;
                    let b_base_albedo = select(
                        detail_offscreen_albedo(bhit_world, bhit.normal, bmaterial.albedo.rgb, bmaterial.surface.w),
                        sample_terrain_albedo(bhit_world, bmaterial.albedo.rgb),
                        bkind > 1.5 && bkind < 2.5,
                    );
                    let b_avg = dot(b_base_albedo, vec3<f32>(0.3333));
                    let b_chroma = clamp(
                        vec3<f32>(b_avg) + (b_base_albedo - vec3<f32>(b_avg)) * 1.35,
                        vec3<f32>(0.05),
                        vec3<f32>(0.94));
                    var gi_pl_direct = vec3<f32>(0.0);
                    if (bounce < 2u && bits_lighting && pl_count > 0u) {
                        let pl_cap = min(pl_count, 8u);
                        for (var pli = 0u; pli < pl_cap; pli = pli + 1u) {
                            let pr = params.point_lights_pos_rad[pli];
                            let to_pl = pr.xyz - bhit_world;
                            if (abs(to_pl.x) >= pr.w || abs(to_pl.z) >= pr.w || abs(to_pl.y) >= pr.w) {
                                continue;
                            }
                            let r_ndl = dot(bhit.normal, to_pl);
                            if (r_ndl <= 0.0) {
                                continue;
                            }
                            let d2 = dot(to_pl, to_pl);
                            if (d2 <= 1e-4 || d2 >= pr.w * pr.w) {
                                continue;
                            }
                            let d_l = sqrt(d2);
                            let att = point_light_falloff(d_l, pr.w);
                            let ci = params.point_lights_col_int[pli];
                            gi_pl_direct = gi_pl_direct + ci.rgb * (ci.w * att * (r_ndl / d_l) * 2.0);
                        }
                        gi_pl_direct = min(gi_pl_direct, vec3<f32>(2.8));
                    }
                    let off_in = b_chroma * (
                        params.sun_colour.xyz * (b_ndl * b_shadow * 2.40)
                        + gi_pl_direct
                        + params.ambient.xyz * (params.tuning.z * 0.22));
                    var incoming = off_in;
                    if (bounce == 0u && bhit.t < 28.0 && b_ndl > 0.04) {
                        let b_screen = sample_screen_radiance(bhit_world, bhit.t);
                        let b_w = clamp(b_screen.a, 0.0, 1.0);
                        let on_in = b_screen.rgb * (b_ndl * b_shadow * 1.45 * sun_strength + 0.18 * day_gi_scale);
                        incoming = mix(off_in, on_in, b_w);
                    }
                    if (bounce + 1u == max_bounces && max_bounces >= 2u) {
                        let cached = rc_query(bhit_world, bhit.normal, path_dist);
                        if (cached.w >= 2.0) {
                            incoming = incoming + b_chroma * min(cached.rgb, vec3<f32>(2.0)) * 0.45;
                        }
                    }
                    if (bounce == 0u && luminance(incoming) > 0.02 && ((id.x + id.y + u32(params.frame.x)) & 3u) == 0u) {
                        rc_update(bhit_world, bhit.normal, path_dist, incoming);
                    }
                    gi_sample = gi_sample + throughput * min(incoming, vec3<f32>(2.8));
                    throughput = throughput * clamp(b_chroma * 1.18, vec3<f32>(0.12), vec3<f32>(0.92));
                    if (max(throughput.r, max(throughput.g, throughput.b)) < 0.04) {
                        break;
                    }
                    cur_origin = bhit_world + bhit.normal * b_bias;
                    cur_onb = orthonormal_basis(bhit.normal);
                    cur_bias = b_bias;
                    cur_hit_world = bhit_world;
                    cur_normal = bhit.normal;
                } else {
                    if (bounce == 0u) {
                        gi_sky_hits = gi_sky_hits + 1.0;
                        gi_first_position = d;
                        gi_first_is_sky = true;
                    }
                    let raw_sky = min(sample_sky_env(d), vec3<f32>(1.25));
                    var vivid_sky = raw_sky;
                    if (params.sun_dir.y > 0.01 && params.sun_dir.y < 0.42 && sun_strength > 0.05) {
                        let golden_w = 1.0 - smoothstep(0.10, 0.42, params.sun_dir.y);
                        let sun_align = dot(
                            normalize(vec3<f32>(d.x, 0.0, d.z) + vec3<f32>(1e-5, 0.0, 0.0)),
                            normalize(vec3<f32>(params.sun_dir.x, 0.0, params.sun_dir.z) + vec3<f32>(1e-5, 0.0, 0.0)),
                        );
                        let warm_cool = mix(vec3<f32>(0.76, 0.84, 1.22), vec3<f32>(1.26, 0.94, 0.68), sun_align * 0.5 + 0.5);
                        vivid_sky = vivid_sky * mix(vec3<f32>(1.0), warm_cool, golden_w * 0.55);
                    }
                    let sky_gain = select(0.34, 0.36, bounce == 0u);
                    gi_sample = gi_sample + throughput * vivid_sky * (sky_gain * day_gi_scale);
                    break;
                }
            }
            gi_sample = gi_sample * (params.tuning.w * 1.25 * (1.0 - fog_t));
            let gi_candidate = min(gi_sample * gi_fade, vec3<f32>(3.2));
            if (params.restir.x > 0.5) {
                let candidate_cosine = max(dot(normal, gi_first_direction), 0.001);
                let candidate_target = max(luminance(gi_candidate) * candidate_cosine, 1e-5);
                let candidate_weight = candidate_target / candidate_cosine;
                restir_update(
                    &restir_current, gi_first_position, gi_candidate,
                    candidate_weight, candidate_weight, 1.0, gi_first_is_sky,
                    point, normal, &state
                );
            } else {
                accum_gi = accum_gi + gi_candidate;
            }
        }
    }

    // Feature #4: Backlit foliage SSS + translucent canopy light
    var sss_light = vec3<f32>(0.0);
    if (bits_shadows && !is_water && fog_t < 0.92 && sun_strength > 0.05) {
        let avg_trans = accum_trans / f32(samples);
        if (dot(avg_trans, vec3<f32>(1.0)) > 1e-4) {
            sss_light = sss_light + avg_trans * params.sun_colour.xyz * (n_dot_l * 0.55 * (1.0 - fog_t));
        }
        let back_dot = clamp(dot(view_dir, params.sun_dir.xyz), 0.0, 1.0);
        let is_foliage_col = (raster_rgb.g > raster_rgb.b * 1.10 && raster_rgb.g > 0.10)
            || (raster_rgb.r > raster_rgb.g * 1.15 && raster_rgb.b > raster_rgb.g * 1.02);
        if (is_foliage_col && back_dot > 0.35 && n_dot_l < 0.35) {
            let b2 = back_dot * back_dot;
            sss_light = sss_light + params.sun_colour.xyz * (b2 * b2 * 0.22 * sun_strength * (1.0 - fog_t));
        }
    }

    // Sun/moon volumetric shafts are intentionally removed. Local point-light halos remain.
    var point_light_halos = vec3<f32>(0.0);

    // Enhancement #18: Water-to-Wall caustic indirect bounce onto bridges, piers & cliffs.
    // Enhancement #3: Subtle, smooth edge-free Volumetric Light Halos (no camera-light orb, no balloon).
    if (bits_lighting && pl_count > 0u && dist > 3.0 && fog_t < 0.95) {
        let dark_fog_w = max(0.45 * (1.0 - sun_strength), fog_t * 0.65);
        if (dark_fog_w > 0.12) {
            let max_h_t = min(dist, 85.0);
            let halo_cap = min(pl_count, 32u);
            var halo_sum = vec3<f32>(0.0);
            var best_h_score = 0.0;
            var best_h_pos = vec3<f32>(0.0);
            var best_h_col = vec3<f32>(0.0);
            for (var hi = 0u; hi < halo_cap; hi = hi + 1u) {
                let pr = params.point_lights_pos_rad[hi];
                if (pr.w <= 1.5) {
                    continue;
                }
                let to_l = pr.xyz - params.eye.xyz;
                let l_dist2 = dot(to_l, to_l);
                if (l_dist2 < 12.25) {
                    continue;
                }
                let t_p = dot(to_l, view_dir);
                if (t_p <= 2.5 || t_p >= max_h_t) {
                    continue;
                }
                let d2 = max(l_dist2 - t_p * t_p, 0.0);
                let r_h = clamp(pr.w * 0.10, 0.7, 3.2);
                let r2_out = r_h * r_h * 4.0;
                if (d2 >= r2_out) {
                    continue;
                }
                let ci = params.point_lights_col_int[hi];
                let u = clamp(d2 / max(r2_out, 0.25), 0.0, 1.0);
                let w_edge = 1.0 - u;
                let scatter = (w_edge * w_edge * w_edge) * exp(-u * 3.5) / (1.0 + t_p * 0.04);
                let contrib = ci.rgb * (ci.w * scatter * 0.014);
                let sc = luminance(contrib);
                if (sc > best_h_score) {
                    best_h_score = sc;
                    best_h_pos = params.eye.xyz + view_dir * t_p;
                    best_h_col = contrib;
                } else {
                    halo_sum = halo_sum + contrib * 0.55;
                }
            }
            if (best_h_score > 1e-4) {
                var vis_h = 1.0;
                if (bits_shadows && occluded(params.eye.xyz, view_dir, 0.5, max(length(best_h_pos - params.eye.xyz) - 0.5, 0.6), state ^ 97u)) {
                    vis_h = 0.0;
                }
                halo_sum = halo_sum + best_h_col * vis_h;
            }
            point_light_halos = point_light_halos + min(halo_sum * dark_fog_w, vec3<f32>(0.12));
        }
    }

    // Feature #5: ReSTIR DI (2-Reservoir RIS) across up to 128 point lights & world effects
    let debug_mode = u32(round(params.counts.w));
    let pl_shadows_enabled = params.point_light_meta.y > 0.5;
    var point_lighting = vec3<f32>(0.0);
    var pl_debug_lit = vec3<f32>(0.0);
    var pl_debug_shadow = 0.0;
    var pl_w_total = 0.0;
    var pl_w_lit = 0.0;
    if (bits_lighting && pl_count > 0u) {
        var any_pl_near = false;
        for (var pre_i = 0u; pre_i < pl_count; pre_i = pre_i + 1u) {
            let pr = params.point_lights_pos_rad[pre_i];
            let d_pre = pr.xyz - point;
            if (abs(d_pre.x) < pr.w && abs(d_pre.z) < pr.w && abs(d_pre.y) < pr.w && dot(d_pre, d_pre) < pr.w * pr.w) {
                any_pl_near = true;
                break;
            }
        }
        var pl_normal = normal;
        if (any_pl_near) {
            let gdim_pl = vec2<i32>(i32(params.extents.x), i32(params.extents.y));
            let cpx = vec2<i32>(id.xy);
            if (dist < 18.0) {
                let d_thresh = clamp(dist * 0.024, 0.08, 0.55);
                var offs12 = array<vec2<i32>, 12>(
                    vec2<i32>(1, 0), vec2<i32>(-1, 0), vec2<i32>(0, 1), vec2<i32>(0, -1),
                    vec2<i32>(2, 1), vec2<i32>(-2, 1), vec2<i32>(2, -1), vec2<i32>(-2, -1),
                    vec2<i32>(1, 2), vec2<i32>(-1, 2), vec2<i32>(1, -2), vec2<i32>(-1, -2)
                );
                var n_acc = normal * 0.65;
                for (var oi = 0u; oi < 12u; oi = oi + 1u) {
                    let npx = clamp(cpx + offs12[oi], vec2<i32>(0), gdim_pl - vec2<i32>(1));
                    let nd = textureLoad(src_gbuf_pos, npx, 0).w;
                    if (nd > 0.0 && abs(nd - dist) < d_thresh) {
                        var nn = textureLoad(src_gbuf_norm, npx, 0).xyz;
                        let nn_l = length(nn);
                        if (nn_l > 1e-4) {
                            nn = nn / nn_l;
                            if (dot(normal, nn) > 0.36) {
                                n_acc = n_acc + nn;
                            }
                        }
                    }
                }
                let acc_l = length(n_acc);
                if (acc_l > 1e-4) {
                    pl_normal = n_acc / acc_l;
                }
            } else {
                let d_thresh = max(0.80, dist * 0.026);
                var offs = array<vec2<i32>, 8>(
                    vec2<i32>(2, 0), vec2<i32>(-2, 0), vec2<i32>(0, 2), vec2<i32>(0, -2),
                    vec2<i32>(4, 4), vec2<i32>(-4, 4), vec2<i32>(4, -4), vec2<i32>(-4, -4)
                );
                var n_acc = normal * 1.25;
                for (var oi = 0u; oi < 8u; oi = oi + 1u) {
                    let npx = clamp(cpx + offs[oi], vec2<i32>(0), gdim_pl - vec2<i32>(1));
                    let nd = textureLoad(src_gbuf_pos, npx, 0).w;
                    if (nd > 0.0 && abs(nd - dist) < d_thresh) {
                        var nn = textureLoad(src_gbuf_norm, npx, 0).xyz;
                        let nn_l = length(nn);
                        if (nn_l > 1e-4) {
                            nn = nn / nn_l;
                            if (dot(normal, nn) > 0.56) {
                                n_acc = n_acc + nn;
                            }
                        }
                    }
                }
                let acc_l = length(n_acc);
                if (acc_l > 1e-4) {
                    pl_normal = n_acc / acc_l;
                }
            }
        }
        let pl_terrain_skip = 2.4 + steepness * 12.0 + dist * 0.020;

        var res0_idx = 0xffffffffu;
        var res0_wsum = 0.0;
        var res0_phat = 0.0;
        var res0_col = vec3<f32>(0.0);
        var res0_w = 0.0;

        var res1_idx = 0xffffffffu;
        var res1_wsum = 0.0;
        var res1_phat = 0.0;
        var res1_col = vec3<f32>(0.0);
        var res1_w = 0.0;

        var cand_count = 0u;
        var exact_idxs = array<u32, 4>(0xffffffffu, 0xffffffffu, 0xffffffffu, 0xffffffffu);
        var exact_ws = array<f32, 4>(0.0, 0.0, 0.0, 0.0);
        var exact_cols = array<vec3<f32>, 4>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));

        let pl_loop_end = select(0u, pl_count, any_pl_near);
        for (var li = 0u; li < pl_loop_end; li = li + 1u) {
            let pl_pr = params.point_lights_pos_rad[li];
            let to_l = pl_pr.xyz - point;
            if (abs(to_l.x) >= pl_pr.w || abs(to_l.z) >= pl_pr.w || abs(to_l.y) >= pl_pr.w) {
                continue;
            }
            let pl_d2 = dot(to_l, to_l);
            if (pl_d2 <= 1e-6 || pl_d2 >= pl_pr.w * pl_pr.w) {
                continue;
            }
            let pl_dist = sqrt(pl_d2);
            let l_dir = to_l / pl_dist;
            let raw_cos = dot(pl_normal, l_dir);
            if (raw_cos <= -0.28) {
                continue;
            }
            let pl_ndl = clamp((raw_cos + 0.32) / 1.32, 0.0, 1.0);
            if (pl_ndl <= 0.001) {
                continue;
            }
            let atten = point_light_falloff(pl_dist, pl_pr.w);
            if (atten <= 1e-5) {
                continue;
            }
            let pl_ci = params.point_lights_col_int[li];
            let w = pl_ci.w * atten * pl_ndl;
            let p_hat = max(luminance(pl_ci.rgb), 0.05) * w;
            if (p_hat <= 1e-6) {
                continue;
            }
            if (cand_count < 4u) {
                exact_idxs[cand_count] = li;
                exact_ws[cand_count] = w;
                exact_cols[cand_count] = pl_ci.rgb;
            }
            cand_count = cand_count + 1u;
            res0_wsum = res0_wsum + p_hat;
            if (next_f32(&state) * res0_wsum <= p_hat) {
                res0_idx = li;
                res0_phat = p_hat;
                res0_col = pl_ci.rgb;
                res0_w = w;
            }
            res1_wsum = res1_wsum + p_hat;
            if (next_f32(&state) * res1_wsum <= p_hat) {
                res1_idx = li;
                res1_phat = p_hat;
                res1_col = pl_ci.rgb;
                res1_w = w;
            }
        }

        if (cand_count > 0u && cand_count <= 4u) {
            for (var ci = 0u; ci < cand_count; ci = ci + 1u) {
                let idx = exact_idxs[ci];
                let pl_pr = params.point_lights_pos_rad[idx];
                let to_l = pl_pr.xyz - point;
                let pl_d2 = dot(to_l, to_l);
                let inv_pl_dist = inverseSqrt(max(pl_d2, 1e-10));
                let pl_dist = pl_d2 * inv_pl_dist;
                let l_dir = to_l * inv_pl_dist;
                var pl_vis = 1.0;
                if (bits_shadows && pl_dist > 1.8 && exact_ws[ci] > 0.025) {
                    let r_emit = clamp(pl_pr.w * 0.032, 0.16, 1.35);
                    let l_onb = orthonormal_basis(l_dir);
                    let ang = rot_angle + f32(ci) * 2.39996323;
                    let jitter_pt = pl_pr.xyz + l_onb * vec3<f32>(cos(ang), sin(ang), 0.0) * (r_emit * sqrt(next_f32(&state)));
                    let j_vec = jitter_pt - ray_origin;
                    let j_dist = length(j_vec);
                    let self_clear = clamp(pl_pr.w * 0.08, 1.4, 3.2);
                    if (j_dist > self_clear + bias && occluded_point_light(ray_origin, j_vec / max(j_dist, 1e-4), bias * 0.5, j_dist - self_clear, pl_terrain_skip, state ^ (idx * 131u + 17u))) {
                        pl_vis = 0.0;
                    }
                }
                pl_w_total = pl_w_total + exact_ws[ci];
                pl_w_lit = pl_w_lit + exact_ws[ci] * pl_vis;
                pl_debug_lit = pl_debug_lit + exact_cols[ci] * (exact_ws[ci] * pl_vis);
                pl_debug_shadow = pl_debug_shadow + exact_ws[ci] * (1.0 - pl_vis);
                point_lighting = point_lighting + exact_cols[ci] * (exact_ws[ci] * pl_vis * 2.4);
            }
        } else if (cand_count > 4u) {
            var r_idxs = array<u32, 2>(res0_idx, res1_idx);
            var r_wsums = array<f32, 2>(res0_wsum, res1_wsum);
            var r_phats = array<f32, 2>(res0_phat, res1_phat);
            var r_ws = array<f32, 2>(res0_w, res1_w);
            var r_cols = array<vec3<f32>, 2>(res0_col, res1_col);
            for (var ri = 0u; ri < 2u; ri = ri + 1u) {
                let idx = r_idxs[ri];
                if (idx == 0xffffffffu || r_phats[ri] <= 1e-6) {
                    continue;
                }
                let pl_pr = params.point_lights_pos_rad[idx];
                let to_l = pl_pr.xyz - point;
                let pl_d2 = dot(to_l, to_l);
                let inv_pl_dist = inverseSqrt(max(pl_d2, 1e-10));
                let pl_dist = pl_d2 * inv_pl_dist;
                let l_dir = to_l * inv_pl_dist;
                var pl_vis = 1.0;
                if (bits_shadows && pl_dist > 1.8) {
                    let r_emit = clamp(pl_pr.w * 0.032, 0.16, 1.35);
                    let l_onb = orthonormal_basis(l_dir);
                    let ang = rot_angle + f32(ri) * 2.39996323 + 1.1;
                    let jitter_pt = pl_pr.xyz + l_onb * vec3<f32>(cos(ang), sin(ang), 0.0) * (r_emit * sqrt(next_f32(&state)));
                    let j_vec = jitter_pt - ray_origin;
                    let j_dist = length(j_vec);
                    let self_clear = clamp(pl_pr.w * 0.08, 1.4, 3.2);
                    if (j_dist > self_clear + bias && occluded_point_light(ray_origin, j_vec / max(j_dist, 1e-4), bias * 0.5, j_dist - self_clear, pl_terrain_skip, state ^ (idx * 131u + 29u))) {
                        pl_vis = 0.0;
                    }
                }
                let ris_scale = min(r_wsums[ri] / max(r_phats[ri], 1e-6), 8.0) * 0.5;
                let eff_w = r_ws[ri] * ris_scale;
                pl_w_total = pl_w_total + eff_w;
                pl_w_lit = pl_w_lit + eff_w * pl_vis;
                pl_debug_lit = pl_debug_lit + r_cols[ri] * (eff_w * pl_vis);
                pl_debug_shadow = pl_debug_shadow + eff_w * (1.0 - pl_vis);
                point_lighting = point_lighting + r_cols[ri] * (eff_w * pl_vis * 2.4);
            }
        }
        let pl_peak = max(point_lighting.r, max(point_lighting.g, point_lighting.b));
        if (pl_peak > 1.35) {
            let comp = 1.35 + (pl_peak - 1.35) / (1.0 + (pl_peak - 1.35) * 0.65);
            point_lighting = point_lighting * (comp / pl_peak);
        }
        point_lighting = min(point_lighting * (1.0 - fog_t), vec3<f32>(2.8));
        if (bits_gi && dist > 8.5 && luminance(point_lighting) > 0.08 && ((id.x + id.y + u32(params.frame.x)) & 7u) == 0u) {
            rc_update(point, pl_normal, dist, point_lighting * 0.55);
        }
    }
    let point_vis = select(1.0, pl_w_lit / max(pl_w_total, 1e-5), pl_w_total > 1e-4);
    let point_shadow_mod = select(1.0, 1.0 - clamp(pl_w_total, 0.0, 0.5) * (1.0 - point_vis), pl_shadows_enabled && pl_w_total > 1e-4);

    if (params.restir.x > 0.5 && bits_gi && max_bounces >= 1u && params.tuning.w > 0.0 && gi_fade > 0.01) {
        restir_reuse_history(
            &restir_current, point, normal, ray_origin, bias, dist,
            id.xy, extent, previous_base, &state
        );
        let selected_sky = restir_current.receiver_normal_flags.w < 0.0;
        var selected_dir = vec3<f32>(0.0, 1.0, 0.0);
        if (selected_sky) {
            selected_dir = normalize(restir_current.sample_pos_weight.xyz);
        } else {
            selected_dir = normalize(restir_current.sample_pos_weight.xyz - point);
        }
        let selected_cosine = max(dot(normal, selected_dir), 0.0);
        let selected_target = luminance(restir_current.radiance_m.xyz) * selected_cosine;
        if (restir_current.radiance_m.w > 0.0 && selected_target > 1e-5) {
            let normalisation = restir_current.sample_pos_weight.w
                / max(restir_current.radiance_m.w * selected_target, 1e-5);
            // The path sample stores the cosine-weighted Monte Carlo estimate
            // (Li * BRDF / pdf), which has the receiver cosine cancelled by the
            // cosine-hemisphere proposal. ReSTIR's target still contains cos(theta),
            // so restore it in the selected integrand before dividing by that target.
            let gi_estimate = restir_current.radiance_m.xyz * normalisation;
            accum_gi = gi_estimate * (f32(samples) * selected_cosine);
        } else {
            accum_gi = vec3<f32>(0.0);
        }
        restir_reservoirs[reservoir_index] = restir_current;
    } else {
        restir_clear(reservoir_index);
    }

    let inv = 1.0 / f32(samples);
    let avg_shadow = accum_shadow * inv;
    let avg_open = accum_open * inv;
    let avg_refl = accum_reflection * inv + point_light_halos;
    let bent_normal = select(normal, normalize(accum_bent_n), dot(accum_bent_n, accum_bent_n) > 1e-4);
    let avg_refl_dist = accum_refl_dist * inv;
    var avg_gi = accum_gi * inv + sss_light;

    // Enhancement #8: Ray-Traced Moonlit Night Sky & Starlight Hemispherical Bounce.
    if (bits_moonlit_night && !is_water && sun_strength < 0.35 && fog_t < 0.94) {
        let night_w = 1.0 - smoothstep(0.05, 0.35, sun_strength);
        let sky_up = clamp(bent_normal.y * 0.65 + 0.35, 0.0, 1.0);
        let moon_align = clamp(dot(bent_normal, params.sun_dir.xyz), 0.0, 1.0) * 0.55 + 0.45;
        let night_sky = min(sample_sky_env(bent_normal), vec3<f32>(0.45, 0.55, 0.75));
        let moon_bounce = mix(vec3<f32>(0.035, 0.055, 0.105), night_sky * 0.35 + vec3<f32>(0.03, 0.05, 0.09), 0.5);
        let open_gate = avg_open * avg_open;
        avg_gi = avg_gi + moon_bounce * (night_w * sky_up * moon_align * open_gate * (1.0 - fog_t));
    }

    // Enhancement #19: Ray-Traced Shadowed Rain & Snow Lighting.
    if (bits_weather && dist < 80.0) {
        let precip_luma = luminance(raster_rgb);
        if (precip_luma > 0.45 && abs(normal.y) < 0.75) {
            let p_vis = select(1.0, avg_shadow, bits_shadows);
            let pl_glow = point_lighting * 0.45;
            avg_gi = avg_gi + (params.sun_colour.xyz * (p_vis * sun_strength * 0.14) + pl_glow) * (1.0 - clamp(dist / 80.0, 0.0, 1.0));
        }
    }

    var out_rgb = vec3<f32>(0.0);
    var out_lf = 1.0;
    if (layered) {
        let sun_e = max(luminance(params.sun_colour.xyz), 0.0) * n_dot_l;
        let amb_e = max(luminance(params.ambient.xyz) * params.tuning.z, 0.005);
        let sun_f = clamp(sun_e / max(sun_e + amb_e, 1e-4), 0.0, 1.0);
        let amb_f = 1.0 - sun_f;
        let amb_base = max(amb_f, mix(0.32, 0.42, sun_strength));
        let sun_base = 1.0 - amb_base;
        let v_sun = select(1.0, avg_shadow, bits_shadows);
        // Enhancement #7 & #16: Bent-normal sky alignment + deep interior sky-portal falloff.
        let sky_portal_vis = select(1.0, mix(0.76, 1.0, gi_sky_hits / max(gi_total_rays, 1.0)), gi_total_rays > 0.5);
        let bent_sky = mix(0.92, 1.0, clamp(bent_normal.y * 0.5 + 0.5, 0.0, 1.0));
        let v_ao = select(1.0, avg_open * bent_sky * sky_portal_vis, bits_ao);
        let gi_active = bits_gi && !is_water && max_bounces >= 1u && params.tuning.w > 0.0 && gi_fade > 0.01;
        let amb_keep = select(1.0, mix(1.0, 0.30, gi_fade), gi_active);
        let night_dark_scale = mix(0.28, 1.0, smoothstep(0.0, 0.25, sun_strength));
        let separated_lf = (amb_base * v_ao * amb_keep * night_dark_scale + sun_base * v_sun) * point_shadow_mod;
        out_lf = mix(separated_lf, 1.0, fog_t);
        // The raster base is already fogged by the client shader. Attenuate every
        // additive RT contribution by the same eye-distance transmittance or GI
        // and point lights remain visible beyond the client's fog end.
        out_rgb = (avg_gi + point_lighting) * (1.0 - fog_t);
    } else {
        let direct = select(vec3<f32>(0.0), params.sun_colour.xyz * (n_dot_l * avg_shadow), bits_lighting);
        let ambient = params.ambient.xyz * (avg_open * params.tuning.z);
        let relit = raster_rgb * (direct + ambient + avg_gi + point_lighting) + point_light_halos;
        out_rgb = mix(relit, params.fog_colour.xyz, fog_t);
        out_lf = 1.0;
    }

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
        out_rgb = pl_debug_lit * 2.0 + vec3<f32>(0.55, 0.04, 0.14) * clamp(pl_debug_shadow * 2.5, 0.0, 1.0);
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    } else if (debug_mode == 7u) {
        var occ_kind = 0.0;
        if (is_water) {
            occ_kind = 1.0;
        } else {
            let ph = closest_hit(params.eye.xyz, view_dir, 0.1, params.lens.y);
            if (ph.index != 0xffffffffu) {
                occ_kind = tris[ph.index].a.w;
            }
        }
        out_rgb = kind_colour(occ_kind);
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    } else if (debug_mode == 8u) {
        var rkind = 0.0;
        if (roughness <= params.effects.x) {
            let rdir = reflect(view_dir, normal);
            let rh = closest_hit(ray_origin, rdir, bias, params.lens.y);
            if (rh.index != 0xffffffffu) {
                rkind = tris[rh.index].a.w;
            }
        }
        out_rgb = kind_colour(rkind);
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    } else if (debug_mode == 9u) {
        let sun_e = max(luminance(params.sun_colour.xyz), 0.0) * n_dot_l;
        let amb_e = max(luminance(params.ambient.xyz) * params.tuning.z, 0.05);
        let sun_f = clamp(sun_e / max(sun_e + amb_e, 1e-4), 0.0, 1.0);
        out_rgb = vec3<f32>(sun_f, avg_shadow, sun_f * avg_shadow);
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    } else if (debug_mode == 10u) {
        // Foliage debug: visualise tree/leaf geometry and alpha-mask coverage.
        //   R = primary hit alpha coverage (1 = solid leaf, 0 = no leaf)
        //   G = shadow occluder foliage (1 = leaf blocks sun)
        //   B = reflection-hit foliage (1 = reflection hits leaf)
        var primary_leaf = 0.0;
        var shadow_leaf = 0.0;
        var refl_leaf = 0.0;

        let ph = closest_hit(params.eye.xyz, view_dir, 0.1, params.lens.y);
        if (ph.index != 0xffffffffu) {
            let raw_lo = bitcast<u32>(tris[ph.index].c.w);
            let raw_hi = bitcast<u32>(tris[ph.index].n.w);
            if ((raw_lo | raw_hi) != 0u) {
                let lo25 = unpack_alpha_mask25(tris[ph.index].c.w);
                let hi24 = unpack_alpha_mask25(tris[ph.index].n.w);
                var total_bits = 0u;
                for (var b = 0u; b < 25u; b = b + 1u) {
                    total_bits = total_bits + ((lo25 >> b) & 1u);
                }
                for (var b = 0u; b < 24u; b = b + 1u) {
                    total_bits = total_bits + ((hi24 >> b) & 1u);
                }
                primary_leaf = clamp(f32(total_bits) / 49.0, 0.0, 1.0);
            }
        }

        if (bits_shadows && n_dot_l > 0.02) {
            let sh = closest_hit(point + normal * bias, params.sun_dir.xyz, bias * 0.5, params.lens.y);
            if (sh.index != 0xffffffffu) {
                let s_lo = bitcast<u32>(tris[sh.index].c.w);
                let s_hi = bitcast<u32>(tris[sh.index].n.w);
                if ((s_lo | s_hi) != 0u) {
                    shadow_leaf = 1.0;
                }
            }
        }

        if (roughness <= params.effects.x) {
            let rdir = reflect(view_dir, normal);
            let rh = closest_hit(ray_origin, rdir, bias, params.lens.y);
            if (rh.index != 0xffffffffu) {
                let r_lo = bitcast<u32>(tris[rh.index].c.w);
                let r_hi = bitcast<u32>(tris[rh.index].n.w);
                if ((r_lo | r_hi) != 0u) {
                    refl_leaf = 1.0;
                }
            }
        }

        out_rgb = vec3<f32>(primary_leaf, shadow_leaf, refl_leaf);
        out_lf = 1.0;
        store_refl = vec3<f32>(0.0);
    }

    let spec_f0 = select(select(0.02, 0.06, is_glossy_trim), 0.18, is_water);
    if (is_water && debug_mode_early == 0u) {
        out_rgb = vec3<f32>(0.0);
        out_lf = 1.0;
    }
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
    inv_vp: mat4x4<f32>,
    prev_vp: mat4x4<f32>,
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

// Phase 7: Reproject using explicit object motion where written (`mv.z >= 0.5`,
// e.g. weather_gpu), and full-frame `prev_vp * inv_vp` camera reprojection for
// all static world geometry (terrain, buildings, models, trees, water). Rejects
// disocclusions with a strict 5% relative depth threshold against `prev_ndc.z`.
fn reproject(pixel: vec2<i32>, size: vec2<i32>, current_depth: f32) -> ReprojectResult {
    var empty: ReprojectResult;
    empty.colour = vec3<f32>(0.0);
    empty.light_factor = 1.0;
    empty.moments = vec2<f32>(0.0);
    empty.length = 0.0;

    if (current_depth <= 0.0) {
        return empty;
    }

    // Opt #15: Skip full-screen src_motion fetch when no dynamic weather/object MVs were written.
    var source = vec2<f32>(pixel) + vec2<f32>(0.5);
    var expected_prev_depth = current_depth;
    var used_obj_mv = false;
    if (params.misc.w < 1.5) {
        let mv = textureLoad(src_motion, pixel, 0);
        if (mv.z >= 0.5) {
            source = source - mv.xy;
            used_obj_mv = true;
        }
    }
    if (!used_obj_mv) {
        let uv = (vec2<f32>(pixel) + vec2<f32>(0.5)) / vec2<f32>(size);
        let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        // `params.prev_vp` holds pre-multiplied `prev_vp * inv_vp` (1 mat4 mul instead of 2).
        let prev_clip = params.prev_vp * vec4<f32>(ndc, current_depth, 1.0);
        if (abs(prev_clip.w) <= 1e-5) {
            return empty;
        }
        let prev_ndc = prev_clip.xyz / prev_clip.w;
        let prev_uv = vec2<f32>(prev_ndc.x * 0.5 + 0.5, 0.5 - prev_ndc.y * 0.5);
        source = prev_uv * vec2<f32>(size);
        expected_prev_depth = prev_ndc.z;
    }

    if (source.x < 0.0 || source.y < 0.0 || source.x >= f32(size.x) || source.y >= f32(size.y)) {
        return empty;
    }
    let index = vec2<i32>(source);
    let prev_m = textureLoad(history_moments, index, 0);
    let previous_depth = prev_m.z;
    if (previous_depth <= 0.0 || expected_prev_depth <= 0.0) {
        return empty;
    }
    if (abs(previous_depth - expected_prev_depth) / max(expected_prev_depth, 1e-3) > 0.038) {
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
    // Boost #15: Skip loading src_reflection on matte land pixels (saves 8 RGBA16F loads/px).
    let is_glossy = textureLoad(src_normals, gp, 0).w <= 0.12;
    var current = cur_raw.rgb;
    if (is_glossy) {
        current = current + textureLoad(src_reflection, gp, 0).rgb;
    }
    let current_lf = cur_raw.a;
    let depth = textureLoad(src_depth, pixel, 0);
    if (depth <= 1e-6) {
        textureStore(out_colour, pixel, vec4<f32>(current, current_lf));
        textureStore(out_moments, pixel, vec4<f32>(0.0, 0.0, depth, 1.0));
        return;
    }
    // Boost #16: Pass already-loaded depth into reproject (saves 1 full-screen Depth32F fetch).
    let prev = reproject(pixel, size, depth);
    let n = min(prev.length, params.misc.z);
    // Opt: Sky pixels (depth <= 1e-6) skip the 3x3 16-fetch SVGF neighborhood loop
    // (atrous_pass skips sky too), passing volumetric clouds straight to composite.
    if (depth <= 1e-6) {
        textureStore(out_colour, pixel, vec4<f32>(current, current_lf));
        textureStore(out_moments, pixel, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        return;
    }
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
                let tap_is_glossy = textureLoad(src_normals, p, 0).w <= 0.12;
                var c = raw.rgb;
                if (tap_is_glossy) {
                    c = c + textureLoad(src_reflection, p, 0).rgb;
                }
                sum = sum + c;
                sum_sq = sum_sq + c * c;
                sum_lf = sum_lf + raw.a;
                sum_lf_sq = sum_lf_sq + raw.a * raw.a;
            }
        }
        let inv9 = 1.0 / 9.0;
        let mean = sum * inv9;
        let stddev = sqrt(max(sum_sq * inv9 - mean * mean, vec3<f32>(0.0))) * params.weights.w;
        // Water reflections are stochastic one-sample signals. Clamping their
        // reprojected history to the current 3x3 sample neighbourhood discards
        // valid temporal energy whenever a reflected edge crosses that kernel,
        // which reads as reflection popping. Keep valid water history; land
        // lighting continues to use the variance clamp.
        let history = select(
            clamp(prev.colour, mean - stddev, mean + stddev),
            prev.colour,
            is_glossy,
        );
        let current_clamped = min(current, mean * 1.75 + vec3<f32>(0.12));
        let mean_lf = sum_lf * inv9;
        let std_lf = sqrt(max(sum_lf_sq * inv9 - mean_lf * mean_lf, 0.0)) * params.weights.w;
        let history_lf = clamp(prev.light_factor, mean_lf - std_lf, mean_lf + std_lf);
        let alpha = max(1.0 / (n + 1.0), params.extent.w);
        blended = mix(history, current_clamped, alpha);
        blended_lf = clamp(mix(history_lf, current_lf, alpha), 0.0, 1.0);
        length = min(n + 1.0, params.misc.z);
        let lum = luminance(blended);
        m1 = mix(prev.moments.x, lum, alpha);
        m2 = mix(prev.moments.y, lum * lum, alpha);
    } else {
        // SVGF spatial variance fallback on disoccluded pixels (history_len == 1):
        // estimate luminance moments from 3x3 ray-grid neighborhood so à-trous immediately
        // filters disocclusion noise cleanly without waiting 4 frames.
        var l_sum = m1;
        var l2_sum = m2;
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                if (dx == 0 && dy == 0) {
                    continue;
                }
                let p = clamp(gp + vec2<i32>(dx, dy), vec2<i32>(0), gmax);
                let raw = textureLoad(src_colour, p, 0);
                let tap_is_glossy = textureLoad(src_normals, p, 0).w <= 0.12;
                var c = raw.rgb;
                if (tap_is_glossy) {
                    c = c + textureLoad(src_reflection, p, 0).rgb;
                }
                let l = luminance(c);
                l_sum = l_sum + l;
                l2_sum = l2_sum + l * l;
            }
        }
        m1 = l_sum * (1.0 / 9.0);
        m2 = l2_sum * (1.0 / 9.0);
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
    // Opt #17: src_normals is already unit-length from gbuf_prep/trace_pass (zero sqrt/div in 3x3 loop).
    let gp = vec2<i32>(clamp((vec2<f32>(pixel) + vec2<f32>(0.5)) * grid_scale, vec2<f32>(0.0), gmax));
    return textureLoad(src_normals, gp, 0).xyz;
}

fn kernel_weight(d: i32) -> f32 {
    if (d == 0) { return 0.5; }
    return 0.25;
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
    if (centre_depth <= 1e-6) {
        textureStore(out_colour, pixel, textureLoad(history_colour, pixel, 0));
        textureStore(out_moments, pixel, textureLoad(history_moments, pixel, 0));
        return;
    }
    let centre_normal = normal_at_scaled(pixel, grid_scale, gmax);
    let centre_moments = textureLoad(history_moments, pixel, 0);
    let centre_variance = variance_of(centre_moments);
    // History-length adaptive filtering: when camera rotates and pixels are newly
    // disoccluded (`history_len < 4`), relax luminance & light-factor edge weights
    // within strict depth/normal bounds to eliminate rotation noise, then snap
    // razor-sharp as soon as temporal history converges.
    let disocc = 1.0 - clamp((centre_moments.w - 1.0) / 3.0, 0.0, 1.0);
    let inv_sigma = (1.0 / max(params.weights.x * sqrt(centre_variance), 1e-6)) * mix(0.35, 0.18, disocc);
    let lf_edge_k = mix(4.5, 2.5, disocc);
    let inv_centre_depth = 1.0 / max(centre_depth, 1e-3);
    let inv_depth_denom = 1.0 / max(params.weights.z * max(abs(centre_depth), 1e-3), 1e-6);
    let centre_raw = textureLoad(history_colour, pixel, 0);
    let centre = centre_raw.rgb;
    let centre_lum = luminance(centre);

    // Fast 3x3 à-trous kernel (2.78x fewer texture fetches than 5x5).
    let w0 = 0.25;
    var sum = centre * w0;
    var weight_sum = w0;
    var sum_lf = centre_raw.a * w0;
    var weight_sum_lf = w0;
    var moment1 = centre_moments.x * w0;
    var moment2 = centre_moments.y * w0;
    let step = i32(params.misc.x);
    let filter_lf = step <= 2;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        let wy = kernel_weight(dy);
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            if (dx == 0 && dy == 0) {
                continue;
            }
            let p = pixel + vec2<i32>(dx * step, dy * step);
            if (p.x < 0 || p.y < 0 || p.x >= size.x || p.y >= size.y) {
                continue;
            }
            let d1 = textureLoad(src_depth, p, 0);
            if (d1 <= 0.0 || abs(centre_depth - d1) * inv_centre_depth > 0.05) {
                continue;
            }
            // Opt #18: Fast rational decay 1/(1+x+0.5*x^2) replaces transcendental exp() in 3x3 à-trous loop.
            let xd = abs(centre_depth - d1) * inv_depth_denom;
            let wd = 1.0 / (1.0 + xd + 0.5 * xd * xd);
            if (wd < 1e-4) {
                continue;
            }
            let n1 = normal_at_scaled(p, grid_scale, gmax);
            let ndot = dot(centre_normal, n1);
            // Zero-visual-cost Perf Boost: reject grazing/corner taps before loading colour & moments.
            if (ndot <= 0.42) {
                continue;
            }
            let curved_seam = smoothstep(0.55, 0.88, ndot);
            let n2 = ndot * ndot;
            let n4 = n2 * n2;
            let wn = curved_seam * curved_seam * (n4 * n4);
            if (wn < 1e-4) {
                continue;
            }
            let sample_raw = textureLoad(history_colour, p, 0);
            let sample = sample_raw.rgb;
            let base_w = wy * kernel_weight(dx) * wn * wd;
            let xl = abs(centre_lum - luminance(sample)) * inv_sigma;
            let w_rgb = base_w / (1.0 + xl + 0.5 * xl * xl);
            if (w_rgb > 1e-6) {
                sum = sum + sample * w_rgb;
                weight_sum = weight_sum + w_rgb;
                if (step < 2) {
                    let moments = textureLoad(history_moments, p, 0);
                    moment1 = moment1 + moments.x * w_rgb;
                    moment2 = moment2 + moments.y * w_rgb;
                }
            }
            if (filter_lf && ndot > 0.88) {
                let xlf = abs(centre_raw.a - sample_raw.a) * lf_edge_k;
                let w_lf = base_w / (1.0 + xlf + 0.5 * xlf * xlf);
                if (w_lf > 1e-6) {
                    sum_lf = sum_lf + sample_raw.a * w_lf;
                    weight_sum_lf = weight_sum_lf + w_lf;
                }
            }
        }
    }
    let inv_w = 1.0 / weight_sum;
    let out_lf = select(centre_raw.a, sum_lf / weight_sum_lf, filter_lf);
    textureStore(out_colour, pixel, vec4<f32>(sum * inv_w, out_lf));
    let out_m1 = select(centre_moments.x, moment1 * inv_w, step < 2);
    let out_m2 = select(centre_moments.y, moment2 * inv_w, step < 2);
    textureStore(out_moments, pixel, vec4<f32>(out_m1, out_m2, centre_moments.z, centre_moments.w));
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
    let grid_scale = gdim / extent.xy;
    let gcoord = vec2<i32>(clamp((vec2<f32>(pixel) + vec2<f32>(0.5)) * grid_scale, vec2<f32>(0.0), gdim - vec2<f32>(1.0)));
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

/// Fullscreen layered composite shader (Phase 6): depth-aware and normal-aware
/// 3x3 joint bilateral upsampling of Raywatcher's denoised lighting layer
/// (`src_rt`) onto the untouched full-resolution raster world render
/// (`src_raster`). Replaces raw bilinear sampling so shadow/AO/GI edges snap
/// cleanly to full-resolution building and terrain silhouettes without bleeding
/// across depth discontinuities.
const COMPOSITE_SHADER: &str = r#"
@group(0) @binding(0) var src_raster: texture_2d<f32>;
@group(0) @binding(1) var src_rt: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;
@group(0) @binding(3) var src_depth: texture_depth_2d;
@group(0) @binding(4) var src_normals: texture_2d<f32>;
@group(0) @binding(5) var src_reflection: texture_2d<f32>;
@group(0) @binding(6) var<uniform> comp_params: vec4<f32>;

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

fn load_unit_normal(coord: vec2<i32>) -> vec3<f32> {
    let n = textureLoad(src_normals, coord, 0).xyz;
    let len = length(n);
    return select(vec3<f32>(0.0, 1.0, 0.0), n / len, len > 1e-4);
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let rdim = vec2<i32>(textureDimensions(src_raster));
    let ddim = vec2<i32>(textureDimensions(src_depth));
    let rt_dim = vec2<i32>(textureDimensions(src_rt));
    let ndim = vec2<i32>(textureDimensions(src_normals));

    let px = clamp(vec2<i32>(i.pos.xy), vec2<i32>(0), rdim - vec2<i32>(1));
    let raster = textureLoad(src_raster, px, 0);

    let dpx = clamp(
        vec2<i32>(i.uv * vec2<f32>(ddim)),
        vec2<i32>(0),
        ddim - vec2<i32>(1),
    );
    let z_center = textureLoad(src_depth, dpx, 0);
    // Sky is already passed through by the reconstruction prep stage. Keep
    // that exact result when this compositor is only restoring native water.
    if (z_center <= 0.0) {
        return select(raster, textureLoad(src_rt, px, 0), comp_params.y > 0.5);
    }

    let n_center_coord = clamp(
        vec2<i32>(i.uv * vec2<f32>(ndim)),
        vec2<i32>(0),
        ndim - vec2<i32>(1),
    );
    let norm_raw = textureLoad(src_normals, n_center_coord, 0);
    let n_len = length(norm_raw.xyz);
    let n_center = select(vec3<f32>(0.0, 1.0, 0.0), norm_raw.xyz / n_len, n_len > 1e-4);
    let center_roughness = norm_raw.w;
    let is_water = center_roughness <= 0.12;
    // Keep the native water byte-for-byte unless an RT reflection was actually
    // traced. In active mode its raster base/refraction stays and RT adds only mirror radiance.
    if (is_water && comp_params.z <= 0.5) {
        return raster;
    }
    // In this mode the ray image is already composited. DLSS RR in layered mode
    // includes the raster base; explicit replace-raster mode does not.
    if (comp_params.y > 0.5) {
        if (is_water) {
            let rt = textureLoad(src_rt, px, 0);
            let water_rgb = select(raster.rgb + rt.rgb, rt.rgb, comp_params.w > 0.5);
            return vec4<f32>(max(water_rgb, vec3<f32>(0.0)), raster.a);
        }
        return textureLoad(src_rt, px, 0);
    }

    // Boost #13: When src_rt is already full-resolution (1:1 with raster), skip the
    // 9-tap bilateral upsample (saves 27 texture fetches + 18 transcendentals per pixel!).
    var rt = vec4<f32>(0.0);
    if (rt_dim.x == rdim.x && rt_dim.y == rdim.y) {
        rt = textureLoad(src_rt, px, 0);
    } else {
        let rt_f = i.uv * vec2<f32>(rt_dim) - vec2<f32>(0.5);
        let rt_base = vec2<i32>(round(rt_f));

        var rt_sum = vec4<f32>(0.0);
        var w_sum = 0.0;
        let inv_rt_dim = vec2<f32>(1.0) / max(vec2<f32>(rt_dim), vec2<f32>(1.0));
        let inv_z_center = 1.0 / max(z_center, 1e-4);
        let inv_depth_scale = 1.0 / max(0.02 * max(z_center, 1e-4), 1e-5);

        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let tap = clamp(rt_base + vec2<i32>(dx, dy), vec2<i32>(0), rt_dim - vec2<i32>(1));
                let tap_uv = (vec2<f32>(tap) + vec2<f32>(0.5)) * inv_rt_dim;
                let tap_dpx = clamp(vec2<i32>(tap_uv * vec2<f32>(ddim)), vec2<i32>(0), ddim - vec2<i32>(1));
                let z_tap = textureLoad(src_depth, tap_dpx, 0);
                if (z_tap <= 0.0) {
                    continue;
                }
                let dz = abs(z_center - z_tap);
                if (dz * inv_z_center > 0.05) {
                    continue;
                }
                let w_depth = exp(-dz * inv_depth_scale);

                let tap_ncoord = clamp(vec2<i32>(tap_uv * vec2<f32>(ndim)), vec2<i32>(0), ndim - vec2<i32>(1));
                let tap_norm_raw = textureLoad(src_normals, tap_ncoord, 0);
                if ((tap_norm_raw.w <= 0.12) != is_water) {
                    continue;
                }
                let tap_n = select(
                    vec3<f32>(0.0, 1.0, 0.0),
                    normalize(tap_norm_raw.xyz),
                    length(tap_norm_raw.xyz) > 1e-4,
                );
                let ndot = max(dot(n_center, tap_n), 0.0);
                let n2 = ndot * ndot;
                let n4 = n2 * n2;
                let n8 = n4 * n4;
                let w_norm = n8 * n8;

                let delta = vec2<f32>(tap) - rt_f;
                let w_spatial = exp(-0.5 * dot(delta, delta));
                let w = w_spatial * w_depth * w_norm;
                if (w > 1e-5) {
                    rt_sum = rt_sum + textureLoad(src_rt, tap, 0) * w;
                    w_sum = w_sum + w;
                }
            }
        }

        let rt_fallback = textureLoad(src_rt, clamp(rt_base, vec2<i32>(0), rt_dim - vec2<i32>(1)), 0);
        rt = select(rt_fallback, rt_sum / w_sum, w_sum > 1e-5);
    }
    let light_factor = clamp(rt.a, 0.0, 1.0);
    // Water (`roughness <= 0.12`) carries additive mirror reflections in `rt.rgb`.
    // Land, buildings, and trees carry demodulated pure GI + point-light irradiance
    // in `rt.rgb`, modulated here by the crisp full-resolution original raster texture
    // so GI never stamps a blurry low-res grey copy over raster models.
    var layered = vec3<f32>(0.0);
    if (center_roughness <= 0.12) {
        layered = max(raster.rgb * light_factor + rt.rgb, vec3<f32>(0.0));
    } else {
        // Uniform per-frame scene-illumination albedo recovery (preserves dark stone/wood
        // albedo by day and reveals crisp textures under night point lights, zero grey veil).
        let scene_gain = clamp(comp_params.x, 1.0, 9.0);
        // Luminance-preserving albedo recovery: dark stone/terrain gets full night texture
        // recovery while already-lit Walker character skin/armor never blows out to 0.92 white.
        let r_lum = dot(raster.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        let eff_gain = mix(scene_gain, 1.0, smoothstep(0.10, 0.36, r_lum));
        let full_albedo = clamp(raster.rgb * eff_gain, vec3<f32>(0.002), vec3<f32>(0.92));
        // Add volumetric god rays (#1) & subtle calibrated glossy trim reflections (#3)
        // directly without multiplying by wall albedo.
        let refl_dim = vec2<i32>(textureDimensions(src_reflection));
        let refl_coord = clamp(
            vec2<i32>(i.uv * vec2<f32>(refl_dim)),
            vec2<i32>(0),
            refl_dim - vec2<i32>(1),
        );
        let add_refl = textureLoad(src_reflection, refl_coord, 0).rgb;
        layered = max(raster.rgb * light_factor + full_albedo * rt.rgb + add_refl, vec3<f32>(0.0));
    }
    return vec4<f32>(layered, raster.a);
}
"#;

pub const COLOUR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// `Rg16Float` would be smaller, but storage textures cannot be 16-bit
/// single/dual channel on every backend, so the guide is RGBA like the rest.
pub const GUIDE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Edge resolution of each face in the 3x2 sky+cloud reflection cubemap atlas.
pub const SKY_ENV_FACE: u32 = 512;
pub const SKY_ENV_WIDTH: u32 = SKY_ENV_FACE * 3;
pub const SKY_ENV_HEIGHT: u32 = SKY_ENV_FACE * 2;
/// Resolution of the top-down real textured terrain albedo map (512x512).
pub const TERRAIN_MAP_SIZE: u32 = 512;
/// Half-extent (world units) covered around the camera snap anchor (1024x1024 total).
pub const TERRAIN_MAP_HALF_EXTENT: f32 = 512.0;

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

const SCREEN_RASTER_COPY_SHADER: &str = r#"
@group(0) @binding(0) var src_raster: texture_2d<f32>;
@group(0) @binding(1) var dst_raster: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn copy_fullres(@builtin(global_invocation_id) id: vec3<u32>) {
    let dim = textureDimensions(src_raster);
    if (id.x >= dim.x || id.y >= dim.y) {
        return;
    }
    let pixel = vec2<i32>(id.xy);
    textureStore(dst_raster, pixel, textureLoad(src_raster, pixel, 0));
}
"#;

/// Shader modules and pipeline layouts, created once.
struct Pipelines {
    gbuf_prep: wgpu::ComputePipeline,
    gbuf_prep_layout: wgpu::BindGroupLayout,
    trace: wgpu::ComputePipeline,
    temporal: wgpu::ComputePipeline,
    atrous: wgpu::ComputePipeline,
    trace_layout: wgpu::BindGroupLayout,
    screen_raster_layout: wgpu::BindGroupLayout,
    screen_raster_copy_layout: wgpu::BindGroupLayout,
    screen_raster_copy: wgpu::ComputePipeline,
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
        let screen_raster_copy_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("raywatcher full-resolution raster copy"),
            source: wgpu::ShaderSource::Wgsl(SCREEN_RASTER_COPY_SHADER.into()),
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
                texture_entry(13, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(14, wgpu::TextureSampleType::Float { filterable: true }),
                wgpu::BindGroupLayoutEntry {
                    binding: 15,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 16,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let screen_raster_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("raywatcher full-resolution screen raster"),
                entries: &[texture_entry(
                    0,
                    wgpu::TextureSampleType::Float { filterable: true },
                )],
            });
        let screen_raster_copy_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("raywatcher full-resolution raster copy"),
                entries: &[
                    texture_entry(0, wgpu::TextureSampleType::Float { filterable: true }),
                    storage_texture_entry(1, wgpu::StorageTextureAccess::WriteOnly, COLOUR_FORMAT),
                ],
            });
        let screen_raster_copy_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("raywatcher full-resolution raster copy"),
                bind_group_layouts: &[&screen_raster_copy_layout],
                push_constant_ranges: &[],
            });
        let screen_raster_copy = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("raywatcher full-resolution raster copy"),
            layout: Some(&screen_raster_copy_pipeline_layout),
            module: &screen_raster_copy_module,
            entry_point: "copy_fullres",
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
            bind_group_layouts: &[&trace_layout, &screen_raster_layout],
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
            screen_raster_layout,
            screen_raster_copy_layout,
            screen_raster_copy,
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
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
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
        // When `scene.nodes` is empty (DXR-only scene built with
        // `build_triangles_only`), DXR owns its own D3D12 BLAS/TLAS & VRAM
        // buffers, so skip uploading ~60 MB of unused wgpu storage buffers.
        let dxr_only = !scene.triangles.is_empty() && scene.nodes.is_empty();
        let tri_slice = if scene.triangles.is_empty() || dxr_only {
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
        let mat_slice = if scene.materials.is_empty() || dxr_only {
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

/// Preserve Raywatcher's original adaptive quality ceiling. The planner's
/// ray grid is an independent spatial-quality control; it must not silently
/// clamp the shader's authored adaptive sampling based on that grid.
fn adaptive_sample_ceiling(_samples: u32) -> f32 {
    4.0
}

/// Software tracing needs a CPU BVH for every non-empty scene. Hardware-only
/// triangle snapshots deliberately omit those nodes to avoid a second
/// acceleration structure; keep native raster active until CPU fallback exists.
fn software_fallback_ready(triangle_count: u32, node_count: u32) -> bool {
    triangle_count == 0 || node_count > 0
}

/// Whether this frame can actually dispatch through DXR. A Hardware mode
/// request alone is insufficient while the acceleration structure is being
/// rebuilt on a worker; the last RT output is held (or raster remains visible)
/// until the matching hardware scene is ready.
fn hardware_trace_ready(
    hardware_mode: bool,
    dxr_failed: bool,
    dxr_ready: bool,
    dxr_initializing: bool,
) -> bool {
    hardware_mode && !dxr_failed && dxr_ready && !dxr_initializing
}

/// Everything Raywatcher owns on the GPU.
pub struct RaywatcherGpu {
    device: std::sync::Arc<wgpu::Device>,
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
    dxr_init: Option<std::sync::mpsc::Receiver<Result<super::dxr::DxrTracer, String>>>,
    /// Scene snapshot owned by the single in-flight DXR build. Newer scenes
    /// are coalesced and rebuilt sequentially when this worker completes.
    dxr_init_scene: Option<Arc<super::bvh::SceneGeometry>>,
    dlss_rr: Option<super::reconstruct::DlssRayReconstructor>,
    dlss_rr_failed: bool,
    dlss_rr_error: Option<String>,
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
    /// Full-resolution linear raster colour used to fetch crisp reflected geometry.
    screen_raster_tex: wgpu::Texture,
    screen_raster_view: wgpu::TextureView,
    screen_raster_bind_group: wgpu::BindGroup,
    screen_raster_copy_bind_group: Option<wgpu::BindGroup>,
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
    composite_uniforms: wgpu::Buffer,
    radiance_cache: wgpu::Buffer,
    restir_reservoirs: wgpu::Buffer,
    restir_reservoirs_enabled: bool,
    last_scene_albedo_gain: f32,
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
    /// 3x2 cubemap atlas (`SKY_ENV_WIDTH x SKY_ENV_HEIGHT`, `COLOUR_FORMAT`)
    /// holding the real `SkyGpu` + `CloudGpu` skybox for reflections & sky GI.
    sky_env_tex: wgpu::Texture,
    sky_env_view: wgpu::TextureView,
    sky_env_face_cursor: u32,
    sky_env_initialized: bool,
    sky_env_dirty: bool,
    sky_env_scratch: Option<(
        wgpu::TextureFormat,
        wgpu::Texture,
        wgpu::TextureView,
        wgpu::Texture,
        wgpu::TextureView,
        wgpu::BindGroup,
        wgpu::RenderPipeline,
    )>,
    /// Top-down real textured terrain albedo map (`TERRAIN_MAP_SIZE^2`, `COLOUR_FORMAT`).
    terrain_map_tex: wgpu::Texture,
    terrain_map_view: wgpu::TextureView,
    terrain_map_anchor: Option<(f32, f32)>,
    terrain_map_dirty: bool,
    terrain_map_scratch: Option<(
        wgpu::TextureFormat,
        wgpu::Texture,
        wgpu::TextureView,
        wgpu::Texture,
        wgpu::TextureView,
        wgpu::BindGroup,
    )>,
    /// Interpolated result, at the render extent, for the upscaler to consume.
    pub output: wgpu::Texture,
    pub output_view: wgpu::TextureView,
    /// Guide planes at render extent: `x` the sky visibility, `y` the reflection
    /// hit distance, `z` the filtered variance, `w` the depth. Handed to the ray
    /// reconstruction providers and to the debug view.
    pub guide: wgpu::Texture,
    pub guide_view: wgpu::TextureView,
    /// Previous frame's view-projection matrix for static camera reprojection.
    prev_view_projection: Option<glam::Mat4>,
    /// Frames rendered since the last reset, saturating, for the history bit.
    frames: u32,
    /// Whether the last frame dispatched the trace pass.
    pub traced_last_frame: bool,
    /// Actual backend used for the previous traced frame; switching backends invalidates GI history.
    last_traced_hardware: Option<bool>,
}

// SAFETY: the initialized value is sent once from its construction worker to
// the render thread before DLSS reconstruction contexts are created. It then
// remains exclusively owned and is never sent back to another thread.
unsafe impl Send for RaywatcherGpu {}

impl RaywatcherGpu {
    pub fn new(
        device: std::sync::Arc<wgpu::Device>,
        scene: Arc<super::bvh::SceneGeometry>,
        render_extent: (u32, u32),
        grid_extent: (u32, u32),
    ) -> Self {
        let device_handle = device;
        let device = device_handle.as_ref();
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
        let sky_env_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raywatcher sky env atlas"),
            size: wgpu::Extent3d {
                width: SKY_ENV_WIDTH,
                height: SKY_ENV_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOUR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let sky_env_view = sky_env_tex.create_view(&Default::default());
        let terrain_map_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raywatcher terrain albedo map"),
            size: wgpu::Extent3d {
                width: TERRAIN_MAP_SIZE,
                height: TERRAIN_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOUR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let terrain_map_view = terrain_map_tex.create_view(&Default::default());
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
        let (screen_raster_tex, screen_raster_view) = plane(
            device,
            "raywatcher full-resolution raster",
            w,
            h,
            COLOUR_FORMAT,
        );
        let screen_raster_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raywatcher full-resolution screen raster"),
            layout: &pipelines.screen_raster_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&screen_raster_view),
            }],
        });
        let (output, output_view) = plane(device, "raywatcher output", w, h, COLOUR_FORMAT);
        let (guide, guide_view) = plane(device, "raywatcher output guide", w, h, GUIDE_FORMAT);
        let compose_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("raywatcher compose params"),
            // `var<uniform> extent: vec4<f32>` is 16 bytes in WGSL (verified
            // under wgpu::InstanceFlags::VALIDATION).
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let composite_uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher composite params"),
            contents: bytemuck::bytes_of(&[1.25f32, 0.0, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        // Explicitly initialise cache confidence to zero. Hardware DXR mirrors
        // this packed layout and performs its own upload-heap clear on creation.
        let radiance_cache_zeroes = vec![0u8; RADIANCE_CACHE_CELLS * 16];
        let radiance_cache = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raywatcher world-space radiance cache (packed atomic)"),
            contents: &radiance_cache_zeroes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let (restir_reservoirs, restir_reservoirs_enabled) =
            create_restir_reservoir_buffer(device, grid_extent);

        let trace_bind_group = Self::trace_bindings(
            device,
            &pipelines,
            &uniforms,
            &scene_buffers,
            &grid,
            &sky_env_view,
            &terrain_map_view,
            &radiance_cache,
            &restir_reservoirs,
        );
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
            device: device_handle,
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
            dxr_init: None,
            dxr_init_scene: None,
            dlss_rr: None,
            dlss_rr_failed: false,
            dlss_rr_error: None,
            blit_pipeline: None,
            blit_bind_group,
            composite_pipeline: None,
            raster_copy: None,
            screen_raster_tex,
            screen_raster_view,
            screen_raster_bind_group,
            screen_raster_copy_bind_group: None,
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
            composite_uniforms,
            radiance_cache,
            restir_reservoirs,
            restir_reservoirs_enabled,
            last_scene_albedo_gain: 1.25,
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
            sky_env_tex,
            sky_env_view,
            sky_env_face_cursor: 0,
            sky_env_initialized: false,
            sky_env_dirty: true,
            sky_env_scratch: None,
            terrain_map_tex,
            terrain_map_view,
            terrain_map_anchor: None,
            terrain_map_dirty: true,
            terrain_map_scratch: None,
            output,
            output_view,
            guide,
            guide_view,
            prev_view_projection: None,
            frames: 0,
            traced_last_frame: false,
            last_traced_hardware: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn trace_bindings(
        device: &wgpu::Device,
        pipelines: &Pipelines,
        uniforms: &wgpu::Buffer,
        scene: &SceneBuffers,
        grid: &GridTargets,
        sky_env_view: &wgpu::TextureView,
        terrain_map_view: &wgpu::TextureView,
        radiance_cache: &wgpu::Buffer,
        restir_reservoirs: &wgpu::Buffer,
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
                wgpu::BindGroupEntry {
                    binding: 13,
                    resource: wgpu::BindingResource::TextureView(sky_env_view),
                },
                wgpu::BindGroupEntry {
                    binding: 14,
                    resource: wgpu::BindingResource::TextureView(terrain_map_view),
                },
                wgpu::BindGroupEntry {
                    binding: 15,
                    resource: radiance_cache.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 16,
                    resource: restir_reservoirs.as_entire_binding(),
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
        if render_changed {
            // The full-resolution output texture is recreated below. It no
            // longer contains a completed frame, even when resize() is called
            // by the renderer before record() can observe the changed extent.
            // Keep the native raster path enabled until a new RT frame lands.
            self.traced_last_frame = false;
            self.last_traced_hardware = None;
        }
        if grid_changed {
            self.grid = GridTargets::new(device, grid_extent.0, grid_extent.1);
            self.grid_extent = grid_extent;
            let (reservoirs, enabled) = create_restir_reservoir_buffer(device, grid_extent);
            self.restir_reservoirs = reservoirs;
            self.restir_reservoirs_enabled = enabled;
            self.prev_view_projection = None;
            self.frames = 0;
        }
        if let Some(dxr) = self.dxr.as_mut() {
            if let Err(err) = dxr.resize(device, self.grid_extent, (w, h)) {
                eprintln!("[Raywatcher] DXR resize failed ({err}); falling back to Software");
                self.dxr = None;
                self.dxr_failed = true;
            }
        }
        if !render_changed {
            let pipelines = &self.pipelines;
            self.trace_bind_group = Self::trace_bindings(
                device,
                pipelines,
                &self.uniforms,
                &self.scene,
                &self.grid,
                &self.sky_env_view,
                &self.terrain_map_view,
                &self.radiance_cache,
                &self.restir_reservoirs,
            );
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
            self.raster_copy = None;
            self.screen_raster_copy_bind_group = None;
            self.prev_view_projection = None;
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
        let (screen_raster_tex, screen_raster_view) = plane(
            device,
            "raywatcher full-resolution raster",
            w,
            h,
            COLOUR_FORMAT,
        );
        let screen_raster_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("raywatcher full-resolution screen raster"),
            layout: &pipelines.screen_raster_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&screen_raster_view),
            }],
        });
        let (output, output_view) = plane(device, "raywatcher output", w, h, COLOUR_FORMAT);
        let (guide, guide_view) = plane(device, "raywatcher output guide", w, h, GUIDE_FORMAT);
        self.trace_bind_group = Self::trace_bindings(
            device,
            pipelines,
            &self.uniforms,
            &self.scene,
            &self.grid,
            &self.sky_env_view,
            &self.terrain_map_view,
            &self.radiance_cache,
            &self.restir_reservoirs,
        );
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
        self.screen_raster_tex = screen_raster_tex;
        self.screen_raster_view = screen_raster_view;
        self.screen_raster_bind_group = screen_raster_bind_group;
        self.screen_raster_copy_bind_group = None;
        self.gbuf_prep_bind_group = None;
        self.output = output;
        self.output_view = output_view;
        self.guide = guide;
        self.guide_view = guide_view;
        // A resize invalidates every reprojection.
        self.prev_view_projection = None;
        self.frames = 0;
        true
    }

    /// Initialize DXR on the startup worker before moving this GPU state to the render thread.
    pub fn initialize_hardware(&mut self) {
        if self.dxr.is_some() || self.dxr_failed {
            return;
        }
        let started = std::time::Instant::now();
        match super::dxr::DxrTracer::new(
            &self.device,
            &self.scene_geom,
            self.grid_extent,
            self.render_extent,
        ) {
            Ok(tracer) => {
                self.dxr = Some(tracer);
                eprintln!(
                    "[Raywatcher] Hardware backend prepared in {:.1}s on initialization worker",
                    started.elapsed().as_secs_f32()
                );
            }
            Err(err) => {
                eprintln!("[Raywatcher] DXR 1.1 init failed ({err}); falling back to Software");
                self.dxr_failed = true;
            }
        }
    }

    /// Rebuild an existing DXR tracer for the newest scene. At most one DXR
    /// worker is allowed in flight; its result is tagged with the Arc snapshot
    /// it built so intermediate scene changes can be coalesced.
    fn begin_dxr_scene_rebuild(
        &mut self,
        mut tracer: super::dxr::DxrTracer,
        scene: Arc<super::bvh::SceneGeometry>,
    ) {
        debug_assert!(self.dxr_init.is_none());
        if self.dxr_init.is_some() {
            self.dxr_failed = true;
            eprintln!("[Raywatcher] Refusing overlapping DXR scene rebuild; using Software");
            return;
        }
        let worker_scene = scene.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawn = std::thread::Builder::new()
            .name("pwe-raywatcher-dxr-scene".to_string())
            .spawn(move || {
                let started = std::time::Instant::now();
                let result = tracer.set_scene(&worker_scene).map(|()| tracer);
                eprintln!(
                    "[Raywatcher] Background DXR scene/AS rebuild took {:.1}s ({} triangles, success={})",
                    started.elapsed().as_secs_f32(),
                    worker_scene.triangles.len(),
                    result.is_ok()
                );
                let _ = tx.send(result);
            });
        match spawn {
            Ok(_) => {
                self.dxr_init = Some(rx);
                self.dxr_init_scene = Some(scene);
            }
            Err(err) => {
                self.dxr_init_scene = None;
                self.dxr_failed = true;
                eprintln!(
                    "[Raywatcher] Could not start DXR scene rebuild worker ({err}); using Software"
                );
            }
        }
    }

    fn begin_dxr_initialization(&mut self) {
        if self.dxr.is_some() || self.dxr_failed || self.dxr_init.is_some() {
            return;
        }
        let worker_device = std::sync::Arc::clone(&self.device);
        let scene = self.scene_geom.clone();
        let grid_extent = self.grid_extent;
        let render_extent = self.render_extent;
        let triangle_count = scene.triangles.len();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_scene = scene.clone();
        let spawn = std::thread::Builder::new()
            .name("pwe-raywatcher-dxr-init".to_string())
            .spawn(move || {
                let started = std::time::Instant::now();
                let result = super::dxr::DxrTracer::new(
                    &worker_device,
                    &worker_scene,
                    grid_extent,
                    render_extent,
                );
                eprintln!(
                    "[Raywatcher] Background DXR build finished in {:.1}s ({} triangles, success={})",
                    started.elapsed().as_secs_f32(),
                    triangle_count,
                    result.is_ok()
                );
                let _ = tx.send(result);
            });
        match spawn {
            Ok(_) => {
                self.dxr_init = Some(rx);
                self.dxr_init_scene = Some(scene);
            }
            Err(err) => {
                self.dxr_init_scene = None;
                self.dxr_failed = true;
                eprintln!("[Raywatcher] Could not start DXR initialization worker ({err}); using Software");
            }
        }
    }

    /// false means the sole in-flight worker is still running, or a stale
    /// result was immediately queued against the latest scene.
    fn poll_dxr_initialization(&mut self, device: &wgpu::Device) -> bool {
        let Some(rx) = self.dxr_init.as_ref() else {
            return true;
        };
        let built_scene = self.dxr_init_scene.clone();
        match rx.try_recv() {
            Ok(Ok(mut tracer)) => {
                self.dxr_init = None;
                self.dxr_init_scene = None;
                if !built_scene
                    .as_ref()
                    .is_some_and(|built| std::sync::Arc::ptr_eq(built, &self.scene_geom))
                {
                    // The camera/world changed during the build. Reuse the now
                    // initialised tracer, but build only the latest scene; do
                    // not throw away the expensive tracer or start another
                    // independent DxrTracer::new() worker.
                    self.begin_dxr_scene_rebuild(tracer, self.scene_geom.clone());
                    return false;
                }
                if let Err(err) = tracer.resize(device, self.grid_extent, self.render_extent) {
                    self.dxr_failed = true;
                    eprintln!("[Raywatcher] DXR target resize after background init failed ({err}); using Software");
                } else {
                    self.dxr = Some(tracer);
                }
                true
            }
            Ok(Err(err)) => {
                self.dxr_init = None;
                self.dxr_init_scene = None;
                self.dxr_failed = true;
                eprintln!("[Raywatcher] DXR 1.1 init failed ({err}); falling back to Software");
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.dxr_init = None;
                self.dxr_init_scene = None;
                self.dxr_failed = true;
                eprintln!("[Raywatcher] DXR initialization worker exited without a result; falling back to Software");
                true
            }
        }
    }

    pub fn hardware_initializing(&self) -> bool {
        self.dxr_init.is_some()
    }

    /// The host calls this while the module is disabled. Keep the GPU state
    /// alive until its detached DXR worker has finished, so re-enabling the
    /// module cannot start a second full initialization in parallel.
    pub fn discard_completed_background_work(&mut self) -> bool {
        let Some(rx) = self.dxr_init.as_ref() else {
            return true;
        };
        match rx.try_recv() {
            Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.dxr_init = None;
                self.dxr_init_scene = None;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
        }
    }

    /// DXR failed and the renderer must harvest a CPU-BVH scene before the
    /// Software fallback can safely replace native raster lighting.
    pub fn hardware_failed(&self) -> bool {
        self.dxr_failed
    }

    /// Whether the previous frame produced a complete RT output. A
    /// triangles-only Hardware scene is not a valid Software replacement after
    /// DXR failure: it has no CPU BVH to provide occlusion or shadows.
    pub fn can_replace_raster_effects(&self) -> bool {
        self.traced_last_frame
            && (!self.dxr_failed
                || software_fallback_ready(self.scene.counts.triangles, self.scene.counts.nodes))
    }

    pub fn can_replace_water_reflections(&self) -> bool {
        self.can_replace_raster_effects()
    }

    /// Replace the scene. Called when the resident terrain set, tree set or
    /// water areas change.
    pub fn set_scene(&mut self, device: &wgpu::Device, scene: Arc<super::bvh::SceneGeometry>) {
        // Do not drop dxr_init: dropping the receiver does not stop its detached
        // worker, and the next frame would start another full acceleration-
        // structure build. Keep the sole worker and let poll_dxr_initialization
        // notice that its scene snapshot is stale, then rebuild the newest one.
        let old_dxr = self.dxr.take();
        self.scene_geom = scene.clone();
        self.scene = SceneBuffers::upload(device, scene.clone());
        self.prev_view_projection = None;
        self.frames = 0;
        // Keep the last complete output available while a replacement AS builds.
        // record() freezes it and returns to the raster pass instead of running
        // a full Software trace in parallel with an expensive Hardware build.
        if let Some(dxr) = old_dxr {
            self.begin_dxr_scene_rebuild(dxr, scene);
        }
        self.trace_bind_group = Self::trace_bindings(
            device,
            &self.pipelines,
            &self.uniforms,
            &self.scene,
            &self.grid,
            &self.sky_env_view,
            &self.terrain_map_view,
            &self.radiance_cache,
            &self.restir_reservoirs,
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
        point_lights: &[super::frame::PointLight],
        unjittered_vp: glam::Mat4,
        has_object_mv: bool,
        walker_pos: Option<(glam::Vec3, bool)>,
        walker_rt_verts: &[[f32; 3]],
    ) {
        let output_was_ready = self.traced_last_frame;
        self.traced_last_frame = false;
        if !plan.trace {
            return;
        }
        // Copy hardware depth FIRST so both the 1:1 G-buffer prep pass and the
        // denoiser/DLSS RR consume the exact raster depth from the world pass.
        // WebGPU/wgpu requires depth-texture copies to cover the ENTIRE source
        // texture (`Transfer(InvalidDepthTextureExtent)` otherwise). Resize
        // internal render targets first if the incoming depth extent differs.
        let depth_ext = (depth.width().max(1), depth.height().max(1));
        let resized = depth_ext != self.render_extent;
        if resized {
            self.resize(device, depth_ext, plan.grid);
        }
        // Build/poll DXR before recording the G-buffer or trace dispatch. While
        // Hardware is preparing a new AS, retain the previous complete RT image
        // (or leave the raster untouched on first use) instead of doubling the
        // load with a Software trace on every frame.
        if hardware_mode && !self.dxr_failed && self.dxr.is_none() {
            self.begin_dxr_initialization();
            if !self.poll_dxr_initialization(device) {
                self.traced_last_frame =
                    output_was_ready && !resized && depth_ext == self.render_extent;
                return;
            }
        }
        // A DXR failure may leave the current snapshot with triangles but no
        // CPU BVH. Do not dispatch Software against dummy buffers; native raster
        // effects remain active until the CPU scene is ready.
        if self.dxr_failed
            && !software_fallback_ready(self.scene.counts.triangles, self.scene.counts.nodes)
        {
            return;
        }
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
                width: rw,
                height: rh,
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

        let mut packed = RayParams::pack(
            params,
            plan,
            transport,
            [
                self.scene.counts.triangles,
                self.scene.counts.nodes,
                self.scene.counts.materials,
            ],
            point_lights,
        );
        packed.prev_view_projection = self
            .prev_view_projection
            .unwrap_or(unjittered_vp)
            .to_cols_array_2d();
        packed.frame[3] = if restir_history_valid_for_frame(
            params.history_valid,
            self.frames,
            self.prev_view_projection.is_some(),
        ) {
            1.0
        } else {
            0.0
        };
        // This is an allocation-valid bit, not the GI effect toggle. If GI is
        // disabled, the shader clears valid history rather than reusing stale light.
        packed.restir[0] = if self.restir_reservoirs_enabled {
            1.0
        } else {
            0.0
        };
        // Keep the original adaptive ray quality; performance is addressed
        // through the asynchronous scene/AS path and reduced redundant work,
        // never by lowering the per-pixel adaptive sample ceiling.
        packed.restir[1] = adaptive_sample_ceiling(plan.samples);
        packed.restir[2] = 4.0;
        packed.restir[3] = (self.frames & 1) as f32;
        if has_object_mv {
            packed.effects[1] = (packed.effects[1] as u32 | 2048u32) as f32;
        }
        if let Some((wpos, flying)) = walker_pos {
            packed.sun_direction[3] = wpos.x;
            packed.sun_colour[3] = wpos.y;
            packed.ambient[3] = wpos.z;
            packed.fog_colour[3] = if flying { 2.0 } else { 1.0 };
        }
        packed.terrain_map_bounds = self.terrain_map_bounds();
        let scene_albedo_gain = compute_scene_albedo_gain(
            params.sun.colour,
            params.sun.intensity,
            transport.lighting.ambient,
            transport.ambient_strength,
        );
        self.last_scene_albedo_gain = scene_albedo_gain;
        queue.write_buffer(
            &self.composite_uniforms,
            0,
            bytemuck::bytes_of(&[scene_albedo_gain, 0.0, 0.0, 0.0]),
        );

        let skip_inhouse = plan.reconstruction
            == super::plan::RayReconstruction::DlssRayReconstruction
            && !self.dlss_rr_failed;

        let mut traced_hw = false;

        // Track the backend actually used for this frame, not the requested
        // mode. Hardware frames are paused while its scene/AS worker runs, so
        // the backend transition resets temporal history only after it is ready.
        let requested_hardware = hardware_trace_ready(
            hardware_mode,
            self.dxr_failed,
            self.dxr.is_some(),
            self.dxr_init.is_some(),
        );
        if self
            .last_traced_hardware
            .is_some_and(|previous| previous != requested_hardware)
        {
            self.frames = 0;
            self.prev_view_projection = None;
            if let Some(dxr) = self.dxr.as_mut() {
                dxr.invalidate_restir_history();
            }
        }

        if requested_hardware {
            if let Some(dxr) = self.dxr.as_mut() {
                let _ = dxr.update_dynamic_character(walker_rt_verts);
                // Copy the freshly built G-buffer planes into the shared DX12
                // inputs and submit the wgpu encoder so the world pass +
                // G-buffer prep complete on the GPU before DXR dispatches.
                dxr.copy_gbuf_inputs(
                    encoder,
                    &self.grid.gbuf_pos,
                    &self.grid.gbuf_norm,
                    &self.grid.gbuf_raster,
                    &self.screen_raster_tex,
                    &self.sky_env_tex,
                    self.sky_env_dirty,
                    &self.terrain_map_tex,
                    self.terrain_map_dirty,
                );
                self.sky_env_dirty = false;
                self.terrain_map_dirty = false;
                let pending = std::mem::replace(
                    encoder,
                    device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("raywatcher post-dxr"),
                    }),
                );
                queue.submit(Some(pending.finish()));
                // Opt #16: Nothing consumes grid.guide after compose_pass elimination; skip UAV write & copy.
                let dst_guide: Option<&wgpu::Texture> = None;
                let dst_albedo = skip_inhouse.then_some(&self.grid.albedo);
                packed.point_light_meta[2] = if dst_albedo.is_some() { 1.0 } else { 0.0 };
                packed.point_light_meta[3] = if dst_guide.is_some() { 1.0 } else { 0.0 };
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
                        if !software_fallback_ready(
                            self.scene.counts.triangles,
                            self.scene.counts.nodes,
                        ) {
                            // Do not produce one invalid, unshadowed frame from
                            // the triangles-only hardware snapshot.
                            self.last_traced_hardware = None;
                            return;
                        }
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
            pass.set_bind_group(1, &self.screen_raster_bind_group, &[]);
            let (w, h) = plan.grid;
            pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        }

        if !skip_inhouse {
            self.record_denoise(
                queue,
                encoder,
                params,
                config,
                motion,
                unjittered_vp,
                has_object_mv,
            );
        }
        self.frames = self.frames.saturating_add(1);
        self.last_traced_hardware = Some(traced_hw);
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
        unjittered_vp: glam::Mat4,
        has_object_mv: bool,
    ) {
        let (rw, rh) = self.render_extent;
        // Opt #15: Skip full-resolution RGBA16F motion-vector texture copy when no weather/object MVs exist.
        if has_object_mv {
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
        }
        let prev_vp = self.prev_view_projection;
        queue.write_buffer(
            &self.temporal_uniforms,
            0,
            bytemuck::bytes_of(&DenoiseParams::pack(
                self.render_extent,
                0,
                config,
                params,
                unjittered_vp,
                prev_vp,
                has_object_mv,
            )),
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
                    unjittered_vp,
                    prev_vp,
                    has_object_mv,
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
        // Opt #16: Replace full-screen compose_pass compute shader (4 loads + 2 stores + barrier)
        // with a direct hardware DMA copy from the final à-trous plane to `self.output`.
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: &self.filter_colour_tex[final_plane],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &self.output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: rw,
                height: rh,
                depth_or_array_layers: 1,
            },
        );
        self.prev_view_projection = Some(unjittered_vp);
    }

    /// Run NVIDIA DLSS Ray Reconstruction (`nvngx_dlssd.dll`) over the freshly
    /// traced ray grid, overwriting `self.output` with the reconstructed image.
    /// Returns `true` when DLSS RR ran; falls back silently to the in-house
    /// denoiser output already in `self.output` on failure.
    #[allow(clippy::too_many_arguments)]
    /// Last runtime DLSS Ray Reconstruction failure diagnostic, if any.
    pub fn dlss_rr_error(&self) -> Option<&str> {
        self.dlss_rr_error.as_deref()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reconstruct_dlss_rr(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        post_encoder: &mut wgpu::CommandEncoder,
        sdk_dir: &std::path::Path,
        params: &FrameParams,
        motion: &wgpu::TextureView,
        reset: bool,
        layered: bool,
        unjittered_vp: glam::Mat4,
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
                    let nvngx = if super::reconstruct::driver_store_has_nvngx() {
                        "present"
                    } else {
                        "missing"
                    };
                    let msg = format!("{err} (DriverStore nvngx.dll: {nvngx})");
                    eprintln!("[Raywatcher] DLSS Ray Reconstruction init failed: {msg}; keeping in-house denoiser");
                    self.dlss_rr_error = Some(msg);
                    self.dlss_rr_failed = true;
                    return false;
                }
            }
        }
        let Some(rr) = self.dlss_rr.as_mut() else {
            return false;
        };
        let history_valid = !reset && self.prev_view_projection.is_some();
        let prev_vp = self.prev_view_projection.unwrap_or(unjittered_vp);
        let prev_vp_inv_vp = (prev_vp * unjittered_vp.inverse()).to_cols_array_2d();
        let inputs = super::reconstruct::DlssRrInputs {
            grid_colour: &self.grid.colour_view,
            grid_reflection: &self.grid.reflection_view,
            grid_normals: &self.grid.normals_view,
            grid_albedo: &self.grid.albedo_view,
            depth: &self.depth,
            raster: self.raster_copy.as_ref().map(|r| &r.3),
            layered,
            scene_albedo_gain: self.last_scene_albedo_gain,
            motion,
            prev_vp_inv_vp,
            history_valid,
            jitter: params.jitter,
            reset,
            world_to_view,
            view_to_clip,
        };
        match rr.evaluate(device, queue, post_encoder, &inputs, &self.output) {
            Ok(()) => {
                self.prev_view_projection = Some(unjittered_vp);
                self.dlss_rr_error = None;
                true
            }
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
    /// Phase 3: Render the real game sky (`SkyGpu`) and cloud billboards
    /// (`CloudGpu`) into the 3x2 cubemap environment atlas (`self.sky_env_tex`).
    /// On the first frame all 6 faces are populated; subsequent frames refresh
    /// one face per frame so clouds and time-of-day stay live at minimal cost.
    #[allow(clippy::too_many_arguments)]
    /// World-space bounds `[min_x, max_z, inv_diameter, valid]` of the top-down
    /// real textured terrain albedo map.
    pub fn terrain_map_bounds(&self) -> [f32; 4] {
        match self.terrain_map_anchor {
            Some((ax, az)) => [
                ax - TERRAIN_MAP_HALF_EXTENT,
                az + TERRAIN_MAP_HALF_EXTENT,
                1.0 / (2.0 * TERRAIN_MAP_HALF_EXTENT),
                1.0,
            ],
            None => [0.0; 4],
        }
    }

    /// Returns `Some((anchor_x, anchor_z))` when the top-down terrain albedo map
    /// should be refreshed (initial frame or camera moved > 192u from last anchor).
    pub fn terrain_map_refresh_anchor(&self, eye_x: f32, eye_z: f32) -> Option<(f32, f32)> {
        let snap = 192.0f32;
        let ax = (eye_x / snap).round() * snap;
        let az = (eye_z / snap).round() * snap;
        match self.terrain_map_anchor {
            Some((old_x, old_z)) if (old_x - ax).abs() < 1.0 && (old_z - az).abs() < 1.0 => None,
            _ => Some((ax, az)),
        }
    }

    /// Prepare scratch render targets (`surface_format` + `Depth32Float`) for
    /// rendering the top-down terrain pass into `terrain_map_tex`.
    pub fn prepare_terrain_map_targets(
        &mut self,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
    ) -> (&wgpu::TextureView, &wgpu::TextureView) {
        let need_alloc = self
            .terrain_map_scratch
            .as_ref()
            .is_none_or(|(fmt, ..)| *fmt != surface_format);
        if need_alloc {
            let color = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("raywatcher terrain map scratch color"),
                size: wgpu::Extent3d {
                    width: TERRAIN_MAP_SIZE,
                    height: TERRAIN_MAP_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: surface_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let color_view = color.create_view(&Default::default());
            let depth = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("raywatcher terrain map scratch depth"),
                size: wgpu::Extent3d {
                    width: TERRAIN_MAP_SIZE,
                    height: TERRAIN_MAP_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let depth_view = depth.create_view(&Default::default());
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("raywatcher terrain map blit"),
                layout: &self.pipelines.blit_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.pipelines.blit_sampler),
                    },
                ],
            });
            self.terrain_map_scratch =
                Some((surface_format, color, color_view, depth, depth_view, bg));
        }
        let (_, _, ref cview, _, ref dview, _) = self.terrain_map_scratch.as_ref().unwrap();
        (cview, dview)
    }

    /// Blit the rendered scratch top-down terrain map into `self.terrain_map_tex`
    /// (`COLOUR_FORMAT`) and record its world-space anchor.
    pub fn finish_terrain_map_blit(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        anchor: (f32, f32),
    ) {
        if self
            .blit_pipeline
            .as_ref()
            .is_none_or(|(fmt, _)| *fmt != COLOUR_FORMAT)
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
                        format: COLOUR_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });
            self.blit_pipeline = Some((COLOUR_FORMAT, pipe));
        }
        let (Some((_, ref pipe)), Some((_, _, _, _, _, ref bg))) =
            (&self.blit_pipeline, &self.terrain_map_scratch)
        else {
            return;
        };
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("raywatcher terrain map blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.terrain_map_view,
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
        drop(rp);
        self.terrain_map_anchor = Some(anchor);
        self.terrain_map_dirty = true;
    }

    pub fn update_sky_env(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        sky: &crate::sky_gpu::SkyGpu,
        cloud: Option<&crate::cloud_gpu::CloudGpu>,
        sky_format: wgpu::TextureFormat,
        sky_frame: crate::sky_gpu::SkyFrame,
        sky_sets: (i32, i32, f32),
        camera_pos: glam::Vec3,
        cloud_detail: bool,
        daylight: f32,
        fog: crate::fog_matrix::Fog,
    ) {
        let needs_alloc = self
            .sky_env_scratch
            .as_ref()
            .is_none_or(|(fmt, ..)| *fmt != sky_format);
        if needs_alloc {
            let scratch_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("raywatcher sky env scratch"),
                size: wgpu::Extent3d {
                    width: SKY_ENV_FACE,
                    height: SKY_ENV_FACE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: sky_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let scratch_view = scratch_tex.create_view(&Default::default());
            let scratch_depth = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("raywatcher sky env depth"),
                size: wgpu::Extent3d {
                    width: SKY_ENV_FACE,
                    height: SKY_ENV_FACE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let scratch_depth_view = scratch_depth.create_view(&wgpu::TextureViewDescriptor {
                aspect: wgpu::TextureAspect::DepthOnly,
                ..Default::default()
            });
            let blit_bg = Self::blit_binding(device, &self.pipelines, &scratch_view);
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("raywatcher sky env blit"),
                bind_group_layouts: &[&self.pipelines.blit_layout],
                push_constant_ranges: &[],
            });
            let blit_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("raywatcher sky env blit"),
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
                        format: COLOUR_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });
            self.sky_env_scratch = Some((
                sky_format,
                scratch_tex,
                scratch_view,
                scratch_depth,
                scratch_depth_view,
                blit_bg,
                blit_pipe,
            ));
        }

        // 6 cubemap face directions matching `sample_sky_env` (faces 0..5 = +X, -X, +Y, -Y, +Z, -Z).
        const FACES: [(glam::Vec3, glam::Vec3, f32, f32); 6] = [
            (
                glam::Vec3::X,
                glam::Vec3::Y,
                std::f32::consts::FRAC_PI_2,
                0.0,
            ),
            (
                glam::Vec3::NEG_X,
                glam::Vec3::Y,
                -std::f32::consts::FRAC_PI_2,
                0.0,
            ),
            (
                glam::Vec3::Y,
                glam::Vec3::NEG_Z,
                0.0,
                std::f32::consts::FRAC_PI_2 * 0.99,
            ),
            (
                glam::Vec3::NEG_Y,
                glam::Vec3::Z,
                0.0,
                -std::f32::consts::FRAC_PI_2 * 0.99,
            ),
            (glam::Vec3::Z, glam::Vec3::Y, std::f32::consts::PI, 0.0),
            (glam::Vec3::NEG_Z, glam::Vec3::Y, 0.0, 0.0),
        ];

        let (face_buf, face_count): ([u32; 6], usize) = if !self.sky_env_initialized {
            self.sky_env_initialized = true;
            self.sky_env_dirty = true;
            ([0, 1, 2, 3, 4, 5], 6)
        } else if self.frames % 24 == 0 {
            // Opt #25: Skip downward -Y face (face 3, occluded by terrain) & amortize to 24 frames (>50% fewer offscreen sky passes).
            const LIVE_FACES: [u32; 5] = [0, 1, 2, 4, 5];
            let f = LIVE_FACES[(self.sky_env_face_cursor as usize) % 5];
            self.sky_env_face_cursor = self.sky_env_face_cursor.wrapping_add(1);
            self.sky_env_dirty = true;
            ([f, 0, 0, 0, 0, 0], 1)
        } else {
            return;
        };
        let faces_to_render = &face_buf[..face_count];

        let multi_face = faces_to_render.len() > 1;
        let sky_proj = glam::Mat4::from_scale(glam::Vec3::new(-1.0, 1.0, 1.0))
            * glam::Mat4::perspective_rh(90.0f32.to_radians(), 1.0, 1.0, 16000.0);
        let cloud_proj =
            glam::Mat4::perspective_infinite_reverse_rh(90.0f32.to_radians(), 1.0, 0.5);

        for &face in faces_to_render {
            let (dir, up, yaw, pitch) = FACES[face as usize];
            let view = glam::Mat4::look_to_rh(glam::Vec3::ZERO, dir, up);
            sky.update_custom(queue, view, sky_proj, SKY_ENV_FACE, sky_frame, sky_sets);

            let mut local_enc = if multi_face {
                Some(
                    device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("raywatcher sky env face"),
                    }),
                )
            } else {
                None
            };
            let enc: &mut wgpu::CommandEncoder = match local_enc.as_mut() {
                Some(e) => e,
                None => encoder,
            };

            let Some((
                _,
                _,
                ref scratch_view,
                _,
                ref scratch_depth_view,
                ref blit_bg,
                ref blit_pipe,
            )) = self.sky_env_scratch
            else {
                return;
            };
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("raywatcher sky env draw"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: scratch_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: scratch_depth_view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                sky.draw(&mut pass, false);
            }
            if let Some(cloud) = cloud {
                let cloud_view = glam::Mat4::look_to_rh(camera_pos, dir, up);
                let cloud_mat = cloud_proj * cloud_view;
                cloud.draw(
                    queue,
                    enc,
                    scratch_view,
                    scratch_depth_view,
                    cloud_mat,
                    yaw,
                    pitch,
                    cloud_detail,
                    daylight,
                    fog,
                    [0.0; 4],
                    false,
                );
            }
            let col = face % 3;
            let row = face / 3;
            let mut blit_pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("raywatcher sky env atlas blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.sky_env_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            blit_pass.set_viewport(
                (col * SKY_ENV_FACE) as f32,
                (row * SKY_ENV_FACE) as f32,
                SKY_ENV_FACE as f32,
                SKY_ENV_FACE as f32,
                0.0,
                1.0,
            );
            blit_pass.set_pipeline(blit_pipe);
            blit_pass.set_bind_group(0, blit_bg, &[]);
            blit_pass.draw(0..3, 0..1);
            drop(blit_pass);

            if let Some(e) = local_enc {
                queue.submit(Some(e.finish()));
            }
        }
    }

    pub fn capture_raster(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::Texture,
    ) {
        let fmt = source.format();
        let ext = (source.width().max(1), source.height().max(1));
        if ext != self.render_extent && !self.resize(device, ext, self.grid_extent) {
            return;
        }
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
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&self.depth),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&self.grid.gbuf_norm_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&self.grid.reflection_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: self.composite_uniforms.as_entire_binding(),
                    },
                ],
            });
            self.raster_copy = Some((fmt, ext, tex, view, bg));
            self.screen_raster_copy_bind_group = None;
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
        if self.screen_raster_copy_bind_group.is_none() {
            let Some((_, _, _, raster_view, _)) = self.raster_copy.as_ref() else {
                return;
            };
            self.screen_raster_copy_bind_group =
                Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("raywatcher full-resolution raster copy"),
                    layout: &self.pipelines.screen_raster_copy_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(raster_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&self.screen_raster_view),
                        },
                    ],
                }));
        }
        let mut copy_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("raywatcher full-resolution raster conversion"),
            timestamp_writes: None,
        });
        copy_pass.set_pipeline(&self.pipelines.screen_raster_copy);
        copy_pass.set_bind_group(0, self.screen_raster_copy_bind_group.as_ref().unwrap(), &[]);
        copy_pass.dispatch_workgroups(ext.0.div_ceil(8), ext.1.div_ceil(8), 1);
    }

    /// Composite Raywatcher's denoised lighting layer on top of the untouched
    /// raster world render (`layered && !dlss_rr_ran`), or blit `self.output`
    /// directly when DLSS RR already produced the composited image or when the
    /// user explicitly opted into `replace_raster`.
    pub fn composite_to(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        destination: &wgpu::Texture,
        layered: bool,
        dlss_rr_ran: bool,
        rt_water_reflections: bool,
    ) {
        if self.raster_copy.is_none() {
            self.blit_to(device, encoder, destination);
            return;
        }
        // For DLSS RR and replace-raster mode, the RT output is already a full
        // image. Still run the native-water bypass so animated PW water remains
        // byte-for-byte raster output rather than being temporally reconstructed.
        let already_composited = !layered || dlss_rr_ran;
        queue.write_buffer(
            &self.composite_uniforms,
            0,
            bytemuck::bytes_of(&[
                self.last_scene_albedo_gain,
                if already_composited { 1.0 } else { 0.0 },
                if rt_water_reflections { 1.0 } else { 0.0 },
                if dlss_rr_ran && layered { 1.0 } else { 0.0 },
            ]),
        );
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
    /// Returns `true` when DXR hardware ray tracing is active on this device.
    pub fn dxr_active(&self) -> bool {
        self.dxr.is_some()
    }

    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Forget the temporal history: a camera cut, a resize, or the module being
    /// switched off and on again.
    pub fn reset(&mut self) {
        self.prev_view_projection = None;
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

#[cfg(test)]
mod phase_r1_tests {
    use super::*;

    #[test]
    fn native_water_is_tagged_only_inside_a_flat_active_water_plane() {
        let water = [super::super::harvest::WaterSurface {
            bounds: [-100.0, 100.0, -100.0, 100.0],
            height: 10.0,
        }];
        let flat_normal = glam::Vec3::Y;
        let underside_normal = glam::Vec3::NEG_Y;
        let slope_normal = glam::Vec3::new(0.2, 0.97, 0.0);
        assert_eq!(
            classify_surface_roughness(flat_normal, glam::Vec3::new(0.0, 10.0, 0.0), &water),
            0.06
        );
        assert_eq!(
            classify_surface_roughness(underside_normal, glam::Vec3::new(0.0, 10.0, 0.0), &water),
            0.06,
            "the underside of native water must stay tagged when viewed underwater"
        );
        assert!(
            GBUF_PREP_SHADER.contains("if (abs(n.y) > 0.985)"),
            "the GPU classifier must accept both sides of a horizontal water plane"
        );
        assert!(
            GBUF_PREP_SHADER.contains("view_dir.y < 0.0"),
            "the water normal must face the camera from above and below"
        );
        assert_eq!(
            classify_surface_roughness(flat_normal, glam::Vec3::new(101.0, 10.0, 0.0), &water),
            0.92
        );
        assert_eq!(
            classify_surface_roughness(flat_normal, glam::Vec3::new(0.0, 10.2, 0.0), &water),
            0.92
        );
        assert_eq!(
            classify_surface_roughness(slope_normal, glam::Vec3::new(0.0, 10.0, 0.0), &water),
            0.92
        );
        assert_eq!(
            classify_surface_roughness(flat_normal, glam::Vec3::new(0.0, 10.0, 0.0), &[]),
            0.92
        );
    }

    #[test]
    fn phase_r4_water_excluded_from_occluder_bvh_while_water_pixels_trace_reflections() {
        use super::super::harvest::{HarvestConfig, Harvester, WaterSurface, KIND_WATER};
        let water = [WaterSurface {
            bounds: [-50.0, 50.0, -50.0, 50.0],
            height: 10.0,
        }];
        let mut harvester = Harvester::new();
        let flat_ground = |_x: f32, _z: f32| -> f32 { 5.0 };
        let scene = harvester.scene_frustum(
            HarvestConfig {
                radius: 64.0,
                chunk_size: 64.0,
                max_distance: 64.0,
                ..HarvestConfig::default()
            },
            glam::Vec3::new(0.0, 20.0, 0.0),
            glam::Vec3::NEG_Z,
            glam::Mat4::IDENTITY,
            64.0,
            2.0,
            1,
            1,
            Some(&flat_ground),
            &[],
            &[],
            &water,
        );
        assert!(
            scene.triangles.iter().all(|t| t.kind() != KIND_WATER),
            "occluder BVH must contain zero KIND_WATER triangles"
        );
        // The flat water plane is tagged for the native-water bypass.
        let cutoff = 0.35_f32;
        let water_r =
            classify_surface_roughness(glam::Vec3::Y, glam::Vec3::new(0.0, 10.0, 0.0), &water);
        assert!(
            water_r <= cutoff,
            "water pixel roughness {water_r} must qualify for the native-water bypass (<= {cutoff})"
        );
        // A surface above the plane remains matte and is not mistaken for water.
        let land_r =
            classify_surface_roughness(glam::Vec3::Y, glam::Vec3::new(0.0, 11.0, 0.0), &water);
        assert!(
            land_r > cutoff,
            "land pixel roughness {land_r} must not qualify for reflections (> {cutoff})"
        );
    }

    #[test]
    fn scene_albedo_gain_preserves_dark_day_walls_and_recovers_night_textures() {
        let day_gain = compute_scene_albedo_gain(
            glam::Vec3::new(1.0, 0.96, 0.90),
            1.0,
            glam::Vec3::new(0.28, 0.30, 0.35),
            0.85,
        );
        // At bright daylight, gain stays near 1.1..1.4 so dark stone walls (0.12) stay dark (~0.15)
        // instead of blowing up to 0.75 chalky grey under GI / DLSS RR.
        assert!(
            day_gain >= 1.0 && day_gain <= 1.5,
            "daytime scene_albedo_gain should be mild (1.0..=1.5), got {day_gain}"
        );
        let night_gain = compute_scene_albedo_gain(
            glam::Vec3::new(0.10, 0.14, 0.24),
            0.15,
            glam::Vec3::new(0.06, 0.08, 0.14),
            0.85,
        );
        // At night, gain scales up (6.0..=9.0) so point lights & effects reveal crisp textures.
        assert!(
            night_gain >= 6.0 && night_gain <= 9.0,
            "nighttime scene_albedo_gain should recover textures (6.0..=9.0), got {night_gain}"
        );
    }

    #[test]
    fn restir_reservoir_matches_the_64_byte_shader_record() {
        assert_eq!(std::mem::size_of::<RestirReservoir>(), 64);
        assert_eq!(std::mem::align_of::<RestirReservoir>(), 4);
    }

    #[test]
    fn restir_di_packs_up_to_128_point_and_effect_lights() {
        let lights: Vec<super::super::frame::PointLight> = (0..150)
            .map(|i| super::super::frame::PointLight {
                position: glam::Vec3::new(i as f32, 2.0, 0.0),
                colour: glam::Vec3::new(1.0, 0.5, 0.2),
                radius: 20.0,
                intensity: 3.0,
                casts_shadow: true,
            })
            .collect();
        let settings = super::super::plan::Settings::default();
        let plan = super::super::plan::Plan::decide(&settings, (640, 360), false, |_| false);
        let lighting = super::super::frame::LightingRecord::default();
        let params = super::super::frame::FrameParams {
            eye: glam::Vec3::ZERO,
            inv_view_projection: glam::Mat4::IDENTITY,
            view_projection: glam::Mat4::IDENTITY,
            extent: (640, 360),
            grid: (640, 360),
            near: 0.1,
            far: 1000.0,
            jitter: (0.0, 0.0),
            frame: 0,
            history_valid: 0,
            sun: super::super::frame::Sun::from_lighting(&lighting),
            lighting,
            bounces: 2,
            samples: 1,
        };
        let config = super::super::shade::TransportConfig::default();
        let packed = RayParams::pack(&params, &plan, &config, [1, 1, 1], &lights);
        assert_eq!(packed.point_light_meta[0] as usize, MAX_RAY_LIGHTS);
        assert_eq!(packed.point_lights_pos_rad[127][0], 127.0);
        assert_eq!(packed.point_lights_col_int[127][3], 3.0);
    }

    #[test]
    fn point_light_pack_culls_only_lights_outside_the_far_influence_sphere() {
        let lighting = super::super::frame::LightingRecord::default();
        let params = super::super::frame::FrameParams {
            eye: glam::Vec3::ZERO,
            inv_view_projection: glam::Mat4::IDENTITY,
            view_projection: glam::Mat4::IDENTITY,
            extent: (64, 64),
            grid: (64, 64),
            near: 0.1,
            far: 100.0,
            jitter: (0.0, 0.0),
            frame: 0,
            history_valid: 0,
            sun: super::super::frame::Sun::from_lighting(&lighting),
            lighting,
            bounces: 1,
            samples: 1,
        };
        let lights = vec![
            super::super::frame::PointLight {
                position: glam::Vec3::new(1000.0, 0.0, 0.0),
                colour: glam::Vec3::ONE,
                radius: 10.0,
                intensity: 1.0,
                casts_shadow: true,
            },
            super::super::frame::PointLight {
                position: glam::Vec3::new(50.0, 0.0, 0.0),
                colour: glam::Vec3::ONE,
                radius: 5.0,
                intensity: 2.0,
                casts_shadow: true,
            },
            super::super::frame::PointLight {
                position: glam::Vec3::new(108.0, 0.0, 0.0),
                colour: glam::Vec3::ONE,
                radius: 10.0,
                intensity: 3.0,
                casts_shadow: true,
            },
        ];
        let plan = super::super::plan::Plan::decide(
            &super::super::plan::Settings::default(),
            (64, 64),
            false,
            |_| false,
        );
        let packed = RayParams::pack(
            &params,
            &plan,
            &super::super::shade::TransportConfig::default(),
            [1, 1, 1],
            &lights,
        );
        assert_eq!(packed.point_light_meta[0], 2.0);
        assert_eq!(packed.point_lights_pos_rad[0][0], 50.0);
        assert_eq!(packed.point_lights_pos_rad[1][0], 108.0);
    }

    #[test]
    fn all_wgsl_shaders_parse_and_validate() {
        let shaders = [
            ("GBUF_PREP_SHADER", GBUF_PREP_SHADER),
            ("RAY_SHADER", RAY_SHADER),
            ("DENOISE_SHADER", DENOISE_SHADER),
            ("COMPOSE_SHADER", COMPOSE_SHADER),
            ("BLIT_SHADER", BLIT_SHADER),
            ("COMPOSITE_SHADER", COMPOSITE_SHADER),
        ];
        let mut validator = wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        );
        for (name, source) in shaders {
            let module = wgpu::naga::front::wgsl::parse_str(source).unwrap_or_else(|err| {
                panic!("{name} WGSL parse failed:\n{}", err.emit_to_string(source))
            });
            validator.validate(&module).unwrap_or_else(|err| {
                panic!(
                    "{name} WGSL validation failed:\n{}",
                    err.emit_to_string(source)
                )
            });
        }
    }
}

#[cfg(test)]
mod shader_optimization_regression_tests {
    use super::{COMPOSITE_SHADER, RAY_SHADER};

    #[test]
    fn gi_adaptive_sampling_increases_near_field_rays_on_both_backends() {
        let dxr = include_str!("dxr_trace.hlsl");
        assert!(RAY_SHADER.contains("(1.0 - smoothstep(40.0, 220.0, dist)) * 0.50"));
        assert!(dxr.contains("(1.0 - smoothstep(40.0, 220.0, dist)) * 0.50"));
    }

    #[test]
    fn flat_water_avoids_duplicate_stochastic_reflection_samples_on_both_backends() {
        let dxr = include_str!("dxr_trace.hlsl");
        assert!(RAY_SHADER.contains("maximum <= base || is_water"));
        assert!(dxr.contains("max_samples <= base || is_water"));
        assert!(RAY_SHADER
            .contains("adaptive_ray_samples(ip, dist, normal, roughness, is_water, base_samples)"));
        assert!(dxr.contains(
            "adaptive_ray_samples(id.xy, dist, normal, roughness, is_water, base_samples)"
        ));
        assert!(RAY_SHADER.contains("(!is_water || s == 0u)"));
        assert!(dxr.contains("(!is_water || s == 0u)"));
    }

    #[test]
    fn native_water_uses_a_stable_planar_mirror_ray_and_physical_dielectric_fresnel() {
        let dxr = include_str!("dxr_trace.hlsl");
        assert!(RAY_SHADER.contains("maximum <= base || is_water"));
        assert!(RAY_SHADER.contains("var h = refl_base_n;"));
        assert!(RAY_SHADER.contains("if (!is_water)"));
        assert!(RAY_SHADER.contains("is_water && dot(base_normal, view_dir) > 0.0"));
        assert!(RAY_SHADER.contains("select(0.04, 0.0204, is_water)"));
        assert!(!RAY_SHADER.contains("complexity = max(complexity, 0.42)"));
        assert!(dxr.contains("if (is_water && !trace_water_reflection)"));
        assert!(dxr.contains("float3 h = refl_base_n;"));
        assert!(dxr.contains("if (!is_water)"));
        assert!(dxr.contains("if (is_water && dot(normal, view_dir) > 0.0)"));
        assert!(dxr.contains("is_water ? 0.0204"));
        assert!(!dxr.contains("complexity = max(complexity, 0.42)"));
    }

    #[test]
    fn restir_gi_reconstructs_the_cosine_weighted_integrand_without_grazing_blowup() {
        let dxr = include_str!("dxr_trace.hlsl");
        let radiance = glam::Vec3::new(0.8, 0.35, 0.12);
        let luminance = radiance.dot(glam::Vec3::new(0.2126, 0.7152, 0.0722));

        // One candidate: W = target / proposal_pdf, with both target and
        // proposal containing cos(theta). The selected integrand must restore
        // that cosine before the 1 / target normalisation is applied.
        for cosine in [1.0_f32, 0.5, 0.1, 0.01, 0.001] {
            let target = luminance * cosine;
            let candidate_weight = target / cosine;
            let normalisation = candidate_weight / target;
            let estimate = radiance * (normalisation * cosine);
            assert!(
                (estimate - radiance).length() < 1e-4,
                "cosine {cosine} changed an otherwise identical one-candidate estimate: {estimate:?}"
            );
        }

        assert!(RAY_SHADER.contains("accum_gi = gi_estimate * (f32(samples) * selected_cosine);"));
        assert!(dxr.contains("normalisation * float(samples) * selected_cosine"));
    }

    #[test]
    fn composite_keeps_native_water_until_rt_reflections_are_ready() {
        let dxr = include_str!("dxr_trace.hlsl");
        assert!(COMPOSITE_SHADER.contains("if (is_water && comp_params.z <= 0.5)"));
        assert!(COMPOSITE_SHADER.contains("if (comp_params.y > 0.5)"));
        assert!(COMPOSITE_SHADER.contains("(tap_norm_raw.w <= 0.12) != is_water"));
        assert!(COMPOSITE_SHADER.contains("raster.rgb + rt.rgb"));
        assert!(COMPOSITE_SHADER.contains("return raster;"));
        assert!(COMPOSITE_SHADER.contains("return textureLoad(src_rt, px, 0);"));
        assert!(RAY_SHADER.contains("(!is_water || s == 0u)"));
        assert!(RAY_SHADER.contains("select(1.0, f32(samples), is_water)"));
        assert!(dxr.contains("(!is_water || s == 0u)"));
    }

    #[test]
    fn planar_water_reflects_into_the_camera_side_above_and_below_the_surface() {
        let incoming_above = glam::Vec3::new(0.3, -0.8, 0.5).normalize();
        let incoming_below = glam::Vec3::new(0.3, 0.8, 0.5).normalize();
        let normal_above = glam::Vec3::Y;
        let normal_below = glam::Vec3::NEG_Y;
        let reflected_above =
            incoming_above - 2.0 * incoming_above.dot(normal_above) * normal_above;
        let reflected_below =
            incoming_below - 2.0 * incoming_below.dot(normal_below) * normal_below;
        assert!(reflected_above.dot(normal_above) > 0.0);
        assert!(reflected_below.dot(normal_below) > 0.0);
    }

    #[test]
    fn water_reflections_use_rt_mirror_rays_on_both_sides_and_replace_the_native_mirror() {
        let dxr = include_str!("dxr_trace.hlsl");
        let water = include_str!("../../water_gpu.rs");
        assert!(!RAY_SHADER.contains("params.eye.y > point.y + 0.05"));
        assert!(!dxr.contains("params.eye.y > world_pos.y + 0.05"));
        assert!(RAY_SHADER.contains("if (is_water && debug_mode_early == 0u)"));
        assert!(dxr.contains("if (is_water && debug_mode_early == 0u)"));
        assert!(water.contains("w.fog.w>0.5"));
        assert!(COMPOSITE_SHADER.contains("comp_params.z <= 0.5"));
        assert!(COMPOSITE_SHADER.contains("raster.rgb + rt.rgb"));
    }

    #[test]
    fn restir_history_requires_the_host_camera_reset_flag() {
        assert!(!super::restir_history_valid_for_frame(0, 12, true));
        assert!(!super::restir_history_valid_for_frame(1, 0, true));
        assert!(!super::restir_history_valid_for_frame(1, 12, false));
        assert!(super::restir_history_valid_for_frame(1, 12, true));
    }

    #[test]
    fn restir_history_reweighting_uses_target_over_cosine_pdf() {
        let dxr = include_str!("dxr_trace.hlsl");
        let radiance_luma = 0.42_f32;
        let old_cosine = 0.8_f32;
        let new_cosine = 0.2_f32;
        let old_target = radiance_luma * old_cosine;
        let new_target = radiance_luma * new_cosine;

        // q = luminance * cosine and p = cosine / PI; PI cancels in
        // the ratio. Identical radiance therefore has the same q/p at both
        // receivers. Reweighting by q alone would incorrectly scale history
        // by new_cosine / old_cosine (= 0.25).
        let old_target_over_pdf = old_target / old_cosine;
        let new_target_over_pdf = new_target / new_cosine;
        let reuse_scale = new_target_over_pdf / old_target_over_pdf;
        assert!((reuse_scale - 1.0).abs() < 1e-6);
        assert!((new_target / old_target - 0.25).abs() < 1e-6);

        assert!(RAY_SHADER.contains("let target_over_pdf = target_density / max(cosine, 1e-5);"));
        assert!(RAY_SHADER.contains("target_over_pdf, weight, represented, is_sky"));
        assert!(dxr.contains("float target_over_pdf = target / max(cosine, 1e-5);"));
        assert!(dxr.contains("target_over_pdf, weight, represented, is_sky"));
    }

    #[test]
    fn collision_prone_radiance_cache_is_disabled_in_both_backends() {
        let dxr = include_str!("dxr_trace.hlsl");
        assert!(RAY_SHADER.contains("fn rc_query("));
        assert!(RAY_SHADER.contains("Disabled: this 16-bit spatial hash stores no key/tag"));
        assert!(!RAY_SHADER.contains("atomicLoad(&radiance_cache"));
        assert!(!RAY_SHADER.contains("atomicAdd(&radiance_cache"));
        assert!(dxr.contains("float4 rc_query("));
        assert!(dxr.contains("Disabled: this 16-bit spatial hash stores no key/tag"));
        assert!(!dxr.contains("InterlockedAdd(radiance_cache[slot]"));
        assert_eq!(super::RADIANCE_CACHE_CELLS * 16, 1024 * 1024);
    }

    #[test]
    fn sun_moon_volumetric_shafts_stay_disabled_while_celestial_sprites_render() {
        let dxr = include_str!("dxr_trace.hlsl");
        assert!(!RAY_SHADER.contains("True World-Space Ray-Marched Volumetric God-Rays"));
        assert!(!dxr.contains("True World-Space Ray-Marched Volumetric God-Rays"));
        assert!(RAY_SHADER.contains("point_light_halos"));
        assert!(dxr.contains("point_light_halos"));

        let sky_source = include_str!("../../sky_gpu.rs");
        let shader = sky_source
            .split("pub(crate) const SHADER: &str = r#\"")
            .nth(1)
            .and_then(|tail| tail.split("\"#;").next())
            .expect("sky shader literal");
        assert!(shader.contains("textureSampleLevel(skies,smp,uv,disc,0.)"));
        assert!(shader.contains("shine_ramp("));
        assert!(shader.contains("textureSampleLevel(skies,smp,suv,glow,0.)"));
        assert!(shader.contains("p.moon_glow.z>0.5"));
        assert!(shader.contains("textureSampleLevel(skies,smp,halo_uv,glow,0.)"));
        assert!(shader.contains("flare_chain("));
        assert!(shader.contains("textureSampleLevel(skies,smp,sunflare_uv,ring,0.)"));
        assert!(shader.contains("textureSampleLevel(skies,smp,uv,flare,0.)"));
    }

    #[test]
    fn software_and_dxr_point_lights_reuse_one_inverse_square_root() {
        let dxr = include_str!("dxr_trace.hlsl");
        assert_eq!(
            RAY_SHADER
                .matches("let inv_pl_dist = inverseSqrt(max(pl_d2, 1e-10));")
                .count(),
            2
        );
        assert_eq!(
            dxr.matches("float inv_pl_dist = rsqrt(max(pl_d2, 1e-10));")
                .count(),
            2
        );
    }

    #[test]
    fn software_screen_radiance_uses_squared_distance_like_dxr() {
        let dxr = include_str!("dxr_trace.hlsl");
        assert!(RAY_SHADER.contains("dot(separation, separation) < tolerance * tolerance"));
        assert!(
            dxr.contains("dot(screen_delta, screen_delta) < screen_tolerance * screen_tolerance")
        );
    }

    #[test]
    fn dxr_integer_sky_powers_use_multiplication_chains() {
        let dxr = include_str!("dxr_trace.hlsl");
        assert!(dxr.contains("float sun_cos2 = sun_cos * sun_cos;"));
        assert!(dxr.contains("float sun_cos4 = sun_cos2 * sun_cos2;"));
        assert!(dxr.contains("float mie_glow = sun_cos4 * sun_cos2 * 0.85 + sun_cos2 * 0.35;"));
        assert!(!dxr.contains("pow(sun_cos, 6.0)"));
        assert!(!dxr.contains("pow(sun_cos, 2.0)"));
    }
}

#[cfg(test)]
mod dxr_backend_selection_tests {
    use super::{adaptive_sample_ceiling, hardware_trace_ready, software_fallback_ready};

    #[test]
    fn adaptive_ray_ceiling_preserves_original_visual_quality() {
        for samples in [0, 1, 2, 4, 8, 16] {
            assert_eq!(adaptive_sample_ceiling(samples), 4.0);
        }
    }

    #[test]
    fn software_fallback_requires_a_cpu_bvh_for_non_empty_scenes() {
        assert!(software_fallback_ready(0, 0));
        assert!(software_fallback_ready(32, 1));
        assert!(!software_fallback_ready(32, 0));
    }

    #[test]
    fn hardware_scene_rebuild_does_not_report_hardware_ready_until_dxr_is_ready() {
        assert!(!hardware_trace_ready(true, false, false, true));
        assert!(!hardware_trace_ready(true, false, false, false));
        assert!(hardware_trace_ready(true, false, true, false));
        assert!(!hardware_trace_ready(true, true, true, false));
        assert!(!hardware_trace_ready(false, false, true, false));
    }
}

#[cfg(test)]
mod reflection_sampling_regression_tests {
    use super::*;

    const DXR_SHADER: &str = include_str!("dxr_trace.hlsl");

    #[test]
    fn reflections_fetch_colour_from_full_resolution_raster() {
        assert!(RAY_SHADER.contains("@group(1) @binding(0) var src_screen_raster"));
        assert!(RAY_SHADER.contains("textureLoad(src_screen_raster, screen_px, 0)"));
        assert!(!RAY_SHADER.contains("textureLoad(src_gbuf_raster, hp, 0).rgb"));
        assert!(DXR_SHADER.contains("src_screen_raster : register(t8)"));
        assert!(DXR_SHADER.contains("src_screen_raster.GetDimensions"));
        assert!(DXR_SHADER.contains("src_screen_raster.Load(int3(screen_px, 0))"));
    }

    #[test]
    fn sky_and_cloud_reflection_atlas_uses_four_times_the_previous_edge_resolution() {
        assert_eq!(SKY_ENV_FACE, 512);
        assert!(DXR_SHADER.contains("SKY_ENV_FACE_F = 512.0"));
    }

    #[test]
    fn layered_rt_radiance_is_attenuated_by_client_fog() {
        assert!(RAY_SHADER.contains("out_rgb = (avg_gi + point_lighting) * (1.0 - fog_t)"));
    }

    #[test]
    fn water_reflection_history_is_not_clamped_to_noisy_neighbourhood_samples() {
        assert!(DENOISE_SHADER.contains("prev.colour,\n            is_glossy,"));
    }

    #[test]
    fn steep_terrain_depth_gradients_are_not_rejected_as_discontinuities() {
        assert!(GBUF_PREP_SHADER.contains("let max_rel_dz = 0.18;"));
    }
}
