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
//!   proxy backbuffer, tags the HUD-less colour and the UI texture, sets the
//!   present id and calls `Present` on the proxy (which paces real and
//!   interpolated frames). Every input is tagged as
//!   `XEFG_SWAPCHAIN_RV_ONLY_NOW` on the app's own command list, so XeSS-FG
//!   copies it on the app's queue instead of reading the live texture during
//!   `Present` — see the note in [`tag_frame_resource_now`].
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
//! UI handling defaults to `XEFG_SWAPCHAIN_UI_MODE_BACKBUFFER_HUDLESS`
//! ("Interpolate hudless color, extract UI from backbuffer"): the caller draws
//! the world into [`XessFrameGeneration::compose_view`], `prepare` copies that
//! into [`XessFrameGeneration::ui_view`] and the UI pass draws the HUD on top of
//! the copy. The copy is what goes into the proxy's backbuffer and the
//! composition is tagged as `XEFG_SWAPCHAIN_RES_HUDLESS_COLOR`, so the model
//! interpolates a HUD-free scene and recomposites the extracted UI — HUD pixels
//! are never interpolated. `PW_XEFG_UI_COMPOSITION=0` falls back to
//! `XEFG_SWAPCHAIN_UI_MODE_NONE` (UI interpolated with the scene), and
//! `PW_XEFG_UI_MODE=3|5` selects the separate-UI-texture variants, where the UI
//! surface is the HUD alone (premultiplied alpha, because egui blends with
//! One/OneMinusSrcAlpha).
//!
//! How many frames are interpolated per real frame is the Graphics tab's
//! Frame multiplier: the request goes into `XefgD3D12InitParams`'
//! `maxInterpolatedFrames` and, for live changes, through
//! `xefgSwapChainSetNumInterpolatedFrames`. The SDK caps it per device
//! (`maxSupportedInterpolations` from `xefgSwapChainD3D12GetProperties`) and
//! the viewer logs what it actually handed over, so a clamp is visible in
//! `session.log` instead of being silently ignored.

use super::loader::{free_sdk_dll, load_sdk_dll};
use super::xell::{
    Xell, XELL_PRESENT_END, XELL_PRESENT_START, XELL_RENDERSUBMIT_END, XELL_RENDERSUBMIT_START,
    XELL_SIMULATION_END, XELL_SIMULATION_START,
};
use crate::fsr::{
    dxgi_for_format, raw_dx12, FsrMotionVectorPass, FsrMvParams, NativeCommandRing,
    NativeTexture2D, D3D12_STATE_COMMON, D3D12_STATE_COPY_DEST, D3D12_STATE_RENDER_TARGET,
};
use glam::Mat4;
use std::ffi::c_void;
use winapi::shared::dxgi::{
    IDXGIFactory, IDXGISwapChain, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
};
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
/// UI composition disabled: the model interpolates whatever the app presents,
/// UI included. Intel's first recommendation, and the default here.
const XEFG_UI_MODE_NONE: i32 = 1;
/// Separate UI texture + HUD-less colour; UI composited by the proxy.
/// `DXGI_MWA_NO_WINDOW_CHANGES` — winapi does not define the
/// `MakeWindowAssociation` flags, so they are spelled out here.
const DXGI_MWA_NO_WINDOW_CHANGES: u32 = 0x1;
/// `DXGI_MWA_NO_ALT_ENTER`.
const DXGI_MWA_NO_ALT_ENTER: u32 = 0x2;

const XEFG_UI_MODE_HUDLESS_UITEXTURE: i32 = 3;
/// `XEFG_SWAPCHAIN_UI_MODE_BACKBUFFER_HUDLESS`: interpolate the HUD-less colour,
/// extract the UI from the backbuffer.
const XEFG_UI_MODE_BACKBUFFER_HUDLESS: i32 = 4;
const XEFG_UI_COMPOSITION_STATE_ENABLED: u32 = 1;
/// Resource validity: XeSS-FG may defer (or skip) the copy entirely and read
/// the resource during `Present`. This viewer no longer uses it: that read can
/// happen on the swapchain's own present queue, which is not ordered against
/// the next frame's writes to the same texture, and on hardware that race
/// showed up as a black frame every other present. Kept because
/// `tools/verify-vendor-sdks.py` checks it against the vendored header.
#[allow(dead_code)]
const XEFG_SWAPCHAIN_RV_UNTIL_NEXT_PRESENT: u32 = 0;
/// Resource validity: XeSS-FG records the copy of the resource onto the
/// command list the app passes to `xefgSwapChainD3D12TagFrameResource`, so the
/// copy executes on the app's own direct queue right after the work that
/// produced it, and the texture may be reused as soon as that list retires.
const XEFG_SWAPCHAIN_RV_ONLY_NOW: u32 = 1;
const XEFG_SWAPCHAIN_RES_HUDLESS_COLOR: u32 = 0;
const XEFG_SWAPCHAIN_RES_DEPTH: u32 = 1;
const XEFG_SWAPCHAIN_RES_MOTION_VECTOR: u32 = 2;
const XEFG_SWAPCHAIN_RES_UI: u32 = 3;
const XEFG_LOGGING_LEVEL_INFO: i32 = 1;

/// Human-readable XeSS-FG result codes for diagnostics.
/// Tag one frame resource as `XEFG_SWAPCHAIN_RV_ONLY_NOW`.
///
/// XeSS-FG records the copy of that resource onto `list`, so it executes on the
/// app's own direct queue immediately after the work that produced the texture
/// — and the texture may be rewritten as soon as the list retires. Tagging with
/// `RV_UNTIL_NEXT_PRESENT` instead lets the SDK read the live texture during
/// `Present`, possibly from the swapchain's own queue, which is where a black
/// alternating frame came from on hardware.
///
/// # Safety
/// `list` must be the app's open command list on the queue that wrote
/// `resource`, and `incoming_state` the state `resource` is in at that point.
#[allow(clippy::too_many_arguments)]
unsafe fn tag_frame_resource_now(
    tag_frame_resource: PfnXefgD3D12TagFrameResource,
    context: *mut c_void,
    list: *mut c_void,
    frame_id: u32,
    resource_type: u32,
    resource: *mut c_void,
    size: (u32, u32),
    incoming_state: u32,
) -> Result<(), String> {
    let data = XefgD3D12ResourceData {
        resource_type,
        validity: XEFG_SWAPCHAIN_RV_ONLY_NOW,
        resource_base: Xefg2d { x: 0, y: 0 },
        resource_size: Xefg2d {
            x: size.0,
            y: size.1,
        },
        resource,
        incoming_state,
    };
    let rc = (tag_frame_resource)(context, list, frame_id, &data);
    if rc != 0 {
        return Err(format!(
            "xefgSwapChainD3D12TagFrameResource(type {}) failed ({})",
            resource_type,
            xefg_result_name(rc)
        ));
    }
    Ok(())
}

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
type PfnXefgSetNumInterpolatedFrames =
    unsafe extern "C" fn(context: *mut c_void, num_interpolated_frames: u32) -> i32;
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
    set_num_interpolated_frames: PfnXefgSetNumInterpolatedFrames,
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
            b"xefgSwapChainSetNumInterpolatedFrames\0",
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
            set_num_interpolated_frames: unsafe { std::mem::transmute(exports[10]) },
            get_last_present_status: unsafe { std::mem::transmute(exports[11]) },
            destroy_context: unsafe { std::mem::transmute(exports[12]) },
            get_properties: unsafe { std::mem::transmute(exports[13]) },
            set_logging: unsafe { std::mem::transmute(exports[14]) },
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
    /// `true` when the proxy composites the UI from the tagged UI texture and
    /// the HUD-less colour; `false` (the default) when the caller draws the UI
    /// into [`Self::compose_view`] and XeSS-FG interpolates it with the scene.
    ui_composed_by_proxy: bool,
    /// The `xefg_swapchain_ui_mode_t` the context was created with; decides
    /// whether the UI surface is the presented frame (2, 4) or the UI alone
    /// (3, 5).
    ui_mode: i32,
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
        interpolated_frames: u32,
    ) -> Result<XessFrameGeneration, String> {
        let _ = queue;
        let interpolated_frames = interpolated_frames.max(1);
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

        // The shared hand-over prefers `FRAME_LATENCY_WAITABLE_OBJECT` (FSR's
        // frame generator manages that object itself). XeSS-FG presents two
        // frames per app Present — the real one and the interpolated one — and
        // its own integrations create the proxy swapchain without the flag:
        // with the flag set the proxy is capped at one queued frame unless the
        // owner also calls `SetMaximumFrameLatency`, which leaves the generated
        // frame nowhere to go. That is what "a black frame every second
        // present" looks like, so drop it for this path.
        desc1.Flags &= !DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
        crate::xess::diag(&format!(
            "[XeSS FG] proxy swapchain: {}x{}, {} buffers, format {}, flags 0x{:x} (waitable object cleared)",
            desc1.Width, desc1.Height, desc1.BufferCount, desc1.Format, desc1.Flags
        ));

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
        // UI handling. `BACKBUFFER_HUDLESS` is the mode that keeps the HUD out
        // of the interpolated image without a second UI surface: the proxy is
        // told the HUD-less colour, the backbuffer it receives contains the HUD,
        // and it extracts the difference and recomposites that onto generated
        // frames ("Interpolate hudless color, extract UI from backbuffer",
        // `xefg_swapchain.h`). Interpolating the whole frame (`NONE`) smears the
        // HUD; `PW_XEFG_UI_COMPOSITION=0` restores it for comparison, and
        // `PW_XEFG_UI_MODE=3|5` selects the separate-UI-texture variants.
        let ui_in_composition = match std::env::var("PW_XEFG_UI_COMPOSITION") {
            Ok(value) => value.trim() != "0",
            Err(_) => true,
        };
        let ui_mode = if ui_in_composition {
            std::env::var("PW_XEFG_UI_MODE")
                .ok()
                .and_then(|value| value.trim().parse::<i32>().ok())
                .filter(|mode| (0..=5).contains(mode))
                .unwrap_or(XEFG_UI_MODE_BACKBUFFER_HUDLESS)
        } else {
            XEFG_UI_MODE_NONE
        };
        // PW_XEFG_ENABLED=0 keeps the proxy swapchain as the presenter but turns
        // frame generation off (`xefgSwapChainSetEnabled(0)`), which is a
        // passthrough that still copies the app's back buffers. It exists to
        // tell "the generated frames are broken" apart from "presenting through
        // the proxy is broken" in one run.
        let frame_generation_enabled = match std::env::var("PW_XEFG_ENABLED") {
            Ok(value) => value.trim() != "0",
            Err(_) => true,
        };
        let mut init_params = XefgD3D12InitParams {
            application_swapchain: std::ptr::null_mut(), // desc-based: XeSS-FG creates the proxy
            init_flags,
            max_interpolated_frames: interpolated_frames,
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
        // `maxInterpolatedFrames` must be between one and the device maximum
        // (or `XEFG_SWAPCHAIN_USE_MAX_SUPPORTED_INTERPOLATED_FRAMES`), and init
        // rejects anything above it, so the request is clamped here instead of
        // failing the whole frame generator on a x3/x4/x6 pick.
        if rc == 0 {
            let max = properties.max_supported_interpolations.max(1);
            crate::xess::diag(&format!(
                "[XeSS FG] maximum supported interpolations on this GPU: {max}"
            ));
            if init_params.max_interpolated_frames > max {
                crate::xess::diag(&format!(
                    "[XeSS FG] frame multiplier x{} becomes x{}: the device supports {max} interpolated frame(s)",
                    init_params.max_interpolated_frames + 1,
                    max + 1
                ));
                init_params.max_interpolated_frames = max;
            }
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
            // DXGI must not take the window over from the app now that the
            // proxy owns it. The association call lives on `IDXGIFactory`,
            // which `IDXGIFactory2` does not derive from (DXGI interfaces are
            // independent), hence the query.
            let mut association: *mut IDXGIFactory = std::ptr::null_mut();
            let hr = (*factory).QueryInterface(
                &IDXGIFactory::uuidof(),
                &mut association as *mut *mut _ as *mut *mut c_void,
            );
            if hr >= 0 && !association.is_null() {
                // winapi's `HWND` is a typed pointer, not `*mut c_void`.
                let window = hwnd as winapi::shared::windef::HWND;
                (*association).MakeWindowAssociation(
                    window,
                    DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES,
                );
                (*association).Release();
            }
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

        // Hand the clamped count to the live swapchain as well: `init` recorded
        // the maximum, and the setter is what the runtime actually generates
        // with (Intel recommends it for changes, and the value starts at the
        // initialised maximum, so this is normally a no-op that proves the
        // export is present and accepted).
        let interpolated_frames = init_params.max_interpolated_frames.max(1);
        unsafe {
            let rc = (loader.set_num_interpolated_frames)(context, interpolated_frames);
            if rc != 0 {
                crate::xess::diag(&format!(
                    "[XeSS FG] xefgSwapChainSetNumInterpolatedFrames({interpolated_frames}) returned {}; staying at the initialised count",
                    xefg_result_name(rc)
                ));
            } else {
                crate::xess::diag(&format!(
                    "[XeSS FG] frame multiplier x{}: {interpolated_frames} interpolated frame(s) per rendered frame",
                    interpolated_frames + 1
                ));
            }
        }

        // UI composition is off unless asked for: with it on, the proxy
        // composites the tagged UI texture over the HUD-less colour, which is
        // the only way to keep the UI perfectly sharp on generated frames but
        // also puts four SDK-visible inputs on the critical path.
        if ui_in_composition {
            let rc = unsafe {
                (loader.set_ui_composition_state)(context, XEFG_UI_COMPOSITION_STATE_ENABLED)
            };
            if rc != 0 {
                crate::xess::diag(&format!(
                    "[XeSS FG] xefgSwapChainSetUiCompositionState(ENABLED) returned {}; UI may be missing from generated frames",
                    xefg_result_name(rc)
                ));
            }
        }
        // Frame generation ships disabled until explicitly enabled.
        let rc = unsafe { (loader.set_enabled)(context, u32::from(frame_generation_enabled)) };
        if rc != 0 {
            return Err(format!(
                "xefgSwapChainSetEnabled({}) failed ({})",
                u32::from(frame_generation_enabled),
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
                    wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
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
                    wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        // In the backbuffer-extraction modes this surface
                        // starts as a copy of the composition (`prepare`).
                        | wgpu::TextureUsages::COPY_DST,
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
            "[XeSS FG] frame generation ready (x{}, depth-inverted, UI composition mode {} = {}, UI drawn {}, frame generation {}) at {}x{}",
            interpolated_frames + 1,
            ui_mode,
            xefg_ui_mode_name(ui_mode),
            if ui_in_composition {
                "into the tagged UI texture"
            } else {
                "into the composition (interpolated by the model)"
            },
            if frame_generation_enabled {
                "enabled"
            } else {
                "disabled by PW_XEFG_ENABLED=0 (proxy as passthrough presenter)"
            },
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
            ui_composed_by_proxy: ui_in_composition,
            ui_mode,
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

    /// View of the surface the UI is drawn into.
    ///
    /// In the default `BACKBUFFER_HUDLESS` mode it already holds a copy of the
    /// composition (see [`Self::ui_surface_prefilled`]) and becomes the
    /// presented frame; in the separate-UI-texture modes it starts transparent.
    /// When the proxy composites nothing ([`Self::ui_in_composition`] is `true`)
    /// the caller draws the UI into [`Self::compose_view`] instead.
    pub fn ui_view(&self) -> wgpu::TextureView {
        self.ui.texture.create_view(&Default::default())
    }

    /// `true` when the UI surface contains the scene, so the UI pass must
    /// `Load` it instead of clearing to transparent.
    ///
    /// Modes 2 and 4 keep the UI in the presented frame and let the proxy
    /// extract it; modes 3 and 5 take the UI on its own (premultiplied alpha,
    /// transparent where nothing was drawn).
    pub fn ui_surface_prefilled(&self) -> bool {
        self.ui_composed_by_proxy && matches!(self.ui_mode, 2 | 4)
    }

    /// `true` when the caller must draw the UI into [`Self::compose_view`]
    /// (the proxy composites nothing) rather than into [`Self::ui_view`].
    pub fn ui_in_composition(&self) -> bool {
        !self.ui_composed_by_proxy
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
        object_mv: &wgpu::TextureView,
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

        // --- wgpu side: raw depth copy + MV pass (same math as FSR).
        let inv_vp = unjittered_vp.inverse();
        let prev_vp = self.prev_view_proj.unwrap_or(*unjittered_vp);
        let params = FsrMvParams {
            inv_vp: inv_vp.to_cols_array_2d(),
            prev_vp: prev_vp.to_cols_array_2d(),
            render_size: [rw as f32, rh as f32],
            prev_valid: u32::from(self.prev_view_proj.is_some() && !reset),
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
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(object_mv),
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
        if self.ui_surface_prefilled() {
            // The presented frame is the composition with the UI on top: start
            // it as an exact copy; the caller draws the UI into `ui_view()`
            // (`Load`) and the proxy extracts the difference.
            encoder.copy_texture_to_texture(
                self.compose.texture.as_image_copy(),
                self.ui.texture.as_image_copy(),
                wgpu::Extent3d {
                    width: self.size.0,
                    height: self.size.1,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(Some(encoder.finish()));

        // --- Native list: hand this frame's depth and motion vectors to
        // XeSS-FG as `RV_ONLY_NOW` resources. The SDK records their copies on
        // this command list, so they run on the app's own direct queue right
        // after the motion-vector pass above instead of being read later from
        // the swapchain's present queue; the same list returns both textures to
        // RENDER_TARGET for the next frame.
        let slot_index = self.ring.acquire("XeSS FG")?;
        let list_ptr = {
            let list = &mut self.ring.slot_mut(slot_index).list;
            list.resource_barrier(&[
                barrier(
                    &self.depth.resource,
                    D3D12_STATE_RENDER_TARGET,
                    D3D12_STATE_COPY_SRC,
                ),
                barrier(
                    &self.mv.resource,
                    D3D12_STATE_RENDER_TARGET,
                    D3D12_STATE_COPY_SRC,
                ),
            ]);
            list.as_mut_ptr() as *mut c_void
        };

        // --- Frame constants and inputs for this frame id.
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
        unsafe {
            tag_frame_resource_now(
                self.loader.tag_frame_resource,
                self.loader.context,
                list_ptr,
                id,
                XEFG_SWAPCHAIN_RES_DEPTH,
                self.depth.resource.as_mut_ptr() as *mut c_void,
                (rw, rh),
                D3D12_STATE_COPY_SRC,
            )?;
            tag_frame_resource_now(
                self.loader.tag_frame_resource,
                self.loader.context,
                list_ptr,
                id,
                XEFG_SWAPCHAIN_RES_MOTION_VECTOR,
                self.mv.resource.as_mut_ptr() as *mut c_void,
                (rw, rh),
                D3D12_STATE_COPY_SRC,
            )?;
        }
        {
            let list = &mut self.ring.slot_mut(slot_index).list;
            list.resource_barrier(&[
                barrier(
                    &self.depth.resource,
                    D3D12_STATE_COPY_SRC,
                    D3D12_STATE_RENDER_TARGET,
                ),
                barrier(
                    &self.mv.resource,
                    D3D12_STATE_COPY_SRC,
                    D3D12_STATE_RENDER_TARGET,
                ),
            ]);
        }
        self.ring.retire(slot_index, &self.queue, "XeSS FG")?;
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

    /// Copy the composition into the proxy backbuffer, hand over the UI inputs
    /// when the proxy composites them, set the present id and present through
    /// the proxy.
    ///
    /// `ui_visible` must reflect whether the UI was drawn into
    /// [`Self::ui_view`] this frame (the surface must still hold the UI or
    /// a fully transparent image, never uninitialized garbage). It is ignored
    /// on the default path, where [`Self::ui_in_composition`] tells the caller
    /// to draw the UI into [`Self::compose_view`] instead.
    pub fn finish(&mut self, ui_visible: bool) -> Result<(), String> {
        let id = self.current_frame_id();
        // Read once: these are `&self` calls, and the command-list borrows below
        // are exclusive of the whole struct.
        let prefilled = self.ui_surface_prefilled();
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

        // --- Native list: composition -> backbuffer, and the two inputs
        // `HUDLESS_UITEXTURE` mode requires (the HUD-less colour and the UI
        // texture) handed to XeSS-FG as `RV_ONLY_NOW` resources. Both tags
        // happen on this command list, so the SDK records its copies here and
        // they execute on the app's own direct queue right after the
        // composition/UI writes. Tagging them as `RV_UNTIL_NEXT_PRESENT`
        // instead made the SDK read the live textures during Present — the
        // proxy's own queue is not ordered against the next frame's writes, and
        // on hardware that showed up as a black frame every other present.
        let barrier = |resource: &d3d12::Resource, before: u32, after: u32| {
            d3d12::ResourceBarrier::transition(
                resource.clone(),
                D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                before,
                after,
                D3D12_RESOURCE_BARRIER_FLAG_NONE,
            )
        };
        let slot_index = self.ring.acquire("XeSS FG")?;
        // What goes into the backbuffer: the presented frame (`ui`: composition
        // plus UI) in the modes that keep the UI in it, the composition itself
        // on the legacy `NONE` path.
        let present_resource: d3d12::Resource = if prefilled {
            self.ui.resource.clone()
        } else {
            self.compose.resource.clone()
        };
        let list_ptr = {
            let list = &mut self.ring.slot_mut(slot_index).list;
            let mut barriers = vec![
                barrier(
                    &present_resource,
                    D3D12_STATE_RENDER_TARGET,
                    D3D12_STATE_COPY_SRC,
                ),
                barrier(&backbuffer, D3D12_STATE_PRESENT, D3D12_STATE_COPY_DEST),
            ];
            let tag_ui = ui_visible && self.ui_composed_by_proxy && !prefilled;
            if tag_ui {
                barriers.push(barrier(
                    &self.ui.resource,
                    D3D12_STATE_RENDER_TARGET,
                    D3D12_STATE_COPY_SRC,
                ));
            }
            list.resource_barrier(&barriers);
            // The d3d12 crate has no safe `CopyResource` wrapper, so call the
            // COM method on the underlying raw interface directly.
            unsafe {
                (*list.as_mut_ptr())
                    .CopyResource(backbuffer.as_mut_ptr(), present_resource.as_mut_ptr());
            }
            list.as_mut_ptr() as *mut c_void
        };
        if self.ui_composed_by_proxy {
            // The HUD-less colour, always: every UI mode that is not `NONE`
            // interpolates this instead of the presented frame.
            unsafe {
                tag_frame_resource_now(
                    self.loader.tag_frame_resource,
                    self.loader.context,
                    list_ptr,
                    id,
                    XEFG_SWAPCHAIN_RES_HUDLESS_COLOR,
                    self.compose.resource.as_mut_ptr() as *mut c_void,
                    (width, height),
                    D3D12_STATE_COPY_SRC,
                )?;
            }
            // The UI on its own only for the separate-texture modes; the
            // backbuffer-extraction modes take the UI from the presented frame.
            if ui_visible && !prefilled {
                unsafe {
                    tag_frame_resource_now(
                        self.loader.tag_frame_resource,
                        self.loader.context,
                        list_ptr,
                        id,
                        XEFG_SWAPCHAIN_RES_UI,
                        self.ui.resource.as_mut_ptr() as *mut c_void,
                        (width, height),
                        D3D12_STATE_COPY_SRC,
                    )?;
                }
            }
        }
        // Otherwise the composition already *is* HUD-less colour + UI (the
        // caller drew both into it), so there is nothing to hand over: mode
        // `NONE` interpolates the presented frame as it is.
        {
            let list = &mut self.ring.slot_mut(slot_index).list;
            let mut restore = vec![
                barrier(
                    &self.compose.resource,
                    D3D12_STATE_COPY_SRC,
                    D3D12_STATE_RENDER_TARGET,
                ),
                barrier(
                    &present_resource,
                    D3D12_STATE_COPY_SRC,
                    D3D12_STATE_RENDER_TARGET,
                ),
                barrier(&backbuffer, D3D12_STATE_COPY_DEST, D3D12_STATE_PRESENT),
            ];
            if ui_visible && self.ui_composed_by_proxy && !prefilled {
                restore.push(barrier(
                    &self.ui.resource,
                    D3D12_STATE_COPY_SRC,
                    D3D12_STATE_RENDER_TARGET,
                ));
            }
            list.resource_barrier(&restore);
        }
        self.ring.retire(slot_index, &self.queue, "XeSS FG")?;

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
        // One-off visibility for retests: 2 frames per Present means the SDK is
        // really interpolating, 1 means it is passing our frame straight
        // through. A black-frame report is impossible to triage without this.
        if rc == 0 && self.frames < 3 {
            super::diag(&format!(
                "[XeSS FG] present id {}: {} frame(s) presented, generation {}, frame generation {}",
                id,
                status.frames_presented,
                xefg_result_name(status.frame_gen_result),
                if status.is_frame_gen_enabled != 0 {
                    "enabled"
                } else {
                    "disabled"
                }
            ));
        }

        self.frame_id += 1;
        self.frames += 1;
        Ok(())
    }
}
