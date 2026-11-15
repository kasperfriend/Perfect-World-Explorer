//! XeSS Frame Generation + XeLL, Windows + DX12.
//!
//! Like the FFX frame generation integration, this cannot be layered on
//! wgpu's swapchain: the interpolation happens *inside* a swapchain the
//! library creates for the hwnd. This module therefore takes presentation
//! over the same way:
//!
//! * On enable, the wgpu surface is dropped (which destroys wgpu-hal's
//!   `IDXGISwapChain`), the XeLL low-latency context is created and
//!   enabled, and the XeSS-FG "swap chain" context is initialized from a
//!   description of a flip-model swapchain. XeSS-FG creates its own proxy
//!   `IDXGISwapChain` for the hwnd internally.
//! * Every frame the caller renders normally, ending with a linear image in
//!   [`XessFrameGeneration::compose_view`]. [`XessFrameGeneration::prepare`]
//!   produces low-resolution reprojected motion vectors from the reversed-Z
//!   depth, tags depth/motion vectors and the frame constants with XeSS-FG,
//!   and [`XessFrameGeneration::finish`] copies the composition into the
//!   proxy backbuffer, tags the UI texture, sets the present id and calls
//!   `Present` on the proxy (which paces real and interpolated frames).
//! * On disable, the XeSS-FG context is destroyed first, then XeLL (the SDK
//!   requires this order), and the engine recreates the wgpu surface.
//!
//! XeLL is not optional: XeSS-FG refuses to initialize its proxy swapchain
//! without a latency-reduction context, and on non-Intel GPUs latency
//! reduction only works while XeSS-FG is enabled. The XeLL context is
//! therefore created and torn down together with frame generation, and the
//! required simulation / render-submit / present markers are delivered
//! every frame using the same frame id that is passed to `xellSleep` and to
//! the XeSS-FG tagging calls.
//!
//! UI composition uses the "separate UI texture" mode
//! (`HUDLESS_UITEXTURE`): the UI is drawn into
//! [`XessFrameGeneration::ui_view`] by the caller and tagged with XeSS-FG
//! every presented frame, so interpolated frames composite a sharp UI on
//! top of the interpolated scene. The UI texture is expected to be
//! premultiplied (egui blends with One/OneMinusSrcAlpha).
//!
//! Non-Intel GPUs fall back to SM 6.4 kernels and produce a maximum of one
//! interpolated frame per real frame; Intel discrete GPUs may support more,
//! but this path asks for exactly one everywhere (2x), matching the FSR
//! frame generation behavior.

use super::loader::{free_sdk_dll, load_sdk_dll};
use super::xell::{
    Xell, XELL_PRESENT_END, XELL_PRESENT_START, XELL_RENDERSUBMIT_END, XELL_RENDERSUBMIT_START,
    XELL_SIMULATION_END, XELL_SIMULATION_START,
};
use crate::fsr::{
    dxgi_for_format, raw_dx12, FsrMotionVectorPass, FsrMvParams, NativeCommandRing,
    NativeTexture2D, D3D12_STATE_COMMON, D3D12_STATE_COPY_DEST, D3D12_STATE_PIXEL_COMPUTE_READ,
    D3D12_STATE_RENDER_TARGET,
};
use glam::Mat4;
use std::ffi::c_void;
use winapi::shared::dxgi::IDXGISwapChain;
use winapi::shared::dxgi1_2::{DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FULLSCREEN_DESC};
use winapi::shared::dxgi1_4::IDXGISwapChain3;
use winapi::shared::dxgiformat::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16_FLOAT, DXGI_FORMAT_R32_FLOAT,
    DXGI_FORMAT_R8G8B8A8_UNORM,
};
use winapi::um::d3d12 as d3d12_ty;
use winapi::Interface;

const D3D12_STATE_COPY_SRC: u32 = d3d12_ty::D3D12_RESOURCE_STATE_COPY_SOURCE;
const D3D12_STATE_PRESENT: u32 = d3d12_ty::D3D12_RESOURCE_STATE_PRESENT;
const D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES: u32 =
    d3d12_ty::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
const D3D12_RESOURCE_BARRIER_FLAG_NONE: u32 = d3d12_ty::D3D12_RESOURCE_BARRIER_FLAG_NONE;

const XEFG_INIT_FLAG_INVERTED_DEPTH: u32 = 1 << 0;
const XEFG_UI_MODE_HUDLESS_UITEXTURE: i32 = 3;
const XEFG_UI_COMPOSITION_STATE_ENABLED: u32 = 1;
const XEFG_SWAPCHAIN_RV_UNTIL_NEXT_PRESENT: u32 = 0;
const XEFG_SWAPCHAIN_RES_HUDLESS_COLOR: u32 = 0;
const XEFG_SWAPCHAIN_RES_DEPTH: u32 = 1;
const XEFG_SWAPCHAIN_RES_MOTION_VECTOR: u32 = 2;
const XEFG_SWAPCHAIN_RES_UI: u32 = 3;
const XEFG_LOGGING_LEVEL_INFO: i32 = 1;

/// Human-readable XeSS-FG result codes for diagnostics.
/// Human-readable UI composition mode, for the ready line and for retests.
fn xefg_ui_mode_name(mode: i32) -> &'static str {
    match mode {
        0 => "AUTO",
        1 => "NONE",
        2 => "BACKBUFFER_UITEXTURE",
        3 => "HUDLESS_UITEXTURE",
        4 => "BACKBUFFER_HUDLESS",
        5 => "BACKBUFFER_HUDLESS_UITEXTURE",
        _ => "unknown",
    }
}

pub(crate) fn xefg_result_name(rc: i32) -> &'static str {
    match rc {
        0 => "XEFG_SWAPCHAIN_RESULT_SUCCESS",
        2 => "XEFG_SWAPCHAIN_RESULT_WARNING_OLD_DRIVER",
        3 => "XEFG_SWAPCHAIN_RESULT_WARNING_TOO_FEW_FRAMES",
        4 => "XEFG_SWAPCHAIN_RESULT_WARNING_FRAMES_ID_MISMATCH",
        6 => "XEFG_SWAPCHAIN_RESULT_WARNING_RESOURCE_SIZES_MISMATCH",
        -1 => "XEFG_SWAPCHAIN_RESULT_ERROR_UNSUPPORTED_DEVICE",
        -2 => "XEFG_SWAPCHAIN_RESULT_ERROR_UNSUPPORTED_DRIVER",
        -3 => "XEFG_SWAPCHAIN_RESULT_ERROR_UNINITIALIZED",
        -4 => "XEFG_SWAPCHAIN_RESULT_ERROR_INVALID_ARGUMENT",
        -5 => "XEFG_SWAPCHAIN_RESULT_ERROR_DEVICE_OUT_OF_MEMORY",
        -6 => "XEFG_SWAPCHAIN_RESULT_ERROR_DEVICE",
        -7 => "XEFG_SWAPCHAIN_RESULT_ERROR_NOT_IMPLEMENTED",
        -8 => "XEFG_SWAPCHAIN_RESULT_ERROR_INVALID_CONTEXT",
        -9 => "XEFG_SWAPCHAIN_RESULT_ERROR_OPERATION_IN_PROGRESS",
        -10 => "XEFG_SWAPCHAIN_RESULT_ERROR_UNSUPPORTED",
        -11 => "XEFG_SWAPCHAIN_RESULT_ERROR_CANT_LOAD_LIBRARY",
        -12 => "XEFG_SWAPCHAIN_RESULT_ERROR_MISMATCH_INPUT_RESOURCES",
        -13 => "XEFG_SWAPCHAIN_RESULT_ERROR_INCORRECT_OUTPUT_RESOURCES",
        -14 => "XEFG_SWAPCHAIN_RESULT_ERROR_INCORRECT_INPUT_RESOURCES",
        -15 => "XEFG_SWAPCHAIN_RESULT_ERROR_LATENCY_REDUCTION_UNSUPPORTED",
        -16 => "XEFG_SWAPCHAIN_RESULT_ERROR_LATENCY_REDUCTION_FUNCTION_MISSING",
        -17 => "XEFG_SWAPCHAIN_RESULT_ERROR_HRESULT_FAILURE",
        -18 => "XEFG_SWAPCHAIN_RESULT_ERROR_DXGI_INVALID_CALL",
        -19 => "XEFG_SWAPCHAIN_RESULT_ERROR_POINTER_STILL_IN_USE",
        -20 => "XEFG_SWAPCHAIN_RESULT_ERROR_INVALID_DESCRIPTOR_HEAP",
        -21 => "XEFG_SWAPCHAIN_RESULT_ERROR_WRONG_CALL_ORDER",
        -1000 => "XEFG_SWAPCHAIN_RESULT_ERROR_UNKNOWN",
        _ => "XEFG_SWAPCHAIN_RESULT_(unknown)",
    }
}

extern "C" fn xefg_log_callback(
    message: *const std::os::raw::c_char,
    level: i32,
    _user_data: *mut c_void,
) {
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
    crate::xess::diag(&format!("[XeSS-FG {level}] {text}"));
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Xefg2d {
    x: u32,
    y: u32,
}

#[repr(C)]
struct XefgFrameConstantData {
    view_matrix: [f32; 16],
    projection_matrix: [f32; 16],
    jitter_offset_x: f32,
    jitter_offset_y: f32,
    motion_vector_scale_x: f32,
    motion_vector_scale_y: f32,
    reset_history: u32,
    frame_render_time: f32,
}

#[repr(C)]
struct XefgD3D12InitParams {
    application_swapchain: *mut c_void,
    init_flags: u32,
    max_interpolated_frames: u32,
    creation_node_mask: u32,
    visible_node_mask: u32,
    temp_buffer_heap: *mut c_void,
    buffer_heap_offset: u64,
    temp_texture_heap: *mut c_void,
    texture_heap_offset: u64,
    pipeline_library: *mut c_void,
    ui_mode: i32,
}

#[repr(C)]
struct XefgD3D12ResourceData {
    resource_type: u32,
    validity: u32,
    resource_base: Xefg2d,
    resource_size: Xefg2d,
    resource: *mut c_void,
    incoming_state: u32,
}

#[repr(C)]
struct XefgProperties {
    required_descriptor_count: u32,
    temp_buffer_heap_size: u64,
    temp_texture_heap_size: u64,
    constant_buffer_size: u64,
    max_supported_interpolations: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XefgPresentStatus {
    frames_presented: u32,
    frame_gen_result: i32,
    is_frame_gen_enabled: u32,
}

type PfnXefgD3D12CreateContext =
    unsafe extern "C" fn(device: *mut c_void, out_context: *mut *mut c_void) -> i32;
type PfnXefgD3D12BuildPipelines = unsafe extern "C" fn(
    context: *mut c_void,
    pipeline_library: *mut c_void,
    blocking: u8,
    init_flags: u32,
) -> i32;
type PfnXefgD3D12InitFromSwapChainDesc = unsafe extern "C" fn(
    context: *mut c_void,
    hwnd: *mut c_void,
    swap_chain_desc: *const DXGI_SWAP_CHAIN_DESC1,
    fullscreen_desc: *const c_void,
    command_queue: *mut c_void,
    dxgi_factory: *mut c_void,
    init_params: *const XefgD3D12InitParams,
) -> i32;
type PfnXefgD3D12GetSwapChainPtr = unsafe extern "C" fn(
    context: *mut c_void,
    riid: *const winapi::shared::guiddef::GUID,
    out: *mut *mut c_void,
) -> i32;
type PfnXefgD3D12TagFrameResource = unsafe extern "C" fn(
    context: *mut c_void,
    command_list: *mut c_void,
    present_id: u32,
    res_data: *const XefgD3D12ResourceData,
) -> i32;
type PfnXefgTagFrameConstants = unsafe extern "C" fn(
    context: *mut c_void,
    present_id: u32,
    constants: *const XefgFrameConstantData,
) -> i32;
type PfnXefgSetEnabled = unsafe extern "C" fn(context: *mut c_void, enable: u32) -> i32;
type PfnXefgSetUiCompositionState = unsafe extern "C" fn(context: *mut c_void, state: u32) -> i32;
type PfnXefgSetLatencyReduction =
    unsafe extern "C" fn(context: *mut c_void, xell: *mut c_void) -> i32;
type PfnXefgSetPresentId = unsafe extern "C" fn(context: *mut c_void, present_id: u32) -> i32;
type PfnXefgGetLastPresentStatus =
    unsafe extern "C" fn(context: *mut c_void, out_status: *mut XefgPresentStatus) -> i32;
type PfnXefgDestroy = unsafe extern "C" fn(context: *mut c_void) -> i32;
type PfnXefgD3D12GetProperties = unsafe extern "C" fn(
    context: *mut c_void,
    init_params: *const XefgD3D12InitParams,
    back_buffer_width: u32,
    back_buffer_height: u32,
    back_buffer_format: DXGI_FORMAT,
    properties: *mut XefgProperties,
) -> i32;
type PfnXefgSetLoggingCallback = unsafe extern "C" fn(
    context: *mut c_void,
    level: i32,
    callback: extern "C" fn(*const std::os::raw::c_char, i32, *mut c_void),
    user_data: *mut c_void,
) -> i32;

struct XefgLoader {
    handle: winapi::shared::minwindef::HMODULE,
    context: *mut c_void,
    set_logging: PfnXefgSetLoggingCallback,
    create_context: PfnXefgD3D12CreateContext,
    build_pipelines: PfnXefgD3D12BuildPipelines,
    init_from_desc: PfnXefgD3D12InitFromSwapChainDesc,
    get_swapchain_ptr: PfnXefgD3D12GetSwapChainPtr,
    tag_frame_resource: PfnXefgD3D12TagFrameResource,
    tag_frame_constants: PfnXefgTagFrameConstants,
    set_enabled: PfnXefgSetEnabled,
    set_ui_composition_state: PfnXefgSetUiCompositionState,
    set_latency_reduction: PfnXefgSetLatencyReduction,
    set_present_id: PfnXefgSetPresentId,
    get_last_present_status: PfnXefgGetLastPresentStatus,
    destroy_context: PfnXefgDestroy,
    get_properties: PfnXefgD3D12GetProperties,
}

impl XefgLoader {
    fn load(sdk_dir: &str) -> Result<Self, String> {
        const EXPORTS: &[&[u8]] = &[
            b"xefgSwapChainD3D12CreateContext\0",
            b"xefgSwapChainD3D12BuildPipelines\0",
            b"xefgSwapChainD3D12InitFromSwapChainDesc\0",
            b"xefgSwapChainD3D12GetSwapChainPtr\0",
            b"xefgSwapChainD3D12TagFrameResource\0",
            b"xefgSwapChainTagFrameConstants\0",
            b"xefgSwapChainSetEnabled\0",
            b"xefgSwapChainSetUiCompositionState\0",
            b"xefgSwapChainSetLatencyReduction\0",
            b"xefgSwapChainSetPresentId\0",
            b"xefgSwapChainGetLastPresentStatus\0",
            b"xefgSwapChainDestroy\0",
            b"xefgSwapChainD3D12GetProperties\0",
            b"xefgSwapChainSetLoggingCallback\0",
        ];
        let (handle, exports) = load_sdk_dll(sdk_dir, "libxess_fg.dll", EXPORTS)?;
        let loader = Self {
            handle,
            context: std::ptr::null_mut(),
            create_context: unsafe { std::mem::transmute(exports[0]) },
            build_pipelines: unsafe { std::mem::transmute(exports[1]) },
            init_from_desc: unsafe { std::mem::transmute(exports[2]) },
            get_swapchain_ptr: unsafe { std::mem::transmute(exports[3]) },
            tag_frame_resource: unsafe { std::mem::transmute(exports[4]) },
            tag_frame_constants: unsafe { std::mem::transmute(exports[5]) },
            set_enabled: unsafe { std::mem::transmute(exports[6]) },
            set_ui_composition_state: unsafe { std::mem::transmute(exports[7]) },
            set_latency_reduction: unsafe { std::mem::transmute(exports[8]) },
            set_present_id: unsafe { std::mem::transmute(exports[9]) },
            get_last_present_status: unsafe { std::mem::transmute(exports[10]) },
            destroy_context: unsafe { std::mem::transmute(exports[11]) },
            get_properties: unsafe { std::mem::transmute(exports[12]) },
            set_logging: unsafe { std::mem::transmute(exports[13]) },
        };
        Ok(loader)
    }
}

impl Drop for XefgLoader {
    fn drop(&mut self) {
        if !self.context.is_null() {
            let _ = unsafe { (self.destroy_context)(self.context) };
            self.context = std::ptr::null_mut();
        }
        unsafe { free_sdk_dll(self.handle) };
    }
}

fn row_major(matrix: &Mat4) -> [f32; 16] {
    // glam stores columns; the transpose's column-major array is the
    // row-major linearisation of the original matrix.
    matrix.transpose().to_cols_array()
}

/// XeSS frame generation pipeline state (Windows + DX12 only).
pub struct XessFrameGeneration {
    loader: XefgLoader,
    /// XeLL is owned here and destroyed *after* the XeSS-FG context.
    xell: Option<Xell>,
    #[allow(dead_code)]
    raw_device: d3d12::Device,
    queue: d3d12::CommandQueue,
    ring: NativeCommandRing,
    /// Proxy `IDXGISwapChain*` handed out by the XeSS-FG swap chain.
    proxy: *mut c_void,
    proxy_alive: bool,
    size: (u32, u32),
    render_extent: (u32, u32),
    compose: NativeTexture2D,
    ui: NativeTexture2D,
    depth: NativeTexture2D,
    mv: NativeTexture2D,
    mv_pass: FsrMotionVectorPass,
    prev_view_proj: Option<Mat4>,
    /// Incremental frame id (starts at 1; the SDK warns about 0).
    frame_id: u64,
    /// Number of frames presented through the proxy.
    frames: u64,
    /// `true` while the UI texture is parked in the compute/read state
    /// XeSS-FG expects during Present; the next `prepare` restores it.
    ui_needs_rt_restore: bool,
    /// `true` while depth/MV are parked in the compute/read state XeSS-FG
    /// reads them in during Present; the next `prepare` restores them
    /// before the motion-vector pass writes them again.
    depth_mv_parked: bool,
    /// `true` while the composition (the HUD-less color XeSS-FG interpolates)
    /// is parked in the compute/read state; the next `prepare` restores it so
    /// the world pass can render into it again.
    compose_parked: bool,
}

impl Drop for XessFrameGeneration {
    fn drop(&mut self) {
        self.ring.wait_idle();
        if self.proxy_alive {
            // Release every outstanding reference before destroying the
            // context, as the SDK instructs.
            unsafe {
                (*(self.proxy as *mut winapi::um::unknwnbase::IUnknown)).Release();
            }
            self.proxy_alive = false;
        }
        if !self.loader.context.is_null() {
            let context = self.loader.context;
            self.loader.context = std::ptr::null_mut();
            let _ = unsafe { (self.loader.destroy_context)(context) };
        }
        // XeSS-FG is gone; only now destroy XeLL.
        drop(self.xell.take());
    }
}

impl XessFrameGeneration {
    /// Create the frame generation pipeline for `hwnd` at `display_extent`.
    ///
    /// The caller must have destroyed the wgpu surface already: DX12 allows
    /// exactly one flip-model swapchain per hwnd, and dropping the surface is
    /// not enough on its own, so [`crate::fsr::swapchain::prepare`] proves
    /// that DXGI hands the window back before the SDK asks for its own.
    ///
    /// # Safety
    /// `hwnd` must be a valid window handle for this thread's process.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sdk_dir: &str,
        hwnd: *mut c_void,
        render_extent: (u32, u32),
        display_extent: (u32, u32),
        surface_format: wgpu::TextureFormat,
        ui_format: wgpu::TextureFormat,
    ) -> Result<XessFrameGeneration, String> {
        let _ = queue;
        let (raw_device, raw_queue) = raw_dx12(device)?;
        let device_ptr = raw_device.as_mut_ptr() as *mut c_void;
        let queue_ptr = raw_queue.as_mut_ptr() as *mut c_void;

        // XeLL first: XeSS-FG refuses to initialize without it.
        let xell = Xell::create(device_ptr, sdk_dir)?;

        // The flip-model backbuffer must be the plain UNORM format (sRGB is
        // not valid in flip model); the sRGB conversion happens in the
        // final composition path, matching the FSR frame generation.
        let backbuffer_format = if crate::fsr::is_bgra_format(surface_format) {
            DXGI_FORMAT_B8G8R8A8_UNORM
        } else {
            DXGI_FORMAT_R8G8B8A8_UNORM
        };

        // Hand the window over from wgpu's swapchain. DXGI allows one
        // flip-model swapchain per window and XeSS-FG is about to create its
        // own, so the surface must not only be dropped but its queued frames
        // retired: `prepare` drains the device, retries until DXGI actually
        // accepts a description for this window, and hands back the
        // description it accepted.
        let prepared = crate::fsr::swapchain::prepare(
            device,
            hwnd,
            display_extent.0,
            display_extent.1,
            backbuffer_format,
        )?;
        let mut desc1 = prepared.desc;
        let mut fullscreen = prepared.fullscreen;
        let handover = prepared.report;
        crate::xess::diag(&format!("[XeSS FG] window hand-over: {handover}"));

        let mut loader = XefgLoader::load(sdk_dir)?;

        let mut context: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { (loader.create_context)(device_ptr, &mut context) };
        if rc != 0 || context.is_null() {
            return Err(format!(
                "xefgSwapChainD3D12CreateContext failed ({} — an SM 6.4 capable GPU is required)",
                xefg_result_name(rc)
            ));
        }
        loader.context = context;
        let _ = unsafe {
            (loader.set_logging)(
                context,
                XEFG_LOGGING_LEVEL_INFO,
                xefg_log_callback,
                std::ptr::null_mut(),
            )
        };
        let init_flags = XEFG_INIT_FLAG_INVERTED_DEPTH;
        // UI composition mode. 3 = HUDLESS_UITEXTURE is what this build ships:
        // the proxy interpolates the tagged HUD-less colour and blends the UI
        // texture on top. `PW_XEFG_UI_MODE` lets a retest try another mode
        // without a rebuild — 5 = BACKBUFFER_HUDLESS_UITEXTURE (UI refinement
        // from the back buffer, the Unreal-Engine-recommended variant) or
        // 2 = BACKBUFFER_UITEXTURE (no HUD-less colour needed).
        let ui_mode = std::env::var("PW_XEFG_UI_MODE")
            .ok()
            .and_then(|value| value.trim().parse::<i32>().ok())
            .filter(|mode| (0..=5).contains(mode))
            .unwrap_or(XEFG_UI_MODE_HUDLESS_UITEXTURE);
        let init_params = XefgD3D12InitParams {
            application_swapchain: std::ptr::null_mut(), // desc-based: XeSS-FG creates the proxy
            init_flags,
            max_interpolated_frames: 1,
            creation_node_mask: 0,
            visible_node_mask: 0,
            temp_buffer_heap: std::ptr::null_mut(),
            buffer_heap_offset: 0,
            temp_texture_heap: std::ptr::null_mut(),
            texture_heap_offset: 0,
            pipeline_library: std::ptr::null_mut(),
            ui_mode,
        };
        let mut properties = XefgProperties {
            required_descriptor_count: 0,
            temp_buffer_heap_size: 0,
            temp_texture_heap_size: 0,
            constant_buffer_size: 0,
            max_supported_interpolations: 0,
        };
        let rc = unsafe {
            (loader.get_properties)(
                context,
                &init_params,
                display_extent.0,
                display_extent.1,
                backbuffer_format,
                &mut properties,
            )
        };
        if rc == 0 {
            crate::xess::diag(&format!(
                "[XeSS FG] maximum supported interpolations on this GPU: {}",
                properties.max_supported_interpolations
            ));
        }

        // Latency reduction is mandatory for XeSS-FG.
        let rc = unsafe { (loader.set_latency_reduction)(context, xell.raw_context()) };
        if rc != 0 {
            return Err(format!(
                "xefgSwapChainSetLatencyReduction failed ({}); XeLL is required for XeSS frame generation",
                xefg_result_name(rc)
            ));
        }

        // Pre-build pipelines so the init call does not stall.
        let rc = unsafe { (loader.build_pipelines)(context, std::ptr::null_mut(), 1, init_flags) };
        if rc != 0 {
            crate::xess::diag(&format!(
                "[XeSS FG] xefgSwapChainD3D12BuildPipelines returned {}; init will compile pipelines instead",
                xefg_result_name(rc)
            ));
        }

        let factory = prepared.factory;
        let rc = unsafe {
            (loader.init_from_desc)(
                context,
                hwnd,
                &desc1,
                &fullscreen as *const DXGI_SWAP_CHAIN_FULLSCREEN_DESC as *const c_void,
                queue_ptr,
                factory as *mut c_void,
                &init_params,
            )
        };
        unsafe {
            (*factory).Release();
        }
        if rc != 0 {
            return Err(format!(
                "xefgSwapChainD3D12InitFromSwapChainDesc failed ({}): DXGI does create a \
                 swapchain for this window with the same description ({handover}), so the \
                 failure is inside XeSS-FG's own swapchain setup",
                xefg_result_name(rc)
            ));
        }

        // The UI composition state is disabled by default; enable it so the
        // UI texture (HUDLESS_UITEXTURE mode) is composited onto real and
        // generated frames.
        let rc = unsafe {
            (loader.set_ui_composition_state)(context, XEFG_UI_COMPOSITION_STATE_ENABLED)
        };
        if rc != 0 {
            crate::xess::diag(&format!(
                "[XeSS FG] xefgSwapChainSetUiCompositionState(ENABLED) returned {}; UI may be missing from generated frames",
                xefg_result_name(rc)
            ));
        }
        // Frame generation ships disabled until explicitly enabled.
        let rc = unsafe { (loader.set_enabled)(context, 1) };
        if rc != 0 {
            return Err(format!(
                "xefgSwapChainSetEnabled(1) failed ({})",
                xefg_result_name(rc)
            ));
        }

        let mut proxy: *mut c_void = std::ptr::null_mut();
        let rc =
            unsafe { (loader.get_swapchain_ptr)(context, &IDXGISwapChain::uuidof(), &mut proxy) };
        if rc != 0 || proxy.is_null() {
            // If a pointer was handed back despite the error, release it
            // before the context is destroyed by the loader's Drop.
            if !proxy.is_null() {
                unsafe {
                    (*(proxy as *mut winapi::um::unknwnbase::IUnknown)).Release();
                }
            }
            return Err(format!(
                "xefgSwapChainD3D12GetSwapChainPtr failed ({})",
                xefg_result_name(rc)
            ));
        }

        // From here on every fallible step must unwind the XeSS-FG context
        // and the XeLL context before returning.
        let resources = (|| {
            let compose = unsafe {
                NativeTexture2D::create(
                    device,
                    &raw_device,
                    "xess frame generation composition",
                    surface_format,
                    dxgi_for_format(surface_format),
                    display_extent,
                    d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    D3D12_STATE_COMMON,
                    false,
                )?
            };
            let ui = unsafe {
                // egui's pipelines are compiled for the viewer's render
                // format (sRGB), so the UI surface must be created with
                // exactly that format or wgpu rejects the pass.
                NativeTexture2D::create(
                    device,
                    &raw_device,
                    "xess frame generation UI",
                    ui_format,
                    dxgi_for_format(ui_format),
                    display_extent,
                    d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    D3D12_STATE_COMMON,
                    false,
                )?
            };
            let depth = unsafe { Self::make_depth(device, &raw_device, render_extent)? };
            let mv = unsafe { Self::make_mv(device, &raw_device, render_extent)? };
            Ok::<_, String>((compose, ui, depth, mv))
        })();
        // Unwind path: release the proxy swapchain and destroy the XeSS-FG
        // context (XeLL drops with `xell`) so the caller can recreate the
        // wgpu surface on this hwnd.
        let unwind = |loader: &mut XefgLoader, proxy: *mut c_void| {
            unsafe {
                if !proxy.is_null() {
                    (*(proxy as *mut winapi::um::unknwnbase::IUnknown)).Release();
                }
            }
            if !loader.context.is_null() {
                let context = loader.context;
                loader.context = std::ptr::null_mut();
                let _ = unsafe { (loader.destroy_context)(context) };
            }
        };
        let (compose, ui, depth, mv) = match resources {
            Ok(parts) => parts,
            Err(error) => {
                unwind(&mut loader, proxy);
                return Err(error);
            }
        };
        let ring = match NativeCommandRing::new(&raw_device, "XeSS FG") {
            Ok(ring) => ring,
            Err(error) => {
                unwind(&mut loader, proxy);
                return Err(error);
            }
        };
        let mv_pass = FsrMotionVectorPass::new(device);

        crate::xess::diag(&format!(
            "[XeSS FG] frame generation ready (2x, depth-inverted, UI composition mode {} = {}) at {}x{}",
            ui_mode,
            xefg_ui_mode_name(ui_mode),
            display_extent.0,
            display_extent.1
        ));

        Ok(XessFrameGeneration {
            loader,
            xell: Some(xell),
            raw_device,
            queue: raw_queue,
            ring,
            proxy,
            proxy_alive: true,
            size: display_extent,
            render_extent,
            compose,
            ui,
            depth,
            mv,
            mv_pass,
            prev_view_proj: None,
            frame_id: 1,
            frames: 0,
            ui_needs_rt_restore: false,
            depth_mv_parked: false,
            compose_parked: false,
        })
    }

    unsafe fn make_depth(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        render_extent: (u32, u32),
    ) -> Result<NativeTexture2D, String> {
        NativeTexture2D::create(
            device,
            raw_device,
            "xess frame generation depth",
            wgpu::TextureFormat::R32Float,
            DXGI_FORMAT_R32_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            D3D12_STATE_COMMON,
            false,
        )
    }

    unsafe fn make_mv(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        render_extent: (u32, u32),
    ) -> Result<NativeTexture2D, String> {
        NativeTexture2D::create(
            device,
            raw_device,
            "xess frame generation motion vectors",
            wgpu::TextureFormat::Rg16Float,
            DXGI_FORMAT_R16G16_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            false,
        )
    }

    /// View of the composition target everything except the UI is drawn into.
    pub fn compose_view(&self) -> wgpu::TextureView {
        self.compose.texture.create_view(&Default::default())
    }

    /// View of the separate UI surface (composited by the proxy swapchain).
    pub fn ui_view(&self) -> wgpu::TextureView {
        self.ui.texture.create_view(&Default::default())
    }

    /// The size of the composition / UI targets.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// How many frames the pipeline has presented through the proxy. Shown
    /// in the on-screen mark.
    pub fn frames_presented(&self) -> u64 {
        self.frames
    }

    /// The size of the world render targets the pipeline consumes.
    pub fn render_extent(&self) -> (u32, u32) {
        self.render_extent
    }

    /// The frame id of the next frame to be presented (1-based).
    fn current_frame_id(&self) -> u32 {
        (self.frame_id & 0x7fff_ffff) as u32
    }

    /// Sleep until this frame may start and deliver the simulation
    /// markers. Call once per frame before any other XeSS-FG / XeLL work.
    pub fn begin_frame(&mut self) {
        let id = self.current_frame_id();
        if let Some(xell) = self.xell.as_mut() {
            xell.sleep(id);
            xell.marker(id, XELL_SIMULATION_START);
            xell.marker(id, XELL_SIMULATION_END);
        }
    }

    /// Raw depth copy + motion-vector pass + frame tagging.
    ///
    /// Runs after the world has been rendered (depth texture is ready) and
    /// before the UI pass. `view`/`projection` are the unjittered camera
    /// matrices for this frame; `unjittered_vp` is their product, used by
    /// the motion-vector pass.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        depth: &wgpu::Texture,
        view: &Mat4,
        projection: &Mat4,
        unjittered_vp: &Mat4,
        frame_time_ms: f32,
        reset: bool,
        jitter: (f32, f32),
    ) -> Result<(), String> {
        let id = self.current_frame_id();
        if let Some(xell) = self.xell.as_mut() {
            xell.marker(id, XELL_RENDERSUBMIT_START);
        }
        let (rw, rh) = self.render_extent;

        // This barrier helper is needed before the motion-vector pass.
        let barrier = |resource: &d3d12::Resource, before: u32, after: u32| {
            d3d12::ResourceBarrier::transition(
                resource.clone(),
                D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                before,
                after,
                D3D12_RESOURCE_BARRIER_FLAG_NONE,
            )
        };

        // --- Native list: un-park the resources the previous present
        // consumed (depth/MV/UI are in PIXEL_COMPUTE_READ after the last
        // finish()). Must run BEFORE the wgpu motion-vector pass writes
        // depth/MV as RENDER_TARGET next frame.
        if self.depth_mv_parked || self.ui_needs_rt_restore || self.compose_parked {
            let slot_index = self.ring.acquire("XeSS FG")?;
            let list = &mut self.ring.slot_mut(slot_index).list;
            let mut barriers = Vec::with_capacity(4);
            if self.depth_mv_parked {
                barriers.push(barrier(
                    &self.depth.resource,
                    D3D12_STATE_PIXEL_COMPUTE_READ,
                    D3D12_STATE_RENDER_TARGET,
                ));
                barriers.push(barrier(
                    &self.mv.resource,
                    D3D12_STATE_PIXEL_COMPUTE_READ,
                    D3D12_STATE_RENDER_TARGET,
                ));
            }
            if self.ui_needs_rt_restore {
                barriers.push(barrier(
                    &self.ui.resource,
                    D3D12_STATE_PIXEL_COMPUTE_READ,
                    D3D12_STATE_RENDER_TARGET,
                ));
            }
            if self.compose_parked {
                barriers.push(barrier(
                    &self.compose.resource,
                    D3D12_STATE_PIXEL_COMPUTE_READ,
                    D3D12_STATE_RENDER_TARGET,
                ));
            }
            list.resource_barrier(&barriers);
            self.ring.retire(slot_index, &self.queue, "XeSS FG")?;
            self.depth_mv_parked = false;
            self.ui_needs_rt_restore = false;
            self.compose_parked = false;
        }

        // --- wgpu side: raw depth copy + MV pass (same math as FSR).
        let inv_vp = unjittered_vp.inverse();
        let prev_vp = self.prev_view_proj.unwrap_or(*unjittered_vp);
        let params = FsrMvParams {
            inv_vp: inv_vp.to_cols_array_2d(),
            prev_vp: prev_vp.to_cols_array_2d(),
            render_size: [rw as f32, rh as f32],
            prev_valid: u32::from(self.prev_view_proj.is_some()),
            pad: 0,
        };
        queue.write_buffer(self.mv_pass.params_buffer(), 0, bytemuck::bytes_of(&params));
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor {
            aspect: wgpu::TextureAspect::All,
            ..Default::default()
        });
        let mv_view = self
            .mv
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let depth_out_view = self
            .depth
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("xess frame generation motion vector bind group"),
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
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("xess frame generation motion vector pass"),
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

        // --- Native list: move depth/MV into the compute/read state
        // XeSS-FG reads them in during Present. They stay parked until the
        // next prepare un-parks them.
        let slot_index = self.ring.acquire("XeSS FG")?;
        let list = &mut self.ring.slot_mut(slot_index).list;
        let barriers = [
            barrier(
                &self.depth.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
            barrier(
                &self.mv.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
        ];
        list.resource_barrier(&barriers);
        self.ring.retire(slot_index, &self.queue, "XeSS FG")?;
        self.depth_mv_parked = true;

        // --- Tag the frame constants and resources for this frame.
        let constants = XefgFrameConstantData {
            view_matrix: row_major(view),
            projection_matrix: row_major(projection),
            jitter_offset_x: jitter.0,
            jitter_offset_y: jitter.1,
            motion_vector_scale_x: 1.0,
            motion_vector_scale_y: 1.0,
            reset_history: u32::from(reset),
            frame_render_time: frame_time_ms.clamp(0.1, 1000.0),
        };
        let rc = unsafe { (self.loader.tag_frame_constants)(self.loader.context, id, &constants) };
        if rc != 0 {
            return Err(format!(
                "xefgSwapChainTagFrameConstants failed ({})",
                xefg_result_name(rc)
            ));
        }
        let tag_resource =
            |resource_type: u32, resource: *mut c_void, size: (u32, u32)| -> Result<(), String> {
                let data = XefgD3D12ResourceData {
                    resource_type,
                    validity: XEFG_SWAPCHAIN_RV_UNTIL_NEXT_PRESENT,
                    resource_base: Xefg2d { x: 0, y: 0 },
                    resource_size: Xefg2d {
                        x: size.0,
                        y: size.1,
                    },
                    resource,
                    incoming_state: D3D12_STATE_PIXEL_COMPUTE_READ,
                };
                let rc = unsafe {
                    (self.loader.tag_frame_resource)(
                        self.loader.context,
                        std::ptr::null_mut(),
                        id,
                        &data,
                    )
                };
                if rc != 0 {
                    return Err(format!(
                        "xefgSwapChainD3D12TagFrameResource(type {}) failed ({})",
                        resource_type,
                        xefg_result_name(rc)
                    ));
                }
                Ok(())
            };
        tag_resource(
            XEFG_SWAPCHAIN_RES_DEPTH,
            self.depth.resource.as_mut_ptr() as *mut c_void,
            (rw, rh),
        )?;
        tag_resource(
            XEFG_SWAPCHAIN_RES_MOTION_VECTOR,
            self.mv.resource.as_mut_ptr() as *mut c_void,
            (rw, rh),
        )?;
        self.prev_view_proj = Some(*unjittered_vp);
        Ok(())
    }

    /// Deliver the render-submit-end marker (call after the UI pass submits).
    pub fn render_submit_end(&mut self) {
        let id = self.current_frame_id();
        if let Some(xell) = self.xell.as_mut() {
            xell.marker(id, XELL_RENDERSUBMIT_END);
        }
    }

    /// Copy the composition into the proxy backbuffer, tag the HUD-less colour
    /// and the UI texture (both required by `HUDLESS_UITEXTURE` mode), set the
    /// present id and present through the proxy.
    ///
    /// `ui_visible` must reflect whether the UI was drawn into
    /// [`Self::ui_view`] this frame (the surface must still hold the UI or
    /// a fully transparent image, never uninitialized garbage).
    pub fn finish(&mut self, ui_visible: bool) -> Result<(), String> {
        let id = self.current_frame_id();
        let (width, height) = self.size;

        // --- Acquire the current backbuffer.
        let sc3 = self.proxy as *mut IDXGISwapChain3;
        let index = unsafe { (*sc3).GetCurrentBackBufferIndex() };
        let sc = self.proxy as *mut IDXGISwapChain;
        let mut raw_backbuffer: *mut d3d12_ty::ID3D12Resource = std::ptr::null_mut();
        let hr = unsafe {
            (*sc).GetBuffer(
                index,
                &d3d12_ty::ID3D12Resource::uuidof(),
                &mut raw_backbuffer as *mut *mut _ as *mut *mut c_void,
            )
        };
        if hr < 0 || raw_backbuffer.is_null() {
            return Err(format!("GetBuffer failed (HRESULT 0x{hr:08x})"));
        }
        let backbuffer: d3d12::Resource = unsafe { std::mem::transmute(raw_backbuffer) };

        // --- Native copy: composition -> backbuffer.
        let slot_index = self.ring.acquire("XeSS FG")?;
        let list = &mut self.ring.slot_mut(slot_index).list;
        let barrier = |resource: &d3d12::Resource, before: u32, after: u32| {
            d3d12::ResourceBarrier::transition(
                resource.clone(),
                D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                before,
                after,
                D3D12_RESOURCE_BARRIER_FLAG_NONE,
            )
        };
        let mut barriers = vec![
            barrier(
                &self.compose.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_COPY_SRC,
            ),
            barrier(&backbuffer, D3D12_STATE_PRESENT, D3D12_STATE_COPY_DEST),
        ];
        if ui_visible {
            barriers.push(barrier(
                &self.ui.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ));
        }
        list.resource_barrier(&barriers);
        // The d3d12 crate has no safe `CopyResource` wrapper, so call the
        // COM method on the underlying raw interface directly.
        unsafe {
            (*list.as_mut_ptr())
                .CopyResource(backbuffer.as_mut_ptr(), self.compose.resource.as_mut_ptr());
        }
        // The composition is the HUD-less colour XeSS-FG interpolates, so it
        // has to stay readable (PIXEL_COMPUTE_READ) when Present runs; the
        // next prepare() puts it back into RENDER_TARGET for the world pass.
        let restore = [
            barrier(
                &self.compose.resource,
                D3D12_STATE_COPY_SRC,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
            barrier(&backbuffer, D3D12_STATE_COPY_DEST, D3D12_STATE_PRESENT),
        ];
        list.resource_barrier(&restore);
        self.ring.retire(slot_index, &self.queue, "XeSS FG")?;
        self.compose_parked = true;
        if ui_visible {
            // XeSS-FG reads the UI surface during Present; it stays parked
            // in PIXEL_COMPUTE_READ until the next prepare restores it.
            self.ui_needs_rt_restore = true;
        }

        // --- Tag the HUD-less colour. `HUDLESS_UITEXTURE` mode has two
        // required inputs — the HUD-less colour and the UI texture — and the
        // proxy's back buffer only supplies the presentation target: without
        // this tag XeSS-FG has no colour to interpolate and composites no UI,
        // which reads on screen as "the whole HUD vanished".
        let hudless = XefgD3D12ResourceData {
            resource_type: XEFG_SWAPCHAIN_RES_HUDLESS_COLOR,
            validity: XEFG_SWAPCHAIN_RV_UNTIL_NEXT_PRESENT,
            resource_base: Xefg2d { x: 0, y: 0 },
            resource_size: Xefg2d {
                x: width,
                y: height,
            },
            resource: self.compose.resource.as_mut_ptr() as *mut c_void,
            incoming_state: D3D12_STATE_PIXEL_COMPUTE_READ,
        };
        let rc = unsafe {
            (self.loader.tag_frame_resource)(
                self.loader.context,
                std::ptr::null_mut(),
                id,
                &hudless,
            )
        };
        if rc != 0 {
            super::diag(&format!(
                "[XeSS FG] tagging the HUD-less colour failed ({}); UI composition will be missing",
                xefg_result_name(rc)
            ));
        }

        // --- Tag the UI texture so generated frames composite it sharp.
        if ui_visible {
            let data = XefgD3D12ResourceData {
                resource_type: XEFG_SWAPCHAIN_RES_UI,
                validity: XEFG_SWAPCHAIN_RV_UNTIL_NEXT_PRESENT,
                resource_base: Xefg2d { x: 0, y: 0 },
                resource_size: Xefg2d {
                    x: width,
                    y: height,
                },
                resource: self.ui.resource.as_mut_ptr() as *mut c_void,
                incoming_state: D3D12_STATE_PIXEL_COMPUTE_READ,
            };
            let rc = unsafe {
                (self.loader.tag_frame_resource)(
                    self.loader.context,
                    std::ptr::null_mut(),
                    id,
                    &data,
                )
            };
            if rc != 0 {
                super::diag(&format!(
                    "[XeSS FG] tagging the UI resource failed ({}); UI may be missing from generated frames",
                    xefg_result_name(rc)
                ));
            }
        }

        // --- Present id and markers.
        let rc = unsafe { (self.loader.set_present_id)(self.loader.context, id) };
        if rc != 0 {
            return Err(format!(
                "xefgSwapChainSetPresentId failed ({})",
                xefg_result_name(rc)
            ));
        }
        if let Some(xell) = self.xell.as_mut() {
            xell.marker(id, XELL_PRESENT_START);
        }
        let hr = unsafe { (*sc).Present(1, 0) };
        if hr < 0 {
            super::diag(&format!(
                "[XeSS FG] present through the frame generation swapchain failed (HRESULT 0x{hr:08x})"
            ));
        }
        if let Some(xell) = self.xell.as_mut() {
            xell.marker(id, XELL_PRESENT_END);
        }
        drop(backbuffer);

        // --- Present status (frame pacing feedback; log anomalies only).
        let mut status = XefgPresentStatus {
            frames_presented: 0,
            frame_gen_result: 0,
            is_frame_gen_enabled: 0,
        };
        let rc = unsafe { (self.loader.get_last_present_status)(self.loader.context, &mut status) };
        if rc == 0 && status.frame_gen_result != 0 {
            super::diag(&format!(
                "[XeSS FG] interpolation reported {} on the last present",
                xefg_result_name(status.frame_gen_result)
            ));
        }

        self.frame_id += 1;
        self.frames += 1;
        Ok(())
    }
}
