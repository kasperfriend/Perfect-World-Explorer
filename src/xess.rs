//! Intel Xe Super Sampling (XeSS) 3 — super resolution and frame generation.
//!
//! ## What this is
//!
//! The engine half of the "Upscaling" and "Frame generation" settings in
//! Graphics. When XeSS is the active upscaler (`settings.upscaling == Xess`
//! and the hardware/SDK gates in `crate::support` pass), the world is rendered
//! at the XeSS-recommended internal resolution, a temporal motion vector is
//! reconstructed from the depth buffer by the same fullscreen pass the FSR
//! path uses, and `xessD3D12Execute` is dispatched on a native D3D12 command
//! list to produce the full-resolution image. XeSS Frame Generation
//! (`settings.frame_generation == Xess`) works the same way as the FSR frame
//! generation integration: the viewer renders the composition (no UI) and the
//! UI separately, and the XeSS-FG proxy swapchain composites and paces real
//! and generated frames.
//!
//! * The DLLs (`libxess.dll`, `libxell.dll`, `libxess_fg.dll`) are loaded
//!   with LoadLibraryW/GetProcAddress from the same SDK directory the probe
//!   scanned (Settings → `upscale_sdk_dir`, `PW_UPSCALER_SDK_DIR`, or
//!   `<exe>\sdk\`). They are **not** vendored in this repository — the XeSS
//!   SDK 3.0.2 release (https://github.com/intel/xess/releases/tag/v3.0.2)
//!   must be downloaded once and its DLLs copied into `sdk/` (see
//!   `sdk/SDK_GOES_HERE.txt`). The SDK headers they were built from are kept
//!   under `sdk/include/xess*` for reference and review.
//! * Color is perceptual sRGB and depth is reversed (near→1, far→0), so the
//!   XeSS-SR context is created with `XESS_INIT_FLAG_INVERTED_DEPTH` and
//!   `XESS_INIT_FLAG_LDR_INPUT_COLOR` and the XeSS-FG swapchain with
//!   `XEFG_SWAPCHAIN_INIT_FLAG_INVERTED_DEPTH`. No exposure texture is
//!   passed; exposure scale is 1.0. The UI texture is premultiplied-alpha
//!   (egui-wgpu blends with One/OneMinusSrcAlpha), so the
//!   `XEFG_SWAPCHAIN_INIT_FLAG_UITEXTURE_NOT_PREMUL_ALPHA` flag is NOT set.
//! * The camera is jittered with an in-tree Halton(2,3) sequence (16 phases),
//!   matching the documented `[-0.5, 0.5]` pixel jitter XeSS-SR expects. The
//!   world pass is the only jittered view; the MV pass and the UI/minimap
//!   overlays use unjittered matrices.
//! * Motion vectors are reconstructed from depth only, so they describe
//!   *camera* motion; actors that move in world space are reprojected as if
//!   they had not moved. There is no per-object velocity pass
//!   (see docs/blockers.md B-5).
//! * XeSS-FG requires XeLL (Intel Xe Low Latency) on every GPU, Intel or
//!   otherwise; without a XeLL context the proxy swapchain fails to
//!   initialize. The frame generation integration therefore creates a XeLL
//!   context itself, enables low-latency mode via `xellSetSleepMode`, calls
//!   `xellSleep` once per frame and delivers the required simulation /
//!   render-submit / present markers each frame. The XeLL context is
//!   destroyed after the XeSS-FG context, as required.
//! * XeSS-FG supports every GPU with Shader Model 6.4 or later (Intel Arc
//!   use XMX matrix cores; other vendors use a DP4a compute path). On
//!   non-Intel GPUs the maximum number of generated frames is one.
//! * wgpu and the native XeSS work synchronise through queue ordering on the
//!   single D3D12 direct queue wgpu-hal creates: wgpu submits, the XeSS
//!   command lists execute, then wgpu composites/presents. Barriers inside
//!   the native lists move resources into and out of the states XeSS expects.
//!
//! ## What is not verified
//!
//! No machine this code is developed on has a GPU, and the CI runners do not
//! either. Everything past "does it compile on Windows" is runtime-unverified:
//! the jitter sign convention, the motion-vector direction and even basic
//! operation. If context creation or a dispatch fails, the viewer logs the
//! error (stderr *and* the always-on `session.log` in the diagnostics
//! folder), shows the reason in the on-screen fallback note, and falls back
//! to the bilinear path for the rest of the session instead of crashing.
//!
//! ## Non-Windows
//!
//! The whole module is a stub off Windows: `XessUpscaler::new` always fails
//! with "not supported on this platform", so Linux builds stay green and the
//! Windows build is the compile authority.

/// Single channel for every XeSS diagnostic: stderr for console runs,
/// and the always-on `session.log` in the diagnostics folder (the viewer
/// is a GUI-subsystem app — stderr alone vanishes when it is launched
/// from Explorer). Never per-frame; call sites are lifecycle events.
pub(crate) fn diag(line: &str) {
    eprintln!("{line}");
    crate::diagnostics::session(line);
}

#[cfg(windows)]
mod fg;
#[cfg(windows)]
mod imp;
#[cfg(windows)]
pub(crate) mod loader;
#[cfg(windows)]
pub(crate) mod xell;
#[cfg(windows)]
pub use fg::*;
#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
mod imp_stub {
    use glam::Mat4;

    /// Stub of the XeSS upscaler; always fails to create.
    pub struct XessUpscaler;

    impl XessUpscaler {
        pub fn new(
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _sdk_dir: &str,
            _render_extent: (u32, u32),
            _display_extent: (u32, u32),
            _format: wgpu::TextureFormat,
            _quality: crate::UpscaleQuality,
        ) -> Result<XessUpscaler, String> {
            Err("XeSS is only supported on Windows with a DX12 backend".to_string())
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
        ) -> Result<(), String> {
            Err("unsupported".to_string())
        }

        pub fn composite(
            &self,
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _target: &wgpu::TextureView,
            _sharpness: f32,
        ) {
        }

        pub fn reset_history(&mut self) {}

        pub fn render_extent(&self) -> (u32, u32) {
            (0, 0)
        }

        pub fn display_extent(&self) -> (u32, u32) {
            (0, 0)
        }

        pub fn jitter(&self) -> (f32, f32) {
            (0.0, 0.0)
        }

        pub fn quality_name(&self) -> &'static str {
            "xess"
        }
    }
}
#[cfg(not(windows))]
pub use imp_stub::*;

#[cfg(not(windows))]
mod fg_stub {
    /// Stub of the XeSS frame generation pipeline; always fails to create.
    pub struct XessFrameGeneration;

    impl XessFrameGeneration {
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
            _interpolated_frames: u32,
        ) -> Result<XessFrameGeneration, String> {
            Err(
                "XeSS Frame Generation is only supported on Windows with a DX12 backend"
                    .to_string(),
            )
        }

        pub fn compose_view(&self) -> wgpu::TextureView {
            unimplemented!()
        }

        pub fn ui_view(&self) -> wgpu::TextureView {
            unimplemented!()
        }

        pub fn size(&self) -> (u32, u32) {
            unimplemented!()
        }

        pub fn render_extent(&self) -> (u32, u32) {
            unimplemented!()
        }

        pub fn begin_frame(&mut self) {}

        #[allow(clippy::too_many_arguments)]
        pub fn prepare(
            &mut self,
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _depth: &wgpu::Texture,
            _object_mv: &wgpu::TextureView,
            _view: &glam::Mat4,
            _projection: &glam::Mat4,
            _unjittered_vp: &glam::Mat4,
            _frame_time_ms: f32,
            _reset: bool,
            _jitter: (f32, f32),
        ) -> Result<(), String> {
            unimplemented!()
        }

        pub fn render_submit_end(&mut self) {}

        pub fn finish(&mut self, _ui_visible: bool) -> Result<(), String> {
            unimplemented!()
        }

        pub fn frames_presented(&self) -> u64 {
            0
        }

        /// The Windows path keeps the UI out of the interpolated image.
        pub fn ui_in_composition(&self) -> bool {
            false
        }

        /// The Windows path pre-fills the UI surface with the composition.
        pub fn ui_surface_prefilled(&self) -> bool {
            true
        }
    }
}

#[cfg(not(windows))]
pub use fg_stub::XessFrameGeneration;
