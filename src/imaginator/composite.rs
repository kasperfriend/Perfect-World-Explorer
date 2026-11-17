//! Mixing the traced picture into the rendered frame.
//!
//! ## The passes
//!
//! 1. **Trace** (`gpu::TRACE`) writes the tracer's channels into a
//!    `rgba32float` texture, sized by `settings.trace_scale`.
//! 2. **Composite** (`gpu::COMPOSITE`) reads that, reads the finished frame
//!    through its *own sRGB view* — so the values are linear, decoded exactly
//!    once by the view — and writes the blended linear radiance into an
//!    `rgba16float` intermediate.
//! 3. **Blit** (`gpu::BLIT`) draws that intermediate onto the frame's sRGB view,
//!    which is the single encode the rest of the viewer also relies on. No
//!    transfer function is written by hand anywhere in the chain, because a
//!    hand-written one would run *in addition* to the target's.
//!
//! ## Where it runs
//!
//! After the upscaler has produced its scene view and before the present
//! encode, so the traced terms land in the image the viewer is about to show
//! and nothing later overwrites them.
//!
//! ## What it does to the frame
//!
//! `COMPOSITE` multiplies the raster frame by the traced sun visibility — the
//! core has no shadow map, so every surface already carries full sun and a
//! shadow has to *remove* light — and adds the traced indirect and reflected
//! radiance. Pixels whose primary ray escaped (negative distance) are copied
//! through untouched: the raster sky is the reference.
//!
//! ## What it deliberately does not do
//!
//! It does not touch the depth buffer, does not disturb the water planes or the
//! water reflection pass, and does not run at all when the module is off. The
//! water path in `gpu.rs` keeps working exactly as before;
//! `water_reflection_fill` only means water pixels get the same traced
//! reflection term every other surface gets.

use super::bvh::SharedScene;
use super::gpu::{self, GpuMaterial, GpuNode, GpuParams, GpuTriangle};
use super::settings::{ImaginatorSettings, Output};
use std::sync::Arc;

/// Linear-radiance format of the intermediate the composite writes and the blit
/// reads. Half float matches the frame's head-room and is storage-writable
/// everywhere `rgba32float` is.
const INTERMEDIATE: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Format of the tracer's channel image.
const TRACED: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

struct Pipelines {
    trace_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    blit_layout: wgpu::BindGroupLayout,
    trace: wgpu::ComputePipeline,
    composite: wgpu::ComputePipeline,
    blit: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    /// `None` when the frame format cannot be a storage texture (a BGRA
    /// swapchain, say). The module then reports why instead of failing per frame.
    unsupported: Option<String>,
}

struct SceneBuffers {
    triangles: wgpu::Buffer,
    nodes: wgpu::Buffer,
    order: wgpu::Buffer,
    materials: wgpu::Buffer,
    triangle_count: u32,
    source: Option<SharedScene>,
}

struct Target {
    traced_texture: wgpu::Texture,
    traced_view: wgpu::TextureView,
    intermediate_texture: wgpu::Texture,
    intermediate_view: wgpu::TextureView,
    /// A frame-resolution copy of what the viewer drew, in the frame's own
    /// format. The swapchain image itself cannot be bound as a texture — DX12
    /// reports no `TEXTURE_BINDING` for a surface — so the composite pass loads
    /// it into this attachment and the blit writes the result back into it.
    scene_texture: wgpu::Texture,
    scene_view: wgpu::TextureView,
    width: u32,
    height: u32,
}

pub struct Composite {
    pipelines: Pipelines,
    scene: Option<SceneBuffers>,
    target: Option<Target>,
    params: wgpu::Buffer,
    /// Traced channels of the last `read_channels`, for the module page.
    pub last_channels: Option<(Vec<f32>, (u32, u32))>,
    /// Cleared when the traced image changes, so the page re-reads it once.
    preview_fresh: bool,
    /// Row pitch of the traced texture, for CPU uploads (`rgba32float`).
    traced_row_pitch: u32,
    pub frames: u64,
    pub last_error: Option<String>,
    frame_format: wgpu::TextureFormat,
    /// The frame size the last `render` composited at.
    ///
    /// A software result can arrive before the first `render`, and allocating the
    /// target then needs the frame's size, not the trace's.
    frame_extent: (u32, u32),
}

impl Composite {
    pub fn new(device: &wgpu::Device, frame_format: wgpu::TextureFormat) -> Self {
        // `COMPOSITE` reads the frame through its own view, so the frame's format
        // has to be one a view decodes to linear: an sRGB target. Every path in
        // the viewer composites into one (`color_chain::RENDER_TARGET_IS_SRGB`),
        // so anything else is a configuration the module reports instead of
        // quietly adding encoded values to linear radiance.
        let unsupported = if frame_format.is_srgb() {
            None
        } else {
            Some(format!(
                "the frame is {frame_format:?}, which is not an sRGB target: the traced \
                 terms cannot be mixed into linear radiance"
            ))
        };

        let trace_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("imaginator trace bindings"),
            entries: &[
                uniform_entry(0),
                read_entry(1),
                read_entry(2),
                read_entry(3),
                read_entry(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: TRACED,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("imaginator composite bindings"),
            entries: &[
                uniform_entry(0),
                traced_texture_entry(1),
                // No sampler: the traced image is unfilterable, so `COMPOSITE`
                // reconstructs the bilinear sample itself.
                filterable_texture_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: INTERMEDIATE,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("imaginator blit bindings"),
            entries: &[filterable_texture_entry(0), sampler_entry(1)],
        });

        let trace = compute_pipeline(device, "imaginator trace", &trace_layout, gpu::TRACE);
        let composite = compute_pipeline(
            device,
            "imaginator composite",
            &composite_layout,
            gpu::COMPOSITE,
        );
        let blit = {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("imaginator blit"),
                source: wgpu::ShaderSource::Wgsl(gpu::BLIT.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("imaginator blit"),
                bind_group_layouts: &[&blit_layout],
                push_constant_ranges: &[],
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("imaginator blit"),
                layout: Some(&layout),
                // wgpu 0.19 has no `compilation_options` on either state; that
                // field arrived in 22. Every other pipeline in this crate (see
                // `src/bin/display-gamma-gpu-check.rs`) fills these two structs
                // without it.
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: "vs",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: "fs",
                    targets: &[Some(wgpu::ColorTargetState {
                        // The frame's own sRGB format: this attachment is where
                        // the linear intermediate is encoded, once.
                        format: frame_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            })
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("imaginator sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("imaginator params"),
            size: std::mem::size_of::<GpuParams>().max(std::mem::size_of::<CompositeParams>())
                as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipelines: Pipelines {
                trace_layout,
                composite_layout,
                blit_layout,
                trace,
                composite,
                blit,
                sampler,
                unsupported,
            },
            scene: None,
            target: None,
            params,
            last_channels: None,
            preview_fresh: false,
            traced_row_pitch: 0,
            frames: 0,
            last_error: None,
            frame_format,
            // Until a frame says otherwise, the trace's own size is the best
            // estimate of the frame; `render` corrects it on the first frame.
            frame_extent: (1, 1),
        }
    }

    pub fn frame_format(&self) -> wgpu::TextureFormat {
        self.frame_format
    }

    /// Why this frame format cannot be composited, when it cannot.
    pub fn unsupported_reason(&self) -> Option<&str> {
        self.pipelines.unsupported.as_deref()
    }

    /// Put a CPU trace's channels into the traced texture.
    ///
    /// The software tracer produces the same four channels the compute shader
    /// does, so it feeds the identical composite. Uploading is cheaper than
    /// dispatching a traversal the CPU has already done.
    pub fn upload_software(
        &mut self,
        packed: &[f32],
        extent: (u32, u32),
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        // A finished CPU trace can arrive before the first `render`, which is
        // where the target used to be allocated — and `upload_software` needs the
        // traced texture. Allocate it here when it is absent, sized the way
        // `render` will size it: traced at `extent`, composited at the frame's own
        // extent.
        if self.target.is_none() {
            let frame = self.frame_extent;
            self.ensure_target(device, extent.0, extent.1, frame.0, frame.1);
        }
        let Some(target) = &self.target else { return };
        // A result whose extent no longer matches the target is stale (the window
        // was resized between the request and the answer). Dropping it is right:
        // the next frame asks for the new size.
        if (target.width, target.height) != extent {
            return;
        }
        if packed.len() < (extent.0 as usize * extent.1 as usize * 4) {
            return;
        }
        // The traced texture is `Rgba32Float`, so a row is `width * 4` floats *
        // 4 bytes. Deriving it here rather than reading `traced_row_pitch` means
        // an upload that lands before the first `render` cannot use a stale
        // stride.
        let row_pitch = extent.0 * 16;
        let bytes = bytemuck::cast_slice(packed);
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &target.traced_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(row_pitch),
                rows_per_image: Some(extent.1),
            },
            wgpu::Extent3d {
                width: extent.0,
                height: extent.1,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Read the traced image back only when the page is open — the preview is the
    /// only consumer, and a readback stalls the queue.
    pub fn refresh_preview(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self
            .last_channels
            .as_ref()
            .is_some_and(|_| self.preview_fresh)
        {
            return;
        }
        if let Some(channels) = self.read_channels(device, queue) {
            self.last_channels = Some(channels);
            self.preview_fresh = true;
        }
    }

    /// Upload the captured scene unless the resident buffers already came from
    /// it. Returns the triangle count when they changed.
    pub fn sync_scene(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &SharedScene,
    ) -> Option<usize> {
        if let Some(buffers) = &self.scene {
            if buffers
                .source
                .as_ref()
                .is_some_and(|s| Arc::ptr_eq(s, scene))
            {
                return None;
            }
        }
        let Some(bvh) = scene.bvh.as_ref() else {
            self.scene = None;
            return None;
        };
        if bvh.triangles.is_empty() {
            self.scene = None;
            return Some(0);
        }
        let (triangles, nodes, order, materials) = gpu::pack_scene(bvh, &scene.materials);
        self.scene = Some(SceneBuffers {
            triangles: upload(device, queue, "imaginator triangles", &triangles),
            nodes: upload(device, queue, "imaginator nodes", &nodes),
            order: upload(device, queue, "imaginator order", &order),
            materials: upload(device, queue, "imaginator materials", &materials),
            triangle_count: triangles.len() as u32,
            source: Some(scene.clone()),
        });
        Some(bvh.triangles.len())
    }

    pub fn triangle_count(&self) -> u32 {
        self.scene.as_ref().map(|s| s.triangle_count).unwrap_or(0)
    }

    /// Trace and composite into `frame`, on `encoder`.
    ///
    /// `frame` is the finished frame's own (sRGB) view. One view serves both
    /// ends of the pass: `textureLoad` decodes it to linear on the way in, and
    /// writing it as an attachment encodes on the way out. No second view and no
    /// hand-written transfer function are needed.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        // The swapchain image, when `frame_view` is that image's own view.
        // `None` when `frame_view` already belongs to a texture the module may
        // sample (the gamma / present-encode intermediates).
        frame_texture: Option<&wgpu::Texture>,
        frame_view: &wgpu::TextureView,
        extent: (u32, u32),
        settings: &ImaginatorSettings,
        params: GpuParams,
        source: TraceSource,
    ) -> Result<(), String> {
        if let Some(why) = &self.pipelines.unsupported {
            return Err(why.clone());
        }
        if self.scene.is_none() {
            return Err("no scene has been captured yet".into());
        }
        let (width, height) = extent;
        if width == 0 || height == 0 {
            return Err("the frame has no extent".into());
        }
        let (tw, th) = gpu::traced_extent(width, height, settings.trace_scale);
        self.frame_extent = (width, height);
        self.ensure_target(device, tw, th, width, height);
        let Some(buffers) = self.scene.as_ref() else {
            return Err("no scene has been captured yet".into());
        };
        let target = self.target.as_ref().ok_or("the traced target is gone")?;

        // The swapchain image cannot be sampled: DX12 reports only
        // `COLOR_TARGET | COPY_SRC | COPY_DST` for a surface, and asking for
        // `TEXTURE_BINDING` makes `Surface::configure` fail outright. `COPY_SRC`
        // is enough, so the frame is copied into a scratch texture that *can* be
        // sampled, and the finished blit is written back to the frame itself.
        if let Some(frame_texture) = frame_texture {
            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: frame_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: &target.scene_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
        // A surface image needed the copy above; an intermediate is sampled where
        // it lies.
        let scene_input = if frame_texture.is_some() {
            &target.scene_view
        } else {
            frame_view
        };

        // A CPU trace has already filled the traced texture, so the dispatch is
        // skipped and the composite reads what was uploaded.
        if source == TraceSource::Gpu {
            let mut trace_params = params;
            trace_params.config[1] = self.frames as u32;
            queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&trace_params));
            let trace_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("imaginator trace"),
                layout: &self.pipelines.trace_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buffers.triangles.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: buffers.nodes.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: buffers.order.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: buffers.materials.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&target.traced_view),
                    },
                ],
            });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("imaginator trace"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.pipelines.trace);
                pass.set_bind_group(0, &trace_bind, &[]);
                pass.dispatch_workgroups(div_ceil(tw, 8), div_ceil(th, 8), 1);
            }
            self.preview_fresh = false;
        }

        // The composite reads the trace image the pass above just wrote; a
        // compute-to-compute boundary inside one encoder needs no explicit
        // barrier in `wgpu`, but the uniform buffer is written between the two
        // passes, and `write_buffer` invalidates every prior read of it.
        let composite_params = CompositeParams::new(settings);
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&composite_params));
        let composite_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("imaginator composite"),
            layout: &self.pipelines.composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&target.traced_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(scene_input),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&target.intermediate_view),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("imaginator composite"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines.composite);
            pass.set_bind_group(0, &composite_bind, &[]);
            pass.dispatch_workgroups(div_ceil(width, 8), div_ceil(height, 8), 1);
        }

        let blit_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("imaginator blit"),
            layout: &self.pipelines.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&target.intermediate_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.pipelines.sampler),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("imaginator blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: frame_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // The blit covers every pixel, including the sky the
                        // composite copied through, so nothing needs preserving.
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipelines.blit);
            pass.set_bind_group(0, &blit_bind, &[]);
            pass.draw(0..3, 0..1);
        }

        self.frames = self.frames.wrapping_add(1);
        self.last_error = None;
        Ok(())
    }

    /// Copy the tracer's channels back to the CPU and return them.
    ///
    /// This blocks until the GPU is idle, so it is called only when the module
    /// page needs a preview — never per frame. The traced image is at
    /// `trace_scale`, so this is a few hundred kilobytes in the normal case, not
    /// a frame-sized read.
    pub fn read_channels(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Option<(Vec<f32>, (u32, u32))> {
        let target = self.target.as_ref()?;
        let (width, height) = (target.width, target.height);
        let padded = padded_bytes_per_row(width, 4 * 4);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("imaginator readback"),
            size: (padded as u64 * height as u64).max(4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &target.traced_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for row in 0..height {
            let start = (row * padded) as usize;
            let end = start + (width * 16) as usize;
            for chunk in data[start..end].chunks_exact(4) {
                out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
        drop(data);
        buffer.unmap();
        let extent = (width, height);
        self.last_channels = Some((out.clone(), extent));
        Some((out, extent))
    }

    /// Allocate (or reallocate) the traced and composite targets.
    ///
    /// Public because a software *result* can arrive before the first
    /// `render`: `upload_software` writes into the traced texture, so the target
    /// has to exist by then or the first frames of a CPU trace are silently
    /// dropped and the window stays blank until the sizes happen to match.
    pub fn ensure_target(
        &mut self,
        device: &wgpu::Device,
        traced_width: u32,
        traced_height: u32,
        width: u32,
        height: u32,
    ) {
        if let Some(target) = &self.target {
            if target.width == traced_width
                && target.height == traced_height
                && target.intermediate_texture.width() == width
                && target.intermediate_texture.height() == height
                && target.scene_texture.width() == width
                && target.scene_texture.height() == height
            {
                return;
            }
        }
        let traced_texture = texture(
            device,
            "imaginator traced",
            traced_width,
            traced_height,
            TRACED,
            wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let traced_view = traced_texture.create_view(&Default::default());
        let intermediate_texture = texture(
            device,
            "imaginator composite",
            width,
            height,
            INTERMEDIATE,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let intermediate_view = intermediate_texture.create_view(&Default::default());
        let scene_texture = texture(
            device,
            "imaginator scene",
            width,
            height,
            self.frame_format,
            // Filled by `copy_texture_to_texture` from the swapchain image and
            // sampled by the composite pass. `RENDER_ATTACHMENT` is not needed:
            // the blit writes the swapchain image itself, and the copy is what
            // fills this one.
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        // The frame's own sRGB view is what the composite used to sample, and
        // that decode is what the blend assumes. Reproduce it here: an sRGB
        // frame gets an sRGB view of the copy, so `textureLoad` still returns
        // linear radiance.
        let scene_view = scene_texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.frame_format),
            ..Default::default()
        });
        self.traced_row_pitch = traced_width * 16;
        self.preview_fresh = false;
        self.target = Some(Target {
            traced_texture,
            traced_view,
            intermediate_texture,
            intermediate_view,
            scene_texture,
            scene_view,
            width: traced_width,
            height: traced_height,
        });
    }
}

/// `COMPOSITE`'s uniform: strength and exposure, plus the mode flag.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CompositeParams {
    strength: [f32; 4],
    flags: [u32; 4],
}

impl CompositeParams {
    pub fn new(settings: &ImaginatorSettings) -> Self {
        let mode = match settings.output {
            Output::Lighting => 0,
            Output::TraceOnly => 1,
        };
        Self {
            strength: [settings.strength, settings.exposure, 0., 0.],
            flags: [mode, 0, 0, 0],
        }
    }
}

// SAFETY: `CompositeParams` is `#[repr(C)]` over `f32`/`u32` only, so every bit
// pattern is valid and the layout matches the WGSL `Params` struct.
unsafe impl bytemuck::Pod for CompositeParams {}
unsafe impl bytemuck::Zeroable for CompositeParams {}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn read_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// The tracer's `rgba32float` image.
///
/// `rgba32float` is only *filterable* when the optional `FLOAT32_FILTERABLE`
/// feature is enabled, which this device does not request. Declaring it
/// filterable here made `create_bind_group` fail validation on the very first
/// frame ("binding 1 expects Float { filterable: true }, but given a view with
/// format = Rgba32Float"), which surfaced as a white window. The composite
/// shader therefore loads four texels and blends them itself.
fn traced_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    texture_entry(binding, false)
}

/// A texture the shaders may sample through a filtering sampler.
fn filterable_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    texture_entry(binding, true)
}

fn texture_entry(binding: u32, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    source: &str,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: "cs",
    })
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn upload<T: bytemuck::Pod>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    data: &[T],
) -> wgpu::Buffer {
    let bytes = bytemuck::cast_slice(data);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (bytes.len() as u64).max(4),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    if !bytes.is_empty() {
        queue.write_buffer(&buffer, 0, bytes);
    }
    buffer
}

fn div_ceil(value: u32, divisor: u32) -> u32 {
    value.div_ceil(divisor).max(1)
}

/// `COPY_BYTES_PER_ROW_ALIGNMENT` is 256; every backend `wgpu` runs accepts that.
fn padded_bytes_per_row(width: u32, bytes_per_pixel: u32) -> u32 {
    let unpadded = width * bytes_per_pixel;
    unpadded.div_ceil(256) * 256
}

/// Where this frame's traced channels come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceSource {
    /// The compute shader walks the BVH on the GPU this frame.
    Gpu,
    /// A CPU trace already filled the traced texture; only composite and blit run.
    Software,
}

/// The composite's arithmetic in Rust, exactly as `gpu::COMPOSITE` performs it.
///
/// Kept here so the blend can be tested without a device: the interesting part
/// of a compositor is the formula, not the dispatch. Values are linear — the
/// caller is expected to have decoded them, which on the GPU is the frame's own
/// sRGB view doing it.
pub fn composite_pixel(
    frame_linear: [f32; 3],
    traced: [f32; 4],
    strength: f32,
    exposure: f32,
    trace_only: bool,
) -> [f32; 3] {
    if trace_only {
        let lit = (traced[1] * 2.0 + traced[0]) * exposure;
        let shadowed = lerp(0.35, 1.0, traced[2]);
        return [lit * shadowed; 3];
    }
    if traced[3] < 0.0 {
        // Sky: the tracer's primary ray escaped, so the raster sky stands.
        return frame_linear;
    }
    let illumination = lerp(
        1.0,
        lerp(0.30, 1.0, traced[2].clamp(0., 1.)),
        strength.clamp(0., 1.),
    );
    let added = (traced[1] + traced[0]) * strength;
    [
        frame_linear[0] * illumination + added,
        frame_linear[1] * illumination + added,
        frame_linear[2] * illumination + added,
    ]
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The packed layouts this file uploads. A type change in `gpu.rs` has to come
/// with a matching change here, and this makes forgetting it a compile error.
const _: () = {
    assert!(std::mem::size_of::<GpuTriangle>() == 4 * 4 * 4);
    assert!(std::mem::size_of::<GpuNode>() == 12 * 4);
    assert!(std::mem::size_of::<GpuMaterial>() == 4 * 4);
};

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ImaginatorSettings {
        ImaginatorSettings::default()
    }

    #[test]
    fn a_shadowed_surface_keeps_some_light_and_loses_the_rest() {
        let s = settings();
        let base = [0.5, 0.5, 0.5];
        let shadowed = composite_pixel(base, [0., 0., 0., 10.], s.strength, 1., false);
        let lit = composite_pixel(base, [0., 0., 1., 10.], s.strength, 1., false);
        assert!(
            shadowed[0] < lit[0],
            "a fully shadowed pixel must be darker than a lit one"
        );
        assert!(
            shadowed[0] > 0.1,
            "a shadow must not be black: {}",
            shadowed[0]
        );
    }

    #[test]
    fn zero_strength_is_a_no_op() {
        let frame = [0.5, 0.25, 0.75];
        let out = composite_pixel(frame, [1., 2., 0., 4.], 0.0, 1., false);
        for (a, b) in out.iter().zip(frame) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn the_sky_is_never_touched() {
        let frame = [0.142, 0.318, 0.716];
        for strength in [0.25, 1.0] {
            let out = composite_pixel(frame, [5., 5., 0., -1.], strength, 1., false);
            assert_eq!(out, frame);
        }
    }

    #[test]
    fn indirect_and_reflections_both_add_light() {
        let s = settings();
        let base = [0.05, 0.05, 0.05];
        let lit = composite_pixel(base, [0., 0., 1., 5.], s.strength, 1., false);
        let reflected = composite_pixel(base, [1., 0., 1., 5.], s.strength, 1., false);
        let bounced = composite_pixel(base, [0., 1., 1., 5.], s.strength, 1., false);
        assert!(reflected[0] > lit[0]);
        assert!(bounced[0] > lit[0]);
        let both = composite_pixel(base, [1., 1., 1., 5.], s.strength, 1., false);
        assert!(both[0] > reflected[0] && both[0] > bounced[0]);
    }

    #[test]
    fn the_diagnostic_mode_ignores_the_raster_frame() {
        let bright_frame = composite_pixel([0.9, 0.9, 0.9], [0., 0., 0., 5.], 0.6, 1., true);
        let dark_frame = composite_pixel([0.0, 0.0, 0.0], [0., 0., 0., 5.], 0.6, 1., true);
        assert_eq!(bright_frame, dark_frame);
        let more_light = composite_pixel([0., 0., 0.], [2., 1., 1., 5.], 0.6, 1., true);
        assert!(more_light[0] > bright_frame[0]);
    }

    #[test]
    fn exposure_only_affects_the_diagnostic_mode() {
        let a = composite_pixel([0.3, 0.3, 0.3], [0.2, 0.4, 1., 5.], 0.6, 1., false);
        let b = composite_pixel([0.3, 0.3, 0.3], [0.2, 0.4, 1., 5.], 0.6, 4., false);
        assert_eq!(a, b, "the lighting mode is physical, not exposure-driven");
        let c = composite_pixel([0.3; 3], [0.2, 0.4, 1., 5.], 0.6, 1., true);
        let d = composite_pixel([0.3; 3], [0.2, 0.4, 1., 5.], 0.6, 4., true);
        assert!(d[0] > c[0]);
    }

    #[test]
    fn the_composite_is_monotonic_in_visibility() {
        let s = settings();
        let mut previous = -1.0;
        for vis in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let out = composite_pixel([0.4; 3], [0., 0., vis, 8.], s.strength, 1., false);
            assert!(out[0] >= previous, "more sun must not mean less light");
            previous = out[0];
        }
    }

    #[test]
    fn params_encode_the_mode_and_the_strength() {
        let mut s = settings();
        let p = CompositeParams::new(&s);
        assert_eq!(p.flags[0], 0);
        assert_eq!(p.strength[0], s.strength);
        s.output = Output::TraceOnly;
        assert_eq!(CompositeParams::new(&s).flags[0], 1);
    }

    #[test]
    fn dispatch_never_asks_for_zero_groups() {
        assert_eq!(div_ceil(1, 8), 1);
        assert_eq!(div_ceil(8, 8), 1);
        assert_eq!(div_ceil(9, 8), 2);
        assert_eq!(div_ceil(0, 8), 1);
    }

    /// The composite reads the frame through its sRGB view and writes the
    /// intermediate onto that same view: one decode in, one encode out. A
    /// non-sRGB frame would make the read un-decoded, so it is refused by name
    /// rather than silently adding encoded values to linear radiance.
    #[test]
    fn a_non_srgb_frame_is_refused_with_the_reason() {
        let linear = wgpu::TextureFormat::Rgba8Unorm;
        assert!(!linear.is_srgb());
        assert!(wgpu::TextureFormat::Rgba8UnormSrgb.is_srgb());
        assert!(wgpu::TextureFormat::Bgra8UnormSrgb.is_srgb());
    }

    #[test]
    fn readback_rows_are_aligned_to_256_bytes() {
        assert_eq!(padded_bytes_per_row(1, 16), 256);
        assert_eq!(padded_bytes_per_row(16, 16), 256);
        assert_eq!(padded_bytes_per_row(17, 16), 512);
        assert_eq!(padded_bytes_per_row(1920, 16), 30720);
    }
}
