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
//! Initialisation tries **every** entry point `nvsdk_ngx.h` declares, because
//! which of them a given driver implements is not knowable up front:
//! `…_Init_with_ProjectID` (the documented way for an application without an
//! NVIDIA-issued id), then `…_Init`, then `…_Init_Ext` with a parameter map.
//! Each is retried with a ladder of SDK API versions, because `FAIL_OutOfDate`
//! means the version asked for is newer than the module. On a *snippet* the same
//! symbol also exists in the Core-to-snippet flavour (no
//! `NVSDK_NGX_FeatureCommonInfo`), which is tried last — and only on a snippet,
//! since a core would dereference the version number as a pointer.
//!
//! A name that the headers do not declare is **not** tried: `…_Init_ProjectID`
//! (a DLSS 2.1-era name) was, for one build, and the process died inside the
//! driver. Every call is logged before it is made, so a crash names itself.
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
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// `NVSDK_NGX_D3D12_Init` as an **SDK-to-Core** signature
/// (`const NVSDK_NGX_FeatureCommonInfo *` before the version).
type PfnInit =
    unsafe extern "C" fn(u64, *const u16, *mut c_void, *const NgxFeatureCommonInfo, i32) -> u32;
/// `NVSDK_NGX_D3D12_Init` as a **Core-to-snippet** signature: no feature info.
/// `nvsdk_ngx.h` declares this flavour under `NGX_SNIPPET_BUILD`, so a snippet
/// implements this one and a core the one above — the viewer tries both.
type PfnInitSnippet = unsafe extern "C" fn(u64, *const u16, *mut c_void, i32) -> u32;
/// `NVSDK_NGX_D3D12_Init_Ext` — the application-id form with a parameter map.
type PfnInitExt = unsafe extern "C" fn(u64, *const u16, *mut c_void, i32, *const c_void) -> u32;
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
/// `NVSDK_NGX_Parameter_GetVoidPointer` — the accessor that hands out the
/// callbacks a capability map carries.
type PfnGetVoid = unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> u32;
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
                // `NVSDK_NGX_LOGGING_LEVEL_VERBOSE`. `ON` (1) is not enough:
                // the lines that explain a refused `EvaluateFeature` — the
                // offending parameter name and its value — are only emitted at
                // VERBOSE (§3.19 of the programming guide). This is a floor, not
                // a ceiling: NGX itself may log less.
                minimum_logging_level: 2,
                disable_other_logging_sinks: 0,
            },
        }
    }
}

/// How many NGX log lines have been forwarded into `session.log` this session.
static NGX_LOG_LINES: AtomicUsize = AtomicUsize::new(0);

/// Where forwarding stops. `MinimumLoggingLevel` is VERBOSE because that is the
/// only level that names a refused parameter, but a refused `EvaluateFeature`
/// repeats every frame and every forwarded line opens the session log — so the
/// flood is bounded instead of being allowed to slow the render thread down.
const NGX_LOG_LIMIT: usize = 4_000;

/// NGX's own log lines, forwarded into `session.log`.
///
/// The callback may arrive on any thread (`nvsdk_ngx_defs.h`: "It must also be
/// fully thread-safe and any number of threads may call into it concurrently"),
/// which `diagnostics::session` is.
unsafe extern "C" fn ngx_log(message: *const c_char, level: i32, feature: i32) {
    if message.is_null() {
        return;
    }
    let count = NGX_LOG_LINES.fetch_add(1, Ordering::Relaxed);
    if count >= NGX_LOG_LIMIT {
        if count == NGX_LOG_LIMIT {
            super::diag("[NGX] further NGX log lines are suppressed for this session");
        }
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
    /// `NVSDK_NGX_D3D12_GetParameters`: the deprecated accessor that is the
    /// only one an older core has (`nvsdk_ngx.h` names it as the fallback when
    /// `AllocateParameters`/`GetCapabilityParameters` answer
    /// `FAIL_OutOfDate`). Its maps are owned by NGX and must not be freed.
    get_parameters: Option<PfnGetCapabilityParameters>,
    set_u32: Option<PfnSetU32>,
    set_i32: Option<PfnSetI32>,
    set_f32: Option<PfnSetF32>,
    set_d3d12: Option<PfnSetPointer>,
    set_void: Option<PfnSetPointer>,
    get_u32: Option<PfnGetU32>,
    get_void: Option<PfnGetVoid>,
}

// The instance is created and used on the render thread only; the statics below
// exist so both features can reach it, not to share it across threads.
unsafe impl Send for NgxCore {}
unsafe impl Sync for NgxCore {}

static CORE: OnceLock<NgxCore> = OnceLock::new();

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
    if let Some(core) = CORE.get() {
        return Ok(core);
    }
    let core = NgxCore::create(sdk_dir, snippet, device)?;
    // Only success is kept. Caching the failure made one bad moment — an SDK
    // folder that was momentarily unreadable, a driver that answered while the
    // feature DLL was still loading — into "DLSS can never work in this
    // process", which the three-second retry ladder in `src/gpu.rs` then
    // re-reported for the rest of the session.
    let _ = CORE.set(core);
    CORE.get()
        .ok_or_else(|| "NGX Core initialised but could not be stored".to_string())
}

/// What DLSS says it is willing to evaluate.
pub(crate) struct OptimalSettings {
    /// The render size the feature has to be created at and evaluated with.
    pub(crate) render: (u32, u32),
    /// The inclusive range `Evaluate` accepts, when the mode supports dynamic
    /// resolution at all. Both are `None` for a mode without it.
    pub(crate) min: Option<(u32, u32)>,
    pub(crate) max: Option<(u32, u32)>,
}

/// `NVSDK_NGX_Parameter_DLSSOptimalSettingsCallback`.
const P_OPTIMAL_SETTINGS_CALLBACK: &str = "DLSSOptimalSettingsCallback";

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
        if let Some(snippet) = snippet.as_deref() {
            log_feature_module(snippet);
        }
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
    ///
    /// `capabilities` picks which end of the ladder is tried first — the
    /// pre-populated capability map for querying features, or the plain
    /// allocator for the map a feature creates and evaluates with
    /// (`docs/blockers.md` B-27).
    pub(crate) fn new_parameters(&self, capabilities: bool) -> Box<NgxParams> {
        self.core.new_parameters(capabilities)
    }

    /// `NGX_DLSS_GET_OPTIMAL_SETTINGS`, spelled out (`nvsdk_ngx_helpers.h`).
    ///
    /// The programming guide is explicit that these values "must then be passed
    /// exactly as given to the next `NGX_API_CREATE_DLSS_EXT()` call", and that
    /// an `InRenderSubrectDimensions` outside the range they define makes
    /// `EvaluateFeature` answer `FAIL_InvalidParameter` (§3.2.2). Deriving the
    /// render size from the quality ratio alone — which is what this viewer did
    /// — is therefore a coin flip that DLSS may answer with `0xbad00005`, and
    /// when the mode supports no dynamic resolution the range collapses to a
    /// single pair of values, so even one pixel is out of range.
    pub(crate) fn optimal_settings(
        &self,
        params: &mut NgxParams,
        display: (u32, u32),
        quality: u32,
    ) -> Result<OptimalSettings, String> {
        let callback = params
            .get_void_pointer(P_OPTIMAL_SETTINGS_CALLBACK)
            .ok_or_else(|| {
                "the parameter map carries no DLSSOptimalSettingsCallback".to_string()
            })?;
        // The query is asked in *display* size and answers with the render size
        // it recommends: `NGX_DLSS_GET_OPTIMAL_SETTINGS` reads the result out of
        // `OutWidth`/`OutHeight`, not out of the inputs.
        params.set_u32("Width", display.0.max(1));
        params.set_u32("Height", display.1.max(1));
        params.set_i32("PerfQualityValue", quality as i32);
        params.set_i32("RTXValue", 0);
        let query: unsafe extern "C" fn(*mut c_void) -> u32 =
            unsafe { std::mem::transmute_copy(&callback) };
        let result = unsafe { query(params.as_ptr()) };
        if !super::succeeded(result) {
            return Err(format!(
                "DLSSOptimalSettingsCallback -> {}",
                super::result_name(result)
            ));
        }
        let width = params
            .get_u32("OutWidth")
            .ok_or_else(|| "the callback wrote no OutWidth".to_string())?;
        let height = params
            .get_u32("OutHeight")
            .ok_or_else(|| "the callback wrote no OutHeight".to_string())?;
        // Zero is what an unpopulated map reads back as, not a render size: a
        // feature created at "0x0" would be clamped to 1x1 and refused by NGX
        // for a reason three calls away from the real one.
        if width == 0 || height == 0 {
            return Err(format!(
                "DLSSOptimalSettingsCallback answered {width}x{height}, which names no render size"
            ));
        }
        let min = match (
            params.get_u32("DLSS.Get.Dynamic.Min.Render.Width"),
            params.get_u32("DLSS.Get.Dynamic.Min.Render.Height"),
        ) {
            (Some(width), Some(height)) => Some((width, height)),
            _ => None,
        };
        let max = match (
            params.get_u32("DLSS.Get.Dynamic.Max.Render.Width"),
            params.get_u32("DLSS.Get.Dynamic.Max.Render.Height"),
        ) {
            (Some(width), Some(height)) => Some((width, height)),
            _ => None,
        };
        Ok(OptimalSettings {
            render: (width, height),
            min,
            max,
        })
    }

    /// Where NGX Core came from (the ready line names it).
    pub(crate) fn core_path(&self) -> &str {
        &self.core.path
    }
}

impl NgxCore {
    fn create(sdk_dir: &str, snippet: Option<&Path>, device: *mut c_void) -> Result<Self, String> {
        let mut candidates = ngx_core_candidates(sdk_dir);
        // The snippet is tried as a *module* only after every core, but its
        // handle is offered to each candidate as the source of the parameter
        // helpers, because a legacy core may not export them.
        let helper = snippet.map(Path::to_path_buf);
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
            match Self::create_at(&path, helper.as_deref(), sdk_dir, device) {
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
    ///
    /// `helper` is the feature snippet the caller is about to use. The
    /// `NVSDK_NGX_Parameter_*` helpers and the parameter allocators are
    /// resolved from the candidate first and from the snippet only when the
    /// candidate does not export them: a legacy `_nvngx.dll` exports
    /// `CreateFeature`/`EvaluateFeature` but not the whole parameter surface,
    /// and the snippet forwards to the same process-global map.
    fn create_at(
        path: &Path,
        helper: Option<&Path>,
        sdk_dir: &str,
        device: *mut c_void,
    ) -> Result<Self, String> {
        // Mandatory: the whole bootstrap.
        const EXPORTS: &[&[u8]] = &[
            b"NVSDK_NGX_D3D12_Init\0",
            b"NVSDK_NGX_D3D12_CreateFeature\0",
            b"NVSDK_NGX_D3D12_EvaluateFeature\0",
            b"NVSDK_NGX_D3D12_ReleaseFeature\0",
        ];
        let (handle, exports) = load_module(path, EXPORTS)?;
        let helper_handle = helper
            .filter(|helper| !same_file(helper, path))
            .and_then(|helper| load_module(helper, &[]).ok())
            .map(|(handle, _)| handle);
        let mut core = Self {
            _handle: handle,
            path: path.to_string_lossy().into_owned(),
            create_feature: unsafe { read_function(exports[1]) },
            evaluate_feature: unsafe { read_function(exports[2]) },
            release_feature: unsafe { read_function(exports[3]) },
            allocate_parameters: unsafe {
                optional_export(
                    handle,
                    helper_handle,
                    b"NVSDK_NGX_D3D12_AllocateParameters\0",
                )
            },
            destroy_parameters: unsafe {
                optional_export(
                    handle,
                    helper_handle,
                    b"NVSDK_NGX_D3D12_DestroyParameters\0",
                )
            },
            get_capability_parameters: unsafe {
                optional_export(
                    handle,
                    helper_handle,
                    b"NVSDK_NGX_D3D12_GetCapabilityParameters\0",
                )
            },
            get_parameters: unsafe {
                optional_export(handle, helper_handle, b"NVSDK_NGX_D3D12_GetParameters\0")
            },
            set_u32: unsafe {
                optional_export(handle, helper_handle, b"NVSDK_NGX_Parameter_SetUI\0")
            },
            set_i32: unsafe {
                optional_export(handle, helper_handle, b"NVSDK_NGX_Parameter_SetI\0")
            },
            set_f32: unsafe {
                optional_export(handle, helper_handle, b"NVSDK_NGX_Parameter_SetF\0")
            },
            set_d3d12: unsafe {
                optional_export(
                    handle,
                    helper_handle,
                    b"NVSDK_NGX_Parameter_SetD3d12Resource\0",
                )
            },
            set_void: unsafe {
                optional_export(
                    handle,
                    helper_handle,
                    b"NVSDK_NGX_Parameter_SetVoidPointer\0",
                )
            },
            get_u32: unsafe {
                optional_export(handle, helper_handle, b"NVSDK_NGX_Parameter_GetUI\0")
            },
            get_void: unsafe {
                optional_export(
                    handle,
                    helper_handle,
                    b"NVSDK_NGX_Parameter_GetVoidPointer\0",
                )
            },
        };
        let exports = init_exports(path);
        super::diag(&format!(
            "[NGX] candidate {} {}, exports: {}",
            path.display(),
            match module_version(path) {
                Some(version) => format!("version {version}"),
                None => "version unknown".to_string(),
            },
            if exports.is_empty() {
                "none of the NVSDK_NGX_D3D12_Init* names".to_string()
            } else {
                exports.join(", ")
            }
        ));
        let surface = parameter_exports(path);
        super::diag(&format!(
            "[NGX] parameter surface of this core: {}",
            if surface.is_empty() {
                "none (no NVSDK_NGX_Parameter_*/*Parameters exports)".to_string()
            } else {
                surface.join(", ")
            }
        ));
        // The module stays loaded even when initialisation fails: NGX keeps
        // process-global state from the attempt, so unloading it would be worse
        // than leaking one handle.
        core.initialise(sdk_dir, device, handle)?;
        Ok(core)
    }

    /// Try every documented initialisation entry point on this module.
    ///
    /// A core and a snippet implement *different* signatures of the same symbol
    /// name (`nvsdk_ngx.h` switches the declarations on `NGX_SNIPPET_BUILD`),
    /// so both are tried, and each is tried with a ladder of SDK API versions:
    /// `FAIL_OutOfDate` means exactly "this call needs a newer driver than the
    /// one installed", i.e. the version we asked for is too new for this
    /// module, and one step down is the documented fix.
    fn initialise(
        &self,
        sdk_dir: &str,
        device: *mut c_void,
        handle: HMODULE,
    ) -> Result<(), String> {
        // NGX's own diagnostics `.log` the folder named here, and the
        // `__NGX_LOG_*` variables are what its binary reads when the registry
        // says nothing (§3.19). Both are best-effort: NGX ignores a path it
        // cannot write and never reports that.
        let log_dir = ngx_app_dir();
        for (name, value) in [
            ("__NGX_LOG_LEVEL", "3"),
            ("NVNGX_LOG_LEVEL", "3"),
            ("__NGX_LOG_PATH_OVERRIDE", log_dir.as_str()),
        ] {
            std::env::set_var(name, value);
        }
        let app_dir = log_dir;
        super::diag(&format!(
            "[NGX] initialising through {} (application data folder: {app_dir})",
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
        let application_id: u64 = 0x5045_5752_4e47_5800;

        let mut failures: Vec<String> = Vec::new();
        unsafe {
            // 1. The documented entry point for an application without an
            //    NVIDIA-issued id. Every signature below is the one the vendored
            //    `nvsdk_ngx.h` declares: a name whose arguments this module
            //    cannot check must not be called on a guess. `…_Init_ProjectID`
            //    (a DLSS 2.1-era name that the modern headers no longer declare)
            //    was tried here for one build and crashed the process inside the
            //    driver, which is why it is gone.
            match resolve_function::<PfnInitWithProjectId>(
                handle,
                b"NVSDK_NGX_D3D12_Init_with_ProjectID\0",
            ) {
                Some(init) => {
                    let mut results = Vec::new();
                    for version in NGX_VERSIONS {
                        calling("NVSDK_NGX_D3D12_Init_with_ProjectID", version);
                        let result = init(
                            project.as_ptr(),
                            NGX_ENGINE_TYPE_CUSTOM,
                            engine.as_ptr(),
                            app_dir_wide.as_ptr(),
                            device,
                            &info,
                            version,
                        );
                        if super::succeeded(result) {
                            return self.ready(
                                "NVSDK_NGX_D3D12_Init_with_ProjectID",
                                version,
                                &paths,
                            );
                        }
                        results.push(result);
                        if !is_out_of_date(result) {
                            break;
                        }
                    }
                    failures.push(describe("NVSDK_NGX_D3D12_Init_with_ProjectID", &results));
                }
                None => failures.push("NVSDK_NGX_D3D12_Init_with_ProjectID is not exported".into()),
            }
            // 2. The application-id forms. `Init` first: it is the one an older
            //    core accepts (a report from an RTX machine shows exactly that,
            //    with `Init_Ext` never having been tried), then `Init_Ext`, which
            //    takes a parameter map and is what a newer core prefers.
            for (name, ext) in [
                ("NVSDK_NGX_D3D12_Init", false),
                ("NVSDK_NGX_D3D12_Init_Ext", true),
            ] {
                let mut results = Vec::new();
                let mut called = false;
                for version in NGX_VERSIONS {
                    let result = if ext {
                        match resolve_function::<PfnInitExt>(handle, b"NVSDK_NGX_D3D12_Init_Ext\0")
                        {
                            Some(init) => {
                                called = true;
                                calling("NVSDK_NGX_D3D12_Init_Ext", version);
                                init(
                                    application_id,
                                    app_dir_wide.as_ptr(),
                                    device,
                                    version,
                                    std::ptr::null(),
                                )
                            }
                            None => break,
                        }
                    } else {
                        match resolve_function::<PfnInit>(handle, b"NVSDK_NGX_D3D12_Init\0") {
                            Some(init) => {
                                called = true;
                                calling("NVSDK_NGX_D3D12_Init", version);
                                init(
                                    application_id,
                                    app_dir_wide.as_ptr(),
                                    device,
                                    &info,
                                    version,
                                )
                            }
                            None => break,
                        }
                    };
                    if super::succeeded(result) {
                        return self.ready(name, version, &paths);
                    }
                    results.push(result);
                    if !is_out_of_date(result) {
                        break;
                    }
                }
                failures.push(if called {
                    describe(name, &results)
                } else {
                    format!("{name} is not exported")
                });
            }
            // 3. `NVSDK_NGX_D3D12_Init` with the Core-to-snippet signature. A
            //    snippet implements this one and a core the one above; passing a
            //    `FeatureCommonInfo*` where a version belongs is exactly the
            //    `FAIL_FeatureNotSupported` a snippet answers an application.
            //
            //    Only ever done on a snippet: the reverse mistake is fatal, not
            //    merely rejected — a core would read the version we pass in the
            //    `FeatureCommonInfo*` slot and dereference a small integer.
            let snippet_module = self
                .path
                .rsplit(['\\', '/'])
                .next()
                .is_some_and(|name| name.to_ascii_lowercase().starts_with("nvngx_dlss"));
            if let (true, Some(init)) = (
                snippet_module,
                resolve_function::<PfnInitSnippet>(handle, b"NVSDK_NGX_D3D12_Init\0"),
            ) {
                let mut results = Vec::new();
                for version in NGX_VERSIONS {
                    calling("NVSDK_NGX_D3D12_Init (snippet signature)", version);
                    let result = init(application_id, app_dir_wide.as_ptr(), device, version);
                    if super::succeeded(result) {
                        return self.ready(
                            "NVSDK_NGX_D3D12_Init (snippet signature)",
                            version,
                            &paths,
                        );
                    }
                    results.push(result);
                    if !is_out_of_date(result) {
                        break;
                    }
                }
                failures.push(describe(
                    "NVSDK_NGX_D3D12_Init (snippet signature)",
                    &results,
                ));
            }
        }
        // The hint is appended once, by `NgxCore::create`, so this is only the
        // list of what this module said.
        Err(failures.join("; "))
    }

    /// Log the line a successful initialisation prints and read the capability
    /// map.
    fn ready(&self, via: &str, version: i32, paths: &[String]) -> Result<(), String> {
        super::diag(&format!(
            "[NGX] initialised through {via} (SDK API {version:#06x}); feature folders: {}",
            paths.join(", ")
        ));
        self.log_capabilities();
        Ok(())
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
                .map(super::result_name)
                .unwrap_or_else(|| "n/a".into()),
        ));
    }

    /// An `NVSDK_NGX_Parameter` map: NGX's own whenever it can hand one out,
    /// otherwise the viewer's table.
    ///
    /// The order matters, and it used to be wrong. The map NGX's features
    /// receive is the one they validate at `EvaluateFeature`, and a foreign
    /// implementation is what turns that call into
    /// `FAIL_InvalidParameter` (`0xbad00005`) on some drivers — so every
    /// NGX-owned accessor is tried *first*:
    ///
    /// 1. `NVSDK_NGX_D3D12_GetCapabilityParameters` — pre-populated with NGX's
    ///    capabilities, which is the map NVIDIA's own DLSS samples create and
    ///    evaluate with. A map from `AllocateParameters` is documented as
    ///    usable "in the same ways", but it is *not* pre-populated
    ///    (`nvsdk_ngx.h:356-359`), and the feature reads capability entries
    ///    that only this one carries.
    /// 2. `NVSDK_NGX_D3D12_AllocateParameters` — the modern allocator,
    /// 3. `NVSDK_NGX_D3D12_GetParameters` — deprecated, and the *only* one an
    ///    older core exports (`nvsdk_ngx.h` documents it as the fallback when
    ///    the other two answer `FAIL_OutOfDate`).
    ///
    /// Lifetimes follow the header: a map from either allocator belongs to the
    /// application and must be freed with `NVSDK_NGX_D3D12_DestroyParameters`
    /// (`nvsdk_ngx.h:346-359, 393-403`), while a map from `…GetParameters` is
    /// NGX's own and must *not* be destroyed (`nvsdk_ngx.h:432-436`).
    ///
    /// The map obtained that way is filled through its own vtable
    /// ([`NgxMap::through_vtable`]), so the exported `NVSDK_NGX_Parameter_Set*`
    /// helpers are not needed to use it. Requiring them first — as this used to
    /// — skipped the whole ladder on the current DLSS 4.x build, which exports
    /// neither the setters nor the allocators in `nvngx_dlss.dll`, and handed
    /// NGX the viewer's own table even when the driver's core could have given
    /// it one.
    fn new_parameters(&self, capabilities: bool) -> Box<NgxParams> {
        let capability = (
            "NVSDK_NGX_D3D12_GetCapabilityParameters",
            self.get_capability_parameters,
            true,
        );
        let allocate = (
            "NVSDK_NGX_D3D12_AllocateParameters",
            self.allocate_parameters,
            true,
        );
        // `nvsdk_ngx.h:393-403` documents the capability map as usable "in the
        // same ways" as an allocated one, and recommends it only for querying —
        // so the queries take it first and the feature map takes the allocator
        // first. Which end answers matters: a map that cannot store
        // `ID3D12Resource *` entries is a feature map that fails every evaluate
        // with `FAIL_InvalidParameter` (`docs/blockers.md` B-27).
        let sources: [(&str, Option<PfnGetCapabilityParameters>, bool); 3] = if capabilities {
            [
                capability,
                allocate,
                ("NVSDK_NGX_D3D12_GetParameters", self.get_parameters, false),
            ]
        } else {
            [
                allocate,
                capability,
                ("NVSDK_NGX_D3D12_GetParameters", self.get_parameters, false),
            ]
        };
        for (name, function, destroyable) in sources {
            let Some(function) = function else {
                super::diag(&format!("[NGX] {name} is not exported by this core"));
                continue;
            };
            let mut parameters: *mut c_void = std::ptr::null_mut();
            let result = unsafe { function(&mut parameters) };
            if !super::succeeded(result) || parameters.is_null() {
                super::diag(&format!("[NGX] {name} -> {}", super::result_name(result)));
                continue;
            }
            let destroy = if destroyable {
                self.destroy_parameters
            } else {
                None
            };
            if super::params::is_writable(parameters) {
                super::diag(&format!(
                    "[NGX] parameter map: NGX's own, from {name} (written through its vtable)"
                ));
                let mut map = NgxMap::through_vtable(parameters, destroy);
                // The declared slot is only a starting point: ask the map which
                // overload it answers on and start the ladder there.
                if let Some(slot) = map.measured_pointer_slot() {
                    map.set_resource_set_slot(slot);
                }
                return NgxParams::from_ngx(map);
            }
            if self.set_u32.is_some() {
                super::diag(&format!(
                    "[NGX] parameter map: NGX's own, from {name} (written through the exported \
                     NVSDK_NGX_Parameter_Set* helpers)"
                ));
                let mut map = NgxMap::new(
                    parameters,
                    destroy,
                    self.set_u32,
                    self.set_i32,
                    self.set_f32,
                    self.set_d3d12,
                    self.set_void,
                    self.get_u32,
                    self.get_void,
                );
                if let Some(slot) = map.measured_pointer_slot() {
                    map.set_resource_set_slot(slot);
                }
                return NgxParams::from_ngx(map);
            }
            super::diag(&format!(
                "[NGX] {name} returned a map the viewer cannot write to; ignoring it"
            ));
            if let Some(destroy) = destroy {
                unsafe { destroy(parameters) };
            }
        }
        super::diag(
            "[NGX] no NVSDK_NGX parameter allocator answered; the viewer's own parameter table \
             is in use (NGX may reject it at EvaluateFeature)",
        );
        NgxParams::new()
    }
}

/// The SDK API versions `init` is tried with, newest first.
///
/// `FAIL_OutOfDate` means "a function was used which requires a newer version of
/// the NVIDIA Display Driver or feature library than is currently installed"
/// (`nvsdk_ngx_defs.h`), i.e. the version asked for is newer than this module.
/// `0x15` is the vendored header's `NVSDK_NGX_VERSION_API_MACRO`; the older
/// entries are what pre-DLSS-3 cores understand.
const NGX_VERSIONS: [i32; 3] = [NGX_VERSION_API, 0x0f, 0x03];

/// Say which DLSS build is installed and what it exports.
///
/// The vendored `nvngx_dlss.dll` of this project is DLSS 4.x (`310.9.16`, the
/// current release). An older file left in `sdk/` by a previous install is the
/// one thing that silently changes how the whole bootstrap behaves, so it is
/// named in the log together with the parameter surface it does or does not
/// export.
fn log_feature_module(path: &Path) {
    let version = module_version(path);
    let major = version
        .as_deref()
        .and_then(|version| version.split('.').next())
        .and_then(|major| major.parse::<u32>().ok());
    super::diag(&format!(
        "[NGX] feature module {} version {}",
        path.display(),
        version.clone().unwrap_or_else(|| "unknown".to_string())
    ));
    if major.is_some_and(|major| major < 310) {
        super::diag(&format!(
            "[NGX] this DLSS build ({}) predates DLSS 4: replace sdk\\nvngx_dlss.dll and \
             sdk\\nvngx_dlssg.dll with the 310.x release (NVIDIA/DLSS, \
             lib/Windows_x86_64/rel/)",
            version.unwrap_or_default()
        ));
    }
    let parameters = parameter_exports(path);
    super::diag(&format!(
        "[NGX] parameter surface of the feature module: {}",
        if parameters.is_empty() {
            "none (no NVSDK_NGX_Parameter_* / *Parameters exports)".to_string()
        } else {
            parameters.join(", ")
        }
    ));
}

/// Announce a call before making it.
///
/// A driver is free to crash the process from inside `Init` (one did), and a
/// log line is the only thing that survives that: the last `[NGX] calling …`
/// line names the entry point and the API version that killed it.
fn calling(name: &str, version: i32) {
    super::diag(&format!("[NGX] calling {name} (SDK API {version:#06x})"));
}

/// `FAIL_OutOfDate` (`0xbad0000c`) is the only result that makes another rung of
/// the version ladder meaningful: it means the version asked for is newer than
/// the module. Anything else (not supported, denied, invalid) would answer the
/// same way at every version, so the ladder stops.
fn is_out_of_date(result: u32) -> bool {
    result & 0xffff == 0x000c
}

/// One `init` variant's outcome across the version ladder, compressed to a
/// single line for `session.log`.
fn describe(name: &str, results: &[u32]) -> String {
    let Some(first) = results.first() else {
        return format!("{name} -> not called");
    };
    if results.iter().all(|result| result == first) {
        return format!("{name} -> {}", super::result_name(*first));
    }
    let ladder: Vec<String> = results
        .iter()
        .zip(NGX_VERSIONS)
        .map(|(result, version)| format!("{version:#06x}: {}", super::result_name(*result)))
        .collect();
    format!("{name} -> {}", ladder.join(", "))
}

/// Whether two paths name the same file, case-insensitively.
fn same_file(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

/// The `NVSDK_NGX_D3D12_Init*` names a module exports, read straight out of its
/// PE export directory.
///
/// This is what makes the bootstrap decidable instead of guessed: which of the
/// entry points a given driver build implements is otherwise invisible from
/// `session.log`, and a name we do not recognise is a call we must not make.
fn exported_names(path: &Path) -> Vec<String> {
    /// `None` for anything that is not a well-formed PE image.
    fn parse(bytes: &[u8]) -> Option<Vec<String>> {
        let read_u16 = |offset: usize| -> Option<u16> {
            Some(u16::from_le_bytes(
                bytes.get(offset..offset + 2)?.try_into().ok()?,
            ))
        };
        let read_u32 = |offset: usize| -> Option<u32> {
            Some(u32::from_le_bytes(
                bytes.get(offset..offset + 4)?.try_into().ok()?,
            ))
        };
        let lfanew = read_u32(0x3c)? as usize;
        if bytes.get(lfanew..lfanew + 4)? != b"PE\0\0".as_slice() {
            return None;
        }
        let section_count = read_u16(lfanew + 6)? as usize;
        let optional_size = read_u16(lfanew + 20)? as usize;
        let optional = lfanew + 24;
        // `IMAGE_OPTIONAL_HEADER64` is 16 bytes longer than the 32-bit one, and
        // the data directories follow the fixed fields.
        let pe32_plus = read_u16(optional)? == 0x20b;
        let directories = optional + if pe32_plus { 112 } else { 96 };
        let export_rva = read_u32(directories)?;
        let export_size = read_u32(directories + 4)?;
        if export_rva == 0 || export_size == 0 {
            return None;
        }
        // Section table, for RVA -> file offset.
        let table = optional + optional_size;
        let sections: Vec<(u32, u32, u32, u32)> = (0..section_count)
            .filter_map(|index| {
                let base = table + index * 40;
                Some((
                    read_u32(base + 12)?, // VirtualAddress
                    read_u32(base + 8)?,  // VirtualSize
                    read_u32(base + 16)?, // SizeOfRawData
                    read_u32(base + 20)?, // PointerToRawData
                ))
            })
            .collect();
        let to_offset = |rva: u32| -> Option<usize> {
            sections.iter().find_map(|&(address, size, raw_size, raw)| {
                let span = size.max(raw_size);
                (rva >= address && rva < address.saturating_add(span))
                    .then(|| (raw + (rva - address)) as usize)
            })
        };
        let export = to_offset(export_rva)?;
        let name_count = read_u32(export + 24)?;
        let names_offset = to_offset(read_u32(export + 32)?)?;
        let names: Vec<String> = (0..name_count.min(4096))
            .filter_map(|index| {
                let slot = names_offset + index as usize * 4;
                let start = to_offset(read_u32(slot)?)?;
                let end = start + bytes.get(start..)?.iter().position(|byte| *byte == 0)?;
                Some(String::from_utf8_lossy(&bytes[start..end]).into_owned())
            })
            .collect();
        Some(names)
    }

    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let mut names: Vec<String> = parse(&bytes)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| name.starts_with("NVSDK_NGX_"))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The `NVSDK_NGX_D3D12_Init*` entries of [`exported_names`].
fn init_exports(path: &Path) -> Vec<String> {
    exported_names(path)
        .into_iter()
        .filter(|name| name.starts_with("NVSDK_NGX_D3D12_Init"))
        .collect()
}

/// The `NVSDK_NGX_Parameter_*` helpers and the parameter-map allocators of
/// [`exported_names`] — the surface `NgxCore::new_parameters` needs to hand NGX
/// a map of its own.
fn parameter_exports(path: &Path) -> Vec<String> {
    exported_names(path)
        .into_iter()
        .filter(|name| name.starts_with("NVSDK_NGX_Parameter_") || name.ends_with("Parameters"))
        .collect()
}

/// The module's `VS_FIXEDFILEINFO` version, e.g. `310.2.1.0`.
///
/// This is what tells a stale NGX core from a current one in a report: DLSS
/// builds report `310.x.y.z`, and the driver's `_nvngx.dll` carries the driver's
/// own `32.0.x.y` version.
fn module_version(path: &Path) -> Option<String> {
    #[link(name = "version")]
    extern "system" {
        fn GetFileVersionInfoSizeW(file: *const u16, handle: *mut u32) -> u32;
        fn GetFileVersionInfoW(file: *const u16, handle: u32, length: u32, data: *mut u8) -> i32;
        fn VerQueryValueW(
            data: *const u8,
            sub: *const u16,
            out: *mut *mut c_void,
            length: *mut u32,
        ) -> i32;
    }
    use std::os::windows::ffi::OsStrExt;
    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let mut handle: u32 = 0;
        let size = GetFileVersionInfoSizeW(file.as_ptr(), &mut handle);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(file.as_ptr(), 0, size, data.as_mut_ptr()) == 0 {
            return None;
        }
        let root: Vec<u16> = "\\".encode_utf16().chain(std::iter::once(0)).collect();
        let mut info: *mut c_void = std::ptr::null_mut();
        let mut length: u32 = 0;
        if VerQueryValueW(data.as_ptr(), root.as_ptr(), &mut info, &mut length) == 0
            || info.is_null()
            || length < 16
        {
            return None;
        }
        // `VS_FIXEDFILEINFO`: signature, struct version, file version MS, LS, …
        let words = std::slice::from_raw_parts(info as *const u32, 4);
        let (ms, ls) = (words[2], words[3]);
        Some(format!(
            "{}.{}.{}.{}",
            ms >> 16,
            ms & 0xffff,
            ls >> 16,
            ls & 0xffff
        ))
    }
}

/// Resolve an optional entry point on `handle`, or on the snippet `helper` when
/// the candidate does not export it.
///
/// # Safety
/// `handle` and `helper` must be modules loaded with `LoadLibraryW`, and `T`
/// must be the exact signature the module implements.
unsafe fn optional_export<T: Copy>(
    handle: HMODULE,
    helper: Option<HMODULE>,
    name: &[u8],
) -> Option<T> {
    resolve_function(handle, name)
        .or_else(|| helper.and_then(|helper| resolve_function(helper, name)))
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
/// Append whatever NGX wrote to its own log files to `session.log`.
///
/// The logging callback in `NVSDK_NGX_FeatureCommonInfo` only reaches an
/// application NGX has accepted as verified; an unverified one gets nothing and
/// no error, which is exactly the silence every run so far showed. Its files do
/// not depend on that, so after a refusal this is the only way to hear NGX's own
/// reason. Missing, empty or unreadable files are simply no lines.
pub(crate) fn dump_ngx_log_files() {
    let dir = PathBuf::from(ngx_app_dir());
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_log = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("log"));
        if !is_log {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let tail: Vec<&str> = text.lines().rev().take(40).collect();
        if tail.is_empty() {
            continue;
        }
        super::diag(&format!(
            "[NGX file {}] last {} line(s):",
            path.display(),
            tail.len()
        ));
        for line in tail.into_iter().rev() {
            super::diag(&format!("[NGX file {}] {line}", path.display()));
        }
    }
}

/// The last thing NGX's own feature log said about a *missing* parameter.
///
/// `EvaluateFeature` answers `FAIL_InvalidParameter` without naming anything, and
/// `NVSDK_NGX_Parameter` gives no error either — but the feature module writes the
/// reason to its log on the same tick:
///
/// ```text
/// [NGXDLAA::EvaluateFeature:788] error: could not find Color parameter
/// ```
///
/// Reading it back turns a blind retry into an experiment: the ladder in
/// [crate::dlss::DlssUpscaler] writes one key through each candidate slot, so the
/// *name* NGX reports is what identifies which slot its own reader agrees with
/// (`docs/blockers.md` B-27).
pub(crate) fn last_missing_parameter() -> Option<String> {
    let dir = PathBuf::from(ngx_app_dir());
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("log"))
        {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if newest.as_ref().is_some_and(|(stamp, _)| *stamp > modified) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let hits: Vec<&str> = text
            .lines()
            .filter(|line| line.contains("could not find"))
            .collect();
        // The count matters: the same complaint repeated after the next
        // attempt means the *same* key is still missing, while a different one
        // means the ladder got past that key and stopped somewhere else. Both
        // are readable from a single line only if the repetition is visible.
        if let Some(line) = hits.last() {
            // Repetitions of *this* complaint count it once per rejected
            // attempt; older complaints about other keys are irrelevant here.
            let repeats = hits.iter().filter(|hit| *hit == line).count();
            let complaint = match repeats {
                0 | 1 => line.trim().to_string(),
                2 => format!("{} (2nd time)", line.trim()),
                count => format!("{} ({count}x)", line.trim()),
            };
            newest = Some((modified, complaint));
        }
    }
    newest.map(|(_, line)| line)
}

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
