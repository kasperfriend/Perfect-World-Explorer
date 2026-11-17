//! Windows + DX12 implementation of XeSS super resolution. See the
//! module-level documentation in `super` for the overall design and limits.
//!
//! The DLLs (`libxess.dll`) export a plain C API; the contexts are created
//! against the raw DX12 device wgpu created, exactly like the FFX
//! integration. Motion vectors come from the shared depth-reprojection pass;
//! they describe camera motion only (docs/blockers.md B-5).

use super::loader::{free_sdk_dll, load_sdk_dll};
use crate::fsr::{
    dxgi_output_format, FsrComposite, FsrMotionVectorPass, FsrMvParams, NativeCommandRing,
    NativeTexture2D,
};
use crate::fsr::{
    raw_dx12, D3D12_STATE_COMMON, D3D12_STATE_COPY_DEST, D3D12_STATE_PIXEL_COMPUTE_READ,
    D3D12_STATE_RENDER_TARGET, D3D12_STATE_UNORDERED_ACCESS,
};
use glam::Mat4;
use std::ffi::c_void;
use winapi::shared::dxgiformat::{DXGI_FORMAT_R16G16_FLOAT, DXGI_FORMAT_R32_FLOAT};
use winapi::um::d3d12 as d3d12_ty;

// `XESS_QUALITY_SETTING_*` values live in `crate::modes` (the provider ×
// quality table): `ULTRA_PERFORMANCE 100`, `PERFORMANCE 101`, `BALANCED 102`,
// `QUALITY 103`, `ULTRA_QUALITY 104`, `AA 106`. They are read from there so
// the mode the UI advertises is exactly the `xess_quality_settings_t` the
// context is created with — XeSS's Native AA mode is `XESS_QUALITY_SETTING_AA`,
// not "Ultra Quality at a 1.0x render".

const XESS_INIT_FLAG_INVERTED_DEPTH: u32 = 1 << 1;
/// The input color is post-tonemap, sRGB-encoded LDR — matching how the FSR
/// path declares NON_LINEAR_COLORSPACE. Disables internal tonemapping.
const XESS_INIT_FLAG_LDR_INPUT_COLOR: u32 = 1 << 6;

const XESS_LOGGING_LEVEL_INFO: i32 = 1;

/// Jitter pattern length; Halton(2,3) is what most engines use for XeSS.
const PHASE_COUNT: usize = 16;

#[repr(C)]
#[derive(Clone, Copy)]
struct Xess2d {
    x: u32,
    y: u32,
}

#[repr(C)]
struct XessInitParams {
    output_resolution: Xess2d,
    quality_setting: i32,
    init_flags: u32,
    creation_node_mask: u32,
    visible_node_mask: u32,
    temp_buffer_heap: *mut c_void,
    buffer_heap_offset: u64,
    temp_texture_heap: *mut c_void,
    texture_heap_offset: u64,
    pipeline_library: *mut c_void,
}

#[repr(C)]
struct XessExecuteParams {
    color_texture: *mut c_void,
    velocity_texture: *mut c_void,
    depth_texture: *mut c_void,
    exposure_scale_texture: *mut c_void,
    responsive_pixel_mask_texture: *mut c_void,
    output_texture: *mut c_void,
    jitter_offset_x: f32,
    jitter_offset_y: f32,
    exposure_scale: f32,
    reset_history: u32,
    input_width: u32,
    input_height: u32,
    input_color_base: Xess2d,
    input_motion_vector_base: Xess2d,
    input_depth_base: Xess2d,
    input_responsive_mask_base: Xess2d,
    reserved0: Xess2d,
    output_color_base: Xess2d,
    descriptor_heap: *mut c_void,
    descriptor_heap_offset: u32,
}

/// Human-readable XeSS result codes for diagnostics.
pub(crate) fn xess_result_name(rc: i32) -> &'static str {
    match rc {
        0 => "XESS_RESULT_SUCCESS",
        1 => "XESS_RESULT_WARNING_NONEXISTING_FOLDER",
        2 => "XESS_RESULT_WARNING_OLD_DRIVER",
        -1 => "XESS_RESULT_ERROR_UNSUPPORTED_DEVICE",
        -2 => "XESS_RESULT_ERROR_UNSUPPORTED_DRIVER",
        -3 => "XESS_RESULT_ERROR_UNINITIALIZED",
        -4 => "XESS_RESULT_ERROR_INVALID_ARGUMENT",
        -5 => "XESS_RESULT_ERROR_DEVICE_OUT_OF_MEMORY",
        -6 => "XESS_RESULT_ERROR_DEVICE",
        -7 => "XESS_RESULT_ERROR_NOT_IMPLEMENTED",
        -8 => "XESS_RESULT_ERROR_INVALID_CONTEXT",
        -9 => "XESS_RESULT_ERROR_OPERATION_IN_PROGRESS",
        -10 => "XESS_RESULT_ERROR_UNSUPPORTED",
        -11 => "XESS_RESULT_ERROR_CANT_LOAD_LIBRARY",
        -12 => "XESS_RESULT_ERROR_WRONG_CALL_ORDER",
        -1000 => "XESS_RESULT_ERROR_UNKNOWN",
        _ => "XESS_RESULT_(unknown)",
    }
}

extern "C" fn xess_log_callback(message: *const std::os::raw::c_char, level: i32) {
    if message.is_null() {
        return;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(message) }.to_string_lossy();
    let level = match level {
        0 => "debug",
        1 => "info",
        2 => "warning",
        3 => "error",
        _ => "log",
    };
    crate::xess::diag(&format!("[XeSS {level}] {text}"));
}

type PfnXessD3D12CreateContext =
    unsafe extern "C" fn(device: *mut c_void, out_context: *mut *mut c_void) -> i32;
type PfnXessSetLoggingCallback = unsafe extern "C" fn(
    context: *mut c_void,
    level: i32,
    callback: extern "C" fn(*const std::os::raw::c_char, i32),
) -> i32;
type PfnXessD3D12BuildPipelines = unsafe extern "C" fn(
    context: *mut c_void,
    pipeline_library: *mut c_void,
    blocking: bool,
    init_flags: u32,
) -> i32;
type PfnXessD3D12Init =
    unsafe extern "C" fn(context: *mut c_void, params: *const XessInitParams) -> i32;
type PfnXessD3D12Execute = unsafe extern "C" fn(
    context: *mut c_void,
    command_list: *mut c_void,
    params: *const XessExecuteParams,
) -> i32;
type PfnXessDestroyContext = unsafe extern "C" fn(context: *mut c_void) -> i32;
type PfnXessIsOptimalDriver = unsafe extern "C" fn(context: *mut c_void) -> i32;
struct XessLoader {
    handle: winapi::shared::minwindef::HMODULE,
    context: *mut c_void,
    create_context: PfnXessD3D12CreateContext,
    build_pipelines: PfnXessD3D12BuildPipelines,
    init: PfnXessD3D12Init,
    execute: PfnXessD3D12Execute,
    destroy_context: PfnXessDestroyContext,
    is_optimal_driver: PfnXessIsOptimalDriver,
    set_logging: PfnXessSetLoggingCallback,
}

impl XessLoader {
    fn load(sdk_dir: &str) -> Result<Self, String> {
        const EXPORTS: &[&[u8]] = &[
            b"xessD3D12CreateContext\0",
            b"xessD3D12BuildPipelines\0",
            b"xessD3D12Init\0",
            b"xessD3D12Execute\0",
            b"xessDestroyContext\0",
            b"xessIsOptimalDriver\0",
            b"xessSetLoggingCallback\0",
        ];
        let (handle, exports) = load_sdk_dll(sdk_dir, "libxess.dll", EXPORTS)?;
        Ok(Self {
            handle,
            context: std::ptr::null_mut(),
            create_context: unsafe { std::mem::transmute(exports[0]) },
            build_pipelines: unsafe { std::mem::transmute(exports[1]) },
            init: unsafe { std::mem::transmute(exports[2]) },
            execute: unsafe { std::mem::transmute(exports[3]) },
            destroy_context: unsafe { std::mem::transmute(exports[4]) },
            is_optimal_driver: unsafe { std::mem::transmute(exports[5]) },
            set_logging: unsafe { std::mem::transmute(exports[6]) },
        })
    }
}

impl Drop for XessLoader {
    fn drop(&mut self) {
        if !self.context.is_null() {
            let _ = unsafe { (self.destroy_context)(self.context) };
            self.context = std::ptr::null_mut();
        }
        unsafe { free_sdk_dll(self.handle) };
    }
}

struct XessNatives {
    color: NativeTexture2D,
    depth: NativeTexture2D,
    mv: NativeTexture2D,
    output: NativeTexture2D,
}

impl XessNatives {
    /// # Safety
    /// `raw_device` must be the device `device` was created from.
    unsafe fn create(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        render_extent: (u32, u32),
        display_extent: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Result<Self, String> {
        // UNORM description, not `_UNORM_SRGB`: the Intel runtime builds its
        // SRV from the resource description, so an sRGB description would hand
        // it hardware-decoded (linear) pixels while `LDR_INPUT_COLOR` tells it
        // the input is display-referred and needs no tonemapping — the world
        // came out darker than the bilinear path. See `FsrNatives::create`.
        let color = NativeTexture2D::create(
            device,
            raw_device,
            "xess color",
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
            "xess depth",
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
            "xess motion vectors",
            wgpu::TextureFormat::Rg16Float,
            DXGI_FORMAT_R16G16_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            false,
        )?;
        // UAV-friendly copy of the display image (same reasoning as the FSR
        // output: no _UNORM_SRGB twins allowed on UAV resources).
        let output = NativeTexture2D::create(
            device,
            raw_device,
            "xess output",
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

fn jitter_for_frame(frame: u64) -> (f32, f32) {
    let n = (frame % PHASE_COUNT as u64) as u32 + 1;
    (halton(n, 2) - 0.5, halton(n, 3) - 0.5)
}

pub struct XessUpscaler {
    loader: XessLoader,
    queue: d3d12::CommandQueue,
    ring: NativeCommandRing,
    render_extent: (u32, u32),
    display_extent: (u32, u32),
    natives: XessNatives,
    mv_pass: FsrMotionVectorPass,
    composite: FsrComposite,
    frame_index: u64,
    current_jitter: (f32, f32),
    prev_view_proj: Option<Mat4>,
    reset_history: bool,
    quality_name: &'static str,
}

impl Drop for XessUpscaler {
    fn drop(&mut self) {
        self.ring.wait_idle();
    }
}

impl XessUpscaler {
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
        // One row of the shared matrix per mode (see `crate::modes`): Native AA
        // is `XESS_QUALITY_SETTING_AA` (1.0x), Ultra Quality is
        // `XESS_QUALITY_SETTING_ULTRA_QUALITY` (1.3x), and a mode XeSS does not
        // have is refused here rather than folded into the nearest one.
        let mode = crate::modes::require(crate::UpscaleMethod::Xess, quality)?;
        let quality_setting = mode.vendor as i32;
        let quality_name = mode.name;
        let (raw_device, raw_queue) = raw_dx12(device)?;
        let device_ptr = raw_device.as_mut_ptr() as *mut c_void;
        let mut loader = XessLoader::load(sdk_dir)?;

        let mut context: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { (loader.create_context)(device_ptr, &mut context) };
        if rc != 0 || context.is_null() {
            return Err(format!(
                "xessD3D12CreateContext failed ({} — an SM 6.4 capable GPU is required)",
                xess_result_name(rc)
            ));
        }
        loader.context = context;
        let _ =
            unsafe { (loader.set_logging)(context, XESS_LOGGING_LEVEL_INFO, xess_log_callback) };
        let rc = unsafe { (loader.is_optimal_driver)(context) };
        if rc == 2 {
            crate::xess::diag("[XeSS] warning: the installed driver may degrade XeSS quality or performance; a newer driver is recommended");
        }

        let init_flags = XESS_INIT_FLAG_INVERTED_DEPTH | XESS_INIT_FLAG_LDR_INPUT_COLOR;
        // Pre-build kernels so init() does not stall on pipeline creation.
        let rc =
            unsafe { (loader.build_pipelines)(context, std::ptr::null_mut(), true, init_flags) };
        if rc != 0 {
            crate::xess::diag(&format!(
                "[XeSS] xessD3D12BuildPipelines returned {}; init will compile pipelines instead",
                xess_result_name(rc)
            ));
        }
        let params = XessInitParams {
            output_resolution: Xess2d {
                x: display_extent.0,
                y: display_extent.1,
            },
            quality_setting,
            init_flags,
            creation_node_mask: 0,
            visible_node_mask: 0,
            temp_buffer_heap: std::ptr::null_mut(),
            buffer_heap_offset: 0,
            temp_texture_heap: std::ptr::null_mut(),
            texture_heap_offset: 0,
            pipeline_library: std::ptr::null_mut(),
        };
        let rc = unsafe { (loader.init)(context, &params) };
        if rc != 0 {
            let name = xess_result_name(rc).to_string();
            // loader.drop destroys the context and unloads the DLL.
            return Err(format!("xessD3D12Init failed ({name})"));
        }
        crate::xess::diag(&format!(
            "[XeSS] super resolution ready ({quality_name}, {}x{} -> {}x{})",
            render_extent.0, render_extent.1, display_extent.0, display_extent.1
        ));

        let natives = match unsafe {
            XessNatives::create(device, &raw_device, render_extent, display_extent, format)
        } {
            Ok(natives) => natives,
            Err(error) => {
                return Err(error);
            }
        };
        let ring = match NativeCommandRing::new(&raw_device, "XeSS") {
            Ok(ring) => ring,
            Err(error) => return Err(error),
        };
        let mv_pass = FsrMotionVectorPass::new(device);
        let composite = FsrComposite::new(device, format);

        Ok(Self {
            loader,
            queue: raw_queue,
            ring,
            render_extent,
            display_extent,
            natives,
            mv_pass,
            composite,
            frame_index: 0,
            current_jitter: (0.0, 0.0),
            prev_view_proj: None,
            reset_history: true,
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
        self.current_jitter = jitter_for_frame(self.frame_index);
        self.current_jitter
    }

    /// The jitter currently in effect.
    pub fn jitter(&self) -> (f32, f32) {
        self.current_jitter
    }

    /// The jittered view-projection for this frame.
    ///
    /// XeSS gets the same pixel offsets in `xessExecute`, and the shear has to
    /// describe the same sub-pixel sample position — see
    /// [`crate::fsr::jittered_clip_vp`].
    pub fn jittered_matrix(&self, matrix: Mat4) -> Mat4 {
        crate::fsr::jittered_clip_vp(matrix, self.current_jitter, self.render_extent)
    }

    /// On camera cuts, teleports and XeSS re-enables the accumulation
    /// history is meaningless; the next execute runs with resetHistory.
    pub fn reset_history(&mut self) {
        self.reset_history = true;
        self.prev_view_proj = None;
    }

    /// Copies the world color/depth into the native targets, runs the MV
    /// pass, then executes XeSS on its own native command list.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color: &wgpu::Texture,
        depth: &wgpu::Texture,
        object_mv: &wgpu::TextureView,
        unjittered_vp: &Mat4,
    ) -> Result<(), String> {
        let (rw, rh) = self.render_extent;
        let size = wgpu::Extent3d {
            width: rw.max(1),
            height: rh.max(1),
            depth_or_array_layers: 1,
        };

        // --- wgpu side: copy world color into the committed texture the
        // native list will read, and gather motion vectors + a raw
        // R32_FLOAT copy of the reversed-Z depth in one fullscreen pass.
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
            label: Some("xess motion vector bind group"),
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
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(object_mv),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("xess motion vector pass"),
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

        // --- Native XeSS step on the same queue.
        let slot_index = self.ring.acquire("XeSS")?;
        let list = &mut self.ring.slot_mut(slot_index).list;
        let subresource = d3d12_ty::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
        let none = d3d12_ty::D3D12_RESOURCE_BARRIER_FLAG_NONE;
        let barrier = |resource: &d3d12::Resource, before: u32, after: u32| {
            d3d12::ResourceBarrier::transition(resource.clone(), subresource, before, after, none)
        };
        // The output resource starts in COMMON on frame 0; afterwards the
        // restore barrier below parks it in PIXEL_COMPUTE_READ.
        let output_before = if self.frame_index == 0 {
            D3D12_STATE_COMMON
        } else {
            D3D12_STATE_PIXEL_COMPUTE_READ
        };
        let barriers = [
            barrier(
                &self.natives.color.resource,
                D3D12_STATE_COPY_DEST,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
            barrier(
                &self.natives.depth.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
            barrier(
                &self.natives.mv.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
            barrier(
                &self.natives.output.resource,
                output_before,
                D3D12_STATE_UNORDERED_ACCESS,
            ),
        ];
        list.resource_barrier(&barriers);

        let exec = XessExecuteParams {
            color_texture: self.natives.color.resource.as_mut_ptr() as *mut c_void,
            velocity_texture: self.natives.mv.resource.as_mut_ptr() as *mut c_void,
            depth_texture: self.natives.depth.resource.as_mut_ptr() as *mut c_void,
            exposure_scale_texture: std::ptr::null_mut(),
            responsive_pixel_mask_texture: std::ptr::null_mut(),
            output_texture: self.natives.output.resource.as_mut_ptr() as *mut c_void,
            jitter_offset_x: self.current_jitter.0,
            jitter_offset_y: self.current_jitter.1,
            exposure_scale: 1.0,
            reset_history: u32::from(self.reset_history),
            input_width: rw,
            input_height: rh,
            input_color_base: Xess2d { x: 0, y: 0 },
            input_motion_vector_base: Xess2d { x: 0, y: 0 },
            input_depth_base: Xess2d { x: 0, y: 0 },
            input_responsive_mask_base: Xess2d { x: 0, y: 0 },
            reserved0: Xess2d { x: 0, y: 0 },
            output_color_base: Xess2d { x: 0, y: 0 },
            descriptor_heap: std::ptr::null_mut(),
            descriptor_heap_offset: 0,
        };
        let rc = unsafe {
            (self.loader.execute)(self.loader.context, list.as_mut_ptr() as *mut c_void, &exec)
        };

        let restore = [
            barrier(
                &self.natives.color.resource,
                D3D12_STATE_PIXEL_COMPUTE_READ,
                D3D12_STATE_COPY_DEST,
            ),
            barrier(
                &self.natives.depth.resource,
                D3D12_STATE_PIXEL_COMPUTE_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(
                &self.natives.mv.resource,
                D3D12_STATE_PIXEL_COMPUTE_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(
                &self.natives.output.resource,
                D3D12_STATE_UNORDERED_ACCESS,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
        ];
        list.resource_barrier(&restore);
        self.ring.retire(slot_index, &self.queue, "XeSS")?;

        if rc != 0 {
            return Err(format!(
                "xessD3D12Execute failed ({})",
                xess_result_name(rc)
            ));
        }

        if self.frame_index == 0 {
            crate::xess::diag(&format!(
                "[XeSS] first execute OK ({}x{} -> {}x{}, {})",
                self.render_extent.0,
                self.render_extent.1,
                self.display_extent.0,
                self.display_extent.1,
                self.quality_name
            ));
        }
        self.reset_history = false;
        self.prev_view_proj = Some(*unjittered_vp);
        self.frame_index += 1;
        Ok(())
    }

    /// Composites the upscaled frame into the composition view.
    /// Blit the XeSS result into the composition.
    ///
    /// `sharpness` runs the viewer's own RCAS-style pass: Intel's SDK 3.0.2
    /// exposes no sharpening control (`xess.h` has only `xessExecute` and the
    /// jitter/logging helpers), so this is the only way to give XeSS the same
    /// slider FSR and DLSS have.
    pub fn composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        sharpness: f32,
    ) {
        self.composite.draw(
            device,
            queue,
            &self.natives.output.texture,
            target,
            sharpness,
        );
    }
}
