//! Encoded-UNORM receiver for the native static 3D decal quads.
//!
//! A3DGFXRenderSlotMan uses MODULATE for colour and alpha, disables lighting,
//! culling and depth writes, and selects the authored D3D8 blend pair. Fog and
//! alpha testing default off in that manager; neither is invented here.

use crate::{gfx_decal, model_queue, model_texture_gpu};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
}

impl From<&gfx_decal::Vertex> for Vertex {
    fn from(vertex: &gfx_decal::Vertex) -> Self {
        Self {
            position: vertex.position,
            uv: vertex.uv,
            color: vertex.color,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    matrix: [[f32; 4]; 4],
    clip_plane: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PipelineKey {
    source: u32,
    destination: u32,
    depth_test: bool,
}

struct Quad {
    key: PipelineKey,
    texture: String,
    vertices: wgpu::Buffer,
    binding: wgpu::BindGroup,
}

pub struct Draw {
    quads: Vec<Quad>,
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,
    uniform: wgpu::Buffer,
    // Bind groups retain references to these resources internally.
    _textures: HashMap<String, wgpu::Texture>,
    _sampler: wgpu::Sampler,
    indices: wgpu::Buffer,
}

fn blend_factor(code: u32) -> Result<wgpu::BlendFactor, String> {
    Ok(match model_queue::blend_factor(code)? {
        model_queue::Factor::Zero => wgpu::BlendFactor::Zero,
        model_queue::Factor::One => wgpu::BlendFactor::One,
        model_queue::Factor::SrcColor => wgpu::BlendFactor::Src,
        model_queue::Factor::InvSrcColor => wgpu::BlendFactor::OneMinusSrc,
        model_queue::Factor::SrcAlpha => wgpu::BlendFactor::SrcAlpha,
        model_queue::Factor::InvSrcAlpha => wgpu::BlendFactor::OneMinusSrcAlpha,
        model_queue::Factor::DestAlpha => wgpu::BlendFactor::DstAlpha,
        model_queue::Factor::InvDestAlpha => wgpu::BlendFactor::OneMinusDstAlpha,
        model_queue::Factor::DestColor => wgpu::BlendFactor::Dst,
        model_queue::Factor::InvDestColor => wgpu::BlendFactor::OneMinusDst,
    })
}

impl Draw {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        depth: Option<(wgpu::TextureFormat, wgpu::CompareFunction)>,
        quads: &[gfx_decal::Quad],
        mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Self, String> {
        if !matches!(
            format,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
        ) {
            return Err("GFX decals require an encoded UNORM target".into());
        }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("native static GFX decal sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("static GFX decal frame constants"),
            contents: bytemuck::bytes_of(&Params {
                matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
                clip_plane: [0.; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("native GFX quad indices"),
            contents: bytemuck::cast_slice(&[0u16, 1, 2, 2, 1, 3]),
            usage: wgpu::BufferUsages::INDEX,
        });
        let binding_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("static GFX decal binding layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
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
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("static GFX decal pipeline layout"),
            bind_group_layouts: &[&binding_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native static GFX decal shader"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let mut pipelines = HashMap::new();
        let mut textures = HashMap::new();
        let mut gpu_quads = Vec::with_capacity(quads.len());
        for quad in quads {
            let key = PipelineKey {
                source: quad.source_blend,
                destination: quad.destination_blend,
                depth_test: quad.depth_test,
            };
            if !pipelines.contains_key(&key) {
                let pipeline = pipeline(device, &pipeline_layout, &shader, format, depth, key)?;
                pipelines.insert(key, pipeline);
            }
            if !textures.contains_key(&quad.texture) {
                let bytes =
                    read(&quad.texture).map_err(|error| format!("{}: {error}", quad.texture))?;
                let texture = model_texture_gpu::upload(
                    device,
                    queue,
                    &bytes,
                    model_texture_gpu::UploadMode::Auto,
                )
                .map_err(|error| format!("{}: {error}", quad.texture))?;
                textures.insert(quad.texture.clone(), texture);
            }
            let texture = &textures[&quad.texture];
            let view = texture.create_view(&Default::default());
            let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("static GFX decal texture/frame binding"),
                layout: &binding_layout,
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
            let vertices = quad.vertices.iter().map(Vertex::from).collect::<Vec<_>>();
            let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("native static GFX decal quad"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
            gpu_quads.push(Quad {
                key,
                texture: quad.texture.clone(),
                vertices,
                binding,
            });
        }
        Ok(Self {
            quads: gpu_quads,
            pipelines,
            uniform,
            _textures: textures,
            _sampler: sampler,
            indices,
        })
    }

    pub fn update(
        &self,
        queue: &wgpu::Queue,
        matrix: [[f32; 4]; 4],
        clip_plane: [f32; 4],
        quads: &[gfx_decal::Quad],
    ) -> Result<(), String> {
        if matrix
            .iter()
            .flatten()
            .chain(clip_plane.iter())
            .any(|v| !v.is_finite())
        {
            return Err("non-finite GFX decal frame constants".into());
        }
        if quads.len() != self.quads.len() {
            return Err("GFX decal topology changed without rebuilding GPU resources".into());
        }
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&Params { matrix, clip_plane }),
        );
        for (gpu, quad) in self.quads.iter().zip(quads) {
            let key = PipelineKey {
                source: quad.source_blend,
                destination: quad.destination_blend,
                depth_test: quad.depth_test,
            };
            if key != gpu.key || quad.texture != gpu.texture {
                return Err(
                    "GFX decal texture/state changed without rebuilding GPU resources".into(),
                );
            }
            let vertices: [Vertex; 4] = std::array::from_fn(|i| Vertex::from(&quad.vertices[i]));
            if bytemuck::cast_slice::<Vertex, f32>(&vertices)
                .iter()
                .any(|value| !value.is_finite())
            {
                return Err("non-finite GFX decal vertex".into());
            }
            queue.write_buffer(&gpu.vertices, 0, bytemuck::cast_slice(&vertices));
        }
        Ok(())
    }

    pub fn draw_quads<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.quads.is_empty() {
            return;
        }
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint16);
        let mut last_key = None;
        for quad in &self.quads {
            if last_key != Some(quad.key) {
                pass.set_pipeline(&self.pipelines[&quad.key]);
                last_key = Some(quad.key);
            }
            pass.set_bind_group(0, &quad.binding, &[]);
            pass.set_vertex_buffer(0, quad.vertices.slice(..));
            pass.draw_indexed(0..6, 0, 0..1);
        }
    }
}

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    depth: Option<(wgpu::TextureFormat, wgpu::CompareFunction)>,
    key: PipelineKey,
) -> Result<wgpu::RenderPipeline, String> {
    let component = wgpu::BlendComponent {
        src_factor: blend_factor(key.source)?,
        dst_factor: blend_factor(key.destination)?,
        operation: wgpu::BlendOperation::Add,
    };
    let attributes = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x2,2=>Float32x4];
    let buffers = [wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    }];
    Ok(
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("native static GFX decal receiver"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: "vs",
                buffers: &buffers,
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: component,
                        alpha: component,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: depth.map(|(format, compare)| wgpu::DepthStencilState {
                format,
                depth_write_enabled: false,
                depth_compare: if key.depth_test {
                    compare
                } else {
                    wgpu::CompareFunction::Always
                },
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        }),
    )
}

const WGSL: &str = r#"
struct Params { matrix:mat4x4<f32>, clip_plane:vec4<f32> };
@group(0) @binding(0) var<uniform> p:Params;
@group(0) @binding(1) var image:texture_2d<f32>;
@group(0) @binding(2) var image_sampler:sampler;
struct Input {
 @location(0) position:vec3<f32>,
 @location(1) uv:vec2<f32>,
 @location(2) color:vec4<f32>,
};
struct Output {
 @builtin(position) position:vec4<f32>,
 @location(0) uv:vec2<f32>,
 @location(1) color:vec4<f32>,
 @location(2) world:vec3<f32>,
};
@vertex fn vs(v:Input)->Output {
 var o:Output;
 o.position=p.matrix*vec4<f32>(v.position,1.);
 o.world=v.position;
 o.uv=v.uv;
 o.color=v.color;
 return o;
}
@fragment fn fs(v:Output)->@location(0) vec4<f32> {
 if dot(vec4<f32>(v.world,1.),p.clip_plane)<0. {discard;}
 return textureSample(image,image_sampler,v.uv)*v.color;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_static_decals_modulate_and_blend_on_encoded_unorm() {
        let instance = wgpu::Instance::default();
        let Some(adapter) = pollster::block_on(instance.request_adapter(&Default::default()))
        else {
            eprintln!("static decal raster test skipped: no WGPU adapter");
            return;
        };
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            },
            None,
        ))
        .unwrap();
        let vertex = |position, uv| gfx_decal::Vertex {
            position,
            uv,
            color: [0.5, 1., 0.25, 0.5],
        };
        let quad = gfx_decal::Quad {
            texture: "test.dds".into(),
            source_blend: 5,
            destination_blend: 2,
            depth_test: false,
            vertices: [
                vertex([-1., 1., 0.], [0., 0.]),
                vertex([1., 1., 0.], [1., 0.]),
                vertex([-1., -1., 0.], [0., 1.]),
                vertex([1., -1., 0.], [1., 1.]),
            ],
        };
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([128, 64, 32, 128]),
        ));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let draw = Draw::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            None,
            std::slice::from_ref(&quad),
            |_| Ok(png.clone()),
        )
        .unwrap();
        draw.update(
            &queue,
            glam::Mat4::IDENTITY.to_cols_array_2d(),
            [0.; 4],
            std::slice::from_ref(&quad),
        )
        .unwrap();
        let size = wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("static decal raster target"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("static decal raster pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.2,
                            g: 0.4,
                            b: 0.6,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            draw.draw_quads(&mut pass);
        }
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("static decal raster readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            size,
        );
        queue.submit(Some(encoder.finish()));
        let slice = readback.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
        device.poll(wgpu::Maintain::Wait);
        receiver.recv().unwrap().unwrap();
        let bytes = slice.get_mapped_range();
        let source: [f32; 4] = [
            128. / 255. * 0.5,
            64. / 255.,
            32. / 255. * 0.25,
            128. / 255. * 0.5,
        ];
        let destination: [f32; 4] = [0.2, 0.4, 0.6, 1.];
        let alpha = source[3];
        for (actual, (source, destination)) in
            bytes[..4].iter().zip(source.into_iter().zip(destination))
        {
            let expected = ((source * alpha + destination) * 255.).clamp(0., 255.);
            assert!(
                ((*actual as f32) - expected).abs() <= 2.,
                "encoded static decal MODULATE/blend mismatch: {:?}, expected {expected}",
                &bytes[..4]
            );
        }
    }
}
