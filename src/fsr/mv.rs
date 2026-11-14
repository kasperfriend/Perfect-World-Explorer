//! The shared depth-reprojection motion-vector pass. Used by the FSR
//! upscaler and by the FSR frame-generation pipeline; the two FFX dispatches
//! consume the same Rg16Float motion vectors and R32_FLOAT raw reversed-Z
//! depth this pass emits.

use wgpu::TextureFormat;

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
                out.mv = (uv - prev_uv) * size;
            }
        }
    }
    return out;
}
"#;

pub(crate) const COMPOSITE_SHADER: &str = r#"
@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) id: u32) -> VOut {
    var o: VOut;
    let x = f32((id << 1u) & 2u);
    let y = f32(id & 2u);
    o.pos = vec4<f32>(x * 2. - 1., 1. - y * 2., 0., 1.);
    o.uv = vec2<f32>(x, y);
    return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(frame, samp, i.uv).rgb, 1.0);
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
