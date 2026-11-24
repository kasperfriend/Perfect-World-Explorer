//! Alternate authored water-area material. Fixed-function texture stages recovered
//! from the PW 1.4.2 client (0x7cb4b0). Lighting/fog and rare hardware variants
//! are still approximations; see CLIENT_RESEARCH.md.
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    matrix: [[f32; 4]; 4],
    data: [f32; 4],
    fog: [f32; 4],
    fog_color: [f32; 4],
}
const SHADER: &str = r#"
struct U {matrix:mat4x4<f32>,data:vec4<f32>,fog:vec4<f32>,fog_color:vec4<f32>};
@group(0) @binding(0) var<uniform> u:U;
@group(0) @binding(1) var tex:texture_2d_array<f32>;
@group(0) @binding(2) var smp:sampler;
struct I {@location(0) pos:vec3<f32>,@location(2) uv:vec2<f32>};
struct O {@builtin(position) p:vec4<f32>,@location(0) uv:vec2<f32>};
@vertex fn vs(i:I)->O {var o:O;o.p=u.matrix*vec4<f32>(i.pos,1.);o.uv=i.uv;return o;}
@fragment fn fs(i:O)->@location(0) vec4<f32> {
 let uv=i.uv*2.5+0.0125*vec2<f32>(cos(u.data.y),sin(u.data.y));
 let t=textureSample(tex,smp,uv,i32(u.data.x));
 // D3DTOP_BLENDTEXTUREALPHA: arg1=texture, arg2=tfactor.
 // Stage 1's MODULATEALPHA_ADDCOLOR adds specular, which the
 // paired water vertex shader outputs as zero. Alpha stage adds
 // texture alpha and the 0xa0 factor alpha, saturating as D3D does.
 var rgb=mix(vec3<f32>(37.,79.,85.)/255.,t.rgb,t.a);
 // Same encoded-colour table-fog stage as reflected water and terrain.
 // Blend on the raw attachment, preserving the authored texture alpha.
 if u.fog.z>0.5 {
  let depth=1./max(i.p.w,0.0000001);
  let span=u.fog.y-u.fog.x;
  let f=select(clamp((depth-u.fog.x)/select(span,0.001,abs(span)<0.001),0.,1.),select(0.,1.,depth>=u.fog.y),abs(span)<0.001);
  rgb=mix(rgb,u.fog_color.rgb,f);
 }
 let a=min(t.a+160./255.,1.);
 return vec4<f32>(rgb,a);
}
"#;
pub struct SimpleWaterGpu {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    uniform: wgpu::Buffer,
}
impl SimpleWaterGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        frames: &[Vec<image::RgbaImage>],
    ) -> Self {
        Self::new_for_map(device, queue, format, frames, true)
    }

    pub fn new_for_map(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        frames: &[Vec<image::RgbaImage>],
        has_water: bool,
    ) -> Self {
        let texture = if has_water {
            assert_eq!(frames.len(), 30);
            let (w, h) = frames[0][0].dimensions();
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("client alternate-water frames"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 30,
                },
                mip_level_count: frames[0].len() as u32,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            for (layer, levels) in frames.iter().enumerate() {
                for (level, img) in levels.iter().enumerate() {
                    let (w, h) = img.dimensions();
                    queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &texture,
                            mip_level: level as u32,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: 0,
                                z: layer as u32,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        img.as_raw(),
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(w * 4),
                            rows_per_image: Some(h),
                        },
                        wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
            texture
        } else {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("client alternate-water frames (dry map dummy)"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("simple water constants"),
            contents: bytemuck::bytes_of(&Uniform {
                matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
                data: [0.; 4],
                fog: [0.; 4],
                fog_color: [0.; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
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
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("client simple water stages"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("simple water encoded blend"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<crate::gpu::Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 24,
                            shader_location: 2,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: format.remove_srgb_suffix(),
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        Self {
            pipeline,
            bind,
            uniform,
        }
    }
    pub fn update(
        &self,
        queue: &wgpu::Queue,
        matrix: glam::Mat4,
        seconds: f64,
        fog: [f32; 4],
        fog_color: [f32; 4],
    ) {
        let phase = if seconds.is_finite() {
            seconds.max(0.)
        } else {
            0.
        };
        let frame = ((phase / 0.033).floor() as u64 % 30) as f32;
        let angle = crate::water_gpu::normal_offset_angle(seconds);
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&Uniform {
                matrix: matrix.to_cols_array_2d(),
                data: [frame, angle, 0., 0.],
                fog,
                fog_color,
            }),
        );
    }
    pub fn draw(
        &self,
        e: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        vertex: &wgpu::Buffer,
        index: &wgpu::Buffer,
        start: u32,
        end: u32,
    ) {
        let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("alternate water area"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        p.set_pipeline(&self.pipeline);
        p.set_bind_group(0, &self.bind, &[]);
        p.set_vertex_buffer(0, vertex.slice(..));
        p.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
        p.draw_indexed(start..end, 0, 0..1);
    }
}
