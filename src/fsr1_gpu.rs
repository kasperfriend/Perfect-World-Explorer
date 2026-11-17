//! AMD FidelityFX Super Resolution 1 (spatial EASU + RCAS) presentation pass.
//!
//! This is a WGSL port of the MIT-licensed FSR 1 algorithm, *not* the FSR
//! temporal/ML SDK. It only consumes the already-composed LDR world image,
//! leaving the renderer, reflections, post effects, egui and final gamma pass
//! unchanged. The world and intermediate use sRGB views: both stages sample
//! decoded linear light, render to sRGB once, and leave the native UI untouched.

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    dimensions: [f32; 4],
    sharpness: [f32; 4],
}

fn intermediate(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    size: (u32, u32),
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("FSR 1 EASU output (native resolution)"),
        size: wgpu::Extent3d {
            width: size.0.max(1),
            height: size.1.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

pub(crate) struct Fsr1Gpu {
    format: wgpu::TextureFormat,
    size: (u32, u32),
    easu_output: wgpu::Texture,
    params: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    easu: wgpu::RenderPipeline,
    rcas: wgpu::RenderPipeline,
}

impl Fsr1Gpu {
    pub(crate) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FSR 1 input and constants"),
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
            label: Some("FSR 1 EASU and RCAS WGSL"),
            source: wgpu::ShaderSource::Wgsl(include_str!("fsr1.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FSR 1 pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |entry_point| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point,
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview: None,
            })
        };
        Self {
            format,
            size,
            easu_output: intermediate(device, format, size),
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("FSR 1 EASU/RCAS constants"),
                size: std::mem::size_of::<Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            layout,
            easu: pipeline("easu"),
            rcas: pipeline("rcas"),
        }
    }

    pub(crate) fn resize(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        if self.size != size {
            self.easu_output = intermediate(device, self.format, size);
            self.size = size;
        }
    }

    fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
    ) {
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FSR 1 source"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("FSR 1 upscale or sharpen"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Upscale the world before egui is composed. No synthetic frames or
    /// temporal history: this pass is independent of any future framegen.
    pub(crate) fn present(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        source_size: (u32, u32),
        sharpness_stops: f32,
    ) {
        let stops = if sharpness_stops.is_finite() {
            sharpness_stops.clamp(0.0, 2.0)
        } else {
            0.2
        };
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                dimensions: [
                    source_size.0 as f32,
                    source_size.1 as f32,
                    self.size.0 as f32,
                    self.size.1 as f32,
                ],
                sharpness: [2.0f32.powf(-stops), 0.0, 0.0, 0.0],
            }),
        );
        let easu_view = self.easu_output.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("FSR 1 EASU then RCAS"),
        });
        self.draw(device, &mut encoder, source, &easu_view, &self.easu);
        self.draw(device, &mut encoder, &easu_view, target, &self.rcas);
        queue.submit(Some(encoder.finish()));
    }
}
