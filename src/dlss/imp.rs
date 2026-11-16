//! Windows + DX12 implementation of DLSS super resolution. See the module
//! documentation in `super` for the overall design and limits.
//!
//! The shape matches the XeSS and FSR paths exactly: the world is rendered into
//! the scaled offscreen target, [`DlssUpscaler::record`] copies that colour,
//! the depth buffer and the reconstructed motion vectors into native textures
//! and runs `NVSDK_NGX_D3D12_EvaluateFeature` on its own D3D12 command list,
//! and [`DlssUpscaler::composite`] blits the full-resolution result into the
//! composition. The NGX call is given an `NVSDK_NGX_Parameter` table the viewer
//! owns (`super::params`), because the NGX DLLs export the feature entry points
//! but no parameter allocator.

use super::loader::{NgxApi, NGX_FEATURE_SUPER_SAMPLING, NGX_VERSION_API};
use super::params::NgxParams;
use crate::fsr::{
    dxgi_output_format, raw_dx12, FsrComposite, FsrMotionVectorPass, FsrMvParams,
    NativeCommandRing, NativeTexture2D, D3D12_STATE_COMMON, D3D12_STATE_COPY_DEST,
    D3D12_STATE_PIXEL_COMPUTE_READ, D3D12_STATE_RENDER_TARGET, D3D12_STATE_UNORDERED_ACCESS,
};
use glam::Mat4;
use std::ffi::c_void;
use winapi::shared::dxgiformat::{DXGI_FORMAT_R16G16_FLOAT, DXGI_FORMAT_R32_FLOAT};
use winapi::um::d3d12 as d3d12_ty;

/// `D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE` — the state NGX wants its
/// inputs in (DLSS and DLSS-G both dispatch compute).
const D3D12_STATE_NON_PIXEL_READ: u32 = d3d12_ty::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;

// `NVSDK_NGX_PerfQuality_Value`, in declaration order.
const NGX_QUALITY_MAX_PERF: u32 = 0;
const NGX_QUALITY_BALANCED: u32 = 1;
const NGX_QUALITY_MAX_QUALITY: u32 = 2;
const NGX_QUALITY_ULTRA_PERFORMANCE: u32 = 3;
const NGX_QUALITY_ULTRA_QUALITY: u32 = 4;
const NGX_QUALITY_DLAA: u32 = 5;

/// `NVSDK_NGX_DLSS_Feature_Flags_MVLowRes`: motion vectors are at render
/// resolution rather than display resolution.
const NGX_DLSS_FEATURE_MV_LOW_RES: i32 = 1 << 1;
/// `NVSDK_NGX_DLSS_Feature_Flags_DepthInverted`: the viewer's depth is
/// reversed-Z (near→1, far→0), the same convention FSR and XeSS are told.
const NGX_DLSS_FEATURE_DEPTH_INVERTED: i32 = 1 << 3;

// Parameter names, spelled as the vendored `nvsdk_ngx_defs.h` defines them.
const P_WIDTH: &str = "Width";
const P_HEIGHT: &str = "Height";
const P_OUT_WIDTH: &str = "OutWidth";
const P_OUT_HEIGHT: &str = "OutHeight";
const P_PERF_QUALITY: &str = "PerfQualityValue";
const P_CREATE_FLAGS: &str = "DLSS.Feature.Create.Flags";
const P_COLOR: &str = "Color";
const P_DEPTH: &str = "Depth";
const P_MOTION_VECTORS: &str = "MotionVectors";
const P_OUTPUT: &str = "Output";
const P_JITTER_X: &str = "Jitter.Offset.X";
const P_JITTER_Y: &str = "Jitter.Offset.Y";
const P_RESET: &str = "Reset";
const P_MV_SCALE_X: &str = "MV.Scale.X";
const P_MV_SCALE_Y: &str = "MV.Scale.Y";
const P_FRAME_TIME_MS: &str = "FrameTimeDeltaInMsec";
const P_RENDER_WIDTH: &str = "DLSS.Render.Subrect.Dimensions.Width";
const P_RENDER_HEIGHT: &str = "DLSS.Render.Subrect.Dimensions.Height";
const P_SHARPNESS: &str = "Sharpness";

/// In-tree Halton jitter in pixels ([-0.5, 0.5)), 16 phases.
fn halton(mut index: u32, base: u32) -> f32 {
    let mut result = 0.0f32;
    let mut factor = 1.0f32 / base as f32;
    while index > 0 {
        result += factor * (index % base) as f32;
        index /= base;
        factor /= base as f32;
    }
    result
}

fn jitter_for_frame(frame: u64, phase_count: u64) -> (f32, f32) {
    let n = (frame % phase_count.max(1)) as u32 + 1;
    (halton(n, 2) - 0.5, halton(n, 3) - 0.5)
}

struct DlssNatives {
    color: NativeTexture2D,
    depth: NativeTexture2D,
    mv: NativeTexture2D,
    output: NativeTexture2D,
}

impl DlssNatives {
    /// # Safety
    /// `raw_device` must be the device `device` was created from.
    unsafe fn create(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        render_extent: (u32, u32),
        display_extent: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Result<Self, String> {
        // UNORM description, not `_UNORM_SRGB`: NGX builds its SRV from the
        // resource description, so an sRGB one would give DLSS linear pixels
        // while `IsHDR` is clear (i.e. "the input is sRGB-encoded LDR"), and it
        // would convert a second time. See `FsrNatives::create`.
        let color = NativeTexture2D::create(
            device,
            raw_device,
            "dlss color",
            format,
            dxgi_output_format(format),
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_NONE,
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            false,
        )?;
        let depth = NativeTexture2D::create(
            device,
            raw_device,
            "dlss depth",
            wgpu::TextureFormat::R32Float,
            DXGI_FORMAT_R32_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            D3D12_STATE_COMMON,
            false,
        )?;
        let mv = NativeTexture2D::create(
            device,
            raw_device,
            "dlss motion vectors",
            wgpu::TextureFormat::Rg16Float,
            DXGI_FORMAT_R16G16_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            false,
        )?;
        // DLSS writes its result with a compute dispatch, so the output needs
        // UAV access — and a UAV cannot be an sRGB format, hence the plain
        // UNORM twin the FSR and XeSS outputs use as well.
        let output = NativeTexture2D::create(
            device,
            raw_device,
            "dlss output",
            format.remove_srgb_suffix(),
            dxgi_output_format(format),
            display_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            true,
        )?;
        Ok(Self {
            color,
            depth,
            mv,
            output,
        })
    }
}

pub struct DlssUpscaler {
    api: NgxApi,
    params: Box<NgxParams>,
    handle: *mut c_void,
    queue: d3d12::CommandQueue,
    ring: NativeCommandRing,
    render_extent: (u32, u32),
    display_extent: (u32, u32),
    natives: DlssNatives,
    mv_pass: FsrMotionVectorPass,
    composite: FsrComposite,
    frame_index: u64,
    phase_count: u64,
    current_jitter: (f32, f32),
    prev_view_proj: Option<Mat4>,
    reset_history: bool,
    last_instant: std::time::Instant,
    quality_name: &'static str,
}

impl Drop for DlssUpscaler {
    fn drop(&mut self) {
        self.ring.wait_idle();
        if !self.handle.is_null() {
            unsafe {
                (self.api.release_feature)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

impl DlssUpscaler {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sdk_dir: &str,
        render_extent: (u32, u32),
        display_extent: (u32, u32),
        format: wgpu::TextureFormat,
        quality: crate::UpscaleQuality,
    ) -> Result<Self, String> {
        let _ = queue;
        let (quality_setting, quality_name) = match quality {
            // Native AA is DLAA: the same feature at a 1.0× ratio, which is
            // what `provider_extent` gives this quality.
            crate::UpscaleQuality::NativeAa => (NGX_QUALITY_DLAA, "DLAA (native)"),
            crate::UpscaleQuality::UltraQuality => (NGX_QUALITY_ULTRA_QUALITY, "ultra quality"),
            crate::UpscaleQuality::Quality => (NGX_QUALITY_MAX_QUALITY, "quality"),
            crate::UpscaleQuality::Balanced => (NGX_QUALITY_BALANCED, "balanced"),
            crate::UpscaleQuality::Performance => (NGX_QUALITY_MAX_PERF, "performance"),
            crate::UpscaleQuality::UltraPerformance => {
                (NGX_QUALITY_ULTRA_PERFORMANCE, "ultra performance")
            }
        };
        let (raw_device, raw_queue) = raw_dx12(device)?;
        let device_ptr = raw_device.as_mut_ptr() as *mut c_void;

        // NGX Core (`nvngx.dll`, from the driver) initialises once for the
        // device and loads the vendored `nvngx_dlss.dll` itself; the snippet is never
        // called directly, because it refuses to initialise anything that is
        // not an NGX runtime (0xBAD00002).
        let api = NgxApi::load(sdk_dir, "nvngx_dlss.dll", device_ptr)?;
        let mut params = api.new_parameters();
        crate::dlss::diag(&format!(
            "[DLSS] NGX ready (API 0x{NGX_VERSION_API:x}, core {}), creating DLSS",
            api.core_path()
        ));

        params.set_u32(P_WIDTH, render_extent.0.max(1));
        params.set_u32(P_HEIGHT, render_extent.1.max(1));
        params.set_u32(P_OUT_WIDTH, display_extent.0.max(1));
        params.set_u32(P_OUT_HEIGHT, display_extent.1.max(1));
        params.set_u32(P_PERF_QUALITY, quality_setting);
            let flags = NGX_DLSS_FEATURE_MV_LOW_RES | NGX_DLSS_FEATURE_DEPTH_INVERTED;
            params.set_i32(P_CREATE_FLAGS, flags);  

        let mut ring = NativeCommandRing::new(&raw_device, "DLSS")?;
        let mut handle: *mut c_void = std::ptr::null_mut();
        // Creation is allowed to record its own work (kernels/uploads) on the
        // list it is handed, so give it a real one and retire it afterwards.
        let slot_index = ring.acquire("DLSS")?;
        let result = {
            let list = &mut ring.slot_mut(slot_index).list;
            unsafe {
                (api.create_feature)(
                    list.as_mut_ptr() as *mut c_void,
                    NGX_FEATURE_SUPER_SAMPLING,
                    params.as_ptr(),
                    &mut handle,
                )
            }
        };
        if !crate::dlss::succeeded(result) || handle.is_null() {
            return Err(format!(
                "NVSDK_NGX_D3D12_CreateFeature(DLSS) failed ({})",
                crate::dlss::result_name(result)
            ));
        }
        let _ = ring.retire(slot_index, &raw_queue, "DLSS");

        crate::dlss::diag(&format!(
            "[DLSS] super resolution ready ({quality_name}, {}x{} -> {}x{})",
            render_extent.0, render_extent.1, display_extent.0, display_extent.1
        ));

        let natives = unsafe {
            DlssNatives::create(device, &raw_device, render_extent, display_extent, format)?
        };
        let mv_pass = FsrMotionVectorPass::new(device);
        let composite = FsrComposite::new(device, format);

        let ratio = display_extent.0.max(1) as f32 / render_extent.0.max(1) as f32;
        let phase_count = ((8.0 * ratio * ratio).ceil() as u64).clamp(16, 64);
        Ok(Self {
            api,
            params,
            handle,
            queue: raw_queue,
            ring,
            render_extent,
            display_extent,
            natives,
            mv_pass,
            composite,
            frame_index: 0,
            phase_count,
            current_jitter: (0.0, 0.0),
            prev_view_proj: None,
            reset_history: true,
            last_instant: std::time::Instant::now(),
            quality_name,
        })
    }

    /// The render extent the native passes use.
    pub fn render_extent(&self) -> (u32, u32) {
        self.render_extent
    }

    /// The display extent the output is produced at.
    pub fn display_extent(&self) -> (u32, u32) {
        self.display_extent
    }

    /// Human-readable name of the active quality preset (UI badge).
    pub fn quality_name(&self) -> &'static str {
        self.quality_name
    }

    /// Halton jitter for the next frame, in pixels. Advances at record().
    pub fn next_jitter(&mut self) -> (f32, f32) {
        self.current_jitter = jitter_for_frame(self.frame_index, self.phase_count);
        self.current_jitter
    }

    /// The jitter currently in effect.
    pub fn jitter(&self) -> (f32, f32) {
        self.current_jitter
    }

    /// The jittered view-projection for this frame.
    ///
    /// DLSS wants the shear on the projection matrix *and* the same pixel
    /// offset in its evaluate call (`Jitter.Offset.X/Y`), so both describe the
    /// same sub-pixel sample position — see
    /// [`crate::fsr::jittered_clip_vp`].
    pub fn jittered_matrix(&self, matrix: Mat4) -> Mat4 {
        crate::fsr::jittered_clip_vp(matrix, self.current_jitter, self.render_extent)
    }

    /// On camera cuts, teleports and DLSS re-enables the accumulation history
    /// is meaningless; the next evaluate runs with `Reset`.
    pub fn reset_history(&mut self) {
        self.reset_history = true;
        self.prev_view_proj = None;
    }

    /// Copies the world colour/depth into the native targets, runs the motion
    /// vector pass, then evaluates DLSS on its own native command list.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color: &wgpu::Texture,
        depth: &wgpu::Texture,
        unjittered_vp: &Mat4,
    ) -> Result<(), String> {
        let now = std::time::Instant::now();
        let frame_time_ms = (now - self.last_instant).as_secs_f32() * 1000.0;
        self.last_instant = now;
        let (rw, rh) = self.render_extent;
        let size = wgpu::Extent3d {
            width: rw.max(1),
            height: rh.max(1),
            depth_or_array_layers: 1,
        };

        // --- wgpu side: world colour into the committed texture the native
        // list reads, plus motion vectors and a raw R32_FLOAT copy of the
        // reversed-Z depth in one fullscreen pass.
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &self.natives.color.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            size,
        );
        let inv_vp = unjittered_vp.inverse();
        let prev_vp = self.prev_view_proj.unwrap_or(*unjittered_vp);
        let params = FsrMvParams {
            inv_vp: inv_vp.to_cols_array_2d(),
            prev_vp: prev_vp.to_cols_array_2d(),
            render_size: [rw as f32, rh as f32],
            // See the FSR pass: on a reset frame the previous matrices
            // describe an unrelated view, so the vectors must be zero.
            prev_valid: u32::from(self.prev_view_proj.is_some() && !self.reset_history),
            pad: 0,
        };
        queue.write_buffer(self.mv_pass.params_buffer(), 0, bytemuck::bytes_of(&params));
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor {
            aspect: wgpu::TextureAspect::All,
            ..Default::default()
        });
        let mv_view = self
            .natives
            .mv
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let depth_out_view = self
            .natives
            .depth
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dlss motion vector bind group"),
            layout: self.mv_pass.layout(),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.mv_pass.params_buffer().as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dlss motion vector pass"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: &mv_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &depth_out_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(self.mv_pass.pipeline());
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));

        // --- Native DLSS step on the same queue.
        let slot_index = self.ring.acquire("DLSS")?;
        let subresource = d3d12_ty::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
        let none = d3d12_ty::D3D12_RESOURCE_BARRIER_FLAG_NONE;
        let barrier = |resource: &d3d12::Resource, before: u32, after: u32| {
            d3d12::ResourceBarrier::transition(resource.clone(), subresource, before, after, none)
        };
        // The output starts in COMMON on frame 0; afterwards the restore
        // barrier below parks it in PIXEL_COMPUTE_READ.
        let output_before = if self.frame_index == 0 {
            D3D12_STATE_COMMON
        } else {
            D3D12_STATE_PIXEL_COMPUTE_READ
        };
        let barriers = [
            barrier(
                &self.natives.color.resource,
                D3D12_STATE_COPY_DEST,
                D3D12_STATE_NON_PIXEL_READ,
            ),
            barrier(
                &self.natives.depth.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_NON_PIXEL_READ,
            ),
            barrier(
                &self.natives.mv.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_NON_PIXEL_READ,
            ),
            barrier(
                &self.natives.output.resource,
                output_before,
                D3D12_STATE_UNORDERED_ACCESS,
            ),
        ];
        let list = &mut self.ring.slot_mut(slot_index).list;
        list.resource_barrier(&barriers);

        self.params.set_d3d12_resource(
            P_COLOR,
            self.natives.color.resource.as_mut_ptr() as *mut c_void,
        );
        self.params.set_d3d12_resource(
            P_DEPTH,
            self.natives.depth.resource.as_mut_ptr() as *mut c_void,
        );
        self.params.set_d3d12_resource(
            P_MOTION_VECTORS,
            self.natives.mv.resource.as_mut_ptr() as *mut c_void,
        );
        self.params.set_d3d12_resource(
            P_OUTPUT,
            self.natives.output.resource.as_mut_ptr() as *mut c_void,
        );
        self.params.set_f32(P_JITTER_X, self.current_jitter.0);
        self.params.set_f32(P_JITTER_Y, self.current_jitter.1);
        self.params.set_i32(P_RESET, i32::from(self.reset_history));
        // Motion vectors are in pixels, so the scale is 1:1.
        self.params.set_f32(P_MV_SCALE_X, 1.0);
        self.params.set_f32(P_MV_SCALE_Y, 1.0);
        let frame_time_ms = frame_time_ms.clamp(0.1, 1000.0);
        self.params.set_f32(P_FRAME_TIME_MS, frame_time_ms);
        // Sharpness is deprecated in current DLSS builds (sharpening moved to
        // the game); 0 keeps the vendor default and is what the guide passes.
        self.params.set_f32(P_SHARPNESS, 0.0);
        self.params
            .set_u32(P_RENDER_WIDTH, self.render_extent.0.max(1));
        self.params
            .set_u32(P_RENDER_HEIGHT, self.render_extent.1.max(1));

        let result = unsafe {
            (self.api.evaluate_feature)(
                list.as_mut_ptr() as *mut c_void,
                self.handle,
                self.params.as_ptr(),
                std::ptr::null(),
            )
        };

        let restore = [
            barrier(
                &self.natives.color.resource,
                D3D12_STATE_NON_PIXEL_READ,
                D3D12_STATE_COPY_DEST,
            ),
            barrier(
                &self.natives.depth.resource,
                D3D12_STATE_NON_PIXEL_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(
                &self.natives.mv.resource,
                D3D12_STATE_NON_PIXEL_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(
                &self.natives.output.resource,
                D3D12_STATE_UNORDERED_ACCESS,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
        ];
        list.resource_barrier(&restore);
        self.ring.retire(slot_index, &self.queue, "DLSS")?;

        if !crate::dlss::succeeded(result) {
            return Err(format!(
                "NVSDK_NGX_D3D12_EvaluateFeature(DLSS) failed ({})",
                crate::dlss::result_name(result)
            ));
        }

        self.reset_history = false;
        self.prev_view_proj = Some(*unjittered_vp);
        self.frame_index += 1;
        Ok(())
    }

    /// Composites the upscaled frame into the composition view.
    pub fn composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
    ) {
        self.composite
            .draw(device, queue, &self.natives.output.texture, target);
    }
}
