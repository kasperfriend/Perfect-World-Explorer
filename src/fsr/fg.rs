//! AMD FSR Frame Generation (swapchain implementation), Windows + DX12.
//!
//! Frame generation cannot be layered on top of wgpu's swapchain the way the
//! upscaler layers on top of wgpu's command queue, because the interpolation
//! and the frame pacing happen *inside* a swapchain that implements
//! `IDXGISwapChain4`. This module therefore takes presentation over:
//!
//! * On enable, the wgpu surface is dropped (which destroys wgpu-hal's
//!   `IDXGISwapChain`), and the FFX `FrameGenerationSwapChainForHwnd` context
//!   is created. FFX creates its own `IDXGISwapChain4` proxy for the hwnd.
//! * Every frame the caller renders normally, ending with a linear image in
//!   [`FrameGeneration::compose_view`] (UNORM or sRGB to mirror what wgpu's
//!   swapchain format would be). The final step calls
//!   [`FrameGeneration::finish`], which copies the composition into the
//!   proxy backbuffer, registers the separate UI texture, configures the
//!   frame generation context, records the interpolation dispatch on the
//!   interpolation command list FFX hands back, and finally `Present`s
//!   through the proxy (which paces real and generated frames).
//! * On disable, the FFX contexts are destroyed and the proxy released,
//!   freeing the hwnd; the engine recreates the wgpu surface afterwards.
//!
//! The UI is separated from the render by default: the caller draws the world
//! into [`FrameGeneration::compose_view`], `prepare` copies that into
//! [`FrameGeneration::ui_view`], and the caller draws the HUD on top of the
//! copy. That copy is the presented frame; the composition goes to FFX as
//! `HUDLessColor`, so it interpolates a HUD-free scene and composites the
//! extracted UI onto generated frames (never interpolated, never blended by
//! FFX itself). See [`FrameGeneration::ui_in_composition`] for the legacy
//! in-composition path.
//!
//! Synchronisation follows the same pattern as the upscaler: everything
//! native runs on the one D3D12 direct queue wgpu-hal created, wgpu submits
//! first, then the native lists execute, then the proxy presents. The native
//! lists restore every tracked state so wgpu's tracker never desynchronises.

use super::imp::{
    choose_version_for_major, raw_dx12, FfxApiDimensions2D, FfxApiFloatCoords2D, FfxApiHeader,
    FfxApiResource, FfxCreateBackendDX12Desc, FfxLoader, FfxOverrideVersion,
    FfxQueryDescGetVersions, NativeTexture2D, FFX_API_RESOURCE_STATE_PIXEL_COMPUTE_READ,
    FFX_API_RESOURCE_STATE_RENDER_TARGET, FFX_API_SURFACE_FORMAT_R16G16_FLOAT,
    FFX_API_SURFACE_FORMAT_R32_FLOAT,
};
use super::mv::{FsrMotionVectorPass, FsrMvParams};
use glam::Mat4;
use std::ffi::c_void;
use winapi::shared::dxgi::IDXGISwapChain;
use winapi::shared::dxgi1_2::{DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FULLSCREEN_DESC};
use winapi::shared::dxgi1_4::IDXGISwapChain3;
use winapi::shared::dxgi1_5::IDXGISwapChain4;
use winapi::shared::dxgiformat::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
    DXGI_FORMAT_R16G16_FLOAT, DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
};
use winapi::um::d3d12 as d3d12_ty;
use winapi::Interface;

pub(crate) const FFX_FRAMEGENERATION_VERSION: u32 = (4 << 22) | (0 << 12) | 2;
pub(crate) const FFX_FRAMEGENERATION_SWAPCHAIN_DX12_VERSION: u32 = (4 << 22) | (0 << 12) | 0;
pub(crate) const FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATION: u64 = 0x00020001;
pub(crate) const FFX_API_CONFIGURE_DESC_TYPE_FRAMEGENERATION: u64 = 0x00020002;
pub(crate) const FFX_API_DISPATCH_DESC_TYPE_FRAMEGENERATION: u64 = 0x00020003;
pub(crate) const FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATION_VERSION: u64 = 0x0002000e;
pub(crate) const FFX_API_DISPATCH_DESC_TYPE_FRAMEGENERATION_PREPARE_V2: u64 = 0x0002000c;
pub(crate) const FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_FOR_HWND_DX12: u64 =
    0x00030006;
pub(crate) const FFX_API_CONFIGURE_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_REGISTERUIRESOURCE_DX12: u64 =
    0x00030002;
pub(crate) const FFX_API_QUERY_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_INTERPOLATIONCOMMANDLIST_DX12:
    u64 = 0x00030003;
pub(crate) const FFX_API_QUERY_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_INTERPOLATIONTEXTURE_DX12: u64 =
    0x00030004;
pub(crate) const FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_VERSION_DX12: u64 =
    0x0003000b;
pub(crate) const FFX_API_RESOURCE_STATE_PRESENT: u32 = 1 << 7;
const FFX_FRAMEGENERATION_ENABLE_DEPTH_INVERTED: u32 = 1 << 3;
const FFX_FRAMEGENERATION_UI_COMPOSITION_FLAG_USE_PREMUL_ALPHA: u32 = 1 << 0;
const FFX_FRAMEGENERATION_UI_COMPOSITION_FLAG_ENABLE_INTERNAL_UI_DOUBLE_BUFFERING: u32 = 1 << 1;
const FFX_API_SURFACE_FORMAT_B8G8R8A8_UNORM: u32 = 14;
const FFX_API_SURFACE_FORMAT_B8G8R8A8_SRGB: u32 = 15;
const FFX_API_SURFACE_FORMAT_R8G8B8A8_UNORM: u32 = 10;
const FFX_API_BACKBUFFER_TRANSFER_FUNCTION_SRGB: u32 = 0;

// States the native lists move resources through and back, mirroring the
// upscaler's bookkeeping so wgpu's tracker stays truthful.
const D3D12_STATE_COPY_SRC: u32 = d3d12_ty::D3D12_RESOURCE_STATE_COPY_SOURCE;
const D3D12_STATE_COPY_DEST: u32 = d3d12_ty::D3D12_RESOURCE_STATE_COPY_DEST;
const D3D12_STATE_PRESENT: u32 = d3d12_ty::D3D12_RESOURCE_STATE_PRESENT;
const D3D12_STATE_RENDER_TARGET: u32 = d3d12_ty::D3D12_RESOURCE_STATE_RENDER_TARGET;
const D3D12_STATE_PIXEL_COMPUTE_READ: u32 = d3d12_ty::D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
    | d3d12_ty::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
const D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES: u32 =
    d3d12_ty::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
const D3D12_RESOURCE_BARRIER_FLAG_NONE: u32 = d3d12_ty::D3D12_RESOURCE_BARRIER_FLAG_NONE;

/// FFX surface format matching a common wgpu color format.
fn ffx_color_for_format(format: wgpu::TextureFormat) -> u32 {
    let bgra = matches!(
        format,
        wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Bgra8Unorm
    );
    match (bgra, format.is_srgb()) {
        (true, true) => FFX_API_SURFACE_FORMAT_B8G8R8A8_SRGB,
        (true, false) => FFX_API_SURFACE_FORMAT_B8G8R8A8_UNORM,
        (false, true) => 12, // FFX_API_SURFACE_FORMAT_R8G8B8A8_SRGB
        (false, false) => FFX_API_SURFACE_FORMAT_R8G8B8A8_UNORM,
    }
}

/// DXGI format matching a common wgpu color format, preserving sRGB-ness so a
/// composition target matches the wgpu texture created from it. (The upscaler
/// *inputs* use the UNORM twins instead: every provider is told its colour is
/// perceptual, so the description must not make the hardware decode first.)
pub(crate) fn dxgi_for_format(format: wgpu::TextureFormat) -> DXGI_FORMAT {
    match format {
        wgpu::TextureFormat::Bgra8Unorm => DXGI_FORMAT_B8G8R8A8_UNORM,
        wgpu::TextureFormat::Bgra8UnormSrgb => DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        wgpu::TextureFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
        wgpu::TextureFormat::Rgba8UnormSrgb => DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        _ => DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
    }
}

#[repr(C)]
pub(crate) struct FfxApiRect2D {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

#[repr(C)]
struct FfxCreateContextDescFrameGeneration {
    header: FfxApiHeader,
    flags: u32,
    display_size: FfxApiDimensions2D,
    max_render_size: FfxApiDimensions2D,
    back_buffer_format: u32,
}

#[repr(C)]
struct FfxCreateContextDescFrameGenerationVersion {
    header: FfxApiHeader,
    version: u32,
}

#[repr(C)]
struct FfxCreateContextDescFrameGenerationSwapChainForHwndDX12 {
    header: FfxApiHeader,
    swapchain: *mut *mut c_void,
    hwnd: *mut c_void,
    desc: *mut DXGI_SWAP_CHAIN_DESC1,
    fullscreen_desc: *mut c_void,
    dxgi_factory: *mut c_void,
    game_queue: *mut c_void,
}

#[repr(C)]
struct FfxCreateContextDescFrameGenerationSwapChainVersionDX12 {
    header: FfxApiHeader,
    version: u32,
}

#[repr(C)]
struct FfxConfigureDescFrameGeneration {
    header: FfxApiHeader,
    swap_chain: *mut c_void,
    present_callback: *mut c_void,
    present_callback_user_context: *mut c_void,
    frame_generation_callback: *mut c_void,
    frame_generation_callback_user_context: *mut c_void,
    frame_generation_enabled: bool,
    allow_async_workloads: bool,
    hudless_color: FfxApiResource,
    flags: u32,
    only_present_generated: bool,
    generation_rect: FfxApiRect2D,
    frame_id: u64,
}

#[repr(C)]
struct FfxConfigureDescFrameGenerationSwapChainRegisterUiResourceDX12 {
    header: FfxApiHeader,
    ui_resource: FfxApiResource,
    flags: u32,
}

#[repr(C)]
struct FfxQueryDescFrameGenerationSwapChainInterpolationCommandListDX12 {
    header: FfxApiHeader,
    p_out_command_list: *mut *mut c_void,
}

#[repr(C)]
struct FfxQueryDescFrameGenerationSwapChainInterpolationTextureDX12 {
    header: FfxApiHeader,
    p_out_texture: *mut FfxApiResource,
}

#[repr(C)]
struct FfxDispatchDescFrameGenerationPrepareV2 {
    header: FfxApiHeader,
    frame_id: u64,
    flags: u32,
    command_list: *mut c_void,
    render_size: FfxApiDimensions2D,
    jitter_offset: FfxApiFloatCoords2D,
    motion_vector_scale: FfxApiFloatCoords2D,
    frame_time_delta: f32,
    reset: bool,
    camera_near: f32,
    camera_far: f32,
    camera_fov_angle_vertical: f32,
    view_space_to_meters_factor: f32,
    depth: FfxApiResource,
    motion_vectors: FfxApiResource,
    camera_position: [f32; 3],
    camera_up: [f32; 3],
    camera_right: [f32; 3],
    camera_forward: [f32; 3],
}

#[repr(C)]
struct FfxDispatchDescFrameGeneration {
    header: FfxApiHeader,
    command_list: *mut c_void,
    present_color: FfxApiResource,
    outputs: [FfxApiResource; 4],
    num_generated_frames: u32,
    reset: bool,
    backbuffer_transfer_function: u32,
    min_max_luminance: [f32; 2],
    generation_rect: FfxApiRect2D,
    frame_id: u64,
}

/// Frame generation pipeline state (Windows + DX12 only).
pub struct FrameGeneration {
    loader: FfxLoader,
    #[allow(dead_code)]
    raw_device: d3d12::Device,
    queue: d3d12::CommandQueue,
    ring: super::imp::NativeCommandRing,
    fg_context: *mut c_void,
    swapchain_context: *mut c_void,
    /// Proxy `IDXGISwapChain4*` handed out by the swapchain context.
    proxy: *mut c_void,
    /// `true` while the proxy is valid.
    proxy_alive: bool,
    size: (u32, u32),
    render_extent: (u32, u32),
    backbuffer_ffx_format: u32,
    /// FFX surface format describing the UI texture.
    ui_ffx_format: u32,
    /// Composition target: everything except the UI is drawn here, then it is
    /// copied into the proxy backbuffer at present time.
    compose: NativeTexture2D,
    /// The frame that is presented: a copy of `compose` with the UI drawn on
    /// top, handed to FFX as `presentColor` while `compose` is handed to it as
    /// `HUDLessColor` (see [`FrameGeneration::ui_in_composition`]).
    ui: NativeTexture2D,
    /// Native depth/MV copies at render extent (independent of the FSR
    /// upscaler, so frame generation works with any upscaler or none).
    depth: NativeTexture2D,
    mv: NativeTexture2D,
    mv_pass: FsrMotionVectorPass,
    prev_view_proj: Option<Mat4>,
    frame_index: u64,
    /// Draw the UI into the composition instead of a separate surface.
    ///
    /// Off by default: the UI is drawn into a copy of the composition and the
    /// composition itself goes to FFX as `HUDLessColor`, so the interpolator
    /// never sees HUD pixels and recomposites them from the presented frame.
    /// `PW_FSRFG_UI_COMPOSITION=1` restores the old in-composition path.
    ui_in_composition: bool,
    /// The FFX provider version the loader actually advertised and the create
    /// call accepted (major << 22 | minor << 12 | patch). `None` when the
    /// enumeration failed and the loader picked the provider itself.
    selected_version: Option<u64>,
}

impl Drop for FrameGeneration {
    fn drop(&mut self) {
        self.ring.wait_idle();
        if !self.fg_context.is_null() {
            // Disabling via configuration first flushes the swapchain's
            // pending work; destroying both contexts afterwards releases the
            // proxy's internal swapchain.
            let mut config = self.configure_desc(false);
            unsafe {
                (self.loader.configure)(&mut self.fg_context, &mut config.header);
            }
            unsafe {
                (self.loader.destroy_context)(&mut self.fg_context, std::ptr::null());
            }
            self.fg_context = std::ptr::null_mut();
        }
        if !self.swapchain_context.is_null() {
            unsafe {
                (self.loader.destroy_context)(&mut self.swapchain_context, std::ptr::null());
            }
            self.swapchain_context = std::ptr::null_mut();
        }
        if self.proxy_alive {
            unsafe {
                (*(self.proxy as *mut IDXGISwapChain4)).Release();
            }
            self.proxy_alive = false;
        }
    }
}

impl FrameGeneration {
    /// Create the frame generation pipeline for `hwnd` at `display_extent`.
    ///
    /// The caller must have destroyed the wgpu surface already: DX12 allows
    /// exactly one flip-model swapchain per hwnd, and dropping the surface is
    /// not enough on its own, so [`crate::fsr::swapchain::prepare`] proves
    /// that DXGI hands the window back before the SDK asks for its own.
    ///
    /// # Safety
    /// `hwnd` must be a valid window handle for this thread's process.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sdk_dir: &str,
        hwnd: *mut c_void,
        render_extent: (u32, u32),
        display_extent: (u32, u32),
        surface_format: wgpu::TextureFormat,
        ui_format: wgpu::TextureFormat,
    ) -> Result<FrameGeneration, String> {
        let _ = queue;
        let loader = FfxLoader::load(sdk_dir)?;
        let (raw_device, raw_queue) = raw_dx12(device)?;
        let device_ptr = raw_device.as_mut_ptr() as *mut c_void;
        let queue_ptr = raw_queue.as_mut_ptr() as *mut c_void;

        // The flip-model backbuffer must be the plain UNORM format (sRGB is
        // not a valid flip-model format; wgpu creates its own backbuffer
        // UNORM for the same reason, with the sRGB conversion done in
        // shaders). DXGI rejects an sRGB flip-model backbuffer, which FFX
        // reports as FFX_API_RETURN_ERROR_RUNTIME_ERROR (0x3) when creating
        // the proxy swapchain. The sRGB conversion happens in the final
        // composite (the caller uses the present-encode path).
        let backbuffer_format = if super::imp::is_bgra_format(surface_format) {
            DXGI_FORMAT_B8G8R8A8_UNORM
        } else {
            DXGI_FORMAT_R8G8B8A8_UNORM
        };
        let ffx_backbuffer_format = if super::imp::is_bgra_format(surface_format) {
            FFX_API_SURFACE_FORMAT_B8G8R8A8_UNORM
        } else {
            FFX_API_SURFACE_FORMAT_R8G8B8A8_UNORM
        };

        // Enumerate the frame generation providers the loader can reach and
        // prefer the newest one, mirroring the upscaler's provider selection.
        let mut count: u64 = 0;
        let mut probe = FfxQueryDescGetVersions {
            header: FfxApiHeader::new(super::imp::FFX_API_QUERY_DESC_TYPE_GET_VERSIONS),
            create_desc_type: FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATION,
            device: device_ptr,
            output_count: &mut count,
            version_ids: std::ptr::null_mut(),
            version_names: std::ptr::null_mut(),
        };
        let rc = unsafe { (loader.query)(std::ptr::null_mut(), &mut probe.header) };
        let mut chosen_version: Option<u64> = None;
        let mut fg_ids: Vec<u64> = Vec::new();
        if rc == super::imp::FFX_API_RETURN_OK && count > 0 {
            let capacity = (count as usize).min(1024);
            let mut ids = vec![0u64; capacity];
            count = capacity as u64;
            let mut probe = FfxQueryDescGetVersions {
                header: FfxApiHeader::new(super::imp::FFX_API_QUERY_DESC_TYPE_GET_VERSIONS),
                create_desc_type: FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATION,
                device: device_ptr,
                output_count: &mut count,
                version_ids: ids.as_mut_ptr(),
                version_names: std::ptr::null_mut(),
            };
            let rc = unsafe { (loader.query)(std::ptr::null_mut(), &mut probe.header) };
            if rc == super::imp::FFX_API_RETURN_OK {
                ids.truncate((count as usize).min(ids.len()));
                ids.retain(|&id| id != 0);
                chosen_version = choose_version_for_major(&ids, 0);
                fg_ids = ids;
                super::diag(&format!(
                    "[FSR FG] frame generation providers {:?}, selected 0x{:x}",
                    fg_ids
                        .iter()
                        .map(|i| format!("0x{i:x}"))
                        .collect::<Vec<_>>(),
                    chosen_version.unwrap_or(0)
                ));
            }
        } else {
            super::diag(&format!(
                "[FSR FG] provider enumeration returned 0x{rc:x}; the loader will pick"
            ));
        }

        // Contexts do not exist yet at this point; a ring failure only
        // leaks nothing beyond what Drops clean up on the way out.
        let ring = super::imp::NativeCommandRing::new(&raw_device, "FSR FG")?;

        // --- Create the proxy swapchain. FFX creates the new swapchain
        // internally, on a present queue of its own, from the factory and the
        // description we hand it. DXGI allows one flip-model swapchain per
        // window, so the window has to be free of wgpu's first: the hand-over
        // drains the device, probes the window with a real
        // CreateSwapChainForHwnd until DXGI accepts a description, and returns
        // that description plus a factory parented to our device's adapter.
        let (width, height) = display_extent;
        let prepared = super::swapchain::prepare(device, hwnd, width, height, backbuffer_format)?;
        let factory = prepared.factory;
        let mut desc1 = prepared.desc;
        let mut fullscreen = prepared.fullscreen;
        let handover = prepared.report;
        let mut proxy: *mut c_void = std::ptr::null_mut();
        let mut version = FfxCreateContextDescFrameGenerationSwapChainVersionDX12 {
            header: FfxApiHeader::new(
                FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_VERSION_DX12,
            ),
            version: FFX_FRAMEGENERATION_SWAPCHAIN_DX12_VERSION,
        };
        let mut swapchain_desc = FfxCreateContextDescFrameGenerationSwapChainForHwndDX12 {
            header: FfxApiHeader::new(
                FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_FOR_HWND_DX12,
            ),
            swapchain: &mut proxy,
            hwnd,
            desc: &mut desc1,
            // A real windowed fullscreen description, as the FSR sample passes
            // (the header also allows NULL, but the sample's choice is the one
            // that has been exercised).
            fullscreen_desc: &mut fullscreen as *mut DXGI_SWAP_CHAIN_FULLSCREEN_DESC as *mut c_void,
            dxgi_factory: factory as *mut c_void,
            game_queue: queue_ptr,
        };
        // --- Pre-flight sanity checks. The hand-over already proved that
        // DXGI will create a swapchain for this window with this description;
        // these checks cover the other two inputs FFX has to be able to use —
        // the game queue (it calls GetDevice on it and creates its own present
        // queue from that device) and the factory (it needs IDXGIFactory2).
        // Failures here do not abort creation; they are logged so a failure
        // below can be attributed to a component.
        let (qi_device_hr, qi_queue_hr) = unsafe {
            let mut probe_device: *mut winapi::um::d3d12::ID3D12Device = std::ptr::null_mut();
            let hr = (*(queue_ptr as *mut winapi::um::unknwnbase::IUnknown)).QueryInterface(
                &winapi::um::d3d12::ID3D12Device::uuidof(),
                &mut probe_device as *mut *mut winapi::um::d3d12::ID3D12Device as *mut *mut c_void,
            );
            if !probe_device.is_null() {
                (*probe_device).Release();
            }
            let mut probe_queue: *mut winapi::um::d3d12::ID3D12CommandQueue = std::ptr::null_mut();
            let hr_q = (*(queue_ptr as *mut winapi::um::unknwnbase::IUnknown)).QueryInterface(
                &winapi::um::d3d12::ID3D12CommandQueue::uuidof(),
                &mut probe_queue as *mut *mut winapi::um::d3d12::ID3D12CommandQueue
                    as *mut *mut c_void,
            );
            if !probe_queue.is_null() {
                (*probe_queue).Release();
            }
            (hr, hr_q)
        };
        super::diag(&format!(
            "[FSR FG] proxy swapchain: hwnd 0x{:x}, extent {}x{}, backbuffer_format 0x{:08x}, buffer_count {}, flags 0x{:x}, alpha {}, scaling {}, factory 0x{:x}, queue GetDevice hr 0x{:08x}, queue QI hr 0x{:08x}; hand-over: {handover}",
            hwnd as usize,
            width,
            height,
            backbuffer_format,
            desc1.BufferCount,
            desc1.Flags,
            desc1.AlphaMode,
            desc1.Scaling,
            factory as usize,
            qi_device_hr,
            qi_queue_hr
        ));
        swapchain_desc.header.p_next = &mut version.header as *mut FfxApiHeader;
        let mut swapchain_context: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            (loader.create_context)(
                &mut swapchain_context,
                &mut swapchain_desc.header as *mut FfxApiHeader,
                std::ptr::null(),
            )
        };
        unsafe {
            (*factory).Release();
        }
        if rc != super::imp::FFX_API_RETURN_OK || proxy.is_null() {
            return Err(format!(
                "creating the frame generation swapchain failed (FFX return code 0x{rc:x}): DXGI                  does create one for this window with the same description ({handover}), so the                  failure is inside FFX's own swapchain setup"
            ));
        }

        // --- Create the frame generation context itself. The SDK 2.x
        // providers require a ffxCreateContextDescFrameGenerationVersion in
        // the chain, and the DX12 backend descriptor must stay reachable
        // from it; retry every advertised FG provider newest-first before
        // giving up.
        let mut override_ids: Vec<u64> = Vec::new();
        if let Some(id) = chosen_version {
            override_ids.push(id);
        }
        for id in fg_ids.iter().copied() {
            if Some(id) != chosen_version {
                override_ids.push(id);
            }
        }
        let candidates: Vec<Option<u64>> = if override_ids.is_empty() {
            vec![None]
        } else {
            override_ids.into_iter().map(Some).collect()
        };
        let mut fg_context: *mut c_void = std::ptr::null_mut();
        let mut last_rc = super::imp::FFX_API_RETURN_OK;
        let mut fg_created = false;
        let mut created_override: Option<u64> = None;
        for candidate in &candidates {
            let mut backend = FfxCreateBackendDX12Desc {
                header: FfxApiHeader::new(
                    super::imp::FFX_API_CREATE_CONTEXT_DESC_TYPE_BACKEND_DX12,
                ),
                device: device_ptr,
            };
            backend.header.p_next = std::ptr::null_mut();
            let mut fg_version = FfxCreateContextDescFrameGenerationVersion {
                header: FfxApiHeader::new(FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATION_VERSION),
                version: FFX_FRAMEGENERATION_VERSION,
            };
            fg_version.header.p_next = &mut backend.header as *mut FfxApiHeader;
            let mut override_version = candidate.map(|id| {
                let mut ov = FfxOverrideVersion {
                    header: FfxApiHeader::new(super::imp::FFX_API_DESC_TYPE_OVERRIDE_VERSION),
                    version_id: id,
                };
                ov.header.p_next = &mut fg_version.header as *mut FfxApiHeader;
                ov
            });
            let mut fg_desc = FfxCreateContextDescFrameGeneration {
                header: FfxApiHeader::new(FFX_API_CREATE_CONTEXT_DESC_TYPE_FRAMEGENERATION),
                flags: FFX_FRAMEGENERATION_ENABLE_DEPTH_INVERTED,
                display_size: FfxApiDimensions2D { width, height },
                max_render_size: FfxApiDimensions2D { width, height },
                back_buffer_format: ffx_backbuffer_format,
            };
            fg_desc.header.p_next = match override_version.as_mut() {
                Some(ov) => &mut ov.header as *mut FfxApiHeader,
                None => &mut fg_version.header as *mut FfxApiHeader,
            };
            let rc = unsafe {
                (loader.create_context)(
                    &mut fg_context,
                    &mut fg_desc.header as *mut FfxApiHeader,
                    std::ptr::null(),
                )
            };
            if rc == super::imp::FFX_API_RETURN_OK && !fg_context.is_null() {
                super::diag(&format!(
                    "[FSR FG] frame generation context created{}",
                    candidate
                        .map(|id| format!(" with override 0x{id:x}"))
                        .unwrap_or_default()
                ));
                fg_created = true;
                created_override = *candidate;
                break;
            }
            super::diag(&format!(
                "[FSR FG] ffxCreateContext with override {}failed (0x{rc:x}); trying the next provider",
                candidate
                    .map(|id| format!("0x{id:x} "))
                    .unwrap_or_default()
            ));
            last_rc = rc;
            fg_context = std::ptr::null_mut();
        }
        if !fg_created {
            // Unwind the swapchain context we created before returning.
            let mut sc_ctx = swapchain_context;
            unsafe {
                (loader.destroy_context)(&mut sc_ctx, std::ptr::null());
                (*(proxy as *mut IDXGISwapChain4)).Release();
            }
            return Err(format!(
                "creating the frame generation context failed (0x{last_rc:x})"
            ));
        }

        // From here on every fallible step must unwind the FFX contexts and
        // the proxy swapchain before returning, or we leak the hwnd's only
        // swapchain slot and the caller cannot recover.
        let resources = (|| {
            let compose =
                unsafe { Self::make_compose(device, &raw_device, display_extent, surface_format)? };
            let ui = unsafe { Self::make_ui(device, &raw_device, display_extent, ui_format)? };
            let depth = unsafe { Self::make_depth(device, &raw_device, render_extent)? };
            let mv = unsafe { Self::make_mv(device, &raw_device, render_extent)? };
            Ok::<_, String>((compose, ui, depth, mv))
        })();
        let (compose, ui, depth, mv) = match resources {
            Ok(parts) => parts,
            Err(error) => {
                let mut fg = fg_context;
                let mut sc = swapchain_context;
                unsafe {
                    let mut disable = FfxConfigureDescFrameGeneration {
                        header: FfxApiHeader::new(FFX_API_CONFIGURE_DESC_TYPE_FRAMEGENERATION),
                        swap_chain: proxy,
                        present_callback: std::ptr::null_mut(),
                        present_callback_user_context: std::ptr::null_mut(),
                        frame_generation_callback: std::ptr::null_mut(),
                        frame_generation_callback_user_context: std::ptr::null_mut(),
                        frame_generation_enabled: false,
                        allow_async_workloads: false,
                        hudless_color: FfxApiResource::EMPTY,
                        flags: 0,
                        only_present_generated: false,
                        generation_rect: FfxApiRect2D {
                            left: 0,
                            top: 0,
                            width: width as i32,
                            height: height as i32,
                        },
                        frame_id: 0,
                    };
                    (loader.configure)(&mut fg, &mut disable.header);
                    (loader.destroy_context)(&mut fg, std::ptr::null());
                    (loader.destroy_context)(&mut sc, std::ptr::null());
                    (*(proxy as *mut IDXGISwapChain4)).Release();
                }
                return Err(error);
            }
        };
        let mv_pass = FsrMotionVectorPass::new(device);

        Ok(FrameGeneration {
            loader,
            raw_device,
            queue: raw_queue,
            ring,
            fg_context,
            swapchain_context,
            proxy,
            proxy_alive: true,
            size: display_extent,
            render_extent,
            backbuffer_ffx_format: ffx_backbuffer_format,
            ui_ffx_format: ffx_color_for_format(ui_format),
            compose,
            ui,
            depth,
            mv,
            mv_pass,
            prev_view_proj: None,
            frame_index: 0,
            ui_in_composition: std::env::var("PW_FSRFG_UI_COMPOSITION")
                .map(|value| value.trim() == "1")
                .unwrap_or(false),
            selected_version: created_override,
        })
    }

    unsafe fn make_compose(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Result<NativeTexture2D, String> {
        // UNORM surface: composition happens in the intermediate exactly as
        // the present-encode path does today; sRGB surface: composition is
        // direct into this sRGB target, mirroring the sRGB swapchain today.
        let target_format = format;
        let dxgi = dxgi_for_format(target_format);
        NativeTexture2D::create(
            device,
            raw_device,
            "frame generation composition",
            target_format,
            dxgi,
            size,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            super::imp::D3D12_STATE_COMMON,
            false,
        )
    }

    unsafe fn make_ui(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Result<NativeTexture2D, String> {
        // egui's pipelines are compiled for the viewer's render format
        // (sRGB), so the UI surface must be created with exactly that
        // format or wgpu rejects the pass.
        NativeTexture2D::create(
            device,
            raw_device,
            "frame generation UI",
            format,
            dxgi_for_format(format),
            size,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                // The presented frame starts as a copy of the composition
                // (`prepare`), then the UI is drawn on top of it.
                | wgpu::TextureUsages::COPY_DST,
            super::imp::D3D12_STATE_COMMON,
            false,
        )
    }

    unsafe fn make_depth(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        render_extent: (u32, u32),
    ) -> Result<NativeTexture2D, String> {
        NativeTexture2D::create(
            device,
            raw_device,
            "frame generation depth",
            wgpu::TextureFormat::R32Float,
            DXGI_FORMAT_R32_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            super::imp::D3D12_STATE_COMMON,
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
            "frame generation motion vectors",
            wgpu::TextureFormat::Rg16Float,
            DXGI_FORMAT_R16G16_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            super::imp::D3D12_STATE_COMMON,
            false,
        )
    }

    /// View of the composition target everything except the UI is drawn into.
    pub fn compose_view(&self) -> wgpu::TextureView {
        self.compose.texture.create_view(&Default::default())
    }

    /// View of the surface the UI is drawn into.
    ///
    /// It already holds an exact copy of the composition when this is called
    /// (see [`Self::ui_surface_prefilled`]), so the UI pass must `Load`, not
    /// `Clear`: the result is the frame that gets presented.
    pub fn ui_view(&self) -> wgpu::TextureView {
        self.ui.texture.create_view(&Default::default())
    }

    /// `true` when the UI surface already contains the scene, i.e. the UI pass
    /// composites on top of it instead of starting from transparency.
    pub fn ui_surface_prefilled(&self) -> bool {
        !self.ui_in_composition
    }

    /// `true` when the UI is part of the composition instead of a separate
    /// surface the interpolator is told about.
    ///
    /// Off by default. The UI is drawn into a copy of the composition, that
    /// copy becomes the presented frame, and the composition itself is handed
    /// to FFX as `HUDLessColor`. FFX extracts the UI from the difference
    /// between the two, interpolates the HUD-less scene and composites the
    /// extracted UI onto generated frames — so HUD pixels are never
    /// interpolated and never pass through FFX's own blend (which is what
    /// washed them out when the UI was registered as a separate resource).
    ///
    /// `PW_FSRFG_UI_COMPOSITION=1` restores the old path, where the UI is drawn
    /// into the composition itself and the interpolator sees it as scene
    /// content.
    pub fn ui_in_composition(&self) -> bool {
        self.ui_in_composition
    }

    /// The size of the composition / UI targets.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// How many frames the pipeline has finished (presented real frames
    /// plus interpolated ones). Shown in the on-screen mark.
    pub fn frames_presented(&self) -> u64 {
        self.frame_index
    }

    /// The FFX provider version actually driving frame generation, when
    /// enumeration succeeded. Decoded for display with
    /// `(id >> 22) & 0x3ff` (major), `(id >> 12) & 0xff` (minor) and
    /// `id & 0xfff` (patch).
    pub fn selected_provider(&self) -> Option<u64> {
        self.selected_version
    }

    /// The size of the world render targets the pipeline consumes.
    pub fn render_extent(&self) -> (u32, u32) {
        self.render_extent
    }

    fn configure_desc(&self, enabled: bool) -> FfxConfigureDescFrameGeneration {
        FfxConfigureDescFrameGeneration {
            header: FfxApiHeader::new(FFX_API_CONFIGURE_DESC_TYPE_FRAMEGENERATION),
            swap_chain: self.proxy,
            present_callback: std::ptr::null_mut(),
            present_callback_user_context: std::ptr::null_mut(),
            frame_generation_callback: std::ptr::null_mut(),
            frame_generation_callback_user_context: std::ptr::null_mut(),
            frame_generation_enabled: enabled,
            allow_async_workloads: false,
            // The scene without the UI. FFX extracts the UI as the difference
            // between it and `presentColor` (the presented frame, which does
            // contain it) and composites that extraction onto generated
            // frames at native resolution, so HUD pixels are never
            // interpolated and never pass through FFX's own blending.
            hudless_color: if enabled && !self.ui_in_composition {
                FfxApiResource::texture2d(
                    &self.compose.resource,
                    self.ui_ffx_format,
                    self.size.0,
                    self.size.1,
                    super::imp::FFX_API_RESOURCE_USAGE_READ_ONLY,
                    FFX_API_RESOURCE_STATE_RENDER_TARGET,
                )
            } else {
                FfxApiResource::EMPTY
            },
            flags: 0,
            only_present_generated: false,
            generation_rect: FfxApiRect2D {
                left: 0,
                top: 0,
                width: self.size.0 as i32,
                height: self.size.1 as i32,
            },
            frame_id: self.frame_index,
        }
    }

    /// Motion-vector pass + frame generation prepare dispatch.
    ///
    /// Runs after the world has been rendered (depth texture is ready) and
    /// before [`FrameGeneration::finish`]. `depth` is the world's reversed-Z
    /// depth texture at render extent.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        depth: &wgpu::Texture,
        object_mv: &wgpu::TextureView,
        unjittered_vp: &Mat4,
        frame_time_ms: f32,
        reset: bool,
        jitter: (f32, f32),
        lens: crate::fsr::CameraLens,
        camera_position: [f32; 3],
        camera_up: [f32; 3],
        camera_right: [f32; 3],
        camera_forward: [f32; 3],
    ) -> Result<(), String> {
        let (rw, rh) = self.render_extent;

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
            label: Some("framegen motion vector bind group"),
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
                label: Some("framegen motion vector pass"),
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
        if !self.ui_in_composition {
            // The presented frame is the composition with the UI on top: start
            // it as an exact copy, then the caller draws the UI into
            // `ui_view()` (`Load`, not `Clear`). FFX gets both images and
            // extracts the UI from the difference.
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

        // --- Native FFX prepare on the same queue, right after wgpu's submit.
        let slot_index = self.ring.acquire("FSR FG")?;
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
        let mut barriers = Vec::with_capacity(3);
        barriers.push(barrier(
            &self.depth.resource,
            D3D12_STATE_RENDER_TARGET,
            D3D12_STATE_PIXEL_COMPUTE_READ,
        ));
        barriers.push(barrier(
            &self.mv.resource,
            D3D12_STATE_RENDER_TARGET,
            D3D12_STATE_PIXEL_COMPUTE_READ,
        ));
        list.resource_barrier(&barriers);
        let mut prepare = FfxDispatchDescFrameGenerationPrepareV2 {
            header: FfxApiHeader::new(FFX_API_DISPATCH_DESC_TYPE_FRAMEGENERATION_PREPARE_V2),
            frame_id: self.frame_index,
            flags: 0,
            command_list: list.as_mut_ptr() as *mut c_void,
            render_size: FfxApiDimensions2D {
                width: rw,
                height: rh,
            },
            jitter_offset: FfxApiFloatCoords2D {
                x: jitter.0,
                y: jitter.1,
            },
            motion_vector_scale: FfxApiFloatCoords2D { x: 1.0, y: 1.0 },
            frame_time_delta: frame_time_ms.clamp(0.1, 1000.0),
            reset,
            // The camera the matrices were built from, not a literal.
            camera_near: lens.near,
            camera_far: lens.far,
            camera_fov_angle_vertical: lens.fov_y_radians,
            view_space_to_meters_factor: 1.0,
            depth: FfxApiResource::texture2d(
                &self.depth.resource,
                FFX_API_SURFACE_FORMAT_R32_FLOAT,
                rw,
                rh,
                super::imp::FFX_API_RESOURCE_USAGE_DEPTHTARGET,
                FFX_API_RESOURCE_STATE_PIXEL_COMPUTE_READ,
            ),
            motion_vectors: FfxApiResource::texture2d(
                &self.mv.resource,
                FFX_API_SURFACE_FORMAT_R16G16_FLOAT,
                rw,
                rh,
                super::imp::FFX_API_RESOURCE_USAGE_READ_ONLY,
                FFX_API_RESOURCE_STATE_PIXEL_COMPUTE_READ,
            ),
            camera_position,
            camera_up,
            camera_right,
            camera_forward,
        };
        let rc = unsafe { (self.loader.dispatch)(&mut self.fg_context, &mut prepare.header) };
        let restore = [
            barrier(
                &self.depth.resource,
                D3D12_STATE_PIXEL_COMPUTE_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(
                &self.mv.resource,
                D3D12_STATE_PIXEL_COMPUTE_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
        ];
        list.resource_barrier(&restore);
        self.ring.retire(slot_index, &self.queue, "FSR FG")?;
        if rc != super::imp::FFX_API_RETURN_OK {
            return Err(format!(
                "ffxDispatch frame generation prepare failed (0x{rc:x})"
            ));
        }
        self.prev_view_proj = Some(*unjittered_vp);
        Ok(())
    }

    /// Copy the presented frame into the proxy backbuffer, hand the UI and
    /// HUD-less inputs to FFX, configure frame generation, dispatch the
    /// interpolation workload, and present through the proxy swapchain.
    ///
    /// `ui_visible` must reflect whether the UI was drawn into [`Self::ui_view`]
    /// this frame. It only matters on the legacy in-composition path, where the
    /// separate-surface registration is skipped entirely.
    pub fn finish(&mut self, ui_visible: bool, reset: bool) -> Result<(), String> {
        // Only the legacy in-composition path registers a UI resource for the
        // swapchain to blend; the default path hands FFX `HUDLessColor` and lets
        // it extract the UI from the presented frame instead.
        let ui_visible = ui_visible && self.ui_in_composition;
        // With the UI on its own surface the presented frame (`ui`: composition
        // plus UI, see `prepare`) is what goes to the backbuffer, and FFX gets
        // the composition as `HUDLessColor` — no UI resource is registered at
        // all, because that would let FFX blend the UI a second time.
        let present_resource: d3d12::Resource = if self.ui_in_composition {
            self.compose.resource.clone()
        } else {
            self.ui.resource.clone()
        };
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
        let slot_index = self.ring.acquire("FSR FG")?;
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
                &present_resource,
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
                .CopyResource(backbuffer.as_mut_ptr(), present_resource.as_mut_ptr());
        }
        let restore = [
            barrier(
                &present_resource,
                D3D12_STATE_COPY_SRC,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(&backbuffer, D3D12_STATE_COPY_DEST, D3D12_STATE_PRESENT),
        ];
        list.resource_barrier(&restore);
        self.ring.retire(slot_index, &self.queue, "FSR FG")?;

        // --- Legacy path only: register the UI so the swapchain blends it.
        let ui_resource = if ui_visible {
            FfxApiResource::texture2d(
                &self.ui.resource,
                self.ui_ffx_format,
                width,
                height,
                super::imp::FFX_API_RESOURCE_USAGE_READ_ONLY,
                FFX_API_RESOURCE_STATE_PIXEL_COMPUTE_READ,
            )
        } else {
            FfxApiResource::EMPTY
        };
        let mut register = FfxConfigureDescFrameGenerationSwapChainRegisterUiResourceDX12 {
            header: FfxApiHeader::new(
                FFX_API_CONFIGURE_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_REGISTERUIRESOURCE_DX12,
            ),
            ui_resource,
            // egui renders premultiplied-alpha content (egui-wgpu blends
            // with One/OneMinusSrcAlpha), so tell FFX the UI is already
            // premultiplied; registering it as straight alpha makes the
            // swapchain multiply by alpha a second time and turns the HUD
            // almost black.
            flags: if ui_visible {
                FFX_FRAMEGENERATION_UI_COMPOSITION_FLAG_USE_PREMUL_ALPHA
                    | FFX_FRAMEGENERATION_UI_COMPOSITION_FLAG_ENABLE_INTERNAL_UI_DOUBLE_BUFFERING
            } else {
                0
            },
        };
        if self.ui_in_composition {
            let rc = unsafe {
                (self.loader.configure)(&mut self.swapchain_context, &mut register.header)
            };
            if rc != super::imp::FFX_API_RETURN_OK {
                super::diag(&format!("[FSR FG] registering the UI resource failed (0x{rc:x}); UI may be missing from generated frames"));
            }
        }

        // --- Configure frame generation for this frame.
        let mut config = self.configure_desc(true);
        let rc = unsafe { (self.loader.configure)(&mut self.fg_context, &mut config.header) };
        if rc != super::imp::FFX_API_RETURN_OK {
            return Err(format!("ffxConfigure frame generation failed (0x{rc:x})"));
        }

        // --- Record the interpolation dispatch on the command list FFX hands
        // back for the interpolation texture.
        let mut interpolation_command_list: *mut c_void = std::ptr::null_mut();
        let mut query = FfxQueryDescFrameGenerationSwapChainInterpolationCommandListDX12 {
            header: FfxApiHeader::new(
                FFX_API_QUERY_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_INTERPOLATIONCOMMANDLIST_DX12,
            ),
            p_out_command_list: &mut interpolation_command_list,
        };
        let rc = unsafe { (self.loader.query)(&mut self.swapchain_context, &mut query.header) };
        if rc != super::imp::FFX_API_RETURN_OK || interpolation_command_list.is_null() {
            return Err(format!(
                "querying the interpolation command list failed (0x{rc:x})"
            ));
        }
        let mut interpolation_texture = FfxApiResource::EMPTY;
        let mut query = FfxQueryDescFrameGenerationSwapChainInterpolationTextureDX12 {
            header: FfxApiHeader::new(
                FFX_API_QUERY_DESC_TYPE_FRAMEGENERATIONSWAPCHAIN_INTERPOLATIONTEXTURE_DX12,
            ),
            p_out_texture: &mut interpolation_texture,
        };
        let rc = unsafe { (self.loader.query)(&mut self.swapchain_context, &mut query.header) };
        if rc != super::imp::FFX_API_RETURN_OK {
            return Err(format!(
                "querying the interpolation texture failed (0x{rc:x})"
            ));
        }
        let present_color = FfxApiResource::texture2d(
            &backbuffer,
            self.backbuffer_ffx_format,
            width,
            height,
            super::imp::FFX_API_RESOURCE_USAGE_READ_ONLY,
            FFX_API_RESOURCE_STATE_PRESENT,
        );
        let mut dispatch = FfxDispatchDescFrameGeneration {
            header: FfxApiHeader::new(FFX_API_DISPATCH_DESC_TYPE_FRAMEGENERATION),
            command_list: interpolation_command_list,
            present_color,
            outputs: [
                interpolation_texture,
                FfxApiResource::EMPTY,
                FfxApiResource::EMPTY,
                FfxApiResource::EMPTY,
            ],
            num_generated_frames: 1,
            reset,
            backbuffer_transfer_function: FFX_API_BACKBUFFER_TRANSFER_FUNCTION_SRGB,
            min_max_luminance: [0.0, 1000.0],
            generation_rect: FfxApiRect2D {
                left: 0,
                top: 0,
                width: width as i32,
                height: height as i32,
            },
            frame_id: self.frame_index,
        };
        let rc = unsafe { (self.loader.dispatch)(&mut self.fg_context, &mut dispatch.header) };
        if rc != super::imp::FFX_API_RETURN_OK {
            drop(backbuffer);
            return Err(format!("ffxDispatch frame generation failed (0x{rc:x})"));
        }

        // --- Present through the proxy (frame pacing + interpolation).
        let hr = unsafe { (*sc).Present(1, 0) };
        if hr < 0 {
            super::diag(&format!("[FSR FG] present through the frame generation swapchain failed (HRESULT 0x{hr:08x})"));
        }
        drop(backbuffer);
        self.frame_index += 1;
        Ok(())
    }
}
