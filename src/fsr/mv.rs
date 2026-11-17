//! The shared motion-vector pass. Every consumer — the FSR upscaler, the FSR
//! frame-generation pipeline, and the DLSS and XeSS equivalents on both paths —
//! runs this one shader and hands the vendor the same Rg16Float vectors and
//! R32_FLOAT raw reversed-Z depth it emits.
//!
//! **Camera motion comes from the depth buffer; object motion comes from an
//! attachment.** The bulk of the pass reconstructs each pixel's world position
//! from depth and reprojects it with the previous view-projection, which is exact
//! for everything that stands still in the world. Content that moves *on its own*
//! cannot be recovered that way — depth carries no identity — so the passes that
//! draw such content rasterise it a second time into `object_mv`, an
//! `Rgba16Float` target at render extent where `xy` is the pixel-space motion
//! vector and `z` is a coverage flag (`> 0.5` means "this pixel is mine"). Those
//! writes are authoritative: where the flag is set the pass takes the vector it
//! was handed, otherwise it keeps the depth reprojection. Precipitation is the
//! one producer today (`weather_gpu`), because its particles are analytic in the
//! clock and therefore have an exact previous position; see `docs/blockers.md`
//! B-25 for why model-attached sheets do not need a producer of their own.

use glam::Mat4;
use wgpu::TextureFormat;

/// Clip-space jitter for a view-projection matrix.
///
/// The jitter must land as a constant *NDC* offset, i.e. `clip.xy += offset *
/// clip.w` — the same thing the vendor guides describe as editing the
/// projection matrix's `[2][0]` / `[2][1]` shear terms (DLSS: "jitter must be
/// applied to the projection matrix", FSR: `jitteredVP`, XeSS: `xessSetJitter`).
///
/// Adding a plain translation to the combined matrix instead (`T * VP`) shifts
/// *clip* space: after the perspective divide the on-screen offset becomes
/// `offset / clip.w`, so it shrinks with depth. Near geometry jumps by whole
/// pixels while the distance barely moves, which is exactly what a temporal
/// accumulator cannot undo — it reads as heavy blur and ghosting that gets worse
/// towards the horizon, and none of DLSS, XeSS or FSR can converge. Building the
/// shear with the offset in the `w` column gives every pixel the same sub-pixel
/// displacement, which is the premise they all share when they are also handed
/// the same offset in pixels.
///
/// `jitter` is in render pixels with y down (the convention every vendor uses);
/// NDC y is up, so the y term flips.
pub(crate) fn jittered_clip_vp(matrix: Mat4, jitter: (f32, f32), extent: (u32, u32)) -> Mat4 {
    let w = extent.0.max(1) as f32;
    let h = extent.1.max(1) as f32;
    let mut shear = Mat4::IDENTITY;
    shear.w_axis = glam::Vec4::new(2.0 * jitter.0 / w, -2.0 * jitter.1 / h, 0.0, 1.0);
    shear * matrix
}

const MV_SHADER: &str = r#"
struct MvParams {
    inv_vp: mat4x4<f32>,
    prev_vp: mat4x4<f32>,
    render_size: vec2<f32>,
    prev_valid: u32,
    _pad: u32,
};
@group(0) @binding(0) var depth: texture_depth_2d;
@group(0) @binding(1) var<uniform> params: MvParams;
// `xy` pixel-space motion (current + mv = previous), `z` coverage. Written by
// the passes that know an object's own motion; zero everywhere else.
@group(0) @binding(2) var object_mv: texture_2d<f32>;
struct VOut { @builtin(position) pos: vec4<f32> };
@vertex fn vs(@builtin(vertex_index) id: u32) -> VOut {
    var o: VOut;
    let x = f32((id << 1u) & 2u);
    let y = f32(id & 2u);
    o.pos = vec4<f32>(x * 2. - 1., 1. - y * 2., 0., 1.);
    return o;
}
struct FOut {
    // Motion vectors in render-resolution pixel space (rg16f).
    @location(0) mv: vec2<f32>,
    // Raw reversed-Z depth passthrough (r32f): the upscaler samples depth
    // straight from this copy so the input is a plain R32_FLOAT surface
    // instead of a depth-stencil-formatted one it cannot bind as an SRV.
    @location(1) raw_depth: f32,
};
@fragment fn fs(i: VOut) -> FOut {
    let size = params.render_size;
    let clamped = clamp(vec2<i32>(floor(i.pos.xy)), vec2<i32>(0, 0), vec2<i32>(size) - vec2<i32>(1, 1));
    let d = textureLoad(depth, clamped, 0);
    var out: FOut;
    out.raw_depth = d;
    out.mv = vec2<f32>(0.0, 0.0);
    if (params.prev_valid != 0u) {
        let px = floor(i.pos.xy) + vec2<f32>(0.5, 0.5);
        let uv = px / size;
        let ndc = vec4<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, d, 1.0);
        var world = params.inv_vp * ndc;
        if (abs(world.w) >= 1e-8) {
            world = world / world.w;
            let clip = params.prev_vp * world;
            if (abs(clip.w) >= 1e-8) {
                let prev_ndc = clip.xy / clip.w;
                let prev_uv = vec2<f32>((prev_ndc.x + 1.0) * 0.5, (1.0 - prev_ndc.y) * 0.5);
                out.mv = (prev_uv - uv) * size;
            }
        }
    }
    // Content that moves on its own overrides the reprojection: the pass that
    // drew it knew both the current and the previous position, which the depth
    // buffer cannot express.
    let object = textureLoad(object_mv, clamped, 0);
    if (object.z > 0.5) {
        out.mv = object.xy;
    }
    return out;
}
"#;

pub(crate) const COMPOSITE_SHADER: &str = r#"
@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
// x = sharpen gain (0 = off; the slider's 1.0 arrives as 1.5), yz = source
// texel size in UV units.
@group(0) @binding(2) var<uniform> sharpen: vec4<f32>;
fn srgb_to_linear(c_: vec3<f32>) -> vec3<f32> {
    let low = c_ / vec3<f32>(12.92);
    let high = pow((c_ + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(high, low, c_ <= vec3<f32>(0.04045));
}
// Unsharp against the 4-neighbour average, clamped to the local neighbourhood so
// even the maximum slider setting cannot ring or halo. `decode` is set when the
// source is sRGB-encoded and the target expects linear (see `fs_decode`).
fn sharpen_rgb(uv: vec2<f32>, centre: vec3<f32>, decode: bool) -> vec3<f32> {
    let t = sharpen.yz;
    var n = textureSample(frame, samp, uv + vec2<f32>(0.0, -t.y)).rgb;
    var s_ = textureSample(frame, samp, uv + vec2<f32>(0.0, t.y)).rgb;
    var e = textureSample(frame, samp, uv + vec2<f32>(t.x, 0.0)).rgb;
    var w = textureSample(frame, samp, uv + vec2<f32>(-t.x, 0.0)).rgb;
    if (decode) {
        n = srgb_to_linear(n);
        s_ = srgb_to_linear(s_);
        e = srgb_to_linear(e);
        w = srgb_to_linear(w);
    }
    let avg = (n + s_ + e + w) * 0.25;
    let lo = min(centre, min(min(n, s_), min(e, w)));
    let hi = max(centre, max(max(n, s_), max(e, w)));
    return clamp(centre + (centre - avg) * sharpen.x, lo, hi);
}
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) id: u32) -> VOut {
    var o: VOut;
    let x = f32((id << 1u) & 2u);
    let y = f32(id & 2u);
    o.pos = vec4<f32>(x * 2. - 1., 1. - y * 2., 0., 1.);
    o.uv = vec2<f32>(x, y);
    return o;
}
// No branch on the strength: at 0 the filter reduces to the centre texel
// exactly (the clamp keeps the centre inside [lo, hi]), so leaving it
// unconditional costs four taps and cannot trip WGSL's uniform-control-flow
// rules for the sampler reads inside `sharpen_rgb`.
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    let c_ = textureSample(frame, samp, i.uv).rgb;
    return vec4<f32>(sharpen_rgb(i.uv, c_, false), 1.0);
}
// The world is rendered into an sRGB target, so the bytes the upscaler reads -
// and the bytes it writes back - are sRGB encoded. Copying them straight into an
// sRGB render target encodes them a second time: mid grey (0.5 encoded) lands at
// 0.735, so every upscaled frame came out washed out and flat while the bilinear
// `RenderScale` path (which samples an sRGB texture, i.e. let the hardware
// decode) looked correct. Decoding here lets the target's own encode restore the
// exact original bytes.
@fragment fn fs_decode(i: VOut) -> @location(0) vec4<f32> {
    let c_ = srgb_to_linear(textureSample(frame, samp, i.uv).rgb);
    return vec4<f32>(sharpen_rgb(i.uv, c_, true), 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FsrMvParams {
    pub(crate) inv_vp: [[f32; 4]; 4],
    pub(crate) prev_vp: [[f32; 4]; 4],
    pub(crate) render_size: [f32; 2],
    pub(crate) prev_valid: u32,
    pub(crate) pad: u32,
}

pub(crate) struct FsrMotionVectorPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
}

impl FsrMotionVectorPass {
    pub(crate) fn pipeline(&self) -> &wgpu::RenderPipeline {
        &self.pipeline
    }

    pub(crate) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    pub(crate) fn params_buffer(&self) -> &wgpu::Buffer {
        &self.params
    }
}

impl FsrMotionVectorPass {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fsr motion vector layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // `textureLoad` only, so it never needs to be filterable.
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fsr motion vector WGSL"),
            source: wgpu::ShaderSource::Wgsl(MV_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fsr motion vector pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fsr motion vector pass"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rg16Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::R32Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fsr motion vector params"),
            size: std::mem::size_of::<FsrMvParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            params,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MV_SHADER;

    /// The pass has two producers: the depth reprojection it computes itself, and
    /// the object-motion attachment a pass that knows an object's own motion
    /// writes. The attachment wins where its coverage flag is set — otherwise a
    /// falling particle would be reprojected as if it stood still, which is the
    /// failure this binding exists to remove.
    #[test]
    fn the_attachment_overrides_the_reprojection_where_it_is_flagged() {
        let read = "textureLoad(object_mv, clamped, 0)";
        assert_eq!(
            MV_SHADER.matches(read).count(),
            1,
            "the pass must read the object-motion attachment"
        );
        assert_eq!(
            MV_SHADER.matches("object.z > 0.5").count(),
            1,
            "the coverage flag decides who owns the pixel"
        );
        // The override has to happen after the reprojection so it replaces it.
        let reproject = MV_SHADER.find("out.mv = (prev_uv - uv) * size").unwrap();
        let override_at = MV_SHADER.find("out.mv = object.xy").unwrap();
        assert!(
            override_at > reproject,
            "the attachment must override the reprojected vector, not be overwritten by it"
        );
    }
}
