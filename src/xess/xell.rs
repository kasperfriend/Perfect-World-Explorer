//! XeLL — Intel Xe Low Latency (required by XeSS-FG on every GPU).
//!
//! XeSS-FG refuses to initialize its proxy swapchain without a XeLL context,
//! and on non-Intel GPUs latency reduction only works while XeSS-FG is
//! enabled. The frame generation integration therefore owns a XeLL context:
//! low-latency mode is enabled once after creation, `xellSleep` is called
//! once per frame, and the required simulation / render-submit / present
//! markers are delivered every frame. The XeLL context is destroyed after
//! the XeSS-FG context, as the SDK requires.

use super::loader::{free_sdk_dll, load_sdk_dll};
use std::ffi::c_void;

pub(crate) const XELL_RESULT_SUCCESS: i32 = 0;
pub(crate) const XELL_SIMULATION_START: i32 = 0;
pub(crate) const XELL_SIMULATION_END: i32 = 1;
pub(crate) const XELL_RENDERSUBMIT_START: i32 = 2;
pub(crate) const XELL_RENDERSUBMIT_END: i32 = 3;
pub(crate) const XELL_PRESENT_START: i32 = 4;
pub(crate) const XELL_PRESENT_END: i32 = 5;

const XELL_LOGGING_LEVEL_WARNING: i32 = 2;

type PfnXellD3D12CreateContext =
    unsafe extern "C" fn(dev: *mut c_void, out_context: *mut *mut c_void) -> i32;
type PfnXellSetSleepMode =
    unsafe extern "C" fn(context: *mut c_void, param: *const XellSleepParams) -> i32;
type PfnXellSleep = unsafe extern "C" fn(context: *mut c_void, frame_id: u32) -> i32;
type PfnXellAddMarkerData =
    unsafe extern "C" fn(context: *mut c_void, frame_id: u32, marker: i32) -> i32;
type PfnXellDestroyContext = unsafe extern "C" fn(context: *mut c_void) -> i32;
type PfnXellGetVersion = unsafe extern "C" fn(version: *mut XellVersion) -> i32;

/// `xell_version_t`: four `uint16_t`s, major.minor.patch. `_reserved` keeps the
/// SDK's fourth word so the layout matches the header; it is never read.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XellVersion {
    major: u16,
    minor: u16,
    patch: u16,
    _reserved: u16,
}
type PfnXellSetLoggingCallback = unsafe extern "C" fn(
    context: *mut c_void,
    level: i32,
    callback: extern "C" fn(*const std::os::raw::c_char, i32),
) -> i32;

#[repr(C)]
struct XellSleepParams {
    minimum_interval_us: u32,
    /// Bitfield mirror of the SDK's `xell_sleep_params_t` flags word:
    /// bit 0 = bLowLatencyMode, bit 1 = bLowLatencyBoost, rest reserved.
    flags: u32,
}

extern "C" fn xell_log_callback(message: *const std::os::raw::c_char, _level: i32) {
    if message.is_null() {
        return;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(message) }.to_string_lossy();
    crate::xess::diag(&format!("[XeLL] {text}"));
}

/// Human-readable XeLL result codes for diagnostics.
pub(crate) fn xell_result_name(rc: i32) -> &'static str {
    match rc {
        0 => "XELL_RESULT_SUCCESS",
        -1 => "XELL_RESULT_ERROR_UNSUPPORTED_DEVICE",
        -2 => "XELL_RESULT_ERROR_UNSUPPORTED_DRIVER",
        -3 => "XELL_RESULT_ERROR_UNINITIALIZED",
        -4 => "XELL_RESULT_ERROR_INVALID_ARGUMENT",
        -6 => "XELL_RESULT_ERROR_DEVICE",
        -7 => "XELL_RESULT_ERROR_NOT_IMPLEMENTED",
        -8 => "XELL_RESULT_ERROR_INVALID_CONTEXT",
        -10 => "XELL_RESULT_ERROR_UNSUPPORTED",
        -1000 => "XELL_RESULT_ERROR_UNKNOWN",
        _ => "XELL_RESULT_(unknown)",
    }
}

/// Owned XeLL context. Low-latency mode is always on while the context
/// exists; the per-frame `sleep`/`marker` calls are cheap no-ops when the
/// library decides nothing is needed.
pub(crate) struct Xell {
    handle: winapi::shared::minwindef::HMODULE,
    context: *mut c_void,
    sleep_fn: PfnXellSleep,
    marker_fn: PfnXellAddMarkerData,
    destroy_context: PfnXellDestroyContext,
    /// Version reported by `xellGetVersion`, formatted once at creation.
    version: String,
    /// A broken XeLL context would otherwise write a diagnostic every frame;
    /// the first failure is reported, the rest are counted.
    failure_logged: bool,
}

impl Xell {
    /// Create the XeLL context and enable low-latency mode.
    /// `device` is the raw `ID3D12Device*` (borrowed, not retained by XeLL).
    pub(crate) fn create(device: *mut c_void, sdk_dir: &str) -> Result<Self, String> {
        const EXPORTS: &[&[u8]] = &[
            b"xellD3D12CreateContext\0",
            b"xellSetSleepMode\0",
            b"xellSleep\0",
            b"xellAddMarkerData\0",
            b"xellDestroyContext\0",
            b"xellSetLoggingCallback\0",
            b"xellGetVersion\0",
        ];
        let (handle, exports) = load_sdk_dll(sdk_dir, "libxell.dll", EXPORTS)?;
        let create_context: PfnXellD3D12CreateContext = unsafe { std::mem::transmute(exports[0]) };
        let set_sleep_mode: PfnXellSetSleepMode = unsafe { std::mem::transmute(exports[1]) };
        let sleep_fn: PfnXellSleep = unsafe { std::mem::transmute(exports[2]) };
        let marker_fn: PfnXellAddMarkerData = unsafe { std::mem::transmute(exports[3]) };
        let destroy_context: PfnXellDestroyContext = unsafe { std::mem::transmute(exports[4]) };
        let set_logging_callback: PfnXellSetLoggingCallback =
            unsafe { std::mem::transmute(exports[5]) };
        let get_version: PfnXellGetVersion = unsafe { std::mem::transmute(exports[6]) };

        let mut context: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { create_context(device, &mut context) };
        if rc != XELL_RESULT_SUCCESS || context.is_null() {
            unsafe { free_sdk_dll(handle) };
            return Err(format!(
                "xellD3D12CreateContext failed ({})",
                xell_result_name(rc)
            ));
        }
        let _ =
            unsafe { set_logging_callback(context, XELL_LOGGING_LEVEL_WARNING, xell_log_callback) };
        // Latency reduction is always enabled while XeLL lives. The frame
        // limiter is not used (0 = no cap), matching the engine's own pace.
        let params = XellSleepParams {
            minimum_interval_us: 0,
            flags: 1, // bLowLatencyMode
        };
        let rc = unsafe { set_sleep_mode(context, &params) };
        if rc != XELL_RESULT_SUCCESS {
            crate::xess::diag(&format!(
                "[XeLL] xellSetSleepMode failed ({}); continuing with latency reduction off",
                xell_result_name(rc)
            ));
        }
        let mut version = XellVersion::default();
        let version = if unsafe { get_version(&mut version) } == XELL_RESULT_SUCCESS {
            format!("{}.{}.{}", version.major, version.minor, version.patch)
        } else {
            "unknown".to_string()
        };
        Ok(Self {
            handle,
            context,
            sleep_fn,
            marker_fn,
            destroy_context,
            version,
            failure_logged: false,
        })
    }

    /// The raw XeLL context handle, for `xefgSwapChainSetLatencyReduction`.
    pub(crate) fn raw_context(&self) -> *mut c_void {
        self.context
    }

    /// XeLL version as reported by `xellGetVersion` ("major.minor.patch").
    pub(crate) fn version_string(&self) -> &str {
        &self.version
    }

    /// Report a per-frame failure once, so a broken context cannot fill the
    /// session log at frame rate.
    fn report(&mut self, what: &str, rc: i32) {
        if self.failure_logged {
            return;
        }
        self.failure_logged = true;
        crate::xess::diag(&format!(
            "[XeLL] {what} failed ({}); latency pacing is now a no-op for the rest of the session",
            xell_result_name(rc)
        ));
    }

    /// Sleep until this frame may start (render-queue reduction). Call once
    /// per frame before any markers for `frame_id`.
    pub(crate) fn sleep(&mut self, frame_id: u32) {
        let rc = unsafe { (self.sleep_fn)(self.context, frame_id) };
        if rc != XELL_RESULT_SUCCESS {
            self.report("xellSleep", rc);
        }
    }

    /// Deliver one phase marker for `frame_id`.
    pub(crate) fn marker(&mut self, frame_id: u32, marker: i32) {
        let _ = unsafe { (self.marker_fn)(self.context, frame_id, marker) };
    }
}

impl Drop for Xell {
    fn drop(&mut self) {
        if !self.context.is_null() {
            let _ = unsafe { (self.destroy_context)(self.context) };
            self.context = std::ptr::null_mut();
        }
        unsafe { free_sdk_dll(self.handle) };
    }
}
