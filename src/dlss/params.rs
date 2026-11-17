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
//! allocator nor the helpers — `nvngx_dlss.dll` 310.9.16 exports no
//! `NVSDK_NGX_Parameter_*` name at all — and the driver's core is where the
//! allocators live. Whatever map the core hands out is then filled through its
//! own vtable ([`NgxMap::through_vtable`]) rather than through the exported
//! helpers, so a core that exports only the allocators is enough. When no
//! allocator answers, the viewer falls back to a complete table of its own
//! ([`Owned`]), which is exactly what the vendored
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

/// The vtable NGX sees.
///
/// The order is **measured, not declared**. `nvsdk_ngx_params.h:55-73` lists the
/// overloads as eight `Set`s (u64, float, double, uint, int, D3D11, D3D12,
/// void*), then eight `Get`s in the same order, then `Reset` — and this table
/// used to follow it. A real driver core (`_nvngx.dll` 32.0.16.2114) implements
/// the *reverse* of that list, which a session measured overload by overload
/// (`docs/blockers.md` B-27):
///
/// ```text
/// Set 0 void*   1 D3D12*  2 D3D11*  3 int  4 uint  5 double  6 float  7 u64   [16 Reset]
/// Get 8 void**  9 D3D12** 10 D3D11** 11 int* 12 uint* 13 double* 14 float* 15 u64*
/// ```
///
/// Every pointer written at slot 6 (which the declaration calls `Set(ID3D12Resource
/// *)`) was therefore stored as a **float**, the `Get` read-backs at 13/14 came
/// back as numeric conversions, and the feature — which reads at slot 9 — never
/// found `Color`. The field order below is that measured order, so both NGX's
/// map and this one answer the same overloads at the same indices.
#[repr(C)]
struct NgxParamVTable {
    set_void: unsafe extern "C" fn(*mut Owned, *const c_char, *mut c_void),
    set_d3d12: unsafe extern "C" fn(*mut Owned, *const c_char, *mut c_void),
    set_d3d11: unsafe extern "C" fn(*mut Owned, *const c_char, *mut c_void),
    set_i32: unsafe extern "C" fn(*mut Owned, *const c_char, i32),
    set_u32: unsafe extern "C" fn(*mut Owned, *const c_char, u32),
    set_f64: unsafe extern "C" fn(*mut Owned, *const c_char, f64),
    set_f32: unsafe extern "C" fn(*mut Owned, *const c_char, f32),
    set_u64: unsafe extern "C" fn(*mut Owned, *const c_char, u64),
    get_void: unsafe extern "C" fn(*mut Owned, *const c_char, *mut *mut c_void) -> u32,
    get_d3d12: unsafe extern "C" fn(*mut Owned, *const c_char, *mut *mut c_void) -> u32,
    get_d3d11: unsafe extern "C" fn(*mut Owned, *const c_char, *mut *mut c_void) -> u32,
    get_i32: unsafe extern "C" fn(*mut Owned, *const c_char, *mut i32) -> u32,
    get_u32: unsafe extern "C" fn(*mut Owned, *const c_char, *mut u32) -> u32,
    get_f64: unsafe extern "C" fn(*mut Owned, *const c_char, *mut f64) -> u32,
    get_f32: unsafe extern "C" fn(*mut Owned, *const c_char, *mut f32) -> u32,
    get_u64: unsafe extern "C" fn(*mut Owned, *const c_char, *mut u64) -> u32,
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

// `NVSDK_NGX_Parameter` vtable slots **as this core implements them**, which is
// the reverse of the declaration in `nvsdk_ngx_params.h:55-73` (see
// [`NgxParamVTable`] for the measurement). The comment on each line is the
// overload that slot actually is on the driver core, and the site that verified
// it is in `docs/blockers.md` B-27.
const SLOT_SET_VOID: usize = 0;
const SLOT_SET_D3D12: usize = 1;
const SLOT_SET_D3D11: usize = 2;
const SLOT_SET_I32: usize = 3;
const SLOT_SET_U32: usize = 4;
const SLOT_SET_F64: usize = 5;
const SLOT_SET_F32: usize = 6;
const SLOT_SET_U64: usize = 7;
const SLOT_GET_VOID: usize = 8;
const SLOT_GET_D3D12: usize = 9;
const SLOT_GET_D3D11: usize = 10;
const SLOT_GET_I32: usize = 11;
const SLOT_GET_U32: usize = 12;
const SLOT_GET_F64: usize = 13;
const SLOT_GET_F32: usize = 14;
const SLOT_GET_U64: usize = 15;

/// The last `Set` overload, i.e. how many of them there are. `SLOT_SET_VOID`
/// used to be the last one and is now the *first*, so nothing may walk
/// `0..=SLOT_SET_VOID` any more — that walks a single slot.
const SLOT_SET_LAST: usize = SLOT_SET_U64;
/// The last `Get` overload, mirroring [`SLOT_SET_LAST`].
#[cfg_attr(not(windows), allow(dead_code))]
const SLOT_GET_LAST: usize = SLOT_GET_U64;

/// The `Set` slots that can carry a resource pointer, in the order the
/// declaration puts them.
///
/// Which of these a given core actually *is* is measured, not assumed: see
/// [`NgxMap::probe_pointer_slots`]. The declared order is tried first so a core
/// that matches its own header costs three probe calls and nothing else.
const POINTER_SET_SLOTS: [usize; 3] = [SLOT_SET_D3D12, SLOT_SET_D3D11, SLOT_SET_VOID];
/// The `Get` slots that can read a resource pointer back, same order.
const POINTER_GET_SLOTS: [usize; 3] = [SLOT_GET_D3D12, SLOT_GET_D3D11, SLOT_GET_VOID];

/// The `Set` slots a resource pointer is tried through, in order: the
/// declaration's `ID3D12Resource *` overload, then the D3D11 one, then `void *`.
///
/// A *runtime* list, not a guess: `DlssUpscaler::record` writes the four evaluate
/// resources through each in turn until the feature accepts the evaluate, because
/// the only authority on which overload its own reader agrees with is the feature
/// itself (`docs/blockers.md` B-27).
pub(crate) const RESOURCE_SET_SLOT_CANDIDATES: [usize; 3] =
    [SLOT_SET_D3D12, SLOT_SET_D3D11, SLOT_SET_VOID];

/// The `Set` slots one session tries, in order, for the resource keys.
///
/// The measured slot goes first — it is the one the map's own pointer
/// round-trip answered, so it is the most likely to be right — and the rest of
/// the ladder follows. On later frames the caller passes only the winner, so
/// this is a first-frame search, not a per-frame cost.
pub(crate) fn resource_slot_ladder(measured: usize) -> Vec<usize> {
    let mut ladder = Vec::with_capacity(RESOURCE_SET_SLOT_CANDIDATES.len());
    ladder.push(measured);
    for slot in RESOURCE_SET_SLOT_CANDIDATES {
        if slot != measured {
            ladder.push(slot);
        }
    }
    ladder
}

/// One `Set` overload of an `NVSDK_NGX_Parameter` the viewer does not own.
///
/// `None` when the object or its vtable is missing, or when the slot is null —
/// a map that cannot be written is worse than no map at all.
unsafe fn vtable_slot<F: Copy>(object: *mut c_void, index: usize) -> Option<F> {
    if object.is_null() {
        return None;
    }
    let vtable = *(object as *const *const *const c_void);
    if vtable.is_null() {
        return None;
    }
    let entry = *vtable.add(index);
    if entry.is_null() {
        return None;
    }
    // A `*const c_void` and a function pointer are both one word here, but
    // Rust forbids `transmute` between them (`E0512` on Windows x64).
    Some(std::mem::transmute_copy(&entry))
}

/// A `Set` overload of an `NVSDK_NGX_Parameter` the viewer does not own.
unsafe fn vtable_setter<F: Copy>(object: *mut c_void, index: usize) -> Option<F> {
    vtable_slot::<F>(object, index)
}

/// A `Get` overload of an `NVSDK_NGX_Parameter` the viewer does not own.
unsafe fn vtable_getter<F: Copy>(object: *mut c_void, index: usize) -> Option<F> {
    vtable_slot::<F>(object, index)
}

/// Whether `object` looks like an `NVSDK_NGX_Parameter` the viewer can fill
/// through its own vtable.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_writable(object: *mut c_void) -> bool {
    type SetU32 = unsafe extern "C" fn(*mut c_void, *const c_char, u32);
    unsafe { vtable_setter::<SetU32>(object, SLOT_SET_U32) }.is_some()
}

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

    /// Read an integer back, the way `NVSDK_NGX_Parameter_GetUI` would.
    pub(crate) fn get_u32(&self, name: &str) -> Option<u32> {
        match self.values.get(name)? {
            Value::U32(value) => Some(*value),
            Value::I32(value) => u32::try_from(*value).ok(),
            Value::U64(value) => u32::try_from(*value).ok(),
            _ => None,
        }
    }

    /// Read a pointer back, the way `NVSDK_NGX_Parameter_GetVoidPointer` would.
    pub(crate) fn get_void_pointer(&self, name: &str) -> Option<*mut c_void> {
        match self.values.get(name)? {
            Value::Ptr(value) => Some(*value),
            _ => None,
        }
    }

    /// Write one real pointer through every declared `Set` overload and read it
    /// back through four readers.
    ///
    /// The viewer's own table stores every pointer the same way, so the matrix
    /// is uniform here — which is what makes it a useful baseline against NGX's
    /// map in the same log.
    fn resource_slot_matrix(
        &self,
        name: &str,
        resource: *mut c_void,
    ) -> (Option<usize>, Vec<String>) {
        pointer_slot_matrix(self as *const Owned as *mut c_void, name, resource)
    }

    /// The numeric half of the same measurement.
    fn numeric_slot_matrix(&self) -> Vec<String> {
        numeric_slot_matrix(self as *const Owned as *mut c_void)
    }

    /// Read one pointer key back through every candidate `Get` slot.
    ///
    /// Every pointer overload of the viewer's own table stores the same
    /// `Value::Ptr`, so all three readers answer with the stored address — the
    /// same shape the diagnostics print for NGX's map when that map behaves.
    fn describe_pointer_gets(&self, name: &str) -> String {
        pointer_get_report(self as *const Owned as *mut c_void, name)
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
/// Two ways in, both supported here: the exported `NVSDK_NGX_Parameter_Set*`
/// helpers ([`NgxMap::new`]), and the object's own vtable
/// ([`NgxMap::through_vtable`]). The vtable layout is not a guess — it is the
/// declaration order in `nvsdk_ngx_params.h`, the same one [`NgxParamVTable`]
/// implements for the map the viewer supplies itself.
pub(crate) struct NgxMap {
    object: *mut c_void,
    destroy: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    set_u32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, u32)>,
    set_i32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, i32)>,
    set_f32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, f32)>,
    set_d3d12: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void)>,
    set_void: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void)>,
    get_u32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut u32) -> u32>,
    get_void: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> u32>,
    /// The `Set` slot resource pointers are written through.
    ///
    /// Starts as the declaration order's `ID3D12Resource *` slot and is replaced
    /// by whichever slot the feature *accepts*, which the caller measures with
    /// real evaluates (see `DlssUpscaler::record`). A wrong slot is silent: the
    /// pointer is stored under a different type tag and the feature reports the
    /// key as missing, which is why this cannot be decided from the header.
    resource_set_slot: usize,
}

/// Read a pointer key back out of an `NVSDK_NGX_Parameter` through every
/// candidate `Get` slot.
///
/// The companion to the `Set` ladder: after a rejected evaluate this says
/// whether the map still holds the pointer that was just written, and through
/// which reader. Three outcomes are distinguishable — an address (stored and
/// readable), `untouched` (the reader did not recognise the key) and `fail` —
/// and they separate "the map lost the value" from "the map has it under a tag
/// the feature does not read" (`docs/blockers.md` B-27).
fn pointer_get_report(object: *mut c_void, name: &str) -> String {
    type GetPtr = unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> u32;
    let Ok(name) = CString::new(name) else {
        return String::from("unreadable key");
    };
    let sentinel = 1usize as *mut c_void;
    let mut parts = Vec::with_capacity(POINTER_GET_SLOTS.len());
    for slot in POINTER_GET_SLOTS {
        let Some(get) = (unsafe { vtable_getter::<GetPtr>(object, slot) }) else {
            parts.push(format!("slot {slot} absent"));
            continue;
        };
        // A map that does not hold the key leaves the out-parameter alone, so it
        // is pre-filled to tell "not stored" from "stored as null".
        let mut value = sentinel;
        let result = unsafe { get(object, name.as_ptr(), &mut value) };
        if result != NGX_SUCCESS {
            parts.push(format!("slot {slot} fail(0x{result:08x})"));
        } else if value == sentinel {
            parts.push(format!("slot {slot} untouched"));
        } else {
            parts.push(format!("slot {slot} 0x{:x}", value as usize));
        }
    }
    parts.join(", ")
}

/// Write one *real* resource pointer through every declared `Set` overload and
/// read it back through four readers, reporting which combination keeps it.
///
/// This is the measurement a null probe cannot make: a null stored as a number
/// and a null stored as a resource both read back as zero. A real handle
/// survives exactly through the overload that stores it as a resource, comes
/// back *converted* through a numeric reader, and comes back as unrelated bytes
/// through a reader of another pointer type. Session-7 showed all three of
/// those shapes for the same key, which is how the map — not the slot — became
/// the suspect (`docs/blockers.md` B-27).
///
/// The returned slot is the `Set` overload after which the
/// `ID3D12Resource **` reader returns the pointer *unchanged* — the pair the
/// feature's own read has to agree with. `None` means no declared pair keeps a
/// resource, which is a different defect from a wrong slot.
fn pointer_slot_matrix(
    object: *mut c_void,
    name: &str,
    resource: *mut c_void,
) -> (Option<usize>, Vec<String>) {
    type SetPtr = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void);
    type GetPtr = unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> u32;
    let Ok(key) = CString::new(name) else {
        return (None, Vec::new());
    };
    let sentinel = 1usize as *mut c_void;
    // Every `Set` overload whose write the `ID3D12Resource **` reader returns
    // unchanged. More than one can qualify — a map that converts pointer types
    // answers through several — so the declared overload is preferred and only
    // then the first that works.
    let mut matches: Vec<usize> = Vec::with_capacity(3);
    let mut lines = Vec::with_capacity(SLOT_SET_LAST + 1);
    for set_slot in 0..=SLOT_SET_LAST {
        let Some(set) = (unsafe { vtable_setter::<SetPtr>(object, set_slot) }) else {
            lines.push(format!("Set {set_slot}: no vtable entry"));
            continue;
        };
        unsafe { set(object, key.as_ptr(), resource) };
        let mut parts = Vec::with_capacity(4);
        for (label, get_slot) in [
            ("d3d12", SLOT_GET_D3D12),
            ("d3d11", SLOT_GET_D3D11),
            ("void", SLOT_GET_VOID),
            ("u64", SLOT_GET_U64),
        ] {
            let Some(get) = (unsafe { vtable_getter::<GetPtr>(object, get_slot) }) else {
                parts.push(format!("{label} absent"));
                continue;
            };
            let mut value = sentinel;
            let result = unsafe { get(object, key.as_ptr(), &mut value) };
            if result != NGX_SUCCESS {
                parts.push(format!("{label} fail(0x{result:08x})"));
            } else if value == resource {
                parts.push(format!("{label} keeps it"));
                if get_slot == SLOT_GET_D3D12 {
                    matches.push(set_slot);
                }
            } else if value == sentinel {
                parts.push(format!("{label} untouched"));
            } else {
                parts.push(format!("{label} ->0x{:x}", value as usize));
            }
        }
        lines.push(format!("Set {set_slot}: {}", parts.join(", ")));
    }
    let found = if matches.contains(&SLOT_SET_D3D12) {
        Some(SLOT_SET_D3D12)
    } else {
        matches.first().copied()
    };
    (found, lines)
}

/// Write one distinctive integer through every declared `Set` overload and read
/// it back through every declared reader.
///
/// The companion of [`pointer_slot_matrix`], and the cheaper half of the same
/// question: it pins where the *numeric* readers sit on this core's vtable. A
/// value that comes back unchanged names a reader; a value that comes back
/// converted names the reader that converted it (`0x123456789abcdef0` as a
/// `float` is `0x5a3a3ee6`, as a `double` it is `0x43a23456789abcde`), so one
/// line per `Set` overload identifies the whole numeric half of the map
/// (`docs/blockers.md` B-27).
fn numeric_slot_matrix(object: *mut c_void) -> Vec<String> {
    type SetU64 = unsafe extern "C" fn(*mut c_void, *const c_char, u64);
    type Get64 = unsafe extern "C" fn(*mut c_void, *const c_char, *mut u64) -> u32;
    const PROBE: u64 = 0x1234_5678_9abc_def0;
    let Ok(key) = CString::new("NGX.Viewer.Probe.ULL") else {
        return Vec::new();
    };
    const READERS: [(usize, &str); 8] = [
        (SLOT_GET_VOID, "void"),
        (SLOT_GET_D3D12, "d3d12"),
        (SLOT_GET_D3D11, "d3d11"),
        (SLOT_GET_I32, "i32"),
        (SLOT_GET_U32, "ui"),
        (SLOT_GET_F64, "f64"),
        (SLOT_GET_F32, "f32"),
        (SLOT_GET_U64, "u64"),
    ];
    let mut lines = Vec::with_capacity(SLOT_SET_LAST + 1);
    for set_slot in 0..=SLOT_SET_LAST {
        let Some(set) = (unsafe { vtable_setter::<SetU64>(object, set_slot) }) else {
            lines.push(format!("Set {set_slot}: no vtable entry"));
            continue;
        };
        unsafe { set(object, key.as_ptr(), PROBE) };
        let mut parts = Vec::with_capacity(READERS.len());
        for (slot, label) in READERS {
            let Some(get) = (unsafe { vtable_getter::<Get64>(object, slot) }) else {
                continue;
            };
            let mut value = u64::MAX;
            let result = unsafe { get(object, key.as_ptr(), &mut value) };
            if result != NGX_SUCCESS {
                continue;
            }
            if value == PROBE {
                parts.push(format!("{label}{slot} exact"));
            } else if value != u64::MAX {
                parts.push(format!("{label}{slot} 0x{value:x}"));
            }
        }
        lines.push(format!("Set {set_slot}: {}", parts.join(", ")));
    }
    lines
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
        get_u32: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut u32) -> u32>,
        get_void: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> u32>,
    ) -> Self {
        Self {
            object,
            destroy,
            set_u32,
            set_i32,
            set_f32,
            set_d3d12,
            set_void,
            get_u32,
            get_void,
            resource_set_slot: SLOT_SET_D3D12,
        }
    }

    /// NGX's own map, written through its vtable.
    ///
    /// The vendored DLSS 4.x build exports none of `NVSDK_NGX_Parameter_Set*`,
    /// `…AllocateParameters`, `…GetCapabilityParameters` or `…GetParameters`
    /// (checked against `nvngx_dlss.dll` 310.9.16's export table), so requiring
    /// those helpers before asking Core for a map was backwards: it sent the
    /// viewer's own table to NGX even when an NGX-owned one was available.
    /// Filling the map through the object's own `Set` slots needs no exported
    /// helper at all.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn through_vtable(
        object: *mut c_void,
        destroy: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    ) -> Self {
        unsafe { Self::report_map_probes(object) };
        Self {
            object,
            destroy,
            set_u32: unsafe { vtable_setter(object, SLOT_SET_U32) },
            set_i32: unsafe { vtable_setter(object, SLOT_SET_I32) },
            set_f32: unsafe { vtable_setter(object, SLOT_SET_F32) },
            set_d3d12: unsafe { vtable_setter(object, SLOT_SET_D3D12) },
            set_void: unsafe { vtable_setter(object, SLOT_SET_VOID) },
            get_u32: unsafe { vtable_setter(object, SLOT_GET_U32) },
            get_void: unsafe { vtable_setter(object, SLOT_GET_VOID) },
            resource_set_slot: SLOT_SET_D3D12,
        }
    }

    /// Write one real resource pointer through every declared `Set` overload and
    /// read it back through four readers (see [`pointer_slot_matrix`]).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn resource_slot_matrix(
        &self,
        name: &str,
        resource: *mut c_void,
    ) -> (Option<usize>, Vec<String>) {
        pointer_slot_matrix(self.object, name, resource)
    }

    /// The numeric half of the same measurement (see [`numeric_slot_matrix`]).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn numeric_slot_matrix(&self) -> Vec<String> {
        numeric_slot_matrix(self.object)
    }

    /// The `Set` slot the map's own pointer round-trip answers, when it
    /// answers.
    ///
    /// A hint, not a verdict: it proves the object stores a pointer and which
    /// overload echoes it back, but the *feature's* reader is a separate
    /// implementation and only a real evaluate tests that one. Using it as the
    /// ladder's first candidate costs nine calls once and skips an attempt that
    /// is known to be pointless when the map agrees with the declared order.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn measured_pointer_slot(&self) -> Option<usize> {
        unsafe { Self::probe_pointer_slots(self.object, std::ptr::null_mut()) }.map(|(set, _)| set)
    }

    /// Which `Set` slot resource pointers go through.
    pub(crate) fn resource_set_slot(&self) -> usize {
        self.resource_set_slot
    }

    /// Record the slot the feature accepted, so the ladder is paid once.
    pub(crate) fn set_resource_set_slot(&mut self, slot: usize) {
        self.resource_set_slot = slot;
    }

    /// Find the `Set`/`Get` pair a resource pointer round-trips through.
    ///
    /// `nvsdk_ngx_params.h` declares eight `Set` overloads — `unsigned long
    /// long`, `float`, `double`, `unsigned int`, `int`, `ID3D11Resource *`,
    /// `ID3D12Resource *`, `void *` — and eight `Get` overloads in the mirror
    /// order, and this file used to address the resource ones by that order. A
    /// real session says the core does not agree with its own header:
    ///
    /// ```text
    /// [NGXDLAA::EvaluateFeature:788] error: could not find Color parameter
    /// NVSDK_NGX_D3D12_EvaluateFeature(DLSS) failed (0xbad00005 FAIL_InvalidParameter)
    /// ```
    ///
    /// — `FAIL_InvalidParameter` on every evaluate, on a map whose integer
    /// entries work (`SuperSampling.Available` reads back, `CreateFeature`
    /// accepts `Width`/`Height`/`PerfQualityValue`). A pointer written through
    /// the wrong one of the three pointer overloads lands under a *different
    /// type tag*, and the feature's `Get(…, ID3D12Resource **)` then reports the
    /// key as missing, which is exactly that message.
    ///
    /// So the pair is measured rather than assumed: a pointer is written through
    /// each candidate `Set` slot and read back through each candidate `Get`
    /// slot, and the first pair that returns the same value is the pair the rest
    /// of the session uses. Nine calls, once. With a null probe only a pair whose
    /// `Get` answers "stored, and it is null" can match, so the measurement also
    /// works before any real resource exists.
    ///
    /// # Safety
    /// `object` must be a live `NVSDK_NGX_Parameter` and `probe` a pointer the
    /// owning feature will accept as the value it was given.
    unsafe fn probe_pointer_slots(
        object: *mut c_void,
        probe: *mut c_void,
    ) -> Option<(usize, usize)> {
        type SetPtr = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void);
        type GetPtr = unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> u32;
        // A name NGX knows, so a rejection cannot be blamed on the key.
        let name = CString::new("Color").ok()?;
        for set_slot in POINTER_SET_SLOTS {
            let Some(set) = vtable_setter::<SetPtr>(object, set_slot) else {
                continue;
            };
            set(object, name.as_ptr(), probe);
            for get_slot in POINTER_GET_SLOTS {
                let Some(get) = vtable_getter::<GetPtr>(object, get_slot) else {
                    continue;
                };
                let mut value: *mut c_void = std::ptr::null_mut();
                if get(object, name.as_ptr(), &mut value) == NGX_SUCCESS && value == probe {
                    return Some((set_slot, get_slot));
                }
            }
        }
        None
    }

    /// Log what the map can and cannot do, once per map.
    ///
    /// Two lines, both cheap, both answered by this session rather than by the
    /// header: whether an integer write round-trips at all, and which pointer
    /// slots the probe found. A wrong-slot pointer write is silent — this is the
    /// only place it can be seen.
    ///
    /// # Safety
    /// `object` must be a live `NVSDK_NGX_Parameter`.
    unsafe fn report_map_probes(object: *mut c_void) {
        let name = "NGX.Viewer.Probe.U32";
        let numeric = match (
            vtable_setter::<unsafe extern "C" fn(*mut c_void, *const c_char, u32)>(
                object,
                SLOT_SET_U32,
            ),
            vtable_getter::<unsafe extern "C" fn(*mut c_void, *const c_char, *mut u32) -> u32>(
                object,
                SLOT_GET_U32,
            ),
        ) {
            (Some(set), Some(get)) => {
                let key = CString::new(name).unwrap_or_default();
                set(object, key.as_ptr(), 0x5a5a);
                let mut value = 0u32;
                get(object, key.as_ptr(), &mut value) == NGX_SUCCESS && value == 0x5a5a
            }
            _ => false,
        };
        super::diag(&format!(
            "[NGX] parameter map: integer write round trip {}",
            if numeric { "works" } else { "does not work" }
        ));
        match unsafe { Self::probe_pointer_slots(object, std::ptr::null_mut()) } {
            Some((set, get)) => super::diag(&format!(
                "[NGX] parameter map: a null pointer round-trips Set slot {set} / Get slot {get} \
                 (a hint only -- the slot resources use is measured with real evaluates)"
            )),
            None => super::diag(
                "[NGX] parameter map: no pointer slot pair matched a null probe; the resource slot \
                 will be measured with real evaluates",
            ),
        }
    }

    fn c_name(name: &str) -> Option<CString> {
        CString::new(name).ok()
    }

    /// Read a pointer key back through every candidate `Get` slot.
    ///
    /// The diagnostic half of the `Set` ladder: it says whether NGX's map still
    /// holds the pointer that was just written, and through which reader.
    pub(crate) fn describe_pointer_gets(&self, name: &str) -> String {
        pointer_get_report(self.object, name)
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

    /// Write an `ID3D12Resource *` (or `nullptr` to clear one).
    ///
    /// The slot is the measured one, not the declared one: a pointer written
    /// through the wrong pointer overload is silently stored under a different
    /// type tag, and the feature's own `Get` then reports the key as missing
    /// (`NGXDLAA::EvaluateFeature:788 error: could not find Color parameter`).
    /// The first real write probes, with this resource as the probe value — it
    /// is the first moment a real pointer exists.
    fn set_d3d12_resource(&mut self, name: &str, value: *mut c_void) {
        let slot = self.resource_set_slot;
        let set = self.set_for_slot(slot);
        Self::set_pointer(set, self.object, name, value);
    }

    /// Write a resource through a specific slot, for the ladder in
    /// `DlssUpscaler::record` that finds the one the feature accepts.
    fn set_d3d12_resource_with_slot(&mut self, slot: usize, name: &str, value: *mut c_void) {
        let set = self.set_for_slot(slot);
        Self::set_pointer(set, self.object, name, value);
    }

    /// The `Set` entry point at `slot`, reusing the one resolved at construction
    /// when the measurement agreed with it.
    fn set_for_slot(
        &self,
        slot: usize,
    ) -> Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void)> {
        match slot {
            SLOT_SET_D3D12 => self.set_d3d12,
            SLOT_SET_VOID => self.set_void,
            other => unsafe { vtable_setter(self.object, other) },
        }
    }

    fn set_void_pointer(&mut self, name: &str, value: *mut c_void) {
        // `SetVoidPointer` is the same three-way choice as a resource, so it uses
        // the slot the feature accepted for resources.
        let slot = self.resource_set_slot;
        let set = self.set_for_slot(slot);
        Self::set_pointer(set, self.object, name, value);
    }

    fn get_u32(&mut self, name: &str) -> Option<u32> {
        let get = self.get_u32?;
        let name = Self::c_name(name)?;
        let mut value = 0u32;
        let result = unsafe { get(self.object, name.as_ptr(), &mut value) };
        (result == NGX_SUCCESS).then_some(value)
    }

    fn get_void_pointer(&mut self, name: &str) -> Option<*mut c_void> {
        // Read through the measured `Get` slot when the probe has settled one; a
        // pointer stored under the wrong tag reads back as a missing key.
        let get = self.get_void?;
        let name = Self::c_name(name)?;
        let mut value: *mut c_void = std::ptr::null_mut();
        let result = unsafe { get(self.object, name.as_ptr(), &mut value) };
        (result == NGX_SUCCESS && !value.is_null()).then_some(value)
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

    /// `NVSDK_NGX_Parameter_GetUI`: read an integer back out of the map.
    ///
    /// NGX writes its own verdicts into the map it is given
    /// (`SuperSampling.Available` from the capability map,
    /// `SuperSampling.FeatureInitResult` from `CreateFeature`), so this is how
    /// the viewer asks "did the feature really initialise?" instead of assuming
    /// success from a `Success` HRESULT.
    pub(crate) fn get_u32(&mut self, name: &str) -> Option<u32> {
        match &mut self.kind {
            Kind::Owned(table) => table.get_u32(name),
            Kind::Ngx(map) => map.get_u32(name),
        }
    }

    /// `NVSDK_NGX_Parameter_GetVoidPointer`: how NGX hands out the callbacks a
    /// capability map carries — `DLSSOptimalSettingsCallback` above all, which
    /// is the only way to ask what render size DLSS is willing to evaluate.
    pub(crate) fn get_void_pointer(&mut self, name: &str) -> Option<*mut c_void> {
        match &mut self.kind {
            Kind::Owned(table) => table.get_void_pointer(name),
            Kind::Ngx(map) => map.get_void_pointer(name),
        }
    }

    /// Which `Set` slot resource pointers are written through.
    pub(crate) fn resource_set_slot(&self) -> usize {
        match &self.kind {
            Kind::Owned(_) => SLOT_SET_D3D12,
            Kind::Ngx(map) => map.resource_set_slot(),
        }
    }

    /// Record the slot the feature accepted.
    ///
    /// The viewer's own table stores every pointer overload in one variant, so
    /// any slot works there and the choice is a no-op.
    pub(crate) fn set_resource_set_slot(&mut self, slot: usize) {
        if let Kind::Ngx(map) = &mut self.kind {
            map.set_resource_set_slot(slot);
        }
    }

    /// Write one real resource pointer through every declared `Set` overload and
    /// read it back through the declared readers, reporting the matrix.
    ///
    /// The viewer's own table stores every pointer the same way, so there is
    /// nothing to measure and the declared slot is the answer.
    pub(crate) fn resource_slot_matrix(
        &self,
        name: &str,
        resource: *mut c_void,
    ) -> (Option<usize>, Vec<String>) {
        match &self.kind {
            Kind::Owned(table) => table.resource_slot_matrix(name, resource),
            Kind::Ngx(map) => map.resource_slot_matrix(name, resource),
        }
    }

    /// The numeric half of the same measurement (see [`numeric_slot_matrix`]).
    pub(crate) fn numeric_slot_matrix(&self) -> Vec<String> {
        match &self.kind {
            Kind::Owned(table) => table.numeric_slot_matrix(),
            Kind::Ngx(map) => map.numeric_slot_matrix(),
        }
    }

    /// Read a pointer key back through every candidate `Get` slot.
    ///
    /// The viewer's own table has a single pointer variant, so there it always
    /// reads back through the one slot it has; NGX's map is asked through all
    /// three, which is what makes a rejected evaluate diagnosable.
    pub(crate) fn describe_pointer_gets(&self, name: &str) -> String {
        match &self.kind {
            Kind::Owned(table) => table.describe_pointer_gets(name),
            Kind::Ngx(map) => map.describe_pointer_gets(name),
        }
    }

    /// Write a resource through a specific `Set` slot, for the ladder.
    pub(crate) fn set_d3d12_resource_with_slot(
        &mut self,
        slot: usize,
        name: &str,
        resource: *mut c_void,
    ) {
        match &mut self.kind {
            Kind::Owned(table) => table.set_d3d12_resource(name, resource),
            Kind::Ngx(map) => map.set_d3d12_resource_with_slot(slot, name, resource),
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
    fn ngx_maps_are_written_through_their_own_vtable() {
        // `Owned` is itself a complete `NVSDK_NGX_Parameter`, so pointing
        // `NgxMap` at one exercises exactly the path an NGX-owned map takes:
        // no exported `NVSDK_NGX_Parameter_Set*` helper in sight.
        let mut own = Owned::new();
        assert!(is_writable(own.as_ptr()));
        {
            let mut map = NgxMap::through_vtable(own.as_ptr(), None);
            map.set_u32("Width", 3440);
            map.set_i32("Reset", 1);
            map.set_f32("Sharpness", 0.35);
            map.set_d3d12_resource("Color", std::ptr::null_mut());
        }
        assert!(matches!(own.values.get("Width"), Some(Value::U32(3440))));
        assert!(matches!(own.values.get("Reset"), Some(Value::I32(1))));
        assert!(matches!(own.values.get("Sharpness"), Some(Value::F32(_))));
        assert!(matches!(own.values.get("Color"), Some(Value::Ptr(_))));
        // A null object has no vtable, so it is never handed to NGX.
        assert!(!is_writable(std::ptr::null_mut()));
    }

    /// The evaluate ladder rewrites the four resources through a *different*
    /// `Set` slot when the feature rejects the first one. The viewer's own table
    /// keeps one variant for all three pointer overloads, so every candidate must
    /// land in the same place — the ladder is a no-op there, never a lost write.
    #[test]
    fn a_resource_lands_whichever_pointer_slot_the_ladder_tries() {
        let mut params = NgxParams::new();
        let own = params.as_ptr() as *mut Owned;
        assert_eq!(params.resource_set_slot(), SLOT_SET_D3D12);
        for slot in RESOURCE_SET_SLOT_CANDIDATES {
            params.set_d3d12_resource_with_slot(slot, "Color", 0x1234 as *mut c_void);
            let stored = unsafe { (*own).values.get("Color").copied() };
            assert!(
                matches!(stored, Some(Value::Ptr(ptr)) if ptr as usize == 0x1234),
                "slot {slot} must store the pointer"
            );
        }
        // And the recorded slot is a no-op for a table the viewer owns.
        params.set_resource_set_slot(SLOT_SET_D3D11);
        params.set_d3d12_resource("Output", 0x5678 as *mut c_void);
        let stored = unsafe { (*own).values.get("Output").copied() };
        assert!(matches!(stored, Some(Value::Ptr(ptr)) if ptr as usize == 0x5678));
    }

    /// The slot table is the *measured* one, not the one the public header
    /// prints, and this pins it: swapping any two of these puts a `Set` where a
    /// reader looks for it and the evaluate fails with a message about a key the
    /// viewer never stopped writing (`docs/blockers.md` B-27).
    #[test]
    fn the_slot_table_is_the_one_this_core_implements() {
        assert_eq!(
            [
                SLOT_SET_VOID,
                SLOT_SET_D3D12,
                SLOT_SET_D3D11,
                SLOT_SET_I32,
                SLOT_SET_U32,
                SLOT_SET_F64,
                SLOT_SET_F32,
                SLOT_SET_U64,
            ],
            [0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(
            [
                SLOT_GET_VOID,
                SLOT_GET_D3D12,
                SLOT_GET_D3D11,
                SLOT_GET_I32,
                SLOT_GET_U32,
                SLOT_GET_F64,
                SLOT_GET_F32,
                SLOT_GET_U64,
            ],
            [8, 9, 10, 11, 12, 13, 14, 15]
        );
        // The pairs the two halves of the API have to agree on.
        assert_eq!(RESOURCE_SET_SLOT_CANDIDATES[0], SLOT_SET_D3D12);
        assert_eq!(POINTER_GET_SLOTS[0], SLOT_GET_D3D12);
        assert_eq!(POINTER_GET_SLOTS.len(), POINTER_SET_SLOTS.len());
    }

    /// The ladder starts with the slot the map's own pointer round-trip
    /// reported, then the rest of the declared candidates, each exactly once.
    ///
    /// Order is the whole experiment: a ladder that started anywhere else could
    /// spend three rejected evaluates before reaching the one slot the map
    /// already answered for, and a duplicated entry would evaluate the same
    /// wrong write twice and read as "two slots failed" (`B-27`).
    #[test]
    fn the_slot_ladder_tries_the_measured_slot_first() {
        for measured in RESOURCE_SET_SLOT_CANDIDATES {
            let ladder = resource_slot_ladder(measured);
            assert_eq!(ladder.first().copied(), Some(measured));
            assert_eq!(ladder.len(), RESOURCE_SET_SLOT_CANDIDATES.len());
            for candidate in RESOURCE_SET_SLOT_CANDIDATES {
                assert_eq!(
                    ladder.iter().filter(|slot| **slot == candidate).count(),
                    1,
                    "slot {candidate} must appear exactly once"
                );
            }
        }
    }

    /// A pointer key the table holds reads back as an address through every
    /// candidate `Get` slot; a key it does not hold reads back as no address at
    /// all.
    ///
    /// That is the distinction the evaluate diagnostics lean on: `0x…` means
    /// "the map still has what was just written, through a reader the feature
    /// may not be using", while `untouched` or `fail` means the key is not
    /// there — the out-parameter is pre-filled with a sentinel so the two can
    /// be told apart, and neither is mistaken for a stored pointer.
    #[test]
    fn a_pointer_key_reads_back_and_a_missing_one_reads_as_untouched() {
        let mut params = NgxParams::new();
        params.set_d3d12_resource("Color", 0x1234 as *mut c_void);
        let stored = params.describe_pointer_gets("Color");
        assert_eq!(
            stored.matches("0x1234").count(),
            POINTER_GET_SLOTS.len(),
            "every getter of a table the viewer owns must see the pointer: {stored}"
        );
        let missing = params.describe_pointer_gets("Transparency");
        assert!(
            !missing.contains("0x1234"),
            "a key that was never written must not read back as the stored pointer: {missing}"
        );
        assert!(
            missing.contains("untouched") || missing.contains("fail"),
            "a missing key must be reported as untouched or failed, not as an address: {missing}"
        );
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
            // No `NVSDK_NGX_Parameter_GetUI` either: a read then answers `None`
            // instead of touching the object.
            None,
            // Nor `NVSDK_NGX_Parameter_GetVoidPointer`.
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
            assert_eq!(params.get_u32("SuperSampling.Available"), None);
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

#[cfg(test)]
mod resource_matrix_tests {
    use super::*;

    /// The matrix exists to separate "the map lost the pointer" from "the map
    /// holds it under a tag the feature does not read", so it has to report a
    /// kept pointer as `keeps it` and a converted one as the converted number —
    /// the two shapes session-7 showed for the same key.
    #[test]
    fn the_matrix_reports_a_kept_pointer_through_every_reader_of_a_table_it_owns() {
        let params = NgxParams::new();
        let (found, lines) = params.resource_slot_matrix("Color", 0x1234 as *mut c_void);
        assert_eq!(
            found,
            Some(SLOT_SET_D3D12),
            "a table the viewer owns keeps a pointer through the declared slot"
        );
        assert_eq!(lines.len(), SLOT_SET_LAST + 1, "one line per declared Set");
        let declared = &lines[SLOT_SET_D3D12];
        assert!(
            declared.contains("d3d12 keeps it"),
            "the declared slot must report the pointer as kept: {declared}"
        );
        assert!(
            declared.contains("void keeps it"),
            "every reader of the viewer's table sees the same pointer: {declared}"
        );
    }
}
