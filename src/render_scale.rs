//! Internal render-resolution scaling (Graphics tab → "Resolution scale").
//!
//! The 3D world is drawn into an offscreen target whose size is the window
//! extent multiplied by the chosen percentage (10–200%), then bilinearly
//! blitted into the full-resolution composition view (`scene_view`). The egui
//! UI, the display-gamma ramp and the UNORM present-encode pass stay at native
//! resolution, so menus, minimap and text remain sharp while the expensive
//! terrain/water/scenery/post passes scale with the setting.
//!
//! The offscreen target uses the renderer's sRGB `format` plus a linear (raw)
//! alternate view, exactly like the swapchain/composition intermediates. The
//! upscale samples the sRGB view (hardware decodes once), filters in linear
//! space, and writes to the sRGB composition view (hardware encodes once), so
//! the blit is colour-neutral and never double-encodes.
//!
//! `scaled_extent` is deliberately separate from GPU objects so its bounds are
//! unit-testable without an adapter.

/// LOD bias for world-model atlas sampling. This branch has no temporal
/// upscaler path, so keep the legacy zero-bias sampling behavior.
pub fn mip_bias() -> f32 {
    0.0
}

/// Scale a window extent by a percentage and clamp to device limits.

/// `percent` is the persisted 10–200 value (100 = native). Out-of-range inputs
/// are clamped to 10–200 before use, and each dimension to
/// `[1, max_dimension]` — a 200% scale on a large window can otherwise exceed
/// `max_texture_dimension_2d`.
pub fn direct_render_compatible(
    scaled: (u32, u32),
    output: (u32, u32),
    render_format: wgpu::TextureFormat,
    target_format: wgpu::TextureFormat,
    composition_required: bool,
) -> bool {
    scaled == output && render_format == target_format && !composition_required
}

pub fn scaled_extent(w: u32, h: u32, percent: f32, max_dimension: u32) -> (u32, u32) {
    let raw = f64::from(percent) / 100.0;
    // `f64::clamp` propagates NaN, so a non-finite input falls back to native.
    let s = if raw.is_finite() {
        raw.clamp(0.1, 2.0)
    } else {
        1.0
    };
    let limit = u64::from(max_dimension.max(1));
    // Widen before rounding so a 200% scale of a large window cannot overflow
    // the intermediate float/int conversion; `as u64` saturates NaN to 0.
    let scale = |v: u32| ((f64::from(v.max(1)) * s).round() as u64).clamp(1, limit) as u32;
    (scale(w), scale(h))
}

const SHADER: &str = r#"
@group(0) @binding(0) var frame:texture_2d<f32>;
@group(0) @binding(1) var samp:sampler;
struct VOut { @builtin(position) pos:vec4<f32>, @location(0) uv:vec2<f32> };
@vertex fn vs(@builtin(vertex_index) id:u32)->VOut {
 var o:VOut;
 let x=f32((id<<1u)&2u);let y=f32(id&2u);
 o.pos=vec4<f32>(x*2.-1.,1.-y*2.,0.,1.);
 o.uv=vec2<f32>(x,y);
 return o;
}
@fragment fn fs(i:VOut)->@location(0) vec4<f32> {
 return vec4<f32>(textureSample(frame,samp,i.uv).rgb,1.);
}
"#;

fn target_texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    w: u32,
    h: u32,
) -> wgpu::Texture {
    let alternate = [format.remove_srgb_suffix()];
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scaled world render target"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: if format.is_srgb() { &alternate } else { &[] },
    })
}

/// Offscreen world target plus the fullscreen bilinear upscale pipeline.
pub struct RenderScale {
    format: wgpu::TextureFormat,
    frame: wgpu::Texture,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl RenderScale {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, w: u32, h: u32) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("resolution scale layout"),
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
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("resolution scale WGSL"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("resolution scale pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("resolution scale blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
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
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("resolution scale sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Self {
            format,
            frame: target_texture(device, format, w, h),
            sampler,
            pipeline,
            layout,
        }
    }

    /// Reallocate the offscreen target when the window or the scale changes.
    /// The pipeline, layout and sampler are size-independent and are reused.
    pub fn resize(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        self.frame = target_texture(device, self.format, w, h);
    }

    /// sRGB view the world is rendered into.
    pub fn view(&self) -> wgpu::TextureView {
        self.frame.create_view(&Default::default())
    }

    /// Raw (encoded-space) view for legacy alpha-blended scenery passes; must
    /// alias the same texture as [`Self::view`].
    pub fn raw_view(&self) -> wgpu::TextureView {
        self.frame.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.format.remove_srgb_suffix()),
            ..Default::default()
        })
    }

    /// Bilinearly present the scaled world into the full-resolution composition
    /// view. Runs after `Renderer::render` and before the UI pass, which loads
    /// this result with `LoadOp::Load`.
    pub fn blit(&self, device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::TextureView) {
        let source = self.view();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resolution scale source"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("resolution scale upscale"),
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
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::{direct_render_compatible, scaled_extent};

    #[test]
    fn direct_route_requires_native_extent_compatible_format_and_no_composition() {
        use wgpu::TextureFormat as F;
        assert!(direct_render_compatible(
            (1280, 720),
            (1280, 720),
            F::Bgra8UnormSrgb,
            F::Bgra8UnormSrgb,
            false
        ));
        assert!(!direct_render_compatible(
            (1280, 720),
            (1280, 720),
            F::Bgra8UnormSrgb,
            F::Rgba8UnormSrgb,
            false
        ));
        assert!(!direct_render_compatible(
            (640, 360),
            (1280, 720),
            F::Bgra8UnormSrgb,
            F::Bgra8UnormSrgb,
            false
        ));
        assert!(!direct_render_compatible(
            (1280, 720),
            (1280, 720),
            F::Bgra8UnormSrgb,
            F::Bgra8UnormSrgb,
            true
        ));
    }

    #[test]
    fn native_scale_is_identity() {
        assert_eq!(scaled_extent(1920, 1080, 100., 8192), (1920, 1080));
        assert_eq!(scaled_extent(1, 1, 100., 8192), (1, 1));
    }

    #[test]
    fn ten_to_two_hundred_percent_maps_the_range() {
        assert_eq!(scaled_extent(1920, 1080, 10., 8192), (192, 108));
        assert_eq!(scaled_extent(1920, 1080, 50., 8192), (960, 540));
        assert_eq!(scaled_extent(1920, 1080, 200., 8192), (3840, 2160));
    }

    #[test]
    fn out_of_range_percent_is_clamped_to_ten_to_two_hundred() {
        assert_eq!(scaled_extent(1000, 1000, 0., 8192), (100, 100));
        assert_eq!(scaled_extent(1000, 1000, -50., 8192), (100, 100));
        assert_eq!(scaled_extent(1000, 1000, 500., 8192), (2000, 2000));
        assert_eq!(scaled_extent(1000, 1000, f32::NAN, 8192), (1000, 1000));
        assert_eq!(scaled_extent(1000, 1000, f32::INFINITY, 8192), (1000, 1000));
    }

    #[test]
    fn dimensions_are_clamped_to_device_limit_and_at_least_one() {
        assert_eq!(scaled_extent(4000, 3000, 200., 4096), (4096, 4096));
        assert_eq!(scaled_extent(4, 6, 10., 8192), (1, 1));
        assert_eq!(scaled_extent(0, 0, 100., 8192), (1, 1));
        assert_eq!(scaled_extent(1000, 1000, 100., 0), (1, 1));
    }
}
