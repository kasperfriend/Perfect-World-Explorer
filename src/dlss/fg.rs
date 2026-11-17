//! Windows + DX12 implementation of DLSS Frame Generation (`nvngx_dlssg.dll`).
//!
//! DLSS-G cannot be layered on wgpu's swapchain. The feature interpolates
//! *presented* frames: it is handed the backbuffers themselves and writes the
//! generated image into a second one, and the caller then presents both. That
//! needs a swapchain the caller owns, so this module takes presentation over
//! exactly like the FSR and XeSS frame generators:
//!
//! * On enable, the wgpu surface is dropped (which destroys wgpu-hal's
//!   `IDXGISwapChain`), [`crate::fsr::swapchain::prepare`] proves DXGI accepts a
//!   flip-model description for the window, and that description is used to
//!   create *our* `IDXGISwapChain` for the hwnd. NGX is then initialised and
//!   the `NVSDK_NGX_Feature_FrameGeneration` feature is created against the
//!   viewer's own parameter table (`super::params`) over the snippet ABI — see
//!   [`super::loader`].
//! * Every frame the caller renders normally, ending with a linear image in
//!   [`DlssFrameGeneration::compose_view`].
//!   [`DlssFrameGeneration::prepare`] produces the reprojected motion vectors
//!   from the reversed-Z depth at internal resolution, and
//!   [`DlssFrameGeneration::finish`] copies the composition into the current
//!   backbuffer, calls `NVSDK_NGX_D3D12_EvaluateFeature` so NGX writes the
//!   interpolated image into the next backbuffer, and presents both.
//! * On disable the feature is released, NGX is shut down and the swapchain is
//!   released, so the engine can recreate the wgpu surface on the window.
//!
//! ## UI
//!
//! The UI is separated from the interpolated image, using DLSS-G's User
//! Interface Recomposition: the scene is handed over as `DLSSG.HUDLess`, the UI
//! on its own as `DLSSG.UI`, and the presented frame (scene plus UI, which is
//! what the viewer draws) as the backbuffer. NGX interpolates the HUD-less
//! scene and recomposites the UI onto generated frames, so HUD pixels are never
//! interpolated and cannot be dropped by a UI mode.
//!
//! ## Presenting
//!
//! One app frame becomes two presents: the real frame (`Present(1, 0)`, which
//! paces against vblank) followed by the generated one (`Present(0, 0)`). The
//! swapchain is created without the frame-latency waitable-object flag for the
//! same reason the XeSS-FG proxy drops it: with the flag set and a latency of
//! one the generated frame has nowhere to go, which shows up as a black frame
//! every second present.

use super::loader::{NgxApi, NGX_FEATURE_FRAME_GENERATION, NGX_VERSION_API};
use super::params::NgxParams;
use crate::fsr::{
    dxgi_for_format, dxgi_output_format, raw_dx12, FsrMotionVectorPass, FsrMvParams,
    NativeCommandRing, NativeTexture2D, D3D12_STATE_COMMON, D3D12_STATE_COPY_DEST,
    D3D12_STATE_RENDER_TARGET, D3D12_STATE_UNORDERED_ACCESS,
};
use glam::Mat4;
use std::ffi::c_void;
use winapi::shared::dxgi::{IDXGISwapChain, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT};
use winapi::shared::dxgi1_2::IDXGISwapChain1;
use winapi::shared::dxgi1_4::IDXGISwapChain3;
use winapi::shared::dxgiformat::{DXGI_FORMAT_R16G16_FLOAT, DXGI_FORMAT_R32_FLOAT};
use winapi::um::d3d12 as d3d12_ty;
// Provides `uuidof()` on the COM interfaces.
use winapi::Interface;

/// `D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE` — the state NGX wants the
/// backbuffer, depth and motion vectors in (DLSS-G dispatches compute).
const D3D12_STATE_NON_PIXEL_READ: u32 = d3d12_ty::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
/// `D3D12_RESOURCE_STATE_COPY_SOURCE` for the composition.
const D3D12_STATE_COPY_SRC: u32 = d3d12_ty::D3D12_RESOURCE_STATE_COPY_SOURCE;
/// `D3D12_RESOURCE_STATE_PRESENT`, the state DXGI requires backbuffers in.
const D3D12_STATE_PRESENT: u32 = d3d12_ty::D3D12_RESOURCE_STATE_PRESENT;

// Create-time parameters, spelled as `nvsdk_ngx_defs_dlssg.h` defines them.
const P_WIDTH: &str = "DLSSG.Width";
const P_HEIGHT: &str = "DLSSG.Height";
const P_INTERNAL_WIDTH: &str = "DLSSG.InternalWidth";
const P_INTERNAL_HEIGHT: &str = "DLSSG.InternalHeight";
const P_BACKBUFFER_FORMAT: &str = "DLSSG.BackbufferFormat";
const P_DYNAMIC_RESOLUTION: &str = "DLSSG.DynamicResolution";
// Per-frame resources.
const P_BACKBUFFER: &str = "DLSSG.Backbuffer";
const P_DEPTH: &str = "DLSSG.Depth";
const P_MVECS: &str = "DLSSG.MVecs";
const P_OUTPUT_INTERPOLATED: &str = "DLSSG.OutputInterpolated";
const P_BACKBUFFER_FRAME_ID: &str = "DLSSG.BackbufferFrameID";
// User Interface Recomposition (UIR). `HUDLess` is the scene before any UI,
// `UI` the UI on its own (premultiplied alpha, as egui renders it); the header
// requires both for UIR: "For User Interface Recomposition (UIR) frame
// generation, both HUDless and one of UI or UIAlpha must be provided."
const P_HUDLESS: &str = "DLSSG.HUDLess";
const P_UI: &str = "DLSSG.UI";
const P_UIR_ENABLED: &str = "DLSSG.UserInterfaceRecompositionEnabled";
// Per-frame scalars.
const P_MULTI_FRAME_COUNT: &str = "DLSSG.MultiFrameCount";
const P_MULTI_FRAME_INDEX: &str = "DLSSG.MultiFrameIndex";
const P_JITTER_X: &str = "DLSSG.JitterOffsetX";
const P_JITTER_Y: &str = "DLSSG.JitterOffsetY";
const P_MVEC_SCALE_X: &str = "DLSSG.MvecScaleX";
const P_MVEC_SCALE_Y: &str = "DLSSG.MvecScaleY";
const P_DEPTH_INVERTED: &str = "DLSSG.DepthInverted";
const P_CAMERA_MOTION_INCLUDED: &str = "DLSSG.CameraMotionIncluded";
const P_COLOR_BUFFERS_HDR: &str = "DLSSG.ColorBuffersHDR";
const P_RESET: &str = "DLSSG.Reset";
const P_NOT_RENDERING_FRAMES: &str = "DLSSG.NotRenderingGameFrames";
const P_ORTHO_PROJECTION: &str = "DLSSG.OrthoProjection";
const P_MENU_DETECTION: &str = "DLSSG.MenuDetectionEnabled";
// Reprojection matrices (void pointers to 16 floats, column major).
const P_CLIP_TO_PREV_CLIP: &str = "DLSSG.ClipToPrevClip";
const P_PREV_CLIP_TO_CLIP: &str = "DLSSG.PrevClipToClip";
const P_CAMERA_POS_X: &str = "DLSSG.CameraPosX";
const P_CAMERA_POS_Y: &str = "DLSSG.CameraPosY";
const P_CAMERA_POS_Z: &str = "DLSSG.CameraPosZ";
const P_CAMERA_UP_X: &str = "DLSSG.CameraUpX";
const P_CAMERA_UP_Y: &str = "DLSSG.CameraUpY";
const P_CAMERA_UP_Z: &str = "DLSSG.CameraUpZ";
const P_CAMERA_RIGHT_X: &str = "DLSSG.CameraRightX";
const P_CAMERA_RIGHT_Y: &str = "DLSSG.CameraRightY";
const P_CAMERA_RIGHT_Z: &str = "DLSSG.CameraRightZ";
const P_CAMERA_FWD_X: &str = "DLSSG.CameraFwdX";
const P_CAMERA_FWD_Y: &str = "DLSSG.CameraFwdY";
const P_CAMERA_FWD_Z: &str = "DLSSG.CameraFwdZ";
const P_CAMERA_NEAR: &str = "DLSSG.CameraNear";
const P_CAMERA_FAR: &str = "DLSSG.CameraFar";
const P_CAMERA_FOV: &str = "DLSSG.CameraFOV";
const P_CAMERA_ASPECT: &str = "DLSSG.CameraAspectRatio";
const P_MVEC_DILATED: &str = "DLSSG.MvecDilated";

/// The NGX handle owner plus everything the frame loop needs.
pub struct DlssFrameGeneration {
    api: NgxApi,
    params: Box<NgxParams>,
    handle: *mut c_void,
    queue: d3d12::CommandQueue,
    ring: NativeCommandRing,
    /// Our own flip-model swapchain for the window.
    swapchain: *mut IDXGISwapChain1,
    buffer_count: u32,
    /// Frames NGX interpolates per rendered frame (`MultiFrameCount`).
    generated_frames: u32,
    display_extent: (u32, u32),
    render_extent: (u32, u32),
    /// The full-resolution image the caller renders into: the scene, without
    /// any UI. Handed to NGX as `DLSSG.HUDLess`.
    compose: NativeTexture2D,
    /// The presented frame: a copy of `compose` with the UI drawn on top.
    present: NativeTexture2D,
    /// The UI on its own, transparent where nothing was drawn. Handed to NGX
    /// as `DLSSG.UI` so generated frames carry HUD pixels that were never
    /// interpolated.
    ui_only: NativeTexture2D,
    depth: NativeTexture2D,
    mv: NativeTexture2D,
    mv_pass: FsrMotionVectorPass,
    clip_to_prev: [[f32; 4]; 4],
    prev_clip_to_clip: [[f32; 4]; 4],
    prev_view_proj: Option<Mat4>,
    /// `true` while NGX has to drop its interpolation history: the first frame
    /// and every camera cut (`prepare`'s `reset`).
    reset_pending: bool,
    /// The camera the matrices were built from, for `DLSSG.Camera*`.
    lens: crate::fsr::CameraLens,
    camera_pos: [f32; 3],
    camera_up: [f32; 3],
    camera_right: [f32; 3],
    camera_forward: [f32; 3],
    frame_index: u64,
    presents: u64,
}

impl Drop for DlssFrameGeneration {
    fn drop(&mut self) {
        self.ring.wait_idle();
        if !self.handle.is_null() {
            unsafe {
                (self.api.release_feature)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
        unsafe {
            if !self.swapchain.is_null() {
                (*self.swapchain).Release();
            }
            self.swapchain = std::ptr::null_mut();
        }
        // NGX Core stays loaded for the process (both features share it) and
        // the feature handle is released in `finish`/`Drop` above.
    }
}

impl DlssFrameGeneration {
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
        generated_frames: u32,
    ) -> Result<Self, String> {
        let generated_frames = generated_frames.clamp(1, 5);
        let _ = queue;
        let (raw_device, raw_queue) = raw_dx12(device)?;
        let device_ptr = raw_device.as_mut_ptr() as *mut c_void;
        let queue_ptr = raw_queue.as_mut_ptr() as *mut c_void;

        // A flip-model backbuffer cannot be sRGB, so the swapchain (and the
        // format NGX is told about) is the plain UNORM twin; the composition
        // the caller renders into keeps the surface's sRGB format so egui's
        // pipelines match it.
        let backbuffer_format = dxgi_output_format(surface_format);
        let compose_dxgi = dxgi_for_format(surface_format);
        let ui_dxgi = dxgi_for_format(ui_format);

        // Hand the window over from wgpu's swapchain: DXGI allows one flip-model
        // swapchain per window and we are about to create our own, so the
        // surface must be dropped and its queued frames retired. `prepare`
        // drains the device, retries until DXGI accepts a description, and
        // returns that description plus a factory parented to our adapter.
        let prepared = crate::fsr::swapchain::prepare(
            device,
            hwnd,
            display_extent.0.max(1),
            display_extent.1.max(1),
            backbuffer_format,
        )?;
        let factory = prepared.factory;
        let mut desc = prepared.desc;
        let handover = prepared.report;
        // Two presents per app frame: no waitable object, see the module docs.
        desc.Flags &= !DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
        // One backbuffer for the real frame and one per generated frame, plus
        // DXGI's spare: a presented group cannot come out of fewer buffers.
        // This has to be decided before the swapchain is created — the buffer
        // count is an immutable property of it.
        desc.BufferCount = desc.BufferCount.max(generated_frames + 2);
        let mut fullscreen = prepared.fullscreen;
        crate::dlss::diag(&format!(
            "[DLSS FG] window hand-over: {handover}; proxy swapchain {}x{}, {} buffers, format 0x{:08x}, flags 0x{:x}",
            desc.Width, desc.Height, desc.BufferCount, desc.Format, desc.Flags
        ));

        let mut swapchain: *mut IDXGISwapChain1 = std::ptr::null_mut();
        let hr = unsafe {
            (*factory).CreateSwapChainForHwnd(
                queue_ptr as *mut winapi::um::unknwnbase::IUnknown,
                hwnd as winapi::shared::windef::HWND,
                &desc,
                &fullscreen,
                std::ptr::null_mut(),
                &mut swapchain,
            )
        };
        unsafe { (*factory).Release() };
        if hr < 0 || swapchain.is_null() {
            return Err(format!(
                "IDXGIFactory2::CreateSwapChainForHwnd failed for DLSS FG (HRESULT 0x{hr:08x})"
            ));
        }
        let buffer_count = desc.BufferCount.max(2);

        // NGX Core (`nvngx.dll`, from the driver) initialises once for the
        // device and loads the vendored `nvngx_dlssg.dll` itself; the snippet is
        // never called directly, because it refuses to initialise anything that
        // is not an NGX runtime (0xBAD00002).
        let api = NgxApi::load(sdk_dir, "nvngx_dlssg.dll", device_ptr)?;
        // The frame-generation feature creates and evaluates with the allocated
        // map, the same choice the super-resolution path makes (B-27).
        let mut params = api.new_parameters(false);
        crate::dlss::diag(&format!(
            "[DLSS FG] NGX ready (API 0x{NGX_VERSION_API:x}, core {}), creating DLSS-G",
            api.core_path()
        ));

        params.set_u32(P_WIDTH, display_extent.0.max(1));
        params.set_u32(P_HEIGHT, display_extent.1.max(1));
        params.set_u32(P_INTERNAL_WIDTH, render_extent.0.max(1));
        params.set_u32(P_INTERNAL_HEIGHT, render_extent.1.max(1));
        params.set_u32(P_BACKBUFFER_FORMAT, backbuffer_format as u32);
        // The viewer always renders at a fixed scale for the session.
        params.set_u32(P_DYNAMIC_RESOLUTION, 0);
        params.set_u32(P_UIR_ENABLED, 1);

        let mut ring = NativeCommandRing::new(&raw_device, "DLSS FG")?;
        let mut handle: *mut c_void = std::ptr::null_mut();
        // Creation may record its own work on the list it is handed, so give it
        // a real one and retire it afterwards (same as the SR path).
        let slot_index = ring.acquire("DLSS FG")?;
        let result = {
            let list = &mut ring.slot_mut(slot_index).list;
            unsafe {
                (api.create_feature)(
                    list.as_mut_ptr() as *mut c_void,
                    NGX_FEATURE_FRAME_GENERATION,
                    params.as_ptr(),
                    &mut handle,
                )
            }
        };
        if !crate::dlss::succeeded(result) || handle.is_null() {
            unsafe { (*swapchain).Release() };
            return Err(format!(
                "NVSDK_NGX_D3D12_CreateFeature(DLSS-G) failed ({}) — an RTX 40-series or newer GPU is required",
                crate::dlss::result_name(result)
            ));
        }
        let _ = ring.retire(slot_index, &raw_queue, "DLSS FG");

        let compose = unsafe {
            NativeTexture2D::create(
                device,
                &raw_device,
                "dlss frame generation composition",
                surface_format,
                compose_dxgi,
                display_extent,
                d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                D3D12_STATE_COMMON,
                false,
            )
        }
        .map_err(|error| {
            unsafe { (*swapchain).Release() };
            error
        })?;
        let present = unsafe {
            NativeTexture2D::create(
                device,
                &raw_device,
                "dlss frame generation present",
                ui_format,
                ui_dxgi,
                display_extent,
                d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST,
                D3D12_STATE_COMMON,
                false,
            )
        }
        .map_err(|error| {
            unsafe { (*swapchain).Release() };
            error
        })?;
        let ui_only = unsafe {
            NativeTexture2D::create(
                device,
                &raw_device,
                "dlss frame generation ui",
                ui_format,
                ui_dxgi,
                display_extent,
                d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                D3D12_STATE_COMMON,
                false,
            )
        }
        .map_err(|error| {
            unsafe { (*swapchain).Release() };
            error
        })?;
        let depth = unsafe {
            NativeTexture2D::create(
                device,
                &raw_device,
                "dlss frame generation depth",
                wgpu::TextureFormat::R32Float,
                DXGI_FORMAT_R32_FLOAT,
                render_extent,
                d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
                D3D12_STATE_COMMON,
                false,
            )
        }
        .map_err(|error| {
            unsafe { (*swapchain).Release() };
            error
        })?;
        let mv = unsafe {
            NativeTexture2D::create(
                device,
                &raw_device,
                "dlss frame generation motion vectors",
                wgpu::TextureFormat::Rg16Float,
                DXGI_FORMAT_R16G16_FLOAT,
                render_extent,
                d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                D3D12_STATE_COMMON,
                false,
            )
        }
        .map_err(|error| {
            unsafe { (*swapchain).Release() };
            error
        })?;
        let mv_pass = FsrMotionVectorPass::new(device);

        crate::dlss::diag(&format!(
            "[DLSS FG] DLSS-G ready: {}x{} internal -> {}x{} presented, {} buffers, x{} ({} generated frame(s) per rendered frame)",
            render_extent.0,
            render_extent.1,
            display_extent.0,
            display_extent.1,
            buffer_count,
            generated_frames + 1,
            generated_frames
        ));

        Ok(Self {
            api,
            params,
            handle,
            queue: raw_queue,
            ring,
            swapchain,
            buffer_count,
            generated_frames,
            display_extent,
            render_extent,
            compose,
            present,
            ui_only,
            depth,
            mv,
            mv_pass,
            clip_to_prev: Mat4::IDENTITY.to_cols_array_2d(),
            prev_clip_to_clip: Mat4::IDENTITY.to_cols_array_2d(),
            prev_view_proj: None,
            reset_pending: true,
            lens: crate::fsr::CameraLens {
                near: 1.0,
                far: 16000.0,
                fov_y_radians: 70f32.to_radians(),
            },
            camera_pos: [0.0; 3],
            camera_up: [0.0, 1.0, 0.0],
            camera_right: [1.0, 0.0, 0.0],
            camera_forward: [0.0, 0.0, -1.0],
            frame_index: 0,
            presents: 0,
        })
    }

    /// The image the caller renders the scene and the UI into.
    pub fn compose_view(&self) -> wgpu::TextureView {
        self.compose.texture.create_view(&Default::default())
    }

    /// View of the surface the UI is drawn into: a copy of the composition, so
    /// the UI pass `Load`s it and the result is the presented frame.
    pub fn ui_view(&self) -> wgpu::TextureView {
        self.present.texture.create_view(&Default::default())
    }

    /// View of the UI-only surface (transparent where nothing was drawn),
    /// handed to NGX as `DLSSG.UI`.
    pub fn ui_only_view(&self) -> Option<wgpu::TextureView> {
        Some(self.ui_only.texture.create_view(&Default::default()))
    }

    /// `true` when the UI surface already contains the scene.
    pub fn ui_surface_prefilled(&self) -> bool {
        true
    }

    /// `false`: the UI is not part of the image the interpolator sees. The
    /// scene goes to NGX as `DLSSG.HUDLess`, the UI as `DLSSG.UI`, and the
    /// runtime recomposes the UI onto generated frames (User Interface
    /// Recomposition), so HUD pixels are never interpolated.
    pub fn ui_in_composition(&self) -> bool {
        false
    }

    /// Presented colour extent.
    pub fn size(&self) -> (u32, u32) {
        self.display_extent
    }

    /// Internal (render) extent.
    pub fn render_extent(&self) -> (u32, u32) {
        self.render_extent
    }

    /// Real frames presented (generated ones add another present each).
    pub fn frames_presented(&self) -> u64 {
        self.presents
    }

    /// No per-frame marker API: DLSS-G is driven entirely by `EvaluateFeature`.
    pub fn begin_frame(&mut self) {}

    /// No per-frame marker API; kept so the frame loop is uniform across the
    /// three frame generators.
    pub fn render_submit_end(&mut self) {}

    /// Reprojects the depth buffer into motion vectors at internal resolution.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        depth: &wgpu::Texture,
        object_mv: &wgpu::TextureView,
        unjittered_vp: &Mat4,
        reset: bool,
        lens: crate::fsr::CameraLens,
        camera_pos: [f32; 3],
        camera_up: [f32; 3],
        camera_right: [f32; 3],
        camera_forward: [f32; 3],
    ) -> Result<(), String> {
        let (rw, rh) = self.render_extent;
        self.lens = lens;
        self.camera_pos = camera_pos;
        self.camera_up = camera_up;
        self.camera_right = camera_right;
        self.camera_forward = camera_forward;
        if reset {
            self.reset_pending = true;
        }
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

        let mut encoder = device.create_command_encoder(&Default::default());
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
            label: Some("dlss fg motion vector bind group"),
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
                label: Some("dlss fg motion vector pass"),
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
        // The presented frame starts as an exact copy of the composition; the
        // caller draws the UI into `ui_view()` on top of it (`Load`), and NGX
        // gets the composition as `DLSSG.HUDLess` plus the UI as `DLSSG.UI`.
        encoder.copy_texture_to_texture(
            self.compose.texture.as_image_copy(),
            self.present.texture.as_image_copy(),
            wgpu::Extent3d {
                width: self.display_extent.0,
                height: self.display_extent.1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        // Matrices for the interpolation: everything here is *unjittered*, as
        // the header requires ("All matrices should NOT contain temporal AA
        // jitter offset").
        if let Some(prev) = self.prev_view_proj.filter(|_| !reset) {
            self.clip_to_prev = (prev * inv_vp).to_cols_array_2d();
            self.prev_clip_to_clip = (*unjittered_vp * prev.inverse()).to_cols_array_2d();
        } else {
            self.clip_to_prev = Mat4::IDENTITY.to_cols_array_2d();
            self.prev_clip_to_clip = Mat4::IDENTITY.to_cols_array_2d();
        }
        self.prev_view_proj = Some(*unjittered_vp);
        Ok(())
    }

    /// Interpolates `generated_frames` frames and presents the real frame
    /// followed by each generated one.
    ///
    /// One `EvaluateFeature` per generated frame, each writing into its own
    /// backbuffer (`DLSSG.MultiFrameIndex` 1..n, `DLSSG.MultiFrameCount` = n) —
    /// that is how the runtime's multi frame generation is driven; it decides
    /// from the GPU architecture whether it will fill more than the first
    /// (`DLSSG.MultiFrameCountMax` is 1 on Ada, higher on Blackwell).
    ///
    /// `jitter` is the sub-pixel offset the world was rendered with this frame
    /// (render-resolution pixels, y down); DLSS-G wants it in clip space, and
    /// the matrices handed to it must stay unjittered — the header is explicit
    /// about both.
    pub fn finish(&mut self, ui_visible: bool, jitter: (f32, f32)) -> Result<(), String> {
        let sc3 = self.swapchain as *mut IDXGISwapChain3;
        let index = unsafe { (*sc3).GetCurrentBackBufferIndex() };
        let count = self.buffer_count.max(2);
        let mut real = self.backbuffer(index)?;
        let mut generated = Vec::with_capacity(self.generated_frames as usize);
        for step in 1..=self.generated_frames {
            generated.push(self.backbuffer((index + step) % count)?);
        }

        let slot_index = self.ring.acquire("DLSS FG")?;
        let subresource = d3d12_ty::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
        let none = d3d12_ty::D3D12_RESOURCE_BARRIER_FLAG_NONE;
        let barrier = |resource: &d3d12::Resource, before: u32, after: u32| {
            d3d12::ResourceBarrier::transition(resource.clone(), subresource, before, after, none)
        };

        let list = &mut self.ring.slot_mut(slot_index).list;
        let mut barriers = Vec::with_capacity(6 + generated.len());
        barriers.push(barrier(
            &self.present.resource,
            D3D12_STATE_RENDER_TARGET,
            D3D12_STATE_COPY_SRC,
        ));
        if ui_visible {
            barriers.push(barrier(
                &self.ui_only.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_NON_PIXEL_READ,
            ));
        }
        barriers.push(barrier(
            &self.depth.resource,
            D3D12_STATE_RENDER_TARGET,
            D3D12_STATE_NON_PIXEL_READ,
        ));
        barriers.push(barrier(
            &self.mv.resource,
            D3D12_STATE_RENDER_TARGET,
            D3D12_STATE_NON_PIXEL_READ,
        ));
        barriers.push(barrier(&real, D3D12_STATE_PRESENT, D3D12_STATE_COPY_DEST));
        for target in &generated {
            barriers.push(barrier(
                target,
                D3D12_STATE_PRESENT,
                D3D12_STATE_UNORDERED_ACCESS,
            ));
        }
        list.resource_barrier(&barriers);
        // `CopyResource` has no safe wrapper in the d3d12 crate.
        unsafe {
            (*list.as_mut_ptr())
                .CopyResource(real.as_mut_ptr(), self.present.resource.as_mut_ptr());
        }
        // NGX reads the real backbuffer and writes the generated ones.
        let to_input = [barrier(
            &real,
            D3D12_STATE_COPY_DEST,
            D3D12_STATE_NON_PIXEL_READ,
        )];
        list.resource_barrier(&to_input);

        self.params
            .set_d3d12_resource(P_BACKBUFFER, real.as_mut_ptr() as *mut c_void);
        // User Interface Recomposition: the scene the interpolator works on,
        // and the UI it recomposites afterwards. Handing over both keeps HUD
        // pixels out of the interpolated image *and* on generated frames.
        self.params
            .set_d3d12_resource(P_HUDLESS, self.compose.resource.as_mut_ptr() as *mut c_void);
        if ui_visible {
            self.params
                .set_d3d12_resource(P_UI, self.ui_only.resource.as_mut_ptr() as *mut c_void);
        } else {
            self.params.set_d3d12_resource(P_UI, std::ptr::null_mut());
        }
        self.params
            .set_d3d12_resource(P_DEPTH, self.depth.resource.as_mut_ptr() as *mut c_void);
        self.params
            .set_d3d12_resource(P_MVECS, self.mv.resource.as_mut_ptr() as *mut c_void);
        // `MultiFrameCount` is how many frames this pair gets; `MultiFrameIndex`
        // identifies which one the call is producing.
        self.params
            .set_u32(P_MULTI_FRAME_COUNT, self.generated_frames.max(1));
        self.params
            .set_u32(P_BACKBUFFER_FRAME_ID, self.frame_index as u32);
        // `DLSSG.JitterOffsetX/Y` is documented as *clip space* jitter, and the
        // matrices handed over deliberately do not contain it, so the offset has
        // to be reported here or the interpolated frame is off by the sub-pixel
        // sample position the real frame used (shimmering edges). The world is
        // jittered by whichever upscaler is active (`frame_jitter`).
        let (rw, rh) = self.render_extent;
        self.params
            .set_f32(P_JITTER_X, 2.0 * jitter.0 / rw.max(1) as f32);
        self.params
            .set_f32(P_JITTER_Y, -2.0 * jitter.1 / rh.max(1) as f32);
        // `DLSSG.MvecScale` "specifies scale factors used to normalize motion
        // vectors (so the values are in [-1,1] range)" — the reprojection pass
        // writes render-resolution *pixels*, so the factor is 1/size.
        self.params.set_f32(P_MVEC_SCALE_X, 1.0 / rw.max(1) as f32);
        self.params.set_f32(P_MVEC_SCALE_Y, 1.0 / rh.max(1) as f32);
        // Reversed-Z depth, camera motion is in the vectors, LDR swapchain.
        self.params.set_u32(P_DEPTH_INVERTED, 1);
        self.params.set_u32(P_CAMERA_MOTION_INCLUDED, 1);
        self.params.set_u32(P_COLOR_BUFFERS_HDR, 0);
        self.params.set_u32(P_NOT_RENDERING_FRAMES, 0);
        self.params.set_u32(P_ORTHO_PROJECTION, 0);
        self.params.set_u32(P_MENU_DETECTION, 0);
        // The optional parameters the header lists (`NVSDK_NGX_DLSSG_Opt_Eval_Params`):
        // the camera the matrices came from, so the runtime can separate camera
        // motion from object motion, and the lens the depth was projected with.
        // Vectors are in world space, `CameraFOV` in radians, aspect = w / h.
        self.params.set_f32(P_CAMERA_POS_X, self.camera_pos[0]);
        self.params.set_f32(P_CAMERA_POS_Y, self.camera_pos[1]);
        self.params.set_f32(P_CAMERA_POS_Z, self.camera_pos[2]);
        self.params.set_f32(P_CAMERA_UP_X, self.camera_up[0]);
        self.params.set_f32(P_CAMERA_UP_Y, self.camera_up[1]);
        self.params.set_f32(P_CAMERA_UP_Z, self.camera_up[2]);
        self.params.set_f32(P_CAMERA_RIGHT_X, self.camera_right[0]);
        self.params.set_f32(P_CAMERA_RIGHT_Y, self.camera_right[1]);
        self.params.set_f32(P_CAMERA_RIGHT_Z, self.camera_right[2]);
        self.params.set_f32(P_CAMERA_FWD_X, self.camera_forward[0]);
        self.params.set_f32(P_CAMERA_FWD_Y, self.camera_forward[1]);
        self.params.set_f32(P_CAMERA_FWD_Z, self.camera_forward[2]);
        self.params.set_f32(P_CAMERA_NEAR, self.lens.near);
        self.params.set_f32(P_CAMERA_FAR, self.lens.far);
        self.params.set_f32(P_CAMERA_FOV, self.lens.fov_y_radians);
        self.params.set_f32(
            P_CAMERA_ASPECT,
            crate::fsr::CameraLens::aspect(self.display_extent.0, self.display_extent.1),
        );
        // The reprojection pass writes one vector per pixel, not a dilated field.
        self.params.set_u32(P_MVEC_DILATED, 0);
        // `Reset` covers the first frame *and* every camera cut: without it a
        // teleport interpolates from a history that has nothing to do with the
        // new view (a visible smear for a second or two).
        self.params.set_u32(
            P_RESET,
            u32::from(self.reset_pending || self.frame_index == 0),
        );
        self.reset_pending = false;
        self.params.set_void_pointer(
            P_CLIP_TO_PREV_CLIP,
            &self.clip_to_prev as *const _ as *mut c_void,
        );
        self.params.set_void_pointer(
            P_PREV_CLIP_TO_CLIP,
            &self.prev_clip_to_clip as *const _ as *mut c_void,
        );

        let mut failure = 0;
        for (step, target) in generated.iter_mut().enumerate() {
            self.params.set_u32(P_MULTI_FRAME_INDEX, step as u32 + 1);
            self.params
                .set_d3d12_resource(P_OUTPUT_INTERPOLATED, target.as_mut_ptr() as *mut c_void);
            let result = unsafe {
                (self.api.evaluate_feature)(
                    list.as_mut_ptr() as *mut c_void,
                    self.handle,
                    self.params.as_ptr(),
                    std::ptr::null(),
                )
            };
            if !crate::dlss::succeeded(result) {
                failure = result;
                break;
            }
        }

        let mut restore = Vec::with_capacity(5 + generated.len());
        restore.push(barrier(
            &self.present.resource,
            D3D12_STATE_COPY_SRC,
            D3D12_STATE_RENDER_TARGET,
        ));
        if ui_visible {
            restore.push(barrier(
                &self.ui_only.resource,
                D3D12_STATE_NON_PIXEL_READ,
                D3D12_STATE_RENDER_TARGET,
            ));
        }
        restore.push(barrier(
            &self.depth.resource,
            D3D12_STATE_NON_PIXEL_READ,
            D3D12_STATE_RENDER_TARGET,
        ));
        restore.push(barrier(
            &self.mv.resource,
            D3D12_STATE_NON_PIXEL_READ,
            D3D12_STATE_RENDER_TARGET,
        ));
        restore.push(barrier(
            &real,
            D3D12_STATE_NON_PIXEL_READ,
            D3D12_STATE_PRESENT,
        ));
        for target in &generated {
            restore.push(barrier(
                target,
                D3D12_STATE_UNORDERED_ACCESS,
                D3D12_STATE_PRESENT,
            ));
        }
        list.resource_barrier(&restore);
        self.ring.retire(slot_index, &self.queue, "DLSS FG")?;

        // Every frame in the group flips on its own vblank. Presenting the
        // generated frames with sync interval 0 instead lets the app queue the
        // next group's real frame on the same vblank, so the generated frames
        // get coalesced away and the result judders — which is exactly what
        // "stutters when frame generation is on" looks like.
        let sc = self.swapchain as *mut IDXGISwapChain;
        let hr = unsafe { (*sc).Present(1, 0) };
        if hr < 0 {
            return Err(format!("DLSS FG Present(real) failed (HRESULT 0x{hr:08x})"));
        }
        self.frame_index += 1;
        if failure != 0 {
            return Err(format!(
                "NVSDK_NGX_D3D12_EvaluateFeature(DLSS-G) failed ({})",
                crate::dlss::result_name(failure)
            ));
        }
        for step in 0..generated.len() {
            let hr = unsafe { (*sc).Present(1, 0) };
            if hr < 0 {
                return Err(format!(
                    "DLSS FG Present(generated {}) failed (HRESULT 0x{hr:08x})",
                    step + 1
                ));
            }
        }
        self.presents += 1;
        Ok(())
    }

    /// Borrows one swapchain buffer as a `d3d12::Resource`. The caller releases
    /// the reference once the frame has been recorded.
    fn backbuffer(&self, index: u32) -> Result<d3d12::Resource, String> {
        let sc = self.swapchain as *mut IDXGISwapChain;
        let mut raw: *mut d3d12_ty::ID3D12Resource = std::ptr::null_mut();
        let hr = unsafe {
            (*sc).GetBuffer(
                index,
                &d3d12_ty::ID3D12Resource::uuidof(),
                &mut raw as *mut *mut _ as *mut *mut c_void,
            )
        };
        if hr < 0 || raw.is_null() {
            return Err(format!(
                "IDXGISwapChain::GetBuffer({index}) failed for DLSS FG (HRESULT 0x{hr:08x})"
            ));
        }
        Ok(unsafe { std::mem::transmute(raw) })
    }
}
