//! The `NVSDK_NGX_Parameter` table the NGX plugins read their inputs from.
//!
//! NGX does not hand the application a parameter object: in the DLSS SDKs the
//! `NVSDK_NGX_Parameter` "class" is a pure-virtual C++ interface and every
//! `NVSDK_NGX_Parameter_Set*` helper the headers show is a thin wrapper over
//! its `Set` overloads. The DLLs export the feature entry points
//! (`NVSDK_NGX_D3D12_Init_Ext`, `…CreateFeature`, `…EvaluateFeature`,
//! `…ReleaseFeature`, `…Shutdown1`) but no `AllocateParameters` /
//! `GetCapabilityParameters` / `Parameter_Set*`, so the table has to be
//! supplied by the application — which is exactly what the vendored
//! `include/nvidia/nvsdk_ngx_params.h` describes:
//!
//! ```c
//! typedef struct NVSDK_NGX_Parameter {
//!     virtual void Set(const char *, unsigned long long) = 0;   // 8 Set …
//!     …
//!     virtual NVSDK_NGX_Result Get(const char *, unsigned long long *) const = 0;
//!     …                                                         // … 8 Get
//!     virtual void Reset() = 0;
//! } NVSDK_NGX_Parameter;
//! ```
//!
//! So [`NgxParams`] is a `repr(C)` object whose first field is a pointer to a
//! static vtable of seventeen `extern "C"` functions — the MSVC ABI for a
//! class with virtual functions and no base class. The plugin calls
//! `param->Set("Width", 1920u)`, we store it; it calls
//! `param->Get("DLSS.Feature.Create.Flags", &flags)`, we hand it back.
//!
//! Values are converted permissively on `Get`: NGX features are inconsistent
//! about whether a flag is an `int` or an `unsigned int` (the DLSS *create*
//! flags go in as `int`, the DLSS-G node masks as `unsigned int`), so an
//! integer read accepts either width, and a missing key is
//! `NVSDK_NGX_Result_Fail` (no value), never a panic.
//!
//! This module is platform-independent on purpose: the object is plain Rust,
//! so it is unit-tested on every CI platform even though only the Windows
//! build drives it.

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};

/// `NVSDK_NGX_Result_Success`. NGX success is `0x1`, not `0`.
pub(crate) const NGX_SUCCESS: u32 = 0x0000_0001;
/// `NVSDK_NGX_Result_Fail` — "no such value" for `Get`.
pub(crate) const NGX_FAIL: u32 = 0xbad0_0000;

/// One stored parameter value, tagged with the overload it came in through.
#[derive(Clone, Copy, Debug)]
enum Value {
    U64(u64),
    F32(f32),
    F64(f64),
    U32(u32),
    I32(i32),
    /// `ID3D11Resource*`, `ID3D12Resource*` and `void*` share one slot: NGX
    /// never stores two of those under one name.
    Ptr(*mut c_void),
}

/// The vtable NGX sees. Field order is the declaration order in
/// `nvsdk_ngx_params.h`; it must not be reordered.
#[repr(C)]
struct NgxParamVTable {
    set_u64: unsafe extern "C" fn(*mut NgxParams, *const c_char, u64),
    set_f32: unsafe extern "C" fn(*mut NgxParams, *const c_char, f32),
    set_f64: unsafe extern "C" fn(*mut NgxParams, *const c_char, f64),
    set_u32: unsafe extern "C" fn(*mut NgxParams, *const c_char, u32),
    set_i32: unsafe extern "C" fn(*mut NgxParams, *const c_char, i32),
    set_d3d11: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut c_void),
    set_d3d12: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut c_void),
    set_void: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut c_void),
    get_u64: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut u64) -> u32,
    get_f32: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut f32) -> u32,
    get_f64: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut f64) -> u32,
    get_u32: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut u32) -> u32,
    get_i32: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut i32) -> u32,
    get_d3d11: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut *mut c_void) -> u32,
    get_d3d12: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut *mut c_void) -> u32,
    get_void: unsafe extern "C" fn(*mut NgxParams, *const c_char, *mut *mut c_void) -> u32,
    reset: unsafe extern "C" fn(*mut NgxParams),
}

static VTABLE: NgxParamVTable = NgxParamVTable {
    set_u64: vtable_set_u64,
    set_f32: vtable_set_f32,
    set_f64: vtable_set_f64,
    set_u32: vtable_set_u32,
    set_i32: vtable_set_i32,
    set_d3d11: vtable_set_ptr,
    set_d3d12: vtable_set_ptr,
    set_void: vtable_set_ptr,
    get_u64: vtable_get_u64,
    get_f32: vtable_get_f32,
    get_f64: vtable_get_f64,
    get_u32: vtable_get_u32,
    get_i32: vtable_get_i32,
    get_d3d11: vtable_get_ptr,
    get_d3d12: vtable_get_ptr,
    get_void: vtable_get_ptr,
    reset: vtable_reset,
};

/// The parameter table. Pass [`NgxParams::as_ptr`] to NGX as
/// `NVSDK_NGX_Parameter*`; the first field is the vtable pointer, so the
/// object and the `this` pointer NGX passes back are the same address.
#[repr(C)]
pub(crate) struct NgxParams {
    vtable: *const NgxParamVTable,
    values: HashMap<String, Value>,
}

impl NgxParams {
    pub(crate) fn new() -> Box<NgxParams> {
        Box::new(NgxParams {
            vtable: &VTABLE,
            values: HashMap::new(),
        })
    }

    /// The `NVSDK_NGX_Parameter*` to hand to NGX.
    pub(crate) fn as_ptr(&mut self) -> *mut c_void {
        self as *mut NgxParams as *mut c_void
    }

    /// `NVSDK_NGX_Parameter_SetI`.
    pub(crate) fn set_i32(&mut self, name: &str, value: i32) {
        self.values.insert(name.to_string(), Value::I32(value));
    }

    /// `NVSDK_NGX_Parameter_SetUI`.
    pub(crate) fn set_u32(&mut self, name: &str, value: u32) {
        self.values.insert(name.to_string(), Value::U32(value));
    }

    /// `NVSDK_NGX_Parameter_SetF`.
    pub(crate) fn set_f32(&mut self, name: &str, value: f32) {
        self.values.insert(name.to_string(), Value::F32(value));
    }

    /// `NVSDK_NGX_Parameter_SetD3d12Resource`.
    pub(crate) fn set_d3d12_resource(&mut self, name: &str, resource: *mut c_void) {
        self.values.insert(name.to_string(), Value::Ptr(resource));
    }

    /// `NVSDK_NGX_Parameter_SetVoidPointer`.
    pub(crate) fn set_void_pointer(&mut self, name: &str, pointer: *mut c_void) {
        self.values.insert(name.to_string(), Value::Ptr(pointer));
    }

    /// Drop every entry; NGX calls this between frames in some paths.
    pub(crate) fn reset(&mut self) {
        self.values.clear();
    }

    /// Number of stored entries (diagnostics and tests).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }
}

/// Read a name NGX passed in. Returns `None` for a null or non-UTF-8 name,
/// which NGX never sends but which must not be able to abort the process.
unsafe fn key(name: *const c_char) -> Option<String> {
    if name.is_null() {
        return None;
    }
    Some(CStr::from_ptr(name).to_string_lossy().into_owned())
}

unsafe fn set_value(this: *mut NgxParams, name: *const c_char, value: Value) {
    if this.is_null() {
        return;
    }
    let Some(key) = key(name) else {
        return;
    };
    (*this).values.insert(key, value);
}

unsafe extern "C" fn vtable_set_u64(this: *mut NgxParams, name: *const c_char, value: u64) {
    set_value(this, name, Value::U64(value));
}

unsafe extern "C" fn vtable_set_f32(this: *mut NgxParams, name: *const c_char, value: f32) {
    set_value(this, name, Value::F32(value));
}

unsafe extern "C" fn vtable_set_f64(this: *mut NgxParams, name: *const c_char, value: f64) {
    set_value(this, name, Value::F64(value));
}

unsafe extern "C" fn vtable_set_u32(this: *mut NgxParams, name: *const c_char, value: u32) {
    set_value(this, name, Value::U32(value));
}

unsafe extern "C" fn vtable_set_i32(this: *mut NgxParams, name: *const c_char, value: i32) {
    set_value(this, name, Value::I32(value));
}

unsafe extern "C" fn vtable_set_ptr(this: *mut NgxParams, name: *const c_char, value: *mut c_void) {
    set_value(this, name, Value::Ptr(value));
}

unsafe extern "C" fn vtable_get_u64(
    this: *mut NgxParams,
    name: *const c_char,
    out: *mut u64,
) -> u32 {
    if this.is_null() || out.is_null() {
        return NGX_FAIL;
    }
    let Some(key) = key(name) else {
        return NGX_FAIL;
    };
    match (*this).values.get(&key) {
        Some(Value::U64(value)) => {
            *out = *value;
            NGX_SUCCESS
        }
        Some(Value::U32(value)) => {
            *out = u64::from(*value);
            NGX_SUCCESS
        }
        Some(Value::I32(value)) => {
            *out = *value as u64;
            NGX_SUCCESS
        }
        _ => NGX_FAIL,
    }
}

unsafe extern "C" fn vtable_get_f32(
    this: *mut NgxParams,
    name: *const c_char,
    out: *mut f32,
) -> u32 {
    if this.is_null() || out.is_null() {
        return NGX_FAIL;
    }
    let Some(key) = key(name) else {
        return NGX_FAIL;
    };
    match (*this).values.get(&key) {
        Some(Value::F32(value)) => {
            *out = *value;
            NGX_SUCCESS
        }
        Some(Value::F64(value)) => {
            *out = *value as f32;
            NGX_SUCCESS
        }
        _ => NGX_FAIL,
    }
}

unsafe extern "C" fn vtable_get_f64(
    this: *mut NgxParams,
    name: *const c_char,
    out: *mut f64,
) -> u32 {
    if this.is_null() || out.is_null() {
        return NGX_FAIL;
    }
    let Some(key) = key(name) else {
        return NGX_FAIL;
    };
    match (*this).values.get(&key) {
        Some(Value::F64(value)) => {
            *out = *value;
            NGX_SUCCESS
        }
        Some(Value::F32(value)) => {
            *out = f64::from(*value);
            NGX_SUCCESS
        }
        _ => NGX_FAIL,
    }
}

unsafe extern "C" fn vtable_get_u32(
    this: *mut NgxParams,
    name: *const c_char,
    out: *mut u32,
) -> u32 {
    if this.is_null() || out.is_null() {
        return NGX_FAIL;
    }
    let Some(key) = key(name) else {
        return NGX_FAIL;
    };
    match (*this).values.get(&key) {
        Some(Value::U32(value)) => {
            *out = *value;
            NGX_SUCCESS
        }
        // NGX mixes the signedness of the same flag between features.
        Some(Value::I32(value)) => {
            *out = *value as u32;
            NGX_SUCCESS
        }
        Some(Value::U64(value)) => {
            *out = *value as u32;
            NGX_SUCCESS
        }
        _ => NGX_FAIL,
    }
}

unsafe extern "C" fn vtable_get_i32(
    this: *mut NgxParams,
    name: *const c_char,
    out: *mut i32,
) -> u32 {
    if this.is_null() || out.is_null() {
        return NGX_FAIL;
    }
    let Some(key) = key(name) else {
        return NGX_FAIL;
    };
    match (*this).values.get(&key) {
        Some(Value::I32(value)) => {
            *out = *value;
            NGX_SUCCESS
        }
        Some(Value::U32(value)) => {
            *out = *value as i32;
            NGX_SUCCESS
        }
        Some(Value::U64(value)) => {
            *out = *value as i32;
            NGX_SUCCESS
        }
        _ => NGX_FAIL,
    }
}

unsafe extern "C" fn vtable_get_ptr(
    this: *mut NgxParams,
    name: *const c_char,
    out: *mut *mut c_void,
) -> u32 {
    if this.is_null() || out.is_null() {
        return NGX_FAIL;
    }
    let Some(key) = key(name) else {
        return NGX_FAIL;
    };
    match (*this).values.get(&key) {
        Some(Value::Ptr(value)) => {
            *out = *value;
            NGX_SUCCESS
        }
        _ => NGX_FAIL,
    }
}

unsafe extern "C" fn vtable_reset(this: *mut NgxParams) {
    if this.is_null() {
        return;
    }
    (*this).values.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn stores_and_returns_every_overload() {
        let mut params = NgxParams::new();
        let ptr = params.as_ptr() as *mut NgxParams;
        let width = CString::new("Width").unwrap();
        let jitter = CString::new("Jitter.Offset.X").unwrap();
        let resource = CString::new("Color").unwrap();
        unsafe {
            (VTABLE.set_u32)(ptr, width.as_ptr(), 1920);
            (VTABLE.set_f32)(ptr, jitter.as_ptr(), -0.25);
            (VTABLE.set_d3d12)(ptr, resource.as_ptr(), 0x1234 as *mut c_void);

            let mut out_u32 = 0u32;
            let result = (VTABLE.get_u32)(ptr, width.as_ptr(), &mut out_u32);
            assert_eq!(result, NGX_SUCCESS);
            assert_eq!(out_u32, 1920);

            let mut out_i32 = 0i32;
            let result = (VTABLE.get_i32)(ptr, width.as_ptr(), &mut out_i32);
            assert_eq!(result, NGX_SUCCESS);
            assert_eq!(out_i32, 1920);

            let mut out_f32 = 0.0f32;
            let result = (VTABLE.get_f32)(ptr, jitter.as_ptr(), &mut out_f32);
            assert_eq!(result, NGX_SUCCESS);
            assert_eq!(out_f32, -0.25);

            let mut out_ptr: *mut c_void = std::ptr::null_mut();
            let result = (VTABLE.get_d3d12)(ptr, resource.as_ptr(), &mut out_ptr);
            assert_eq!(result, NGX_SUCCESS);
            assert_eq!(out_ptr as usize, 0x1234);
        }
    }

    #[test]
    fn a_missing_name_is_a_failure_not_a_panic() {
        let mut params = NgxParams::new();
        let ptr = params.as_ptr() as *mut NgxParams;
        let missing = CString::new("Nope").unwrap();
        unsafe {
            let mut out_u32 = 7u32;
            let result = (VTABLE.get_u32)(ptr, missing.as_ptr(), &mut out_u32);
            assert_eq!(result, NGX_FAIL);
            assert_eq!(out_u32, 7, "a failed Get must not touch the output");
        }
    }

    #[test]
    fn pointer_names_keep_the_last_value() {
        let mut params = NgxParams::new();
        params.set_d3d12_resource("Output", 0x1 as *mut c_void);
        params.set_d3d12_resource("Output", 0x2 as *mut c_void);
        assert_eq!(params.len(), 1);
        params.reset();
        assert_eq!(params.len(), 0);
    }
}
