//! NGX bootstrap: the driver's NGX **Core** (`nvngx.dll`) driving the vendored
//! DLSS feature DLLs.
//!
//! ## Why the core and not the feature DLL
//!
//! `nvngx_dlss.dll` and `nvngx_dlssg.dll` are NGX *snippets*: they are written
//! to be loaded and driven by NGX Core, which ships inside the display driver
//! (`_nvngx.dll` in the `DriverStore` since driver 471.11). `Init_Ext`, the
//! Core-to-snippet entry point, refuses to run when the application calls it —
//! its prologue resolves the module the call came from
//! (`GetModuleHandleExW(FROM_ADDRESS, <return address>, ...)`), takes that
//! module's file name, and requires it to contain `nvngx.dll`; anything else is
//! answered with `0xBAD00002` (`FAIL_PlatformError`) and the log line "Not
//! called from NGX runtime". That is why an earlier revision of this module,
//! which loaded the snippet itself and called `Init_Ext` on it, reported
//! `NVSDK_NGX_D3D12_Init_Ext failed (0xbad00002 FAIL_PlatformError)` on
//! perfectly good RTX hardware (an RTX 3080 among them) for every DLSS version.
//!
//! So the viewer now does what an `nvsdk_ngx_*.lib` application does:
//!
//! 1. load NGX Core from the driver folder,
//! 2. initialise it once for the wgpu device,
//! 3. create and evaluate both features through it.
//!
//! Core loads the snippet itself. `NVSDK_NGX_FeatureCommonInfo.PathListInfo`
//! names the folders to search *before* the application directory, so the
//! viewer puts its own `sdk` folder first: the DLSS build that ships with the
//! viewer is the one that runs, not whatever copy the driver keeps.
//!
//! ## Which ABI
//!
//! Core exposes the **SDK** side of the API — `NVSDK_NGX_D3D12_Init`,
//! `NVSDK_NGX_D3D12_Init_with_ProjectID`, `…CreateFeature`, `…EvaluateFeature`,
//! `…ReleaseFeature`, `…Shutdown1`, `…AllocateParameters`,
//! `…DestroyParameters`, `…GetCapabilityParameters` and the
//! `NVSDK_NGX_Parameter_Set*` / `Get*` helpers (see `nvsdk_ngx.h` and
//! `nvsdk_ngx_params.h`; the headers mark the snippet exclusives with
//! `NGX_SNIPPET_BUILD`). `CreateFeature`/`EvaluateFeature` take the same
//! arguments in both flavours, so `super::imp` and `super::fg` only needed the
//! module they call to change.
//!
//! Initialisation uses `NVSDK_NGX_D3D12_Init_with_ProjectID` with a GUID-shaped
//! project id and `NVSDK_NGX_ENGINE_TYPE_CUSTOM` (the documented way for an
//! application without an NVIDIA-issued application id), and falls back to
//! `NVSDK_NGX_D3D12_Init` if a driver build rejects that.
//!
//! The `0xBAD00002` above is specific to `Init_Ext`, which is the
//! **Core-to-snippet** entry point (`nvsdk_ngx.h` groups it with the
//! `NGX_SNIPPET_BUILD` signatures). `Init` / `Init_with_ProjectID` are the
//! **SDK-to-Core** ones, and they are what shipped titles call — either on the
//! core or, since a snippet resolves the core itself, on the snippet. Both are
//! tried here: the snippet is the last resort after every core module.
//!
//! ## Which module is initialised
//!
//! `nvsdk_ngx_loader.h` (`NGXLoadCoreLibrary`) is the reference for where Core
//! lives: next to the executable, then the driver's `NGXCore` registry path,
//! then the legacy location. Driver 471.11 and later ship it as `_nvngx.dll` in
//! the `DriverStore`; a leftover `nvngx.dll` in `System32` belongs to an older
//! driver and answers `FAIL_OutOfDate` (`0xBAD0000C`, "a function was used which
//! requires a newer version of the NVIDIA Display Driver … than is currently
//! installed") to anything newer than itself.
//!
//! So `ngx_core_candidates` builds the whole list — DriverStore packages
//! newest first, then the executable's and the viewer's `sdk/` folders, then
//! `System32` — and **every** candidate is tried until one initialises. One
//! stale copy used to end the search and disable DLSS for the whole session.
//!
//! ## Diagnostics
//!
//! Everything NGX decides is logged: which DLL was loaded, the application data
//! folder, the result of each init call, NGX's own log lines (through the
//! `NVSDK_NGX_LoggingInfo` callback, so they land in `session.log` instead of a
//! separate `nvngx*.log` nobody reads) and, when Core exports the capability
//! accessor, `SuperSampling.Available` / `SuperSampling.FeatureInitResult`.

use super::params::{NgxMap, NgxParams};
use crate::xess::loader::load_sdk_dll;
use std::ffi::{c_char, c_void, CStr};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use winapi::shared::minwindef::HMODULE;

/// `NVSDK_NGX_VERSION_API_MACRO` from the vendored `nvsdk_ngx_defs.h`
/// (`NGX_VERSION_DOT 1.5.0`).
pub(crate) const NGX_VERSION_API: i32 = 0x0000_0015;

/// `NVSDK_NGX_Feature_SuperSampling`.
pub(crate) const NGX_FEATURE_SUPER_SAMPLING: i32 = 1;
/// `NVSDK_NGX_Feature_FrameGeneration`.
pub(crate) const NGX_FEATURE_FRAME_GENERATION: i32 = 11;

/// `NVSDK_NGX_ENGINE_TYPE_CUSTOM`.
const NGX_ENGINE_TYPE_CUSTOM: i32 = 0;

/// Project id handed to `NVSDK_NGX_D3D12_Init_with_ProjectID`. The driver only
/// validates that it is GUID-shaped and uses it to name the log files, so a
/// stable viewer-specific value is enough (this project has no NVIDIA-issued
/// application id).
const NGX_PROJECT_ID: &str = "5f2c9a41-7d13-4e6b-9c88-1a2b3d4e5f60";
/// Version string reported for that engine.
const NGX_ENGINE_VERSION: &str = "1.0.0";

/// `SuperSampling.Available` / `SuperSampling.FeatureInitResult` — the two
/// capability values that explain a refusal (`nvsdk_ngx_defs.h`).
const P_SUPER_SAMPLING_AVAILABLE: &str = "SuperSampling.Available";
const P_SUPER_SAMPLING_INIT_RESULT: &str = "SuperSampling.FeatureInitResult";

type PfnInit =
    unsafe extern "C" fn(u64, *const u16, *mut c_void, *const NgxFeatureCommonInfo, i32) -> u32;
type PfnInitWithProjectId = unsafe extern "C" fn(
    *const c_char,
    i32,
    *const c_char,
    *const u16,
    *mut c_void,
    *const NgxFeatureCommonInfo,
    i32,
) -> u32;
type PfnCreateFeature =
    unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut *mut c_void) -> u32;
type PfnEvaluateFeature =
    unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void, *const c_void) -> u32;
type PfnReleaseFeature = unsafe extern "C" fn(*mut c_void) -> u32;
type PfnAllocateParameters = unsafe extern "C" fn(*mut *mut c_void) -> u32;
type PfnDestroyParameters = unsafe extern "C" fn(*mut c_void) -> u32;
type PfnGetCapabilityParameters = unsafe extern "C" fn(*mut *mut c_void) -> u32;
type PfnSetU32 = unsafe extern "C" fn(*mut c_void, *const c_char, u32);
type PfnSetI32 = unsafe extern "C" fn(*mut c_void, *const c_char, i32);
type PfnSetF32 = unsafe extern "C" fn(*mut c_void, *const c_char, f32);
type PfnSetPointer = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void);
type PfnGetU32 = unsafe extern "C" fn(*mut c_void, *const c_char, *mut u32) -> u32;

/// `NVSDK_NGX_AppLogCallback` — `NVSDK_CONV` is the platform default calling
/// convention on x64, which is the only target this viewer builds for.
type PfnLogCallback = unsafe extern "C" fn(*const c_char, i32, i32);

/// `NVSDK_NGX_PathListInfo`.
#[repr(C)]
struct NgxPathListInfo {
    path: *const *const u16,
    length: u32,
}

/// `NVSDK_NGX_LoggingInfo`.
#[repr(C)]
struct NgxLoggingInfo {
    logging_callback: Option<PfnLogCallback>,
    minimum_logging_level: i32,
    /// C `bool`.
    disable_other_logging_sinks: u8,
}

/// `NVSDK_NGX_FeatureCommonInfo`.
#[repr(C)]
struct NgxFeatureCommonInfo {
    path_list_info: NgxPathListInfo,
    /// Used internally by NGX; must be null.
    internal_data: *mut c_void,
    logging_info: NgxLoggingInfo,
}

impl Default for NgxFeatureCommonInfo {
    fn default() -> Self {
        Self {
            path_list_info: NgxPathListInfo {
                path: std::ptr::null(),
                length: 0,
            },
            internal_data: std::ptr::null_mut(),
            logging_info: NgxLoggingInfo {
                logging_callback: Some(ngx_log),
                // `NVSDK_NGX_LOGGING_LEVEL_ON`: init-time problems are what we
                // want; VERBOSE also logs per-evaluate chatter.
                minimum_logging_level: 1,
                disable_other_logging_sinks: 0,
            },
        }
    }
}

/// NGX's own log lines, forwarded into `session.log`.
unsafe extern "C" fn ngx_log(message: *const c_char, level: i32, feature: i32) {
    if message.is_null() {
        return;
    }
    let text = CStr::from_ptr(message).to_string_lossy();
    super::diag(&format!("[NGX level {level} feature {feature}] {text}"));
}

/// One initialised NGX Core instance for the process.
///
/// NGX Core is process-global per device, so both features share it: `nvngx.dll`
/// is loaded once, initialised once, and `super::imp` / `super::fg` create their
/// features from the same instance.
pub(crate) struct NgxCore {
    /// Kept for the lifetime of the process; `Drop` never runs in practice.
    _handle: HMODULE,
    /// Where `nvngx.dll` came from, for the ready line.
    path: String,
    /// `NVSDK_NGX_D3D12_CreateFeature`.
    create_feature: PfnCreateFeature,
    /// `NVSDK_NGX_D3D12_EvaluateFeature`.
    evaluate_feature: PfnEvaluateFeature,
    /// `NVSDK_NGX_D3D12_ReleaseFeature`.
    release_feature: PfnReleaseFeature,
    /// `NVSDK_NGX_D3D12_AllocateParameters`, when this build exports it.
    allocate_parameters: Option<PfnAllocateParameters>,
    destroy_parameters: Option<PfnDestroyParameters>,
    get_capability_parameters: Option<PfnGetCapabilityParameters>,
    set_u32: Option<PfnSetU32>,
    set_i32: Option<PfnSetI32>,
    set_f32: Option<PfnSetF32>,
    set_d3d12: Option<PfnSetPointer>,
    set_void: Option<PfnSetPointer>,
    get_u32: Option<PfnGetU32>,
}

// The instance is created and used on the render thread only; the statics below
// exist so both features can reach it, not to share it across threads.
unsafe impl Send for NgxCore {}
unsafe impl Sync for NgxCore {}

static CORE: OnceLock<Result<NgxCore, String>> = OnceLock::new();

/// The process-wide NGX Core, initialised on first use.
///
/// `snippet` is the feature DLL the caller is about to use. It is the last
/// resort for initialisation: a DLSS snippet is itself a valid SDK surface (it
/// forwards to the core it resolves internally), which is how every shipped
/// DLSS title calls `NVSDK_NGX_D3D12_Init_with_ProjectID`.
fn core(
    sdk_dir: &str,
    snippet: Option<&Path>,
    device: *mut c_void,
) -> Result<&'static NgxCore, String> {
    match CORE.get_or_init(|| NgxCore::create(sdk_dir, snippet, device)) {
        Ok(core) => Ok(core),
        Err(error) => Err(error.clone()),
    }
}

/// The per-feature handle. It carries no state of its own: Core owns the
/// instance, and the feature DLLs are loaded by Core.
pub(crate) struct NgxApi {
    core: &'static NgxCore,
    pub(crate) create_feature: PfnCreateFeature,
    pub(crate) evaluate_feature: PfnEvaluateFeature,
    pub(crate) release_feature: PfnReleaseFeature,
}

impl NgxApi {
    /// Prepare NGX for `dll_name` (`nvngx_dlss.dll` or `nvngx_dlssg.dll`).
    ///
    /// Loading the vendored snippet here has two purposes: it proves the file
    /// the packaging script ships is usable before NGX is asked to load it, and
    /// it keeps the file named in `src/`, which is what
    /// `tools/verify-vendor-sdks.py` audits. Core is told to search that same
    /// folder first, so it uses this build rather than the driver's copy.
    pub(crate) fn load(sdk_dir: &str, dll_name: &str, device: *mut c_void) -> Result<Self, String> {
        let (_snippet, _) = load_sdk_dll(sdk_dir, dll_name, &[])?;
        let snippet = sdk_module_path(sdk_dir, dll_name);
        let core = core(sdk_dir, snippet.as_deref(), device)?;
        Ok(Self {
            core,
            create_feature: core.create_feature,
            evaluate_feature: core.evaluate_feature,
            release_feature: core.release_feature,
        })
    }

    /// A `NVSDK_NGX_Parameter` map: NGX's own when Core can allocate one,
    /// otherwise the viewer's table.
    pub(crate) fn new_parameters(&self) -> Box<NgxParams> {
        self.core.new_parameters()
    }

    /// Where NGX Core came from (the ready line names it).
    pub(crate) fn core_path(&self) -> &str {
        &self.core.path
    }
}

impl NgxCore {
    fn create(sdk_dir: &str, snippet: Option<&Path>, device: *mut c_void) -> Result<Self, String> {
        let mut candidates = ngx_core_candidates(sdk_dir);
        if let Some(snippet) = snippet {
            push_candidate(&mut candidates, snippet.to_path_buf());
        }
        if candidates.is_empty() {
            return Err(
                "the NVIDIA NGX runtime (nvngx.dll) was not found: it ships with the display \
                 driver, which keeps it in the DriverStore as `_nvngx.dll` since driver 471.11, \
                 so reinstall the driver with all components (a driver cleaned by \
                 NVCleanstall/DDU can leave it out)"
                    .to_string(),
            );
        }
        // Every candidate is tried, not just the first one that exists: a stale
        // `nvngx.dll` (an old copy left behind in `System32` or next to the
        // executable) answers `FAIL_OutOfDate` — "a function was used which
        // requires a newer version of the NVIDIA Display Driver or feature
        // library than is currently installed", `nvsdk_ngx_defs.h` — to
        // everything it does not implement, including
        // `NVSDK_NGX_D3D12_Init_with_ProjectID`. Letting that end the search is
        // what turned a working RTX install into "NGX refused to initialise".
        let mut failures: Vec<String> = Vec::new();
        for path in candidates {
            match Self::create_at(&path, sdk_dir, device) {
                Ok(core) => {
                    if !failures.is_empty() {
                        super::diag(&format!(
                            "[NGX] {} is the first of {} candidate NGX modules that initialised",
                            path.display(),
                            failures.len() + 1
                        ));
                    }
                    return Ok(core);
                }
                Err(error) => {
                    super::diag(&format!("[NGX] {} cannot be used: {error}", path.display()));
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    failures.push(format!("{name}: {error}"));
                }
            }
        }
        let mut detail = failures.join("; ");
        if detail.chars().count() > 400 {
            detail = detail.chars().take(400).collect::<String>() + "…";
        }
        Err(format!(
            "NGX refused to initialise: none of the {} NVSDK_NGX modules found could be used \
             ({detail}) — {}",
            failures.len(),
            device_hint()
        ))
    }

    /// Load and initialise one candidate module that provides the SDK side of
    /// the NGX API (`nvsdk_ngx.h`: `Init`, `Init_with_ProjectID`,
    /// `CreateFeature`, `EvaluateFeature`, `ReleaseFeature`).
    fn create_at(path: &Path, sdk_dir: &str, device: *mut c_void) -> Result<Self, String> {
        // Mandatory: the whole Core bootstrap.
        const EXPORTS: &[&[u8]] = &[
            b"NVSDK_NGX_D3D12_Init\0",
            b"NVSDK_NGX_D3D12_CreateFeature\0",
            b"NVSDK_NGX_D3D12_EvaluateFeature\0",
            b"NVSDK_NGX_D3D12_ReleaseFeature\0",
        ];
        let (handle, exports) = load_module(path, EXPORTS)?;
        let mut core = Self {
            _handle: handle,
            path: path.to_string_lossy().into_owned(),
            create_feature: unsafe { read_function(exports[1]) },
            evaluate_feature: unsafe { read_function(exports[2]) },
            release_feature: unsafe { read_function(exports[3]) },
            allocate_parameters: None,
            destroy_parameters: None,
            get_capability_parameters: None,
            set_u32: None,
            set_i32: None,
            set_f32: None,
            set_d3d12: None,
            set_void: None,
            get_u32: None,
        };
        // Everything else is optional: older drivers lack parts of it, and the
        // viewer degrades to its own parameter table instead of failing.
        unsafe {
            core.allocate_parameters =
                resolve_function(handle, b"NVSDK_NGX_D3D12_AllocateParameters\0");
            core.destroy_parameters =
                resolve_function(handle, b"NVSDK_NGX_D3D12_DestroyParameters\0");
            core.get_capability_parameters =
                resolve_function(handle, b"NVSDK_NGX_D3D12_GetCapabilityParameters\0");
            core.set_u32 = resolve_function(handle, b"NVSDK_NGX_Parameter_SetUI\0");
            core.set_i32 = resolve_function(handle, b"NVSDK_NGX_Parameter_SetI\0");
            core.set_f32 = resolve_function(handle, b"NVSDK_NGX_Parameter_SetF\0");
            core.set_d3d12 = resolve_function(handle, b"NVSDK_NGX_Parameter_SetD3d12Resource\0");
            core.set_void = resolve_function(handle, b"NVSDK_NGX_Parameter_SetVoidPointer\0");
            core.get_u32 = resolve_function(handle, b"NVSDK_NGX_Parameter_GetUI\0");
        }
        // The module stays loaded even when initialisation fails: NGX keeps
        // process-global state from the attempt, so unloading it would be worse
        // than leaking one handle.
        core.initialise(sdk_dir, device, handle, unsafe {
            read_function(exports[0])
        })?;
        Ok(core)
    }

    fn initialise(
        &self,
        sdk_dir: &str,
        device: *mut c_void,
        handle: HMODULE,
        init: PfnInit,
    ) -> Result<(), String> {
        let init_with_project_id: Option<PfnInitWithProjectId> =
            unsafe { resolve_function(handle, b"NVSDK_NGX_D3D12_Init_with_ProjectID\0") };
        let app_dir = ngx_app_dir();
        super::diag(&format!(
            "[NGX] core {} initialising (application data folder: {app_dir})",
            self.path
        ));

        // Feature search order: the viewer's SDK folder first, so Core picks the
        // vendored DLSS build, then the executable's folder (Core's default).
        let paths = feature_search_paths(sdk_dir);
        let wide: Vec<Vec<u16>> = paths
            .iter()
            .map(|path| path.encode_utf16().chain(std::iter::once(0)).collect())
            .collect();
        let pointers: Vec<*const u16> = wide.iter().map(|path| path.as_ptr()).collect();
        let mut info = NgxFeatureCommonInfo::default();
        if !pointers.is_empty() {
            info.path_list_info = NgxPathListInfo {
                path: pointers.as_ptr(),
                length: pointers.len() as u32,
            };
        }
        let app_dir_wide: Vec<u16> = app_dir.encode_utf16().chain(std::iter::once(0)).collect();
        let project = std::ffi::CString::new(NGX_PROJECT_ID).expect("project id is ASCII");
        let engine = std::ffi::CString::new(NGX_ENGINE_VERSION).expect("engine version is ASCII");

        let mut failures: Vec<String> = Vec::new();
        if let Some(init_with_project_id) = init_with_project_id {
            let result = unsafe {
                init_with_project_id(
                    project.as_ptr(),
                    NGX_ENGINE_TYPE_CUSTOM,
                    engine.as_ptr(),
                    app_dir_wide.as_ptr(),
                    device,
                    &info,
                    NGX_VERSION_API,
                )
            };
            if super::succeeded(result) {
                super::diag(&format!(
                    "[NGX] initialised through NVSDK_NGX_D3D12_Init_with_ProjectID (project id {NGX_PROJECT_ID}); feature folders: {}",
                    paths.join(", ")
                ));
                self.log_capabilities();
                return Ok(());
            }
            failures.push(format!(
                "NVSDK_NGX_D3D12_Init_with_ProjectID -> {}",
                super::result_name(result)
            ));
        } else {
            failures.push("NVSDK_NGX_D3D12_Init_with_ProjectID is not exported".to_string());
        }

        // Driver builds that reject a custom project id still accept the plain
        // application-id entry point.
        let result = unsafe {
            init(
                0x5045_5752_4e47_5800,
                app_dir_wide.as_ptr(),
                device,
                &info,
                NGX_VERSION_API,
            )
        };
        if super::succeeded(result) {
            super::diag(&format!(
                "[NGX] initialised through NVSDK_NGX_D3D12_Init; feature folders: {}",
                paths.join(", ")
            ));
            self.log_capabilities();
            return Ok(());
        }
        failures.push(format!(
            "NVSDK_NGX_D3D12_Init -> {}",
            super::result_name(result)
        ));
        // The hint is appended once, by `NgxCore::create`, so this is only the
        // list of what this module said.
        Err(failures.join(", "))
    }

    /// Log `SuperSampling.Available` / `FeatureInitResult` when the driver
    /// exposes the capability accessor. These two values are what NGX uses to
    /// answer "why not", and they never reach the app otherwise.
    fn log_capabilities(&self) {
        let (Some(get), Some(get_u32)) = (self.get_capability_parameters, self.get_u32) else {
            return;
        };
        let mut parameters: *mut c_void = std::ptr::null_mut();
        let result = unsafe { get(&mut parameters) };
        if !super::succeeded(result) || parameters.is_null() {
            super::diag(&format!(
                "[NGX] NVSDK_NGX_D3D12_GetCapabilityParameters -> {}",
                super::result_name(result)
            ));
            return;
        }
        let read = |name: &str| {
            let name = std::ffi::CString::new(name).ok()?;
            let mut value: u32 = 0;
            let result = unsafe { get_u32(parameters, name.as_ptr(), &mut value) };
            super::succeeded(result).then_some(value)
        };
        let available = read(P_SUPER_SAMPLING_AVAILABLE);
        let init_result = read(P_SUPER_SAMPLING_INIT_RESULT);
        super::diag(&format!(
            "[NGX] capabilities: SuperSampling.Available={} SuperSampling.FeatureInitResult={}",
            available
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/a".into()),
            init_result
                .map(|value| format!("{} ({})", super::result_name(value), result_code(value)))
                .unwrap_or_else(|| "n/a".into()),
        ));
    }

    fn new_parameters(&self) -> Box<NgxParams> {
        let (Some(allocate), Some(destroy)) = (self.allocate_parameters, self.destroy_parameters)
        else {
            return NgxParams::new();
        };
        if self.set_u32.is_none()
            || self.set_i32.is_none()
            || self.set_f32.is_none()
            || self.set_d3d12.is_none()
            || self.set_void.is_none()
        {
            return NgxParams::new();
        }
        let mut parameters: *mut c_void = std::ptr::null_mut();
        let result = unsafe { allocate(&mut parameters) };
        if !super::succeeded(result) || parameters.is_null() {
            super::diag(&format!(
                "[NGX] NVSDK_NGX_D3D12_AllocateParameters -> {}; falling back to the viewer's \
                 own parameter table",
                super::result_name(result)
            ));
            return NgxParams::new();
        }
        NgxParams::from_ngx(NgxMap::new(
            parameters,
            Some(destroy),
            self.set_u32,
            self.set_i32,
            self.set_f32,
            self.set_d3d12,
            self.set_void,
        ))
    }
}

/// `GetProcAddress`, returning `None` instead of a null pointer.
unsafe fn resolve(handle: HMODULE, name: &[u8]) -> Option<*const c_void> {
    let proc = winapi::um::libloaderapi::GetProcAddress(handle, name.as_ptr() as *const i8);
    (!proc.is_null()).then_some(proc as *const c_void)
}

/// One resolved entry point as its `extern "C"` function type.
///
/// `transmute_copy` rather than `transmute`: a function pointer and a data
/// pointer are the same width on every target this viewer builds for, and
/// `transmute_copy` keeps a single generic path for both the mandatory and the
/// optional entry points instead of one `transmute` per signature.
unsafe fn read_function<T: Copy>(pointer: *const c_void) -> T {
    debug_assert_eq!(
        std::mem::size_of::<T>(),
        std::mem::size_of::<*const c_void>(),
        "an entry point must be one pointer wide"
    );
    std::mem::transmute_copy(&pointer)
}

/// `resolve` + `read_function`: an optional entry point.
unsafe fn resolve_function<T: Copy>(handle: HMODULE, name: &[u8]) -> Option<T> {
    resolve(handle, name).map(|pointer| read_function(pointer))
}

/// Every module that may provide the SDK side of the NGX API, in the order
/// they are tried.
///
/// `nvsdk_ngx_loader.h` (`NGXLoadCoreLibrary`) is the reference: it looks next
/// to the executable first, then at the driver's `NGXCore` registry path, then
/// at the legacy location. The viewer does not read the registry (no extra
/// dependency for one string) but sweeps the `DriverStore\FileRepository` tree
/// that path points at on DCH installs, and it deliberately puts
/// `%SystemRoot%\System32` *last* — a leftover `nvngx.dll` there belongs to a
/// pre-471.11 driver and answers `FAIL_OutOfDate` to everything newer.
fn ngx_core_candidates(sdk_dir: &str) -> Vec<PathBuf> {
    let names: [&str; 2] = ["_nvngx.dll", "nvngx.dll"];
    let mut candidates: Vec<PathBuf> = Vec::new();
    let push_dir = |dir: &Path, candidates: &mut Vec<PathBuf>| {
        for name in names {
            push_candidate(candidates, dir.join(name));
        }
    };

    let system_root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from);

    // 1. The driver's own core, newest driver-store package first.
    if let Some(root) = system_root.as_ref() {
        let repository = root
            .join("System32")
            .join("DriverStore")
            .join("FileRepository");
        if let Ok(entries) = std::fs::read_dir(&repository) {
            let mut packages: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                let is_nvidia = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.to_ascii_lowercase().starts_with("nv"));
                if !is_nvidia || !path.is_dir() {
                    continue;
                }
                if !names.iter().any(|name| path.join(name).is_file()) {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                packages.push((modified, path));
            }
            packages.sort_by(|a, b| b.0.cmp(&a.0));
            for (_, package) in packages {
                push_dir(&package, &mut candidates);
            }
        }
    }

    // 2. Next to the executable and in the viewer's own SDK folders — where a
    //    core shipped with the viewer, or dropped in by hand, lives.
    for folder in feature_search_paths(sdk_dir) {
        push_dir(Path::new(&folder), &mut candidates);
    }

    // 3. The legacy System32 copy, which is what a pre-471.11 driver left there.
    if let Some(root) = system_root.as_ref() {
        push_dir(&root.join("System32"), &mut candidates);
    }
    candidates
}

/// Add `path` unless it is missing or already present (Windows paths are
/// case-insensitive).
fn push_candidate(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if !path.is_file() {
        return;
    }
    let text = path.to_string_lossy().to_ascii_lowercase();
    if candidates
        .iter()
        .any(|existing| existing.to_string_lossy().to_ascii_lowercase() == text)
    {
        return;
    }
    candidates.push(path);
}

/// `LoadLibraryW` plus `GetProcAddress` for one exact module path.
fn load_module(path: &Path, required: &[&[u8]]) -> Result<(HMODULE, Vec<*const c_void>), String> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe { winapi::um::libloaderapi::LoadLibraryW(wide.as_ptr()) };
    if handle.is_null() {
        return Err("LoadLibraryW failed".to_string());
    }
    let mut pointers: Vec<*const c_void> = Vec::with_capacity(required.len());
    for name in required {
        let proc =
            unsafe { winapi::um::libloaderapi::GetProcAddress(handle, name.as_ptr() as *const i8) };
        if proc.is_null() {
            unsafe { winapi::um::libloaderapi::FreeLibrary(handle) };
            return Err(format!("does not export {}", String::from_utf8_lossy(name)));
        }
        pointers.push(proc as *const c_void);
    }
    Ok((handle, pointers))
}

/// First existing copy of `dll_name` in the folders `load_sdk_dll` searches.
fn sdk_module_path(sdk_dir: &str, dll_name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if !sdk_dir.trim().is_empty() {
        dirs.push(PathBuf::from(sdk_dir.trim()));
    }
    if let Some(dir) = std::env::var_os("PW_UPSCALER_SDK_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("sdk"));
            if let Some(repo) = dir.parent().and_then(|dir| dir.parent()) {
                dirs.push(repo.join("sdk"));
            }
        }
    }
    dirs.push(PathBuf::from("sdk"));
    dirs.into_iter()
        .map(|dir| dir.join(dll_name))
        .find(|path| path.is_file())
}

/// Folders NGX Core searches for feature DLLs, most specific first.
fn feature_search_paths(sdk_dir: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut push = |candidate: PathBuf| {
        if !candidate.is_dir() {
            return;
        }
        let text = candidate.to_string_lossy().into_owned();
        if !paths
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&text))
        {
            paths.push(text);
        }
    };
    if !sdk_dir.trim().is_empty() {
        push(PathBuf::from(sdk_dir.trim()));
    }
    if let Some(dir) = std::env::var_os("PW_UPSCALER_SDK_DIR").map(PathBuf::from) {
        push(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            push(dir.to_path_buf());
            push(dir.join("sdk"));
            // Cargo layout: `<repo>/target/<profile>/*.exe` -> `<repo>/sdk`.
            if let Some(repo) = dir.parent().and_then(|dir| dir.parent()) {
                push(repo.join("sdk"));
            }
        }
    }
    paths
}

/// The folder NGX Core writes its logs and temporary files to.
///
/// It has to be writable: NGX answers `FAIL_PlatformError` when it cannot open
/// its log there, which is what happens when the viewer runs from `Program
/// Files`. `%LOCALAPPDATA%` is the first choice for that reason.
fn ngx_app_dir() -> String {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local)
                .join("PerfectWorldExplorer")
                .join("NGX"),
        );
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("ngx-logs"));
        }
    }
    candidates.push(std::env::temp_dir().join("PerfectWorldExplorer-NGX"));
    for candidate in candidates {
        if std::fs::create_dir_all(&candidate).is_err() {
            continue;
        }
        if is_writable(&candidate) {
            return candidate.to_string_lossy().into_owned();
        }
    }
    ".".to_string()
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!("write-probe-{}.tmp", std::process::id()));
    match std::fs::write(&probe, b"ngx") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// One line of advice appended to a failed init.
///
/// `FAIL_OutOfDate` (`0xbad0000c`) is not about the GPU: `nvsdk_ngx_defs.h`
/// defines it as "a function was used which requires a newer version of the
/// NVIDIA Display Driver or feature library than is currently installed" — the
/// `nvngx.dll` that was found is older than the driver. `FAIL_PlatformError`
/// (`0xbad00002`) is the one that means a foreign overlay or injector.
fn device_hint() -> &'static str {
    "0xbad0000c FAIL_OutOfDate means the nvngx.dll that was found is older than the \
     installed driver (a stale copy in System32 or next to the viewer shadows the \
     DriverStore one); 0xbad00002 FAIL_PlatformError means another overlay, injector or \
     NGX consumer is in the process, or the driver was installed without the NGX component"
}

/// `0xbad00002` as hex, for the capability line.
fn result_code(value: u32) -> String {
    format!("0x{value:08x}")
}
