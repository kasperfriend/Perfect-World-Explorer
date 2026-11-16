//! NVIDIA DLSS — super resolution and frame generation, on top of the NGX API.
//!
//! ## What this is
//!
//! The engine half of the "Upscaling" and "Frame generation" settings for
//! NVIDIA GPUs. When DLSS is the active upscaler (`settings.upscaling ==
//! Dlss` and the gates in `crate::support` pass), the world is rendered at the
//! DLSS-recommended internal resolution, the same fullscreen pass the FSR and
//! XeSS paths use reconstructs a temporal motion vector from the depth buffer,
//! and `NVSDK_NGX_D3D12_EvaluateFeature` runs on a native D3D12 command list to
//! produce the full-resolution image. DLSS Frame Generation (`settings.
//! frame_generation == Dlss`) takes the window over through a proxy swapchain,
//! like the FSR and XeSS frame generators do.
//!
//! * The vendor DLLs are vendored in `sdk/`: `nvngx_dlss.dll` (super
//!   resolution) and `nvngx_dlssg.dll` (frame generation), taken from the
//!   official [NVIDIA/DLSS](https://github.com/NVIDIA/DLSS) repository
//!   (`lib/Windows_x86_64/rel/`). The SDK headers they were built from are
//!   kept under `sdk/include/nvidia/` for reference and review.
//! * Those two DLLs are NGX *snippets*: they only initialise when NGX **Core**
//!   (`%SystemRoot%\System32\nvngx.dll`, installed with the display driver)
//!   drives them — called directly they answer `0xBAD00002`
//!   (`FAIL_PlatformError`), which is what an earlier revision reported on
//!   perfectly good RTX hardware. `loader` therefore loads the driver's core,
//!   initialises it once per device and creates both features through it, with
//!   the viewer's SDK folder first in `PathListInfo` so the vendored DLSS build
//!   is the one that runs.
//! * `NVSDK_NGX_Parameter` is a C++ object. Core allocates it
//!   (`NVSDK_NGX_D3D12_AllocateParameters`) and exports the `Set*` helpers the
//!   viewer writes it through; `params` also carries a complete Rust
//!   implementation of the object (eight `Set` / eight `Get` overloads plus
//!   `Reset`) as the fallback for drivers without those exports.
//! * `NVSDK_NGX_Result_Success` is **`0x1`**, not 0: every result is checked
//!   with `NVSDK_NGX_SUCCEED` semantics (`value & 0xfff0_0000 != 0xbad0_0000`)
//!   and the failing codes are reported by name (`0xbad00001` is
//!   `FAIL_FeatureNotSupported`, i.e. "not an RTX GPU / driver too old").
//! * Color is perceptual sRGB and depth is reversed (near→1, far→0), so the
//!   DLSS feature is created with `NVSDK_NGX_DLSS_Feature_Flags_DepthInverted`
//!   and without `IsHDR`. Motion vectors are in the sign convention DLSS
//!   documents (`previous - current` in pixels, y down); the reconstructed pass
//!   is the same one FSR 2/3 and XeSS consume.
//! * The camera is jittered with an in-tree Halton(2,3) sequence (16 phases),
//!   matching the documented `[-0.5, 0.5]` pixel jitter DLSS expects, and the
//!   same offset is passed as `Jitter.Offset.X/Y`.
//! * Motion vectors are reconstructed from depth only, so they describe
//!   *camera* motion; actors that move in world space are reprojected as if
//!   they had not moved. There is no per-object velocity pass
//!   (see docs/blockers.md B-5).
//! * Frame generation is implemented against `nvngx_dlssg.dll`'s
//!   `NVSDK_NGX_Feature_FrameGeneration` feature: the viewer owns a flip-model
//!   swapchain for the window (the same hand-over the other two frame
//!   generators use), hands DLSS-G the composition, the depth buffer and the
//!   motion vectors, and presents the real frame and the generated one.
//! * wgpu and the native DLSS work synchronise through queue ordering on the
//!   single D3D12 direct queue wgpu-hal creates: wgpu submits, the DLSS command
//!   list executes, then wgpu composites/presents. Barriers inside the native
//!   list move resources into and out of the states NGX expects
//!   (`NON_PIXEL_SHADER_RESOURCE` for the inputs, `UNORDERED_ACCESS` for the
//!   output).
//!
//! ## What is not verified
//!
//! No machine this code is developed on has a GPU, and the CI runners do not
//! either. Everything past "does it compile on Windows" is runtime-unverified:
//! the jitter sign convention, the motion-vector direction and even basic
//! operation. If context creation or a dispatch fails, the viewer logs the
//! error (stderr *and* the always-on `session.log` in the diagnostics folder),
//! shows the reason in the on-screen fallback note, and falls back to the
//! bilinear path for the rest of the session instead of crashing.
//!
//! ## Non-Windows
//!
//! The whole module is a stub off Windows: `DlssUpscaler::new` and
//! `DlssFrameGeneration::new` always fail with "not supported on this
//! platform", so Linux builds stay green and the Windows build is the compile
//! authority.

/// Single channel for every DLSS diagnostic: stderr for console runs, and the
/// always-on `session.log` in the diagnostics folder (the viewer is a
/// GUI-subsystem app — stderr alone vanishes when it is launched from
/// Explorer). Never per-frame; call sites are lifecycle events.
pub(crate) fn diag(line: &str) {
    eprintln!("{line}");
    crate::diagnostics::session(line);
}

/// NGX reports success as `0x1`; every failure shares the `0xbad00000` mask.
pub(crate) fn succeeded(result: u32) -> bool {
    (result & 0xfff0_0000) != 0xbad0_0000
}

/// Human-readable name for an `NVSDK_NGX_Result`, for the session log.
pub(crate) fn result_name(result: u32) -> String {
    let base = result & 0xffff;
    let name = match base {
        0x0000 => "SUCCESS",
        0x0001 => "FAIL_FeatureNotSupported",
        0x0002 => "FAIL_PlatformError",
        0x0003 => "FAIL_FeatureAlreadyExists",
        0x0004 => "FAIL_FeatureNotFound",
        0x0005 => "FAIL_InvalidParameter",
        0x0006 => "FAIL_ScratchBufferTooSmall",
        0x0007 => "FAIL_NotInitialized",
        0x0008 => "FAIL_UnsupportedInputFormat",
        0x0009 => "FAIL_RWFlagMissing",
        0x000a => "FAIL_MissingInput",
        0x000b => "FAIL_UnableToInitializeFeature",
        0x000c => "FAIL_OutOfDate",
        0x000d => "FAIL_OutOfGPUMemory",
        0x000e => "FAIL_UnsupportedFormat",
        0x000f => "FAIL_UnableToWriteToAppDataPath",
        0x0010 => "FAIL_UnsupportedParameter",
        0x0011 => "FAIL_Denied",
        0x0012 => "FAIL_NotImplemented",
        _ => "unknown",
    };
    format!("0x{result:08x} {name}")
}

#[cfg(windows)]
mod fg;
#[cfg(windows)]
mod imp;
#[cfg(windows)]
mod loader;
// The parameter table is plain Rust and unit-tested on every platform; only
// the Windows build drives it.
#[cfg_attr(not(windows), allow(dead_code))]
mod params;
#[cfg(windows)]
pub use fg::*;
#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
mod imp_stub {
    use glam::Mat4;

    /// Stub of the DLSS upscaler; always fails to create.
    pub struct DlssUpscaler;

    impl DlssUpscaler {
        pub fn new(
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _sdk_dir: &str,
            _render_extent: (u32, u32),
            _display_extent: (u32, u32),
            _format: wgpu::TextureFormat,
            _quality: crate::UpscaleQuality,
        ) -> Result<DlssUpscaler, String> {
            Err("DLSS is only supported on Windows with a DX12 backend".to_string())
        }

        pub fn next_jitter(&mut self) -> (f32, f32) {
            (0.0, 0.0)
        }

        pub fn jittered_matrix(&self, matrix: Mat4) -> Mat4 {
            matrix
        }

        pub fn jitter(&self) -> (f32, f32) {
            (0.0, 0.0)
        }

        pub fn reset_history(&mut self) {}

        pub fn render_extent(&self) -> (u32, u32) {
            (0, 0)
        }

        pub fn display_extent(&self) -> (u32, u32) {
            (0, 0)
        }

        pub fn quality_name(&self) -> &'static str {
            "dlss"
        }

        pub fn record(
            &mut self,
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _color: &wgpu::Texture,
            _depth: &wgpu::Texture,
            _unjittered_vp: &Mat4,
        ) -> Result<(), String> {
            Err("DLSS is only supported on Windows with a DX12 backend".to_string())
        }

        pub fn composite(
            &self,
            _device: &wgpu::Device,
            _queue: &wgpu::Queue,
            _target: &wgpu::TextureView,
        ) {
        }
    }
}

#[cfg(not(windows))]
mod fg_stub {
    /// Stub of the DLSS frame-generation pipeline; always fails to create.
    pub struct DlssFrameGeneration;

    impl DlssFrameGeneration {
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
            _generated_frames: u32,
        ) -> Result<DlssFrameGeneration, String> {
            Err(
                "DLSS Frame Generation is only supported on Windows with a DX12 backend"
                    .to_string(),
            )
        }

        pub fn compose_view(&self) -> wgpu::TextureView {
            unimplemented!()
        }

        /// The Windows path draws the UI into a copy of the composition.
        pub fn ui_view(&self) -> wgpu::TextureView {
            unimplemented!()
        }

        /// The Windows path hands NGX a UI-only surface (`DLSSG.UI`).
        pub fn ui_only_view(&self) -> Option<wgpu::TextureView> {
            None
        }

        /// The Windows path pre-fills the UI surface with the composition.
        pub fn ui_surface_prefilled(&self) -> bool {
            true
        }

        /// The Windows path separates the UI from the interpolated image.
        pub fn ui_in_composition(&self) -> bool {
            false
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
            _unjittered_vp: &glam::Mat4,
            _reset: bool,
            _lens: crate::fsr::CameraLens,
            _camera_pos: [f32; 3],
            _camera_up: [f32; 3],
            _camera_right: [f32; 3],
            _camera_forward: [f32; 3],
        ) -> Result<(), String> {
            unimplemented!()
        }

        pub fn render_submit_end(&mut self) {}

        pub fn finish(&mut self, _ui_visible: bool, _jitter: (f32, f32)) -> Result<(), String> {
            unimplemented!()
        }

        pub fn frames_presented(&self) -> u64 {
            0
        }
    }
}

#[cfg(not(windows))]
pub use fg_stub::DlssFrameGeneration;
#[cfg(not(windows))]
pub use imp_stub::DlssUpscaler;
