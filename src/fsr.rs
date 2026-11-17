//! AMD FidelityFX Super Resolution — the real integration (Windows + DX12).
//!
//! ## What this is
//!
//! The engine half of the "Upscaling" setting in Graphics. When FSR is the
//! active provider (`settings.upscaling == Fsr` and the hardware/SDK gates in
//! `crate::support` pass), the world is rendered at the FSR-recommended
//! internal resolution, a temporal motion vector is reconstructed from the
//! depth buffer by a fullscreen pass, and the AMD FidelityFX SDK FSR upscaler
//! is dispatched on its own native D3D12 command list to produce the
//! full-resolution image. The result is composited into the usual composition
//! view, so UI, minimap, gamma ramp and present-encode are unchanged.
//!
//! * The loader (`amd_fidelityfx_loader_dx12.dll`) is loaded with
//!   LoadLibraryW/GetProcAddress from the same SDK directory the probe scanned
//!   (Settings → `upscale_sdk_dir`, `PW_UPSCALER_SDK_DIR`, or `<exe>\sdk\`).
//!   The provider version is queried with `ffxQueryDescGetVersions` and the
//!   newest FSR provider is requested with `ffxOverrideVersion`; the provider
//!   actually selected is logged to stderr. The provider version is never
//!   hardcoded.
//! * The dispatch descriptor matches `ffxDispatchDescUpscale` in
//!   sdk/include/upscalers/ffx_upscale.h (FidelityFX SDK 2.4.0).
//! * Color is perceptual sRGB and depth is reversed (near→1, far→0), so the
//!   context is created with `FFX_UPSCALE_ENABLE_DEPTH_INVERTED` and
//!   `FFX_UPSCALE_ENABLE_NON_LINEAR_COLORSPACE`, and the dispatch carries
//!   `FFX_UPSCALE_FLAG_NON_LINEAR_COLOR_SRGB`. No exposure texture is passed;
//!   `preExposure` is 1.0.
//! * The camera is jittered with the FFX jitter sequence
//!   (ffxQueryDescUpscaleGetJitterPhaseCount / GetJitterOffset). The world
//!   pass is the only jittered view; the MV pass and the UI/minimap overlays
//!   use unjittered matrices.
//! * Motion vectors are reconstructed from depth only, so they describe
//!   *camera* motion; actors that move in world space are reprojected as if
//!   they had not moved. There is no per-object velocity pass
//!   (see docs/blockers.md B-5).
//! * wgpu and the native FFX work synchronise through queue ordering on the
//!   single D3D12 direct queue wgpu-hal creates: wgpu submits, then the FFX
//!   command list executes, then wgpu composites. Barriers inside the FFX
//!   list move every input/output into the state FFX expects and back to the
//!   state wgpu's tracker believes in, so no trackers desynchronise.
//!
//! ## What is not verified
//!
//! No machine this code is developed on has a GPU, and the CI runners do not
//! either. Everything past "does it compile on Windows" is runtime-unverified:
//! the jitter sign convention, the motion-vector direction and even basic
//! operation. If FFX context creation or the dispatch fails, the viewer logs
//! the error (stderr *and* the always-on `session.log` in the diagnostics
//! folder), shows the reason in the on-screen fallback note, and falls back
//! to the bilinear path for the rest of the session instead of crashing.
//! This module owns the upscaler abstraction only; XeSS (`src/xess`) and DLSS
//! (`src/dlss`) are separate backends behind the same interface. FSR frame
//! generation takes over the HWND with its own proxy swapchain
//! (`FrameGenerationSwapChainForHwndDX12`), renders the UI into a separate
//! native-resolution texture and presents through the FFX frame-interpolation
//! swapchain.
//!
//! ## Non-Windows
//!
/// Single channel for every FSR/FG diagnostic: stderr for console runs,
/// and the always-on `session.log` in the diagnostics folder (the viewer
/// is a GUI-subsystem app — stderr alone vanishes when it is launched
/// from Explorer). Never per-frame; call sites are lifecycle events.
pub(crate) fn diag(line: &str) {
    eprintln!("{line}");
    crate::diagnostics::session(line);
}

/// The lens of the world camera, exactly as the projection matrix was built
/// from it.
///
/// Every upscaler takes the same three numbers (FFX: `cameraNear`, `cameraFar`,
/// `cameraFovAngleVertical`; XeSS derives them from the projection it is handed
/// and the depth flags; NGX-SR needs none and NGX-G takes the whole basis). They
/// are handed over instead of being repeated as literals in each provider file,
/// because a far plane that disagrees with the matrix makes the vendor's
/// depth-linearisation — and with it every disocclusion test — wrong, which
/// reads as ghosting on geometry edges and blur on the horizon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CameraLens {
    /// Distance to the near plane, in world units.
    pub(crate) near: f32,
    /// Distance to the far plane, in world units.
    pub(crate) far: f32,
    /// Vertical field of view, in radians.
    pub(crate) fov_y_radians: f32,
}

impl CameraLens {
    /// Aspect ratio helper (`view space width / height`), as DLSS-G wants it.
    pub(crate) fn aspect(width: u32, height: u32) -> f32 {
        width.max(1) as f32 / height.max(1) as f32
    }
}

// The whole module is a stub off Windows: `FsrUpscaler::new` always fails
// with "not supported on this platform", so Linux builds stay green and the
// Windows build is the compile authority.

#[cfg(windows)]
mod fg;
#[cfg(windows)]
mod imp;
#[cfg(windows)]
mod mv;
#[cfg(windows)]
pub use fg::*;
#[cfg(windows)]
pub use imp::*;
#[cfg(windows)]
pub(crate) use mv::{jittered_clip_vp, FsrMotionVectorPass, FsrMvParams};
#[cfg(windows)]
pub(crate) mod swapchain;

#[cfg(not(windows))]
mod imp_stub {
    use glam::Mat4;

    pub struct FsrUpscaler;

    impl FsrUpscaler {
        pub fn new(
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _sdk_dir: &str,
            _render_extent: (u32, u32),
            _display_extent: (u32, u32),
            _format: wgpu::TextureFormat,
            _fsr_version_major: u32,
        ) -> Result<FsrUpscaler, String> {
            Err("FSR is only supported on Windows with a DX12 backend".to_string())
        }

        pub fn next_jitter(&mut self) -> (f32, f32) {
            (0.0, 0.0)
        }

        pub fn jittered_matrix(&self, matrix: Mat4) -> Mat4 {
            matrix
        }

        pub fn record(
            &mut self,
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _color: &wgpu::Texture,
            _depth: &wgpu::Texture,
            _object_mv: &wgpu::TextureView,
            _unjittered_vp: &glam::Mat4,
            _lens: crate::fsr::CameraLens,
            _sharpness: f32,
        ) -> Result<(), String> {
            Err("unsupported".to_string())
        }

        pub fn composite(
            &self,
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _target: &wgpu::TextureView,
        ) {
        }

        pub fn reset_history(&mut self) {}

        pub fn render_extent(&self) -> (u32, u32) {
            (0, 0)
        }

        pub fn display_extent(&self) -> (u32, u32) {
            (0, 0)
        }

        pub fn version_major(&self) -> u32 {
            0
        }

        pub fn selected_provider(&self) -> Option<u64> {
            None
        }

        pub fn jitter(&self) -> (f32, f32) {
            (0.0, 0.0)
        }

        pub fn wants_reset(&self) -> bool {
            true
        }
    }
}
#[cfg(not(windows))]
pub use imp_stub::*;

#[cfg(not(windows))]
mod fg_stub {
    /// Stub of the frame generation pipeline; always fails to create.
    pub struct FrameGeneration;

    impl FrameGeneration {
        #[allow(clippy::too_many_arguments)]
        pub fn new(
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _sdk_dir: &str,
            _hwnd: *mut std::ffi::c_void,
            _render_extent: (u32, u32),
            _display_extent: (u32, u32),
            _surface_format: wgpu::TextureFormat,
            _ui_format: wgpu::TextureFormat,
        ) -> Result<FrameGeneration, String> {
            Err("FSR Frame Generation is only supported on Windows with a DX12 backend".to_string())
        }

        pub fn compose_view(&self) -> wgpu::TextureView {
            unimplemented!()
        }

        pub fn ui_view(&self) -> wgpu::TextureView {
            unimplemented!()
        }

        /// The Windows path keeps the UI on its own surface, so this is the
        /// value the real frame generator reports.
        pub fn ui_in_composition(&self) -> bool {
            false
        }

        /// The Windows path pre-fills the UI surface with the composition.
        pub fn ui_surface_prefilled(&self) -> bool {
            true
        }

        pub fn size(&self) -> (u32, u32) {
            unimplemented!()
        }

        pub fn render_extent(&self) -> (u32, u32) {
            unimplemented!()
        }

        #[allow(clippy::too_many_arguments)]
        pub fn prepare(
            &mut self,
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _depth: &wgpu::Texture,
            _object_mv: &wgpu::TextureView,
            _unjittered_vp: &glam::Mat4,
            _frame_time_ms: f32,
            _reset: bool,
            _jitter: (f32, f32),
            _lens: crate::fsr::CameraLens,
            _camera_position: [f32; 3],
            _camera_up: [f32; 3],
            _camera_right: [f32; 3],
            _camera_forward: [f32; 3],
        ) -> Result<(), String> {
            unimplemented!()
        }

        pub fn finish(&mut self, _ui_visible: bool, _reset: bool) -> Result<(), String> {
            unimplemented!()
        }

        pub fn frames_presented(&self) -> u64 {
            0
        }

        pub fn selected_provider(&self) -> Option<u64> {
            None
        }
    }
}

#[cfg(not(windows))]
pub use fg_stub::FrameGeneration;
