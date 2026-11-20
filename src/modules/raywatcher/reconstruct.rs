//! Ray reconstruction: the vendor networks that replace the in-house denoiser's
//! output just before the upscaler.
//!
//! Two providers, one interface. Both take the same inputs — noisy ray signals,
//! the G-buffer, motion vectors, and the ray-specific guides (hit distances,
//! ray directions, normals packed with roughness) — and produce a cleaner image
//! at the render extent. Which one runs is a module setting:
//!
//! * **DLSS Ray Reconstruction** (`nvngx_dlssd.dll`, NGX feature
//!   `NVSDK_NGX_Feature_RayReconstruction`) on RTX hardware, through the same
//!   NGX loader the DLSS upscaler already uses,
//! * **FSR Ray Regeneration** (`amd_fidelityfx_denoiser_dx12.dll`, the FFX
//!   denoiser API) on DX12, RDNA hardware for the machine-learning path and any
//!   other DX12 GPU for the SDK's own spatial fallback.
//!
//! Both are *optional*: a missing DLL, a non-Windows build or an unsupported
//! adapter leaves the provider unavailable, the tab says so, and the in-house
//! denoiser keeps running. Nothing here is on the critical path of a frame that
//! does not select it.

use std::path::Path;

/// Which provider a frame will use. Mirrors
/// [`super::plan::RayReconstruction`] but keeps the module's own vocabulary out
/// of the FFI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    DlssRr,
    FsrRr,
}

/// What a provider reports about itself. `available` is the gate the plan
/// consults; `note` is what the tab shows either way.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub available: bool,
    pub note: String,
}

impl Probe {
    fn unavailable(note: impl Into<String>) -> Self {
        Self {
            available: false,
            note: note.into(),
        }
    }

    fn available(note: impl Into<String>) -> Self {
        Self {
            available: true,
            note: note.into(),
        }
    }
}

/// The DLL names the two providers load. Kept public so the packaging scripts,
/// the verifier and the tab all read one list.
pub const DLSS_RR_DLL: &str = "nvngx_dlssd.dll";
pub const FSR_RR_DLL: &str = "amd_fidelityfx_denoiser_dx12.dll";

/// Whether `dll` is present in `sdk_dir` (or beside the executable, where the
/// vendored loader looks first).
pub fn dll_present(sdk_dir: &Path, dll: &str) -> bool {
    if sdk_dir.join(dll).is_file() {
        return true;
    }
    // The FFX loader resolves the denoiser beside *itself*, so the loader's own
    // folder counts too.
    sdk_dir.join("amd_fidelityfx_loader_dx12.dll").is_file() && sdk_dir.join(dll).is_file()
}

/// Probe both providers without creating a device.
///
/// This is the decision the availability gate needs, and it is deliberately
/// separate from context creation: a DLL can be present and the adapter still
/// refuse the feature, which is the case the plan's `reconstruction_available`
/// predicate re-checks once a device exists.
pub fn probe_offline(sdk_dir: &Path, platform_supported: bool) -> (Probe, Probe) {
    if !platform_supported {
        let note = "Windows + DX12 only".to_string();
        return (Probe::unavailable(note.clone()), Probe::unavailable(note));
    }
    let dlss = if dll_present(sdk_dir, DLSS_RR_DLL) {
        Probe::available(format!("{DLSS_RR_DLL} found"))
    } else {
        Probe::unavailable(format!("{DLSS_RR_DLL} not found in {}", sdk_dir.display()))
    };
    let fsr = if dll_present(sdk_dir, FSR_RR_DLL) {
        Probe::available(format!("{FSR_RR_DLL} found"))
    } else {
        Probe::unavailable(format!("{FSR_RR_DLL} not found in {}", sdk_dir.display()))
    };
    (dlss, fsr)
}

#[cfg(windows)]
mod ffi {
    //! The FFX denoiser API, transcribed from `sdk/include/denoisers/ffx_denoiser.h`.
    //!
    //! The FFX API is versioned by descriptor type, not by symbol: every entry
    //! point is one of the five `ffx*` functions, and which effect runs is
    //! decided by the header `type` of the descriptor passed in. The denoiser is
    //! effect id `0x00050000`, so a descriptor type is
    //! `0x00050000 << ... | sub_id` — the same `FFX_API_MAKE_EFFECT_SUB_ID`
    //! macro the header expands.
    #![allow(dead_code)]

    use super::Probe;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    // `load` and `probe` take `&Path`, and a child module does not see the
    // parent's imports — this has to be here for the Windows build to resolve
    // it.
    use std::path::Path;

    pub const FFX_DENOISER_VERSION: u32 = (1 << 22) | (2 << 12) | 1;
    pub const FFX_API_EFFECT_ID_DENOISER: u64 = 0x0005_0000;
    /// `FFX_API_MAKE_EFFECT_SUB_ID(effect, sub)` = `effect | sub << 16`.
    pub const fn sub_id(effect: u64, sub: u64) -> u64 {
        effect | (sub << 16)
    }
    pub const CREATE_DESC_TYPE_DENOISER: u64 = sub_id(FFX_API_EFFECT_ID_DENOISER, 0x01);
    pub const CONFIGURE_DESC_TYPE_DENOISER_KEYVALUE: u64 = sub_id(FFX_API_EFFECT_ID_DENOISER, 0x21);
    pub const DISPATCH_DESC_TYPE_DENOISER: u64 = sub_id(FFX_API_EFFECT_ID_DENOISER, 0x41);
    pub const DISPATCH_DESC_TYPE_DENOISER_AMBIENT_OCCLUSION: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x43);
    pub const DISPATCH_DESC_TYPE_DENOISER_DIRECT_DIFFUSE: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x44);
    pub const DISPATCH_DESC_TYPE_DENOISER_DIRECT_SPECULAR: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x45);
    pub const DISPATCH_DESC_TYPE_DENOISER_DOMINANT_LIGHT: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x46);
    pub const DISPATCH_DESC_TYPE_DENOISER_INDIRECT_DIFFUSE: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x47);
    pub const DISPATCH_DESC_TYPE_DENOISER_INDIRECT_SPECULAR: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x48);
    pub const DISPATCH_DESC_TYPE_DENOISER_SPECULAR_OCCLUSION: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x49);

    pub const SIGNAL_AMBIENT_OCCLUSION: u32 = 1;
    pub const SIGNAL_DIRECT_DIFFUSE: u32 = 2;
    pub const SIGNAL_DIRECT_SPECULAR: u32 = 4;
    pub const SIGNAL_DOMINANT_LIGHT: u32 = 8;
    pub const SIGNAL_INDIRECT_DIFFUSE: u32 = 16;
    pub const SIGNAL_INDIRECT_SPECULAR: u32 = 32;
    pub const SIGNAL_SPECULAR_OCCLUSION: u32 = 64;

    pub const DISPATCH_FLAG_RESET: u32 = 1;
    pub const DISPATCH_FLAG_NON_ENCODED_ALBEDO: u32 = 2;

    pub const CREATE_FLAG_VALIDATION: u32 = 2;
    pub const RETURN_OK: u32 = 0;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Header {
        pub r#type: u64,
        pub p_next: *mut Header,
    }

    impl Header {
        pub fn new(r#type: u64) -> Self {
            Self {
                r#type,
                p_next: std::ptr::null_mut(),
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Dimensions2D {
        pub width: u32,
        pub height: u32,
    }

    /// `FfxApiFloatCoords3D`.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Coords3D {
        pub x: f32,
        pub y: f32,
        pub z: f32,
    }

    /// `FfxApiResource` — the renderer's own texture handle plus the description
    /// the SDK needs to interpret it. This mirrors `fsr::imp::FfxApiResource`,
    /// which is `pub(crate)` and private to that module.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Resource {
        pub resource: *mut c_void,
        pub kind: u32,
        pub format: u32,
        pub width: u32,
        pub height: u32,
        pub depth: u32,
        pub mip_count: u32,
        pub flags: u32,
        pub usage: u32,
        pub state: u32,
    }

    impl Resource {
        pub const EMPTY: Resource = Resource {
            resource: std::ptr::null_mut(),
            kind: 0,
            format: 0,
            width: 0,
            height: 0,
            depth: 0,
            mip_count: 0,
            flags: 0,
            usage: 0,
            state: 0,
        };
    }

    pub const RESOURCE_TYPE_TEXTURE2D: u32 = 2;
    pub const RESOURCE_USAGE_UAV: u32 = 1 << 1;
    pub const RESOURCE_STATE_UNORDERED_ACCESS: u32 = 1 << 1;
    pub const RESOURCE_STATE_PIXEL_COMPUTE_READ: u32 = (1 << 2) | (1 << 3);
    pub const SURFACE_FORMAT_R16G16B16A16_FLOAT: u32 = 20;
    pub const SURFACE_FORMAT_R32_FLOAT: u32 = 28;
    pub const SURFACE_FORMAT_R8G8B8A8_UNORM: u32 = 10;
    pub const SURFACE_FORMAT_R16G16_FLOAT: u32 = 18;
    pub const SURFACE_FORMAT_R16_UINT: u32 = 41;

    #[repr(C)]
    pub struct CreateContextDescDenoiser {
        pub header: Header,
        pub version: u32,
        pub max_render_size: Dimensions2D,
        pub signal_flags: u32,
        pub checkerboard_signal_flags: u32,
        pub flags: u32,
    }

    #[repr(C)]
    pub struct DispatchDescDenoiser {
        pub header: Header,
        pub command_list: *mut c_void,
        pub linear_depth: Resource,
        pub motion_vectors: Resource,
        pub normals: Resource,
        pub specular_albedo: Resource,
        pub diffuse_albedo: Resource,
        pub motion_vector_scale: Coords3D,
        pub jitter_offsets: [f32; 2],
        pub camera_position_delta: Coords3D,
        pub view: [[f32; 4]; 4],
        pub projection: [[f32; 4]; 4],
        pub linear_depth_bounds: [f32; 2],
        pub render_size: Dimensions2D,
        pub frame_index: u32,
        pub flags: u32,
    }

    /// `FfxApiDenoiserSignal`: one input, one output, and the checkerboard
    /// origin (zero when that signal is not checkerboarded).
    #[repr(C)]
    pub struct Signal {
        pub input: Resource,
        pub output: Resource,
        pub checkerboard_origin: u32,
    }

    #[repr(C)]
    pub struct DispatchDescSignal {
        pub header: Header,
        pub signal: Signal,
    }

    #[repr(C)]
    pub struct DispatchDescDominantLight {
        pub header: Header,
        pub signal: Signal,
        pub direction: Coords3D,
        pub emission: Coords3D,
        pub angular_radius: f32,
    }

    pub type PfnCreateContext =
        unsafe extern "C" fn(*mut *mut c_void, *mut Header, *const c_void) -> u32;
    pub type PfnDestroyContext = unsafe extern "C" fn(*mut *mut c_void, *const c_void) -> u32;
    pub type PfnQuery = unsafe extern "C" fn(*mut *mut c_void, *mut Header) -> u32;
    pub type PfnDispatch = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> u32;
    pub type PfnConfigure = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> u32;

    /// The five FFX entry points, loaded from the denoiser DLL.
    pub struct FfxDenoiser {
        handle: winapi::shared::minwindef::HMODULE,
        pub create_context: PfnCreateContext,
        pub destroy_context: PfnDestroyContext,
        pub query: PfnQuery,
        pub dispatch: PfnDispatch,
        pub configure: PfnConfigure,
    }

    impl Drop for FfxDenoiser {
        fn drop(&mut self) {
            unsafe { winapi::um::libloaderapi::FreeLibrary(self.handle) };
        }
    }

    impl FfxDenoiser {
        /// Load the denoiser from `dir`. The SDK is explicit that the denoiser
        /// DLL is loaded directly (not through the FFX loader): its exports are
        /// the five entry points, like the upscaler's.
        pub fn load(dir: &Path) -> Result<Self, String> {
            let candidate = dir.join(super::FSR_RR_DLL);
            if !candidate.is_file() {
                return Err(format!("{} not found", candidate.display()));
            }
            let wide: Vec<u16> = candidate
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let handle = unsafe { winapi::um::libloaderapi::LoadLibraryW(wide.as_ptr()) };
            if handle.is_null() {
                return Err(format!("LoadLibraryW({}) failed", candidate.display()));
            }
            let get = |name: &[u8]| -> Result<*const c_void, String> {
                let proc = unsafe {
                    winapi::um::libloaderapi::GetProcAddress(handle, name.as_ptr() as *const i8)
                };
                if proc.is_null() {
                    Err(format!(
                        "GetProcAddress({}) failed",
                        String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
                    ))
                } else {
                    Ok(proc as *const c_void)
                }
            };
            let loaded: Result<Self, String> = (|| {
                Ok(Self {
                    handle,
                    create_context: unsafe { std::mem::transmute(get(b"ffxCreateContext\0")?) },
                    destroy_context: unsafe { std::mem::transmute(get(b"ffxDestroyContext\0")?) },
                    query: unsafe { std::mem::transmute(get(b"ffxQuery\0")?) },
                    dispatch: unsafe { std::mem::transmute(get(b"ffxDispatch\0")?) },
                    configure: unsafe { std::mem::transmute(get(b"ffxConfigure\0")?) },
                })
            })();
            if loaded.is_err() {
                unsafe { winapi::um::libloaderapi::FreeLibrary(handle) };
            }
            loaded
        }
    }

    /// The signal set Raywatcher feeds the denoiser: exactly the channels it
    /// produces. `AMBIENT_OCCLUSION` is the ray-hit visibility image,
    /// `DIRECT_DIFFUSE`/`DIRECT_SPECULAR` are the sun terms, and the two
    /// indirect channels are what the bounce rays gathered.
    pub fn signal_flags(effects: super::super::plan::Effects) -> u32 {
        let mut flags = 0;
        if effects.ambient_occlusion {
            flags |= SIGNAL_AMBIENT_OCCLUSION;
        }
        if effects.lighting {
            flags |= SIGNAL_DIRECT_DIFFUSE;
        }
        if effects.reflections {
            flags |= SIGNAL_DIRECT_SPECULAR;
        }
        if effects.global_illumination {
            flags |= SIGNAL_INDIRECT_DIFFUSE;
        }
        if effects.shadows {
            flags |= SIGNAL_DOMINANT_LIGHT;
        }
        flags
    }

    /// Probe the DLL without creating a device.
    pub fn probe(dir: &Path) -> Probe {
        match FfxDenoiser::load(dir) {
            Ok(_) => Probe::available(format!("{} loaded", super::FSR_RR_DLL)),
            Err(error) => Probe::unavailable(error),
        }
    }
}

#[cfg(windows)]
pub use ffi::FfxDenoiser;

/// Non-Windows has no providers, and the module must still compile and test.
#[cfg(not(windows))]
mod ffi_stub {
    use super::Probe;
    use std::path::Path;

    /// A placeholder with the same shape as the Windows loader so the callers
    /// above are identical on every platform.
    #[derive(Debug)]
    pub struct FfxDenoiser {
        pub _private: (),
    }

    impl FfxDenoiser {
        pub fn load(_dir: &Path) -> Result<Self, String> {
            Err("the FFX denoiser is Windows + DX12 only".to_string())
        }
    }

    pub fn probe(_dir: &Path) -> Probe {
        Probe::unavailable("the FFX denoiser is Windows + DX12 only".to_string())
    }
}

#[cfg(not(windows))]
use ffi_stub as ffi;
#[cfg(not(windows))]
pub use ffi_stub::FfxDenoiser;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_provider_dlls_are_named_exactly_as_the_sdks_ship_them() {
        assert_eq!(DLSS_RR_DLL, "nvngx_dlssd.dll");
        assert_eq!(FSR_RR_DLL, "amd_fidelityfx_denoiser_dx12.dll");
    }

    #[test]
    fn probing_on_an_unsupported_platform_explains_both_providers() {
        let (dlss, fsr) = probe_offline(Path::new("/nonexistent"), false);
        assert!(!dlss.available);
        assert!(!fsr.available);
        assert!(dlss.note.contains("Windows"));
        assert!(fsr.note.contains("Windows"));
    }

    #[test]
    fn a_missing_dll_is_reported_with_the_folder_that_was_searched() {
        let dir = std::env::temp_dir().join("raywatcher-reconstruct-probe-empty");
        let (_, fsr) = probe_offline(&dir, true);
        assert!(!fsr.available);
        assert!(fsr.note.contains("amd_fidelityfx_denoiser_dx12.dll"));
        assert!(fsr.note.contains(&dir.display().to_string()));
    }

    #[test]
    fn a_present_dll_is_reported_as_available() {
        // The probe only checks presence; creating a context is the device's
        // job. A real file in a temp folder is enough to pin that contract.
        let dir = std::env::temp_dir().join("raywatcher-reconstruct-probe-present");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(FSR_RR_DLL);
        std::fs::write(&path, b"placeholder").expect("write");
        let (_, fsr) = probe_offline(&dir, true);
        assert!(fsr.available, "{}", fsr.note);
        assert!(fsr.note.contains("found"));
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(all(test, windows))]
mod ffi_tests {
    use super::ffi::*;
    use super::FSR_RR_DLL;

    #[test]
    fn the_descriptor_types_match_the_sdk_header() {
        // `FFX_API_MAKE_EFFECT_SUB_ID(FFX_API_EFFECT_ID_DENOISER, sub)`.
        assert_eq!(CREATE_DESC_TYPE_DENOISER, 0x0005_0100);
        assert_eq!(CONFIGURE_DESC_TYPE_DENOISER_KEYVALUE, 0x0005_2100);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER, 0x0005_4100);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_AMBIENT_OCCLUSION, 0x0005_4300);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_DIRECT_DIFFUSE, 0x0005_4400);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_DIRECT_SPECULAR, 0x0005_4500);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_DOMINANT_LIGHT, 0x0005_4600);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_INDIRECT_DIFFUSE, 0x0005_4700);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_INDIRECT_SPECULAR, 0x0005_4800);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_SPECULAR_OCCLUSION, 0x0005_4900);
        // The version the SDK header demands.
        assert_eq!(FFX_DENOISER_VERSION, (1 << 22) | (2 << 12) | 1);
    }

    #[test]
    fn the_signal_flags_are_the_ones_the_header_defines() {
        assert_eq!(
            [
                SIGNAL_AMBIENT_OCCLUSION,
                SIGNAL_DIRECT_DIFFUSE,
                SIGNAL_DIRECT_SPECULAR,
                SIGNAL_DOMINANT_LIGHT,
                SIGNAL_INDIRECT_DIFFUSE,
                SIGNAL_INDIRECT_SPECULAR,
                SIGNAL_SPECULAR_OCCLUSION,
            ],
            [1, 2, 4, 8, 16, 32, 64]
        );
    }
}
