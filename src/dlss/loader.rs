//! NGX bootstrap: the driver's NGX **Core** (`nvngx.dll`) driving the vendored
//! DLSS feature DLLs.
//!
//! ## Why the core and not the feature DLL
//!
//! `nvngx_dlss.dll` and `nvngx_dlssg.dll` are NGX *snippets*: they are written
//! to be loaded and driven by NGX Core, which ships inside the display driver as
//! `%SystemRoot%\System32\nvngx.dll`. They refuse to initialise when the
//! application calls them directly — the snippet's `NVSDK_NGX_D3D12_Init_Ext`
//! prologue resolves the module the call came from
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
fn core(sdk_dir: &str, device: *mut c_void) -> Result<&'static NgxCore, String> {
    match CORE.get_or_init(|| NgxCore::create(sdk_dir, device)) {
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
        let core = core(sdk_dir, device)?;
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
    fn create(sdk_dir: &str, device: *mut c_void) -> Result<Self, String> {
        // Mandatory: the whole Core bootstrap.
        const EXPORTS: &[&[u8]] = &[
            b"NVSDK_NGX_D3D12_Init\0",
            b"NVSDK_NGX_D3D12_CreateFeature\0",
            b"NVSDK_NGX_D3D12_EvaluateFeature\0",
            b"NVSDK_NGX_D3D12_ReleaseFeature\0",
        ];
        let system32 = system32_dir().ok_or_else(|| {
            "the NVIDIA NGX runtime (nvngx.dll) is not installed: it ships with the display \
             driver, so reinstall the driver with all components (a driver cleaned by \
             NVCleanstall/DDU can leave it out)"
                .to_string()
        })?;
        let (handle, exports) = load_sdk_dll(&system32.to_string_lossy(), "nvngx.dll", EXPORTS)
            .map_err(|error| {
                format!(
                    "the NVIDIA NGX runtime (nvngx.dll) could not be loaded from {}: {error}",
                    system32.display()
                )
            })?;
        let mut core = Self {
            _handle: handle,
            path: system32.join("nvngx.dll").to_string_lossy().into_owned(),
            create_feature: unsafe { std::mem::transmute(exports[1]) },
            evaluate_feature: unsafe { std::mem::transmute(exports[2]) },
            release_feature: unsafe { std::mem::transmute(exports[3]) },
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
                std::mem::transmute(resolve(handle, b"NVSDK_NGX_D3D12_AllocateParameters\0"));
            core.destroy_parameters =
                std::mem::transmute(resolve(handle, b"NVSDK_NGX_D3D12_DestroyParameters\0"));
            core.get_capability_parameters = std::mem::transmute(resolve(
                handle,
                b"NVSDK_NGX_D3D12_GetCapabilityParameters\0",
            ));
            core.set_u32 = std::mem::transmute(resolve(handle, b"NVSDK_NGX_Parameter_SetUI\0"));
            core.set_i32 = std::mem::transmute(resolve(handle, b"NVSDK_NGX_Parameter_SetI\0"));
            core.set_f32 = std::mem::transmute(resolve(handle, b"NVSDK_NGX_Parameter_SetF\0"));
            core.set_d3d12 =
                std::mem::transmute(resolve(handle, b"NVSDK_NGX_Parameter_SetD3d12Resource\0"));
            core.set_void =
                std::mem::transmute(resolve(handle, b"NVSDK_NGX_Parameter_SetVoidPointer\0"));
            core.get_u32 = std::mem::transmute(resolve(handle, b"NVSDK_NGX_Parameter_GetUI\0"));
        }
        core.initialise(sdk_dir, device, handle, unsafe {
            std::mem::transmute(exports[0])
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
        let init_with_project_id: Option<PfnInitWithProjectId> = unsafe {
            std::mem::transmute(resolve(handle, b"NVSDK_NGX_D3D12_Init_with_ProjectID\0"))
        };
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
        Err(format!(
            "NGX refused to initialise ({}); see the [NGX] lines above and session.log \
             — {}",
            failures.join(", "),
            device_hint()
        ))
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

/// `%SystemRoot%\System32`, which is where the driver installs `nvngx.dll`.
fn system32_dir() -> Option<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)?;
    let dir = root.join("System32");
    dir.join("nvngx.dll").is_file().then_some(dir)
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

/// One line of advice appended to a failed init: `FAIL_PlatformError` from the
/// *core* is about the process or the installation, not about the GPU.
fn device_hint() -> &'static str {
    "on a supported RTX GPU this usually means another overlay, injector or NGX consumer \
     loaded into the process, or a driver install without the NGX component"
}

/// `0xbad00002` as hex, for the capability line.
fn result_code(value: u32) -> String {
    format!("0x{value:08x}")
}
