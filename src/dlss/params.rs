//! The `NVSDK_NGX_Parameter` table the NGX features read their inputs from.
//!
//! `NVSDK_NGX_Parameter` is a pure-virtual C++ interface and every
//! `NVSDK_NGX_Parameter_Set*` helper in the headers is a thin wrapper over its
//! `Set` overloads. NGX **Core** (`nvngx.dll`, from the driver) allocates those
//! maps (`NVSDK_NGX_D3D12_AllocateParameters`) and exports the `Set*` helpers,
//! so on a real install the viewer fills NGX's own object; [`NgxParams::from_ngx`]
//! is that path and [`NgxMap`] the thin wrapper around it.
//!
//! The *feature* DLLs (`nvngx_dlss.dll`, `nvngx_dlssg.dll`) export neither the
//! allocator nor the helpers, so the viewer also carries a complete table of its
//! own ([`Owned`]) as the fallback — which is exactly what the vendored
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
//! So [`Owned`] is a `repr(C)` object whose first field is a pointer to a static
//! vtable of seventeen `extern "C"` functions — the MSVC ABI for a class with
//! virtual functions and no base class. The feature calls
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
use std::ffi::{c_char, c_void, CStr, CString};

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
    set_u64: unsafe extern "C" fn(*mut Owned, *const c_char, u64),
    set_f32: unsafe extern "C" fn(*mut Owned, *const c_char, f32),
    set_f64: unsafe extern "C" fn(*mut Owned, *const c_char, f64),
    set_u32: unsafe extern "C" fn(*mut Owned, *const c_char, u32),
    set_i32: unsafe extern "C" fn(*mut Owned, *const c_char, i32),
    set_d3d11: unsafe extern "C" fn(*mut Owned, *const c_char, *mut c_void),
    set_d3d12: unsafe extern "C" fn(*mut Owned, *const c_char, *mut c_void),
    set_void: unsafe extern "C" fn(*mut Owned, *const c_char, *mut c_void),
    get_u64: unsafe extern "C" fn(*mut Owned, *const c_char, *mut u64) -> u32,
    get_f32: unsafe extern "C" fn(*mut Owned, *const c_char, *mut f32) -> u32,
    get_f64: unsafe extern "C" fn(*mut Owned, *const c_char, *mut f64) -> u32,
    get_u32: unsafe extern "C" fn(*mut Owned, *const c_char, *mut u32) -> u32,
    get_i32: unsafe extern "C" fn(*mut Owned, *const c_char, *mut i32) -> u32,
    get_d3d11: unsafe extern "C" fn(*mut Owned, *const c_char, *mut *mut c_void) -> u32,
    get_d3d12: unsafe extern "C" fn(*mut Owned, *const c_char, *mut *mut c_void) -> u32,
    get_void: unsafe extern "C" fn(*mut Owned, *const c_char, *mut *mut c_void) -> u32,
    reset: unsafe extern "C" fn(*mut Owned),
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

/// The parameter table the viewer supplies itself. Pass [`Owned::as_ptr`] to
/// NGX as `NVSDK_NGX_Parameter*`; the first field is the vtable pointer, so the
/// object and the `this` pointer NGX passes back are the same address.
#[repr(C)]
pub(crate) struct Owned {
    vtable: *const NgxParamVTable,
    values: HashMap<String, Value>,
}

impl Owned {
    pub(crate) fn new() -> Box<Owned> {
        Box::new(Owned {
            vtable: &VTABLE,
            values: HashMap::new(),
        })
    }

    /// The `NVSDK_NGX_Parameter*` to hand to NGX.
    pub(crate) fn as_ptr(&mut self) -> *mut c_void {
        self as *mut Owned as *mut c_void
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

/// A `NVSDK_NGX_Parameter` map, from either side of the API.
///
/// * [`Kind::Owned`] is the table above: hand-rolled, ABI-compatible, and the
///   fallback when NGX Core does not export its allocator.
/// * [`Kind::Ngx`] is a map NGX Core allocated itself and that the viewer fills
///   through Core's exported `NVSDK_NGX_Parameter_Set*` helpers. That is the
///   path used on every real driver install: the feature DLLs then receive the
///   runtime's own parameter object instead of a foreign one.
pub(crate) struct NgxParams {
    kind: Kind,
}

enum Kind {
    Owned(Box<Owned>),
    Ngx(NgxMap),
}

/// NGX Core's parameter map plus the entry points that write into it.
///
/// Only the Windows build can create one, so the Linux build sees the type
/// unused — it still has to compile, and the unit test below exercises it.
#[cfg_attr(not(windows), allow(dead_code))]
///
/// The `Set*` family is exported by `nvngx.dll` (`nvsdk_ngx_params.h`), so the
/// viewer never has to guess the C++ vtable layout of NGX's own class.
pub(crate) struct NgxMap {
    object: *mut c_void,
    destroy: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    set_u32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, u32)>,
    set_i32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, i32)>,
    set_f32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, f32)>,
    set_d3d12: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void)>,
    set_void: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void)>,
}

impl NgxMap {
    pub(crate) fn new(
        object: *mut c_void,
        destroy: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
        set_u32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, u32)>,
        set_i32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, i32)>,
        set_f32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, f32)>,
        set_d3d12: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void)>,
        set_void: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void)>,
    ) -> Self {
        Self {
            object,
            destroy,
            set_u32,
            set_i32,
            set_f32,
            set_d3d12,
            set_void,
        }
    }

    fn c_name(name: &str) -> Option<CString> {
        CString::new(name).ok()
    }

    fn set_u32(&mut self, name: &str, value: u32) {
        if let (Some(set), Some(name)) = (self.set_u32, Self::c_name(name)) {
            unsafe { set(self.object, name.as_ptr(), value) }
        }
    }

    fn set_i32(&mut self, name: &str, value: i32) {
        if let (Some(set), Some(name)) = (self.set_i32, Self::c_name(name)) {
            unsafe { set(self.object, name.as_ptr(), value) }
        }
    }

    fn set_f32(&mut self, name: &str, value: f32) {
        if let (Some(set), Some(name)) = (self.set_f32, Self::c_name(name)) {
            unsafe { set(self.object, name.as_ptr(), value) }
        }
    }

    fn set_pointer(
        function: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void)>,
        object: *mut c_void,
        name: &str,
        value: *mut c_void,
    ) {
        if let (Some(set), Some(name)) = (function, Self::c_name(name)) {
            unsafe { set(object, name.as_ptr(), value) }
        }
    }

    fn set_d3d12_resource(&mut self, name: &str, value: *mut c_void) {
        Self::set_pointer(self.set_d3d12, self.object, name, value);
    }

    fn set_void_pointer(&mut self, name: &str, value: *mut c_void) {
        Self::set_pointer(self.set_void, self.object, name, value);
    }
}

impl Drop for NgxMap {
    fn drop(&mut self) {
        // The map belongs to NGX, so only its own `DestroyParameters` may free it.
        if let Some(destroy) = self.destroy {
            unsafe { destroy(self.object) };
        }
    }
}

impl NgxParams {
    /// The viewer-owned table (fallback, and what the tests exercise).
    pub(crate) fn new() -> Box<NgxParams> {
        Box::new(NgxParams {
            kind: Kind::Owned(Owned::new()),
        })
    }

    /// A map allocated by NGX Core.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn from_ngx(map: NgxMap) -> Box<NgxParams> {
        Box::new(NgxParams {
            kind: Kind::Ngx(map),
        })
    }

    /// The `NVSDK_NGX_Parameter*` to hand to NGX.
    pub(crate) fn as_ptr(&mut self) -> *mut c_void {
        match &mut self.kind {
            Kind::Owned(table) => table.as_ptr(),
            Kind::Ngx(map) => map.object,
        }
    }

    /// `NVSDK_NGX_Parameter_SetI`.
    pub(crate) fn set_i32(&mut self, name: &str, value: i32) {
        match &mut self.kind {
            Kind::Owned(table) => table.set_i32(name, value),
            Kind::Ngx(map) => map.set_i32(name, value),
        }
    }

    /// `NVSDK_NGX_Parameter_SetUI`.
    pub(crate) fn set_u32(&mut self, name: &str, value: u32) {
        match &mut self.kind {
            Kind::Owned(table) => table.set_u32(name, value),
            Kind::Ngx(map) => map.set_u32(name, value),
        }
    }

    /// `NVSDK_NGX_Parameter_SetF`.
    pub(crate) fn set_f32(&mut self, name: &str, value: f32) {
        match &mut self.kind {
            Kind::Owned(table) => table.set_f32(name, value),
            Kind::Ngx(map) => map.set_f32(name, value),
        }
    }

    /// `NVSDK_NGX_Parameter_SetD3d12Resource`.
    pub(crate) fn set_d3d12_resource(&mut self, name: &str, resource: *mut c_void) {
        match &mut self.kind {
            Kind::Owned(table) => table.set_d3d12_resource(name, resource),
            Kind::Ngx(map) => map.set_d3d12_resource(name, resource),
        }
    }

    /// `NVSDK_NGX_Parameter_SetVoidPointer`.
    pub(crate) fn set_void_pointer(&mut self, name: &str, pointer: *mut c_void) {
        match &mut self.kind {
            Kind::Owned(table) => table.set_void_pointer(name, pointer),
            Kind::Ngx(map) => map.set_void_pointer(name, pointer),
        }
    }

    /// Drop every entry. NGX's own map is only reset by NGX itself (there is no
    /// exported `Reset`), so this is a no-op for it.
    #[allow(dead_code)] // exercised by the tests; nothing resets a live map
    pub(crate) fn reset(&mut self) {
        if let Kind::Owned(table) = &mut self.kind {
            table.reset();
        }
    }

    /// Number of stored entries (diagnostics and tests).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        match &self.kind {
            Kind::Owned(table) => table.len(),
            Kind::Ngx(_) => 0,
        }
    }

    /// `true` while the map belongs to NGX Core (tests).
    #[cfg(test)]
    pub(crate) fn is_ngx(&self) -> bool {
        matches!(self.kind, Kind::Ngx(_))
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

unsafe fn set_value(this: *mut Owned, name: *const c_char, value: Value) {
    if this.is_null() {
        return;
    }
    let Some(key) = key(name) else {
        return;
    };
    (*this).values.insert(key, value);
}

unsafe extern "C" fn vtable_set_u64(this: *mut Owned, name: *const c_char, value: u64) {
    set_value(this, name, Value::U64(value));
}

unsafe extern "C" fn vtable_set_f32(this: *mut Owned, name: *const c_char, value: f32) {
    set_value(this, name, Value::F32(value));
}

unsafe extern "C" fn vtable_set_f64(this: *mut Owned, name: *const c_char, value: f64) {
    set_value(this, name, Value::F64(value));
}

unsafe extern "C" fn vtable_set_u32(this: *mut Owned, name: *const c_char, value: u32) {
    set_value(this, name, Value::U32(value));
}

unsafe extern "C" fn vtable_set_i32(this: *mut Owned, name: *const c_char, value: i32) {
    set_value(this, name, Value::I32(value));
}

unsafe extern "C" fn vtable_set_ptr(this: *mut Owned, name: *const c_char, value: *mut c_void) {
    set_value(this, name, Value::Ptr(value));
}

unsafe extern "C" fn vtable_get_u64(this: *mut Owned, name: *const c_char, out: *mut u64) -> u32 {
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

unsafe extern "C" fn vtable_get_f32(this: *mut Owned, name: *const c_char, out: *mut f32) -> u32 {
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

unsafe extern "C" fn vtable_get_f64(this: *mut Owned, name: *const c_char, out: *mut f64) -> u32 {
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

unsafe extern "C" fn vtable_get_u32(this: *mut Owned, name: *const c_char, out: *mut u32) -> u32 {
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

unsafe extern "C" fn vtable_get_i32(this: *mut Owned, name: *const c_char, out: *mut i32) -> u32 {
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
    this: *mut Owned,
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

unsafe extern "C" fn vtable_reset(this: *mut Owned) {
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
        let ptr = params.as_ptr() as *mut Owned;
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
        let ptr = params.as_ptr() as *mut Owned;
        let missing = CString::new("Nope").unwrap();
        unsafe {
            let mut out_u32 = 7u32;
            let result = (VTABLE.get_u32)(ptr, missing.as_ptr(), &mut out_u32);
            assert_eq!(result, NGX_FAIL);
            assert_eq!(out_u32, 7, "a failed Get must not touch the output");
        }
    }

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static SET_U32_CALLS: AtomicUsize = AtomicUsize::new(0);
    static DESTROYED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn counting_set_u32(object: *mut c_void, _name: *const c_char, value: u32) {
        assert_eq!(
            object as usize, 0xdead_beef,
            "the object must be passed through"
        );
        assert_eq!(value, 1920);
        SET_U32_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn counting_destroy(_object: *mut c_void) -> u32 {
        DESTROYED.store(true, Ordering::SeqCst);
        NGX_SUCCESS
    }

    /// A map allocated by NGX Core is written through Core's exported setters,
    /// not through the viewer's table, and is handed back to NGX on drop.
    #[test]
    fn a_core_map_is_written_through_the_ngx_exports() {
        let map = NgxMap::new(
            0xdead_beef as *mut c_void,
            Some(counting_destroy),
            Some(counting_set_u32),
            None,
            None,
            None,
            None,
        );
        {
            let mut params = NgxParams::from_ngx(map);
            assert!(params.is_ngx());
            assert_eq!(params.as_ptr() as usize, 0xdead_beef);
            params.set_u32("Width", 1920);
            // A setter the driver does not export is a silent no-op, never a
            // panic: NGX's own map is the only thing that may write itself.
            params.set_f32("Jitter.Offset.X", 0.25);
            params.set_d3d12_resource("Color", 0x10 as *mut c_void);
            params.reset();
            assert_eq!(SET_U32_CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(params.len(), 0, "NGX's map has no local entries");
        }
        assert!(
            DESTROYED.load(Ordering::SeqCst),
            "the map must go back to NGX"
        );
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
