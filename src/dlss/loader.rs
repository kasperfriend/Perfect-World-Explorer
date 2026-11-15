//! Eager `LoadLibraryW`/`GetProcAddress` loader for the NGX entry points.
//!
//! Two DLLs share this API: `nvngx_dlss.dll` (super resolution) and
//! `nvngx_dlssg.dll` (frame generation). Each exports the same
//! `NVSDK_NGX_D3D12_*` bootstrap, but they are separate NGX *instances* —
//! each gets its own `Init_Ext`/`Shutdown1` pair — so one [`NgxApi`] wraps
//! exactly one DLL.
//!
//! ## Which ABI this is
//!
//! The vendored headers document two signatures for the same exported names
//! ("functions under the same name and different function signatures exist
//! between the NGX SDK, NGX Core (driver), and NGX Snippets"): the *SDK*
//! flavour takes an `NVSDK_NGX_FeatureCommonInfo*` and is what an application
//! links against when it uses `nvsdk_ngx_*.lib` — that runtime then loads
//! `nvngx_dlss.dll` as a feature plugin and talks to it over the *snippet*
//! ABI, where the same call takes an `NVSDK_NGX_Parameter*` instead:
//!
//! ```c
//! // NGX_SNIPPET_BUILD flavour, what the DLLs export:
//! NVSDK_NGX_D3D12_Init_Ext(u64 appId, const wchar_w *appDataPath,
//!                          ID3D12Device *device, NVSDK_NGX_Version version,
//!                          const NVSDK_NGX_Parameter *params);
//! ```
//!
//! The viewer loads the DLLs directly (no `nvsdk_ngx_*.lib`, no NGX runtime
//! DLL — both of which NVIDIA only ships inside the `developer.nvidia.com`
//! SDK download), so it stands in for NGX Core and uses the snippet ABI with
//! its own parameter table (`super::params`). That is the same arrangement the
//! community integrations use, and it keeps the vendored files to the two
//! DLLs plus the headers.
//!
//! Nothing is loaded implicitly: every export the engine uses is resolved at
//! context creation, so a wrong-version SDK fails with a named missing export
//! instead of a crash mid-frame.

use super::params::NgxParams;
use crate::xess::loader::{free_sdk_dll, load_sdk_dll};
use std::ffi::c_void;
use winapi::shared::minwindef::HMODULE;

/// `NVSDK_NGX_VERSION_API_MACRO` from the vendored `nvsdk_ngx_defs.h`
/// (`NGX_VERSION_DOT 1.5.0`).
pub(crate) const NGX_VERSION_API: i32 = 0x0000_0015;

/// `NVSDK_NGX_Feature_SuperSampling`.
pub(crate) const NGX_FEATURE_SUPER_SAMPLING: i32 = 1;
/// `NVSDK_NGX_Feature_FrameGeneration`.
pub(crate) const NGX_FEATURE_FRAME_GENERATION: i32 = 11;

type PfnInitExt = unsafe extern "C" fn(u64, *const u16, *mut c_void, i32, *mut c_void) -> u32;
type PfnCreateFeature =
    unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut *mut c_void) -> u32;
type PfnEvaluateFeature =
    unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void, *const c_void) -> u32;
type PfnReleaseFeature = unsafe extern "C" fn(*mut c_void) -> u32;
type PfnShutdown1 = unsafe extern "C" fn(*mut c_void) -> u32;

/// The NGX API surface one feature DLL provides.
pub(crate) struct NgxApi {
    handle: HMODULE,
    pub(crate) init_ext: PfnInitExt,
    pub(crate) create_feature: PfnCreateFeature,
    pub(crate) evaluate_feature: PfnEvaluateFeature,
    pub(crate) release_feature: PfnReleaseFeature,
    pub(crate) shutdown1: PfnShutdown1,
    /// True once `init_ext` returned success, so `Drop` only shuts down a
    /// context that exists.
    pub(crate) initialised: bool,
    /// The device the context was created for, needed by `Shutdown1`.
    pub(crate) device: *mut c_void,
}

impl NgxApi {
    pub(crate) fn load(sdk_dir: &str, dll_name: &str) -> Result<Self, String> {
        const EXPORTS: &[&[u8]] = &[
            b"NVSDK_NGX_D3D12_Init_Ext\0",
            b"NVSDK_NGX_D3D12_CreateFeature\0",
            b"NVSDK_NGX_D3D12_EvaluateFeature\0",
            b"NVSDK_NGX_D3D12_ReleaseFeature\0",
            b"NVSDK_NGX_D3D12_Shutdown1\0",
        ];
        // The two names are spelled out so `tools/verify-vendor-sdks.py` can
        // check the requested exports against each DLL's export table.
        let (handle, exports) = match dll_name {
            "nvngx_dlss.dll" => load_sdk_dll(sdk_dir, "nvngx_dlss.dll", EXPORTS)?,
            "nvngx_dlssg.dll" => load_sdk_dll(sdk_dir, "nvngx_dlssg.dll", EXPORTS)?,
            other => return Err(format!("{other} is not an NGX feature library this viewer loads")),
        };
        Ok(Self {
            handle,
            init_ext: unsafe { std::mem::transmute(exports[0]) },
            create_feature: unsafe { std::mem::transmute(exports[1]) },
            evaluate_feature: unsafe { std::mem::transmute(exports[2]) },
            release_feature: unsafe { std::mem::transmute(exports[3]) },
            shutdown1: unsafe { std::mem::transmute(exports[4]) },
            initialised: false,
            device: std::ptr::null_mut(),
        })
    }

    /// `NVSDK_NGX_D3D12_Init_Ext` (snippet ABI).
    ///
    /// `app_dir` is the writable folder NGX puts its logs in; `app_id` is the
    /// identifier NVIDIA hands out to registered applications. Neither the
    /// vendor nor the SDK validates an unregistered id, and the value only
    /// names the log files, so a stable viewer-specific constant is used.
    pub(crate) fn init(
        &mut self,
        app_id: u64,
        app_dir: &str,
        device: *mut c_void,
        params: &mut NgxParams,
    ) -> Result<(), String> {
        let wide: Vec<u16> = app_dir.encode_utf16().chain(std::iter::once(0)).collect();
        let table = params.as_ptr();
        let result =
            unsafe { (self.init_ext)(app_id, wide.as_ptr(), device, NGX_VERSION_API, table) };
        if !super::succeeded(result) {
            return Err(format!(
                "NVSDK_NGX_D3D12_Init_Ext failed ({}) — an RTX GPU with a current driver is required",
                super::result_name(result)
            ));
        }
        self.initialised = true;
        self.device = device;
        Ok(())
    }
}

impl Drop for NgxApi {
    fn drop(&mut self) {
        if self.initialised && !self.device.is_null() {
            unsafe {
                (self.shutdown1)(self.device);
            }
            self.initialised = false;
        }
        unsafe { free_sdk_dll(self.handle) };
    }
}
