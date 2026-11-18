//! Final output encode for swapchains that offer no sRGB format.
//!
//! Every material shader in this viewer returns *linear* color and relies on an
//! sRGB render target to perform exactly one encode on write. When the surface
//! reports an sRGB format (the normal case) the frame is rendered directly into
//! the swapchain and nothing here is used.
//!
//! If the surface only accepts UNORM formats the viewer composites the visible
//! frame – world, post chain, map and UI – into an sRGB intermediate and this
//! pass copies those already-encoded bytes to the swapchain. The copy reads a
//! raw (UNORM) view of the intermediate, so no second transfer function is
//! applied: the output bytes are exactly the encoded client framebuffer values.
//!
//! This is a viewer presentation route. It is not evidence about the retail
//! hardware gamma ramp, and the synthetic readback tests that exercise it are
//! not physical-device presentation proof.

/// Compositing format used for the intermediate frame.
///
/// Both are core wgpu sRGB formats that are valid render targets. The
/// intermediate never has to match the swapchain, so byte order does not
/// matter; RGBA is chosen so the raw view is available everywhere.
pub const INTERMEDIATE: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The format the renderer must composite into for a given swapchain format.
///
/// sRGB surfaces are used directly; otherwise the viewer composites into
/// [`INTERMEDIATE`] and encodes once at presentation.
pub fn render_format(surface: wgpu::TextureFormat) -> wgpu::TextureFormat {
    if surface.is_srgb() {
        surface
    } else {
        INTERMEDIATE
    }
}

const SHADER: &str = r#"
@group(0) @binding(0) var frame:texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) id:u32)->@builtin(position) vec4<f32> {
 let x=f32((id<<1u)&2u);let y=f32(id&2u);
 return vec4<f32>(x*2.-1.,1.-y*2.,0.,1.);
}
// One-to-one copy of already encoded bytes. The source is bound through a
// UNORM view, so this pass neither decodes nor encodes.
@fragment fn fs(@builtin(position) pos:vec4<f32>)->@location(0) vec4<f32> {
 let p=vec2<i32>(pos.xy);
 return vec4<f32>(textureLoad(frame,p,0).rgb,1.);
}
"#;

pub struct PresentEncode {
    surface_format: wgpu::TextureFormat,
    frame: wgpu::Texture,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

impl PresentEncode {
    fn texture(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
        let alternate = [INTERMEDIATE.remove_srgb_suffix()];
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("presentation encode intermediate"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: INTERMEDIATE,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &alternate,
        })
    }

    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat, w: u32, h: u32) -> Self {
        let frame = Self::texture(device, w, h);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present encode layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("present encode"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("present encode pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("present encode pipeline"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
        });
        let bind_group = Self::create_bind_group(device, &layout, &frame);
        Self {
            surface_format,
            frame,
            pipeline,
            layout,
            bind_group,
            width: w.max(1),
            height: h.max(1),
        }
    }

    pub fn resize(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if (self.width, self.height) == (w.max(1), h.max(1)) {
            return;
        }
        self.width = w.max(1);
        self.height = h.max(1);
        self.frame = Self::texture(device, self.width, self.height);
        self.bind_group = Self::create_bind_group(device, &self.layout, &self.frame);
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// sRGB view for rendering, postprocessing and UI composition.
    pub fn view(&self) -> wgpu::TextureView {
        self.frame.create_view(&Default::default())
    }

    /// Raw view for passes that must blend encoded bytes (shoreline waves,
    /// scene copies) exactly like the sRGB-target path.
    pub fn raw_view(&self) -> wgpu::TextureView {
        self.frame.create_view(&wgpu::TextureViewDescriptor {
            format: Some(INTERMEDIATE.remove_srgb_suffix()),
            ..Default::default()
        })
    }

    fn create_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        frame: &wgpu::Texture,
    ) -> wgpu::BindGroup {
        let source = frame.create_view(&wgpu::TextureViewDescriptor {
            format: Some(INTERMEDIATE.remove_srgb_suffix()),
            ..Default::default()
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present_encode_bg"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source),
            }],
        })
    }

    /// Copy the encoded frame to the swapchain. Runs after the whole-frame
    /// display ramp (which writes into [`Self::view`]).
    pub fn present(&self, device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::TextureView) {
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present encoded frame"),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}
