//! Hand-declared Direct3D 12 ray-tracing interfaces.
//!
//! ## Why this file exists
//!
//! wgpu 0.19 cannot build an acceleration structure: `BindingType::
//! AccelerationStructure` is `todo!()`, its DX12 backend has no
//! `BuildRaytracingAccelerationStructure`/`DispatchRays` wiring, and
//! `Features::RAY_QUERY` is a bit with nothing behind it. The alternatives were
//! worse:
//!
//! * `winapi` 0.3.9 stops at `ID3D12Device1` and declares no ray-tracing type at
//!   all;
//! * `gfx-rs/d3d12-rs` (the only crate wrapping raw D3D12) has no `ID3D12Device5`,
//!   no state-object support and no command-list ray-tracing members even on
//!   master, and its tags stop at `v0.7.0` — there is no release matched to
//!   wgpu-hal 0.19.
//!
//! So the members below are declared here, the same way `winapi`'s own `RIDL!`
//! expansion declares them: one function pointer per vtable slot. `winapi`'s
//! `d3d12` module is still used for everything that exists there (devices,
//! command lists, resources, fences); only the ray-tracing *additions* are
//! declared here, which keeps the ABI risk to the new slots.
//!
//! ## The slots
//!
//! Every number below is derived from the public `d3d12.h` interface chain and
//! is asserted at compile time by the `const _: () = assert!(..)` blocks, so an
//! accidental edit to a padding array is a build error instead of a wrong-slot
//! call:
//!
//! ```text
//! IUnknown              3 slots   (QueryInterface, AddRef, Release)
//! ID3D12Object         +4  ->   7
//! ID3D12DeviceChild    +1  ->   8
//! ID3D12Pageable       +0  ->   8
//! ID3D12Device        +40  ->  48
//! ID3D12Device1        +3  ->  51
//! ID3D12Device2        +1  ->  52
//! ID3D12Device3        +3  ->  55
//! ID3D12Device4        +7  ->  62
//! ID3D12Device5  CreateLifetimeTracker 62 .. CreateStateObject 67,
//!                GetRaytracingAccelerationStructurePrebuildInfo 68
//!
//! ID3D12CommandList    +1  ->   9  (CommandList, from DeviceChild at 8)
//! ID3D12GraphicsCommandList   +51 -> 60
//! ID3D12GraphicsCommandList1  +6  -> 66
//! ID3D12GraphicsCommandList2  +1  -> 67
//! ID3D12GraphicsCommandList3  +1  -> 68
//! ID3D12GraphicsCommandList4  BeginRenderPass 68 .. BuildRaytracing…
//!                             72, Emit… 73, Copy… 74, SetPipelineState1 75,
//!                             DispatchRays 76
//! ```
//!
//! ## Why inline ray query, not a ray-tracing pipeline
//!
//! The module's hardware path uses **DXR 1.1 inline ray query** inside a compute
//! shader rather than a state object with raygen/miss/hit shader tables:
//!
//! * traversal still happens in the fixed-function ray-tracing units over a
//!   hardware-built BVH — it is real hardware ray tracing, not a software walk;
//! * it needs no state object, no shader tables, no `DispatchRays` and no
//!   local-root-signature rules, which removes the largest and least testable
//!   part of the FFI surface;
//! * it runs on the same command list and the same root signature style the rest
//!   of the module already uses, so the buffer/descriptor plumbing is shared with
//!   the compute tracer.
//!
//! The one requirement it adds is **DXIL**: an inline `RayQuery` must be
//! compiled for shader model 6.5 by DXC, which is not a Rust dependency.
//! `tools/imaginator-shaders.sh` produces it; when the blob is absent the module
//! says so on its page and keeps the compute tracer.
#![allow(non_upper_case_globals, non_camel_case_types, dead_code)]

use std::ffi::c_void;
use winapi::shared::guiddef::GUID;
use winapi::shared::minwindef::UINT;
use winapi::shared::winerror::HRESULT;
use winapi::um::unknwnbase::IUnknown;

pub type GpuAddress = u64;

/// A GPU virtual address plus the stride of the records it points at.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
    pub StartAddress: GpuAddress,
    pub StrideInBytes: u64,
}

// Slot indices, counted from the C++ vtable declarations in `d3d12.h` (the
// `ID3D12Xxx : public ID3D12Yyy { virtual ... }` blocks, where each block lists
// only its *own* members). The chain that matters:
//
//   IUnknown 3 | Object 4 | DeviceChild 1 | Device 40 | Device1 3 | Device2 1
//   Device3 3 | Device4 6 | Device5 8            => 69 slots, 0..=68
//
// `CreateStateObject` is 67 and `GetRaytracingAccelerationStructurePrebuildInfo`
// is 68 — the last member of the interface, which is why it is also the last
// slot.
pub const IUNKNOWN_SLOTS: usize = 3;
/// Slots of `ID3D12Device5` as `d3d12.h` declares it (0..=68).
pub const DEVICE5_SLOTS: usize = 69;
/// Slots of `ID3D12GraphicsCommandList4` (0..=76).
pub const GCL4_SLOTS: usize = 77;

pub const DEVICE5_CREATE_STATE_OBJECT_SLOT: usize = 67;
pub const DEVICE5_PREBUILD_INFO_SLOT: usize = 68;
pub const GCL4_BUILD_AS_SLOT: usize = 72;

pub const IID_ID3D12DEVICE5: &str = "8b4f173b-2fea-4b80-8f58-4307191ab95d";
pub const IID_ID3D12GRAPHICSCOMMANDLIST4: &str = "8754318e-d3a9-4541-98cf-645b50dc4874";

// ---------------------------------------------------------------------------
// Plain-data descriptors (public d3d12.h values)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
    pub Transform3x4: GpuAddress,
    pub IndexFormat: u32,
    pub VertexFormat: u32,
    pub IndexCount: u32,
    pub VertexCount: u32,
    pub IndexBuffer: GpuAddress,
    pub VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE,
}

/// The union arm used by the module: triangle geometry. `D3D12_RAYTRACING_
/// GEOMETRY_DESC` is `{ Type, Flags, union { Triangles, AABBs } }`; both arms
/// start with a GPU address and the triangle arm is the larger one, so declaring
/// only the triangle arm keeps the size and the offsets of both correct.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_RAYTRACING_GEOMETRY_DESC {
    pub Type: u32,
    pub Flags: u32,
    pub Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_DESCS {
    pub InstanceDescs: GpuAddress,
    pub pGeometryDescs: *const D3D12_RAYTRACING_GEOMETRY_DESC,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
    pub Type: u32,
    pub Flags: u32,
    pub NumDescs: UINT,
    pub DescsLayout: u32,
    pub Descs: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_DESCS,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
    pub DestAccelerationStructureData: GpuAddress,
    pub Inputs: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS,
    pub SourceAccelerationStructureData: GpuAddress,
    pub ScratchAccelerationStructureData: GpuAddress,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO {
    pub ResultDataMaxSizeInBytes: u64,
    pub ScratchDataSizeInBytes: u64,
    pub UpdateScratchDataSizeInBytes: u64,
}

// 16-byte aligned, so the 4 bytes between `Flags` and the AS address are tail
// padding the struct already inserts. `Default` fills everything but that gap,
// which is why the instance buffer is zeroed before the rows are written.
#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
pub struct D3D12_RAYTRACING_INSTANCE_DESC {
    pub Transform: [f32; 12],
    pub InstanceID: u32,
    pub InstanceContributionToHitGroupIndex: u32,
    pub Flags: u32,
    pub AccelerationStructure: GpuAddress,
}

impl D3D12_RAYTRACING_INSTANCE_DESC {
    /// Identity transform, opaque, no instance contribution.
    pub fn identity(acceleration_structure: GpuAddress) -> Self {
        Self {
            Transform: [
                1., 0., 0., 0., //
                0., 1., 0., 0., //
                0., 0., 1., 0.,
            ],
            InstanceID: 0,
            InstanceContributionToHitGroupIndex: 0,
            Flags: INSTANCE_FLAG_FORCE_OPAQUE,
            AccelerationStructure: acceleration_structure,
        }
    }
}

/// `D3D12_FEATURE_D3D12_OPTIONS5`, the only feature query `winapi` 0.3.9 does
/// not declare. Its `RaytracingTier` is what the probe reports before anything
/// is built.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct D3D12_FEATURE_DATA_D3D12_OPTIONS5 {
    pub SRVOnlyTiledResourceTier3: i32,
    pub RenderPassesTier: u32,
    pub RaytracingTier: u32,
}

// Values from d3d12.h.
pub const FEATURE_D3D12_OPTIONS5: u32 = 27;
pub const RAYTRACING_TIER_NOT_SUPPORTED: u32 = 0;
pub const RAYTRACING_TIER_1_0: u32 = 10;
pub const RAYTRACING_TIER_1_1: u32 = 11;
pub const RAYTRACING_TIER_1_2: u32 = 12;
pub const TYPE_BOTTOM_LEVEL: u32 = 0;
pub const TYPE_TOP_LEVEL: u32 = 1;
pub const BUILD_FLAG_NONE: u32 = 0x00;
pub const BUILD_FLAG_ALLOW_UPDATE: u32 = 0x01;
pub const BUILD_FLAG_ALLOW_COMPACTION: u32 = 0x02;
pub const BUILD_FLAG_PREFER_FAST_TRACE: u32 = 0x04;
pub const BUILD_FLAG_PREFER_FAST_BUILD: u32 = 0x08;
pub const ELEMENTS_LAYOUT_ARRAY: u32 = 0;
pub const GEOMETRY_TYPE_TRIANGLES: u32 = 0;
pub const GEOMETRY_FLAG_OPAQUE: u32 = 0x01;
pub const INSTANCE_FLAG_NONE: u32 = 0;
pub const INSTANCE_FLAG_TRIANGLE_CULL_DISABLE: u32 = 0x02;
pub const INSTANCE_FLAG_FORCE_OPAQUE: u32 = 0x01;
pub const DXGI_FORMAT_R32G32B32_FLOAT: u32 = 6;
pub const DXGI_FORMAT_R32_UINT: u32 = 42;

// ---------------------------------------------------------------------------
// Vtables
// ---------------------------------------------------------------------------

/// `ID3D12Device5`, addressing only `GetRaytracingAccelerationStructurePrebuildInfo`.
///
/// The three typed `IUnknown` entries are followed by padding for slots 3..=67 so
/// the one declaration that matters lands on slot 68, which the assertions below
/// pin against the header's own member counts.
#[repr(C)]
pub struct ID3D12Device5Vtbl {
    pub QueryInterface:
        unsafe extern "system" fn(*mut IUnknown, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(*mut IUnknown) -> u32,
    pub Release: unsafe extern "system" fn(*mut IUnknown) -> u32,
    /// Slots 3..=66: `ID3D12Object` through `EnumerateMetaCommandParameters`.
    pub prefix: [usize; DEVICE5_CREATE_STATE_OBJECT_SLOT - IUNKNOWN_SLOTS],
    /// Slot 67: build a ray-tracing state object from a DXIL library.
    pub CreateStateObject: unsafe extern "system" fn(
        *mut c_void,
        *const D3D12_STATE_OBJECT_DESC,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub GetRaytracingAccelerationStructurePrebuildInfo: unsafe extern "system" fn(
        *mut c_void,
        *const D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS,
        *mut D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO,
    ),
}

#[repr(C)]
pub struct ID3D12Device5 {
    pub lpVtbl: *mut ID3D12Device5Vtbl,
}

/// `ID3D12GraphicsCommandList4`, addressing only the acceleration-structure
/// build. Slots 0..2 are `IUnknown`; 3..=71 are padding; the build call is slot
/// 72 — the last slot this module needs, so the declared prefix stops there
/// rather than padding out through `DispatchRays` at 76. A struct shorter than
/// the real vtable is harmless: only the address of each slot matters.
#[repr(C)]
pub struct ID3D12GraphicsCommandList4Vtbl {
    pub QueryInterface:
        unsafe extern "system" fn(*mut IUnknown, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(*mut IUnknown) -> u32,
    pub Release: unsafe extern "system" fn(*mut IUnknown) -> u32,
    /// Slots 3..=71: `ID3D12Object` through `ExecuteMetaCommand`, i.e.
    /// everything before the build call.
    pub prefix: [usize; GCL4_BUILD_AS_SLOT - IUNKNOWN_SLOTS],
    pub BuildRaytracingAccelerationStructure: unsafe extern "system" fn(
        *mut c_void,
        *const D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC,
        UINT,
        *const c_void,
    ),
    pub BuildRaytracingAccelerationStructurePostbuildInfo: *const c_void,
    pub CopyRaytracingAccelerationStructure: *const c_void,
    /// Slot 75: binds the ray-tracing pipeline state object.
    pub SetPipelineState1: unsafe extern "system" fn(*mut c_void, *mut c_void),
    /// Slot 76: launches the shader that was bound with `SetPipelineState1`.
    pub DispatchRays: unsafe extern "system" fn(*mut c_void, *const D3D12_DISPATCH_RAYS_DESC),
}

#[repr(C)]
pub struct ID3D12GraphicsCommandList4 {
    pub lpVtbl: *mut ID3D12GraphicsCommandList4Vtbl,
}

/// `D3D12_DISPATCH_RAYS_DESC`: the three shader tables and the launch size.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_DISPATCH_RAYS_DESC {
    pub RayGenerationShaderRecord: D3D12_GPU_VIRTUAL_ADDRESS_RANGE,
    pub MissShaderTable: D3D12_SHADER_TABLE,
    pub HitGroupTable: D3D12_SHADER_TABLE,
    pub CallableShaderTable: D3D12_SHADER_TABLE,
    pub Width: UINT,
    pub Height: UINT,
    pub Depth: UINT,
}

/// `D3D12_GPU_VIRTUAL_ADDRESS_RANGE` (a start and a length, no stride).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_GPU_VIRTUAL_ADDRESS_RANGE {
    pub StartAddress: GpuAddress,
    pub SizeInBytes: u64,
}

/// `D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE` as a *shader table*: unlike the
/// vertex-buffer form above it carries a size as well, because the table's
/// records are read by the traversal as a contiguous range.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_SHADER_TABLE {
    pub StartAddress: GpuAddress,
    pub SizeInBytes: u64,
    pub StrideInBytes: u64,
}

// ---------------------------------------------------------------------------
// Ray-tracing pipeline objects (D3D12 state objects, DXIL subobjects)
//
// These mirror `d3d12.h`. The state object is how DX12 spells a ray-tracing
// pipeline: one description carrying the DXIL library, the hit groups and the
// global root signature, turned into a single object that `SetPipelineState1`
// binds and `DispatchRays` launches.
// ---------------------------------------------------------------------------

/// A shader identifier is 32 bytes: `D3D12_SHADER_IDENTIFIER_SIZE_IN_BYTES`. A
/// shader table record starts with one, then the record's own data.
pub const SHADER_IDENTIFIER_SIZE: usize = 32;

/// `D3D12_STATE_OBJECT_TYPE_RAYTRACING_PIPELINE`.
pub const D3D12_STATE_OBJECT_TYPE_RAYTRACING_PIPELINE: u32 = 3;

/// `D3D12_STATE_SUBOBJECT_TYPE_*`, taken from the enum in `d3d12.h` rather than
/// from memory: the numbering is not the declaration order and guessing it
/// produces a state object the runtime rejects.
pub const D3D12_STATE_SUBOBJECT_TYPE_STATE_OBJECT_CONFIG: u32 = 0;
pub const D3D12_STATE_SUBOBJECT_TYPE_GLOBAL_ROOT_SIGNATURE: u32 = 1;
pub const D3D12_STATE_SUBOBJECT_TYPE_DXIL_LIBRARY: u32 = 5;
pub const D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_SHADER_CONFIG: u32 = 9;
pub const D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_PIPELINE_CONFIG: u32 = 10;
pub const D3D12_STATE_SUBOBJECT_TYPE_HIT_GROUP: u32 = 11;

/// `D3D12_SHADER_VISIBILITY_ALL`, the visibility a ray-tracing export ignores.
pub const D3D12_SHADER_VISIBILITY_ALL: u32 = 0;

#[repr(C)]
pub struct D3D12_STATE_OBJECT_CONFIG {
    pub Flags: u32,
}

/// `D3D12_DXIL_LIBRARY_DESC`: the compiled blob plus optional per-export renames.
#[repr(C)]
pub struct D3D12_DXIL_LIBRARY_DESC {
    pub DXILLibrary: D3D12_SHADER_BYTECODE,
    pub NumExports: u32,
    pub pExports: *const D3D12_EXPORT_DESC,
}

#[repr(C)]
pub struct D3D12_SHADER_BYTECODE {
    pub pShaderBytecode: *const c_void,
    pub BytecodeLength: usize,
}

#[repr(C)]
pub struct D3D12_EXPORT_DESC {
    pub Name: *const u16,
    pub ExportToRename: *const u16,
    pub Flags: u32,
}

/// `D3D12_HIT_GROUP_DESC`: which intersection, any-hit and closest-hit shaders
/// make up one hit group, and the name a shader table record addresses.
#[repr(C)]
pub struct D3D12_HIT_GROUP_DESC {
    pub HitGroupExport: *const u16,
    pub Type: u32,
    pub AnyHitShaderImport: *const u16,
    pub ClosestHitShaderImport: *const u16,
    pub IntersectionShaderImport: *const u16,
}

/// `D3D12_RAYTRACING_SHADER_CONFIG`: the payload and attribute sizes the
/// traversal must reserve. The payload is 8 floats — the four traced channels
/// the shader writes — and the attribute is a triangle's barycentrics.
#[repr(C)]
pub struct D3D12_RAYTRACING_SHADER_CONFIG {
    pub MaxPayloadSizeInBytes: u32,
    pub MaxAttributeSizeInBytes: u32,
}

/// `D3D12_RAYTRACING_PIPELINE_CONFIG`: the recursion depth. This tracer is
/// iterative, so one level of inline `RayQuery` per ray, never nested.
#[repr(C)]
pub struct D3D12_RAYTRACING_PIPELINE_CONFIG {
    pub MaxTraceRecursionDepth: u32,
}

#[repr(C)]
pub struct D3D12_STATE_SUBOBJECT {
    pub Type: u32,
    pub pDesc: *const c_void,
}

/// `D3D12_STATE_OBJECT_DESC`. `Subobjects` and `pSubobjects` are *aliased* in
/// the C struct as `{ UINT NumSubobjects; const D3D12_STATE_SUBOBJECT
/// *pSubobjects; }`, spelled here as a pointer to a small struct so the layout
/// stays honest on 64-bit.
#[repr(C)]
pub struct D3D12_STATE_OBJECT_DESC {
    pub Type: u32,
    pub NumSubobjects: u32,
    pub pSubobjects: *const D3D12_STATE_SUBOBJECT,
}

#[repr(C)]
pub struct D3D12_ROOT_SIGNATURE_DESC {
    pub NumParameters: u32,
    pub pParameters: *const c_void,
    pub NumStaticSamplers: u32,
    pub pStaticSamplers: *const c_void,
    pub Flags: u32,
}

/// `ID3D12RootSignature` is an `ID3D12DeviceChild`; only the `IUnknown` head is
/// needed to keep and release one.
#[repr(C)]
pub struct ID3D12RootSignature {
    pub lpVtbl: *const c_void,
}

/// `ID3D12StateObject` likewise.
#[repr(C)]
pub struct ID3D12StateObject {
    pub lpVtbl: *const c_void,
}

/// `ID3D12StateObjectProperties`, whose one interesting call hands back the
/// 32-byte shader identifier a shader table record starts with.
#[repr(C)]
pub struct ID3D12StateObjectPropertiesVtbl {
    pub QueryInterface:
        unsafe extern "system" fn(*mut IUnknown, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(*mut IUnknown) -> u32,
    pub Release: unsafe extern "system" fn(*mut IUnknown) -> u32,
    pub GetShaderIdentifier: unsafe extern "system" fn(*mut c_void, *const u16) -> *const c_void,
    pub GetShaderStackSize: unsafe extern "system" fn(*mut c_void, *const u16) -> u64,
    pub GetPipelineStackSize: unsafe extern "system" fn(*mut c_void) -> u64,
    pub SetPipelineStackSize: unsafe extern "system" fn(*mut c_void, u64),
}

#[repr(C)]
pub struct ID3D12StateObjectProperties {
    pub lpVtbl: *mut ID3D12StateObjectPropertiesVtbl,
}

/// `__uuidof(ID3D12StateObjectProperties)`, from the `MIDL_INTERFACE` in
/// `d3d12.h`.
pub const IID_ID3D12STATEOBJECTPROPERTIES: &str = "de5fa827-9bf9-4f26-89ff-d7f56fde3860";

// ---------------------------------------------------------------------------
// Compile-time slot assertions
// ---------------------------------------------------------------------------

const _: () = assert!(
    DEVICE5_PREBUILD_INFO_SLOT + 1 == DEVICE5_SLOTS,
    "Device5 ends at the prebuild-info call: 69 slots, index 68"
);
const _: () = assert!(
    IUNKNOWN_SLOTS + 4 /* Object */ + 1 /* DeviceChild */ + 40 /* Device */
        + 3 /* Device1 */ + 1 /* Device2 */ + 3 /* Device3 */ + 6 /* Device4 */
        + 8 /* Device5 */ == DEVICE5_SLOTS,
    "the Device chain's own-member counts must sum to 69 slots"
);
const _: () = assert!(
    IUNKNOWN_SLOTS + 69 == GCL4_BUILD_AS_SLOT,
    "ID3D12GraphicsCommandList4 prefix must place BuildRaytracing… on slot 72"
);
const _: () = assert!(
    GCL4_BUILD_AS_SLOT + 4 == GCL4_SLOTS - 1,
    "the build call is followed by four members (postbuild, copy, PS1, DispatchRays) and is slot 72 of 77"
);
const _: () = assert!(
    GCL4_BUILD_AS_SLOT == IUNKNOWN_SLOTS + 69,
    "the declared command-list vtable stops on the build call"
);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub unsafe fn release(ptr: *mut c_void) -> u32 {
    if ptr.is_null() {
        return 0;
    }
    let unknown = ptr as *mut IUnknown;
    ((*unknown).lpVtbl.as_ref().unwrap().Release)(unknown)
}

pub unsafe fn add_ref(ptr: *mut c_void) -> u32 {
    if ptr.is_null() {
        return 0;
    }
    let unknown = ptr as *mut IUnknown;
    ((*unknown).lpVtbl.as_ref().unwrap().AddRef)(unknown)
}

/// `QueryInterface` through the `IUnknown` prefix, for reaching `Device5` and
/// `GraphicsCommandList4` from the base interfaces wgpu already owns.
pub unsafe fn query_interface(
    ptr: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if ptr.is_null() {
        return winapi::shared::winerror::E_POINTER;
    }
    let unknown = ptr as *mut IUnknown;
    ((*unknown).lpVtbl.as_ref().unwrap().QueryInterface)(unknown, iid, out)
}

/// Parse a GUID literal (`8b4f173b-2fea-4b80-8f58-4307191ab95d`).
///
/// `winapi` generates these from Rust string literals only through a macro that
/// needs the whole GUID spelled as four typed fields; parsing the documented
/// text form keeps the constants in this file readable and verifiable against
/// `d3d12.h`.
pub fn guid(text: &str) -> Option<GUID> {
    let mut parts = text.trim().split('-');
    let data1 = u32::from_str_radix(parts.next()?, 16).ok()?;
    let data2 = u16::from_str_radix(parts.next()?, 16).ok()?;
    let data3 = u16::from_str_radix(parts.next()?, 16).ok()?;
    let d4 = u16::from_str_radix(parts.next()?, 16).ok()?;
    let tail = parts.next()?;
    if parts.next().is_some() || tail.len() != 12 {
        return None;
    }
    let d5 = u64::from_str_radix(tail, 16).ok()?;
    let mut data4 = [0u8; 8];
    data4[..2].copy_from_slice(&d4.to_be_bytes());
    data4[2..].copy_from_slice(&d5.to_be_bytes()[2..]);
    Some(GUID {
        Data1: data1,
        Data2: data2,
        Data3: data3,
        Data4: data4,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_parse_to_the_documented_bits() {
        let g = guid(IID_ID3D12DEVICE5).expect("device5 guid");
        assert_eq!(g.Data1, 0x8b4f173b);
        assert_eq!(g.Data2, 0x2fea);
        assert_eq!(g.Data3, 0x4b80);
        assert_eq!(g.Data4, [0x8f, 0x58, 0x43, 0x07, 0x19, 0x1a, 0xb9, 0x5d]);
        assert!(guid("nope").is_none());
        assert!(guid("8b4f173b-2fea-4b80-8f58").is_none());
    }

    #[test]
    fn descriptor_value_types_match_d3d12_h() {
        assert_eq!(
            std::mem::size_of::<D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO>(),
            24
        );
        assert_eq!(std::mem::size_of::<D3D12_RAYTRACING_INSTANCE_DESC>(), 80);
        assert_eq!(std::mem::align_of::<D3D12_RAYTRACING_INSTANCE_DESC>(), 16);
        // Triangle arm: transform address (8), two formats and two counts (16),
        // index address (8), vertex address + stride (16) = 48; plus the
        // Type/Flags header = 56.
        assert_eq!(
            std::mem::size_of::<D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC>(),
            48
        );
        assert_eq!(std::mem::size_of::<D3D12_RAYTRACING_GEOMETRY_DESC>(), 56);
        // Destination (8) + inputs (24: type, flags, count, layout, 8-byte
        // union) + source (8) + scratch (8) = 48.
        assert_eq!(
            std::mem::size_of::<D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS>(),
            24
        );
        assert_eq!(
            std::mem::size_of::<D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC>(),
            48
        );
        assert_eq!(
            std::mem::size_of::<D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE>(),
            16
        );
        // BOOL must be a 4-byte Win32 BOOL here, not Rust's `bool`.
        assert_eq!(std::mem::size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS5>(), 12);
    }

    /// The state-object numbering is not the declaration order, and guessing it
    /// from memory produced a wrong set the runtime would reject outright.
    #[test]
    fn state_subobject_types_match_the_header_enum() {
        // From the `D3D12_STATE_SUBOBJECT_TYPE` enum in `d3d12.h`.
        assert_eq!(D3D12_STATE_SUBOBJECT_TYPE_STATE_OBJECT_CONFIG, 0);
        assert_eq!(D3D12_STATE_SUBOBJECT_TYPE_GLOBAL_ROOT_SIGNATURE, 1);
        assert_eq!(D3D12_STATE_SUBOBJECT_TYPE_DXIL_LIBRARY, 5);
        assert_eq!(D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_SHADER_CONFIG, 9);
        assert_eq!(D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_PIPELINE_CONFIG, 10);
        assert_eq!(D3D12_STATE_SUBOBJECT_TYPE_HIT_GROUP, 11);
        assert_eq!(SHADER_IDENTIFIER_SIZE, 32);
    }

    /// The dispatch description is handed straight to `DispatchRays`, so its
    /// layout has to be the C one.
    #[test]
    fn the_dispatch_description_is_the_c_layout() {
        let word = std::mem::size_of::<usize>();
        // RayGenerationShaderRecord: {u64 address, u64 size}
        assert_eq!(std::mem::size_of::<D3D12_GPU_VIRTUAL_ADDRESS_RANGE>(), 2 * 8);
        // Each shader table: {u64 address, u64 size, u64 stride}
        assert_eq!(std::mem::size_of::<D3D12_SHADER_TABLE>(), 3 * 8);
        // One address range (2 words) and three shader tables (3 words each) is
        // 88 bytes, then Width/Height/Depth is 12 more. The struct is 8-byte
        // aligned, so the C compiler pads it to 104 and Rust must agree — a
        // mismatch here would hand `DispatchRays` a truncated description.
        assert_eq!(
            std::mem::size_of::<D3D12_DISPATCH_RAYS_DESC>(),
            104,
            "88 bytes of tables plus 3 UINTs, padded to the struct's 8-byte alignment"
        );
        assert_eq!(104 % word, 0);
    }

    #[test]
    fn the_vtables_are_exactly_as_wide_as_the_headers_say() {
        // 8 bytes per slot on every D3D12 ABI this targets.
        assert_eq!(
            std::mem::size_of::<ID3D12Device5Vtbl>(),
            (DEVICE5_PREBUILD_INFO_SLOT + 1) * std::mem::size_of::<usize>()
        );
        // The list vtable now runs through `DispatchRays` (slot 76) because the
        // hardware tracer dispatches rays rather than only building structures.
        assert_eq!(
            std::mem::size_of::<ID3D12GraphicsCommandList4Vtbl>(),
            GCL4_SLOTS * std::mem::size_of::<usize>()
        );
        // `DispatchRays` is the last member the header declares, so the struct
        // has to end exactly there.
        assert_eq!(GCL4_SLOTS - 1, 76, "DispatchRays is slot 76 of 77");
    }
}
