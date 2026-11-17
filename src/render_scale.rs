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

use std::sync::atomic::{AtomicU32, Ordering};

/// Scale a window extent by a percentage and clamp to device limits.
///
/// `percent` is the persisted 10–200 value (100 = native). Out-of-range inputs
/// are clamped to 10–200 before use, and each dimension to
/// `[1, max_dimension]` — a 200% scale on a large window can otherwise exceed
/// `max_texture_dimension_2d`.
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

/// LOD (mip) bias for the world pass, stored as `f32` bits (`0` = no bias).
///
/// The world atlases are mipmapped, so an upscaled frame would otherwise pick
/// the mips that belong to the *render* resolution and lose the detail the
/// display resolution implies. The figure is the programming guide's
/// `log2(render/display) - 1` while a temporal upscaler runs, and the plain
/// `log2(render/display)` for the resolution slider on its own.
static MIP_BIAS: AtomicU32 = AtomicU32::new(0);

/// Record the bias for the current render/display extent pair.
///
/// Supersampled render sizes use no bias at all, and neither does the slider on
/// its own at 100 %. A temporal upscaler keeps the guide's `- 1` even at a 1.0x
/// ratio, because that is what DLAA is. The result is clamped to `-3..=0` so a
/// degenerate extent can never push a world atlas past its last mip.
pub fn set_mip_bias(render: (u32, u32), display: (u32, u32), temporal: bool) {
    let bias = if render.0 == 0 || render.1 == 0 || display.0 == 0 || display.1 == 0 {
        0.
    } else if render.0 <= display.0 && render.1 <= display.1 {
        let ratio = (render.0 as f32 / display.0 as f32).log2();
        ratio - if temporal { 1. } else { 0. }
    } else {
        0.
    };
    MIP_BIAS.store(bias.clamp(-3., 0.).to_bits(), Ordering::Relaxed);
}

/// Current LOD bias in mip levels; negative biases towards the sharper mips.
pub fn mip_bias() -> f32 {
    f32::from_bits(MIP_BIAS.load(Ordering::Relaxed))
}

/// `exp2` of the bias: the factor that folds a mip bias into a texture gradient.
pub fn mip_gradient_scale() -> f32 {
    mip_bias().exp2()
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
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
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
    /// Lazily built FSR 1 spatial upscaler; only used while
    /// `upscaling == Fsr1` and both axes are inside EASU's supported range.
    fsr1: Option<crate::fsr1_gpu::Fsr1Gpu>,
    input_extent: (u32, u32),
    output_extent: (u32, u32),
}

impl RenderScale {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        w: u32,
        h: u32,
        output_w: u32,
        output_h: u32,
    ) -> Self {
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
            fsr1: None,
            input_extent: (w, h),
            output_extent: (output_w, output_h),
        }
    }

    /// Reallocate the offscreen target when the window or the scale changes.
    /// The pipeline, layout and sampler are size-independent and are reused.
    pub fn resize(&mut self, device: &wgpu::Device, w: u32, h: u32, output_w: u32, output_h: u32) {
        if self.input_extent != (w, h) {
            self.frame = target_texture(device, self.format, w, h);
            self.input_extent = (w, h);
        }
        if self.output_extent != (output_w, output_h) {
            self.output_extent = (output_w, output_h);
            if let Some(fsr1) = &mut self.fsr1 {
                fsr1.resize(device, self.output_extent);
            }
        }
    }

    /// The raw texture; used by the FSR path to copy the world frame into
    /// its native target.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.frame
    }

    /// Extent the world is currently rendered at. The frame loop compares this
    /// with the extent it wants (see the `upscaler_running` check in
    /// `src/gpu.rs`) so a provider that failed to initialise cannot leave the
    /// world at a reduced resolution.
    pub fn input_extent(&self) -> (u32, u32) {
        self.input_extent
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

    /// Present the scaled world into the full-resolution composition view.
    /// Runs after `Renderer::render` and before the UI pass, which loads this
    /// result with `LoadOp::Load`. FSR 1 replaces the bilinear blit while it
    /// is selected and both axes stay inside EASU's supported range; anything
    /// else (including a device-limit clamp) falls back to bilinear.
    pub fn blit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        upscaling: crate::UpscaleMethod,
        sharpness_stops: f32,
    ) {
        let source = self.view();
        if upscaling == crate::UpscaleMethod::Fsr1
            && crate::fsr1_eligible(self.input_extent, self.output_extent)
        {
            let size = self.output_extent;
            let format = self.format;
            let fsr1 = self
                .fsr1
                .get_or_insert_with(|| crate::fsr1_gpu::Fsr1Gpu::new(device, format, size));
            fsr1.present(
                device,
                queue,
                &source,
                target,
                self.input_extent,
                sharpness_stops,
            );
            return;
        }
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
    use super::{mip_bias, mip_gradient_scale, scaled_extent, set_mip_bias};

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

    /// One test, not two: the bias lives in a process-wide atomic, and separate
    /// tests would race each other's stores.
    #[test]
    fn mip_bias_follows_the_guide_formula_and_stays_bounded() {
        // 3440x1440 display from a 2293x960 DLSS render extent.
        set_mip_bias((2293, 960), (3440, 1440), true);
        assert!((mip_bias() + 1.585).abs() < 0.01, "{}", mip_bias());
        assert!((mip_gradient_scale() - 1. / 3.).abs() < 0.01);
        // The resolution slider on its own only carries the ratio.
        set_mip_bias((1720, 720), (3440, 1440), false);
        assert!((mip_bias() + 1.).abs() < 1e-6, "{}", mip_bias());
        // Native AA is the same feature at a 1.0x ratio (DLAA), so the guide's
        // -1 still applies; the slider alone at 100 % takes none.
        set_mip_bias((3440, 1440), (3440, 1440), true);
        assert_eq!(mip_bias(), -1.);
        set_mip_bias((3440, 1440), (3440, 1440), false);
        assert_eq!(mip_bias(), 0.);
        // Supersampling is never biased, with or without an upscaler.
        set_mip_bias((6880, 2880), (3440, 1440), false);
        assert_eq!(mip_bias(), 0.);
        set_mip_bias((6880, 2880), (3440, 1440), true);
        assert_eq!(mip_bias(), 0.);
        assert_eq!(mip_gradient_scale(), 1.);
        // 1x1 from a full-size display would be log2(1/8192) - 1 = -14.
        set_mip_bias((1, 1), (8192, 8192), true);
        assert_eq!(mip_bias(), -3.);
        set_mip_bias((0, 0), (0, 0), true);
        assert_eq!(mip_bias(), 0.);
    }

    #[test]
    fn dimensions_are_clamped_to_device_limit_and_at_least_one() {
        assert_eq!(scaled_extent(4000, 3000, 200., 4096), (4096, 4096));
        assert_eq!(scaled_extent(4, 6, 10., 8192), (1, 1));
        assert_eq!(scaled_extent(0, 0, 100., 8192), (1, 1));
        assert_eq!(scaled_extent(1000, 1000, 100., 0), (1, 1));
    }
}
