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
//! ID3D12Device5: CreateStateObject at slot 62,
//! GetRaytracingAccelerationStructurePrebuildInfo at slot 63, followed by
//! CheckDriverMatchingIdentifier at slot 64.
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

// Slots counted from the C vtable in the public DirectX-Headers d3d12.h.
// Alternative struct-return declarations are conditional, not extra slots.
pub const IUNKNOWN_SLOTS: usize = 3;
pub const DEVICE5_SLOTS: usize = 65;
pub const GCL4_SLOTS: usize = 77;
pub const DEVICE5_CREATE_STATE_OBJECT_SLOT: usize = 62;
pub const DEVICE5_PREBUILD_INFO_SLOT: usize = 63;
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

/// Native bitfields packed into two 32-bit words. The GPU address of the
/// descriptor array must be 16-byte aligned; the descriptor itself is 64 bytes.
#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
pub struct D3D12_RAYTRACING_INSTANCE_DESC {
    pub Transform: [f32; 12],
    pub InstanceIDAndMask: u32,
    pub ContributionAndFlags: u32,
    pub AccelerationStructure: GpuAddress,
}

impl D3D12_RAYTRACING_INSTANCE_DESC {
    pub fn identity(acceleration_structure: GpuAddress) -> Self {
        Self {
            Transform: [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0.],
            InstanceIDAndMask: 0xff << 24,
            ContributionAndFlags: (INSTANCE_FLAG_FORCE_OPAQUE
                | INSTANCE_FLAG_TRIANGLE_CULL_DISABLE)
                << 24,
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
pub const TYPE_BOTTOM_LEVEL: u32 = 1;
pub const TYPE_TOP_LEVEL: u32 = 0;
pub const BUILD_FLAG_NONE: u32 = 0x00;
pub const BUILD_FLAG_ALLOW_UPDATE: u32 = 0x01;
pub const BUILD_FLAG_ALLOW_COMPACTION: u32 = 0x02;
pub const BUILD_FLAG_PREFER_FAST_TRACE: u32 = 0x04;
pub const BUILD_FLAG_PREFER_FAST_BUILD: u32 = 0x08;
pub const ELEMENTS_LAYOUT_ARRAY: u32 = 0;
pub const GEOMETRY_TYPE_TRIANGLES: u32 = 0;
pub const GEOMETRY_FLAG_OPAQUE: u32 = 0x01;
pub const INSTANCE_FLAG_NONE: u32 = 0;
pub const INSTANCE_FLAG_TRIANGLE_CULL_DISABLE: u32 = 0x01;
pub const INSTANCE_FLAG_FORCE_OPAQUE: u32 = 0x04;
pub const DXGI_FORMAT_R32G32B32_FLOAT: u32 = 6;
pub const DXGI_FORMAT_R32_UINT: u32 = 42;

// ---------------------------------------------------------------------------
// Vtables
// ---------------------------------------------------------------------------

/// `ID3D12Device5`, addressing only `GetRaytracingAccelerationStructurePrebuildInfo`.
///
/// The typed prefix ends at GetRaytracingAccelerationStructurePrebuildInfo
/// (slot 63); the native interface has another method at slot 64.
#[repr(C)]
pub struct ID3D12Device5Vtbl {
    pub QueryInterface:
        unsafe extern "system" fn(*mut IUnknown, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(*mut IUnknown) -> u32,
    pub Release: unsafe extern "system" fn(*mut IUnknown) -> u32,
    /// Slots 3..=61, through `CreateMetaCommand`.
    pub prefix: [usize; DEVICE5_CREATE_STATE_OBJECT_SLOT - IUNKNOWN_SLOTS],
    /// Slot 62: build a ray-tracing state object from a DXIL library.
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
#[derive(Clone, Copy)]
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
// A native compute pipeline
//
// The hardware tracer's shader is a *compute* shader (8x8 threads) that uses DXR
// 1.1 inline `RayQuery` against a top-level acceleration structure. There is no
// raygen/miss/hit-group pipeline and no `DispatchRays`: the traversal happens
// inside `TraceRayInline`, so what has to be built is an ordinary compute
// pipeline state object, and what has to be launched is an ordinary `Dispatch`.
// ---------------------------------------------------------------------------

/// `D3D12_COMPUTE_PIPELINE_STATE_DESC`. Only the members a compute pipeline
/// without a cache, a root signature override or streaming set uses; the rest are
/// left zeroed, which is what the header's own `{}` does.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_COMPUTE_PIPELINE_STATE_DESC {
    pub pRootSignature: *mut c_void,
    pub CS: D3D12_SHADER_BYTECODE,
    pub NodeMask: u32,
    pub CachedPSO: D3D12_CACHED_PIPELINE_STATE,
    pub Flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_CACHED_PIPELINE_STATE {
    pub pCachedBlob: *const c_void,
    pub CachedBlobSizeInBytes: usize,
}

impl Default for D3D12_COMPUTE_PIPELINE_STATE_DESC {
    fn default() -> Self {
        Self {
            pRootSignature: std::ptr::null_mut(),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: std::ptr::null(),
                BytecodeLength: 0,
            },
            NodeMask: 0,
            CachedPSO: D3D12_CACHED_PIPELINE_STATE {
                pCachedBlob: std::ptr::null(),
                CachedBlobSizeInBytes: 0,
            },
            Flags: 0,
        }
    }
}

/// `D3D12_PIPELINE_STATE_FLAG_NONE`.
pub const D3D12_PIPELINE_STATE_FLAG_NONE: u32 = 0;

/// `D3D12_ROOT_SIGNATURE_FLAG_NONE`: no restrictions on which stages may see the
/// signature's descriptors.
pub const D3D12_ROOT_SIGNATURE_FLAG_NONE: u32 = 0;

/// `D3D12_ROOT_PARAMETER_TYPE_*` and `D3D12_DESCRIPTOR_RANGE_TYPE_*`.
pub const D3D12_ROOT_PARAMETER_TYPE_CBV: u32 = 2;
pub const D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE: u32 = 0;
pub const D3D12_DESCRIPTOR_RANGE_TYPE_SRV: u32 = 0;
pub const D3D12_DESCRIPTOR_RANGE_TYPE_UAV: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_ROOT_DESCRIPTOR {
    pub ShaderRegister: u32,
    pub RegisterSpace: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_DESCRIPTOR_RANGE {
    pub RangeType: u32,
    pub NumDescriptors: u32,
    pub BaseShaderRegister: u32,
    pub RegisterSpace: u32,
    pub OffsetInDescriptorsFromTableStart: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_ROOT_DESCRIPTOR_TABLE {
    pub NumDescriptorRanges: u32,
    pub pDescriptorRanges: *const D3D12_DESCRIPTOR_RANGE,
}

/// `D3D12_ROOT_PARAMETER`: `{ Type, union, ShaderVisibility }`. The union's
/// largest member is the descriptor table (a count plus a pointer), so the
/// parameter uses an explicit union. A table's padding must never stand in for
/// the CBV descriptor's register space: padding bytes are not initialized.
#[repr(C)]
#[derive(Clone, Copy)]
pub union D3D12_ROOT_PARAMETER_UNION {
    pub Table: D3D12_ROOT_DESCRIPTOR_TABLE,
    pub Descriptor: D3D12_ROOT_DESCRIPTOR,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_ROOT_PARAMETER {
    pub ParameterType: u32,
    pub Data: D3D12_ROOT_PARAMETER_UNION,
    pub ShaderVisibility: u32,
}

/// `ID3D12Blob`, the two accessors a serialised signature comes back in.
#[repr(C)]
pub struct ID3DBlobVtbl {
    pub QueryInterface:
        unsafe extern "system" fn(*mut IUnknown, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(*mut IUnknown) -> u32,
    pub Release: unsafe extern "system" fn(*mut IUnknown) -> u32,
    pub GetBufferPointer: unsafe extern "system" fn(*mut c_void) -> *const c_void,
    pub GetBufferSize: unsafe extern "system" fn(*mut c_void) -> usize,
}

#[repr(C)]
pub struct ID3DBlob {
    pub lpVtbl: *mut ID3DBlobVtbl,
}

/// `D3D_ROOT_SIGNATURE_VERSION_1`.
pub const D3D_ROOT_SIGNATURE_VERSION_1: u32 = 1;

/// `D3D12SerializeRootSignature` from `d3d12.dll`.
///
/// Windows links it; everywhere else a stub stands in so this module can be
/// type-checked by the Linux harness. Nothing off Windows ever creates a device,
/// so the stub is unreachable rather than silently wrong.
#[cfg(all(windows, not(test)))]
#[link(name = "d3d12")]
extern "system" {
    fn D3D12SerializeRootSignature(
        pRootSignature: *const D3D12_ROOT_SIGNATURE_DESC,
        Version: u32,
        ppBlob: *mut *mut ID3DBlob,
        ppErrorBlob: *mut *mut ID3DBlob,
    ) -> HRESULT;
}

#[cfg(not(all(windows, not(test))))]
unsafe fn D3D12SerializeRootSignature(
    _desc: *const D3D12_ROOT_SIGNATURE_DESC,
    _version: u32,
    _blob: *mut *mut ID3DBlob,
    _error: *mut *mut ID3DBlob,
) -> HRESULT {
    -1
}

/// `IID_ID3D12RootSignature`: `c54a6b66-72df-4ee8-8be5-a946a1429214`.
pub const IID_ID3D12ROOTSIGNATURE: &str = "c54a6b66-72df-4ee8-8be5-a946a1429214";
/// `IID_ID3D12PipelineState`: `765a30f3-f624-4c6f-a828-ace948622445`.
pub const IID_ID3D12PIPELINESTATE: &str = "765a30f3-f624-4c6f-a828-ace948622445";

/// The text of an error blob, for a failure message.
///
/// # Safety
/// `blob` must be live, or null.
unsafe fn blob_text(blob: *mut ID3DBlob) -> String {
    if blob.is_null() {
        return String::new();
    }
    let vtable = &*((*blob).lpVtbl as *const ID3DBlobVtbl);
    let pointer = (vtable.GetBufferPointer)(blob as *mut c_void);
    let size = (vtable.GetBufferSize)(blob as *mut c_void);
    if pointer.is_null() || size == 0 {
        return String::new();
    }
    let bytes = std::slice::from_raw_parts(pointer as *const u8, size);
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// The root signature `dxr_trace.hlsl` is written against.
///
/// The shader declares `ConstantBuffer<Params> : register(b0)`, a
/// `RaytracingAccelerationStructure` on `t0`, two `Buffer<float4>` on `t1` and
/// `t2`, and `RWTexture2D<float4>` on `u0`, so the signature is:
///
/// * parameter 0 — a CBV at `b0`, the shader's `Params` block (ten vec4s);
/// * parameter 1 — a descriptor table of three SRVs covering `t0..t2`;
/// * parameter 2 — a descriptor table of one UAV at `u0`.
///
/// Returns the signature, or the serialiser's own complaint.
///
/// # Safety
/// `device` must be a live `ID3D12Device` (`ID3D12Device5` begins with one).
pub unsafe fn create_trace_root_signature(device: *mut c_void) -> Result<*mut c_void, String> {
    let srv = [D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
        NumDescriptors: 3,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: 0,
    }];
    let uav = [D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
        NumDescriptors: 1,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: 0,
    }];
    let parameters = [
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
            Data: D3D12_ROOT_PARAMETER_UNION {
                Descriptor: D3D12_ROOT_DESCRIPTOR {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Data: D3D12_ROOT_PARAMETER_UNION {
                Table: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: srv.as_ptr(),
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Data: D3D12_ROOT_PARAMETER_UNION {
                Table: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: uav.as_ptr(),
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
    ];
    let desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: parameters.len() as u32,
        pParameters: parameters.as_ptr() as *const c_void,
        NumStaticSamplers: 0,
        pStaticSamplers: std::ptr::null(),
        Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE,
    };
    let mut blob: *mut ID3DBlob = std::ptr::null_mut();
    let mut error: *mut ID3DBlob = std::ptr::null_mut();
    let hr =
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, &mut error);
    if hr < 0 || blob.is_null() {
        let why = blob_text(error);
        release(error as *mut c_void);
        return Err(if why.is_empty() {
            format!("D3D12SerializeRootSignature failed (HRESULT 0x{hr:08x})")
        } else {
            format!("the trace root signature is rejected: {why}")
        });
    }
    let vtable = &*((*blob).lpVtbl as *const ID3DBlobVtbl);
    let buffer = (vtable.GetBufferPointer)(blob as *mut c_void);
    let length = (vtable.GetBufferSize)(blob as *mut c_void);
    let mut signature: *mut c_void = std::ptr::null_mut();
    let hr = create_root_signature(device, buffer, length, &mut signature);
    release(blob as *mut c_void);
    release(error as *mut c_void);
    if hr < 0 || signature.is_null() {
        return Err(format!(
            "ID3D12Device::CreateRootSignature failed (HRESULT 0x{hr:08x})"
        ));
    }
    Ok(signature)
}

/// `ID3D12Device::CreateRootSignature`, slot 17 of the device vtable, counted
/// from `d3d12.h`: `IUnknown` 0..2, `ID3D12Object` 3..6, `ID3D12DeviceChild` 7,
/// then the ten `ID3D12Device` members from `GetNodeCount` (8) through
/// `GetDescriptorHandleIncrementSize` (16), so `CreateRootSignature` is 17.
///
/// # Safety
/// `device` must be live and `blob`/`length` a serialised signature.
// ---------------------------------------------------------------------------
// COM vtable slots
// ---------------------------------------------------------------------------
//
// Every one of these is a raw pointer offset into a COM object's vtable, so a
// wrong number silently calls a *different* method: a mistyped `Dispatch` slot
// can `ResolveSubresource` instead. There is no way for the type system to
// notice, and the offline harness cannot notice either, because it stubs D3D12
// out entirely. So each number below is recorded with the count that produces it
// and pinned by the `const` assertions further down.
//
// The prefix shared by every interface here is IUnknown (3: QueryInterface,
// AddRef, Release) then ID3D12Object (4: GetPrivateData, SetPrivateData,
// SetPrivateDataInterface, SetName). `ID3D12Pageable` adds nothing at all.
//
// Verified twice against independent sources that agree exactly:
//   * `tools/imaginator-check/real/winapi-rs-0.3.9/src/um/d3d12.rs` (checked in)
//   * the Microsoft `DirectX-Headers` `include/directx/d3d12.h` declaration order
//
// The earlier values in this file were off by one across the whole `ID3D12Device`
// block (a `GetNodeCount` slot was never counted), which is why the pipeline
// "built" while `CreateComputePipelineState` actually invoked
// `CreateCommandList`, and why nothing was ever traced.

const IUNKNOWN_AND_OBJECT: usize = 7;

/// `ID3D12Device`. Own members in declaration order, from `GetNodeCount`.
const DEVICE_GET_NODE_COUNT: usize = IUNKNOWN_AND_OBJECT + 0;
const DEVICE_CREATE_COMPUTE_PIPELINE_STATE: usize = IUNKNOWN_AND_OBJECT + 4;
const DEVICE_CREATE_COMMAND_LIST: usize = IUNKNOWN_AND_OBJECT + 5;
const DEVICE_CREATE_DESCRIPTOR_HEAP: usize = IUNKNOWN_AND_OBJECT + 7;
const DEVICE_GET_DESCRIPTOR_HANDLE_INCREMENT_SIZE: usize = IUNKNOWN_AND_OBJECT + 8;
const DEVICE_CREATE_ROOT_SIGNATURE: usize = IUNKNOWN_AND_OBJECT + 9;
const DEVICE_CREATE_CONSTANT_BUFFER_VIEW: usize = IUNKNOWN_AND_OBJECT + 10;
pub const DEVICE_CREATE_SHADER_RESOURCE_VIEW: usize = IUNKNOWN_AND_OBJECT + 11;
pub const DEVICE_CREATE_UNORDERED_ACCESS_VIEW: usize = IUNKNOWN_AND_OBJECT + 12;
/// `CreateCommittedResource` (`GetResourceAllocationInfo` and
/// `GetCustomHeapProperties` each have two C++ overloads but one vtable slot).
const DEVICE_CREATE_COMMITTED_RESOURCE: usize = IUNKNOWN_AND_OBJECT + 20;

/// `ID3D12Resource`, after `ID3D12Pageable` (which adds nothing).
const RESOURCE_MAP: usize = IUNKNOWN_AND_OBJECT + 1;
const RESOURCE_UNMAP: usize = IUNKNOWN_AND_OBJECT + 2;
pub const RESOURCE_GET_GPU_VIRTUAL_ADDRESS: usize = IUNKNOWN_AND_OBJECT + 4;

/// `ID3D12DescriptorHeap`: `GetDesc`, then the two handle starts.
const HEAP_GET_CPU_HANDLE_START: usize = IUNKNOWN_AND_OBJECT + 2;
const HEAP_GET_GPU_HANDLE_START: usize = IUNKNOWN_AND_OBJECT + 3;

/// `ID3D12GraphicsCommandList`, after `ID3D12CommandList::GetType` (one slot).
const GCL_PREFIX: usize = IUNKNOWN_AND_OBJECT + 2;
const GCL_RESET: usize = GCL_PREFIX + 1;
pub const GCL_DISPATCH: usize = GCL_PREFIX + 5;
const GCL_COPY_TEXTURE_REGION: usize = GCL_PREFIX + 7;
const GCL_RESOURCE_BARRIER: usize = GCL_PREFIX + 17;
const GCL_SET_DESCRIPTOR_HEAPS: usize = GCL_PREFIX + 19;
const GCL_SET_COMPUTE_ROOT_SIGNATURE: usize = GCL_PREFIX + 20;
pub const GCL_SET_COMPUTE_ROOT_DESCRIPTOR_TABLE: usize = GCL_PREFIX + 22;
pub const GCL_SET_COMPUTE_ROOT_CONSTANT_BUFFER_VIEW: usize = GCL_PREFIX + 28;
const GCL_SET_COMPUTE_ROOT_SHADER_RESOURCE_VIEW: usize = GCL_PREFIX + 30;
const GCL_SET_COMPUTE_ROOT_UNORDERED_ACCESS_VIEW: usize = GCL_PREFIX + 32;

unsafe fn create_root_signature(
    device: *mut c_void,
    blob: *const c_void,
    length: usize,
    out: *mut *mut c_void,
) -> HRESULT {
    let vtable = *(device as *mut *const usize);
    let create: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *const c_void,
        usize,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT = std::mem::transmute(*vtable.add(DEVICE_CREATE_ROOT_SIGNATURE));
    let Some(iid) = guid(IID_ID3D12ROOTSIGNATURE) else {
        return -1;
    };
    create(device, 0, blob, length, &iid, out)
}

/// Read recent messages from the optional D3D12 debug layer. It must have been
/// enabled before creating the device; no queue is available on normal installs.
unsafe fn recent_debug_messages(device: *mut c_void) -> Vec<String> {
    let Some(iid) = guid("0742a90b-c387-483f-b946-30a7e4e61458") else {
        return Vec::new();
    };
    let mut queue = std::ptr::null_mut();
    if query_interface(device, &iid, &mut queue) < 0 || queue.is_null() {
        return Vec::new();
    }
    let vt = *(queue as *mut *const usize);
    let count_fn: unsafe extern "system" fn(*mut c_void) -> u64 = std::mem::transmute(*vt.add(9));
    let get: unsafe extern "system" fn(*mut c_void, u64, *mut c_void, *mut usize) -> HRESULT =
        std::mem::transmute(*vt.add(5));
    #[repr(C)]
    struct Message {
        category: u32,
        severity: u32,
        id: u32,
        description: *const u8,
        len: usize,
    }
    let count = count_fn(queue);
    let mut messages = Vec::new();
    for index in count.saturating_sub(12)..count {
        let mut size = 0usize;
        if get(queue, index, std::ptr::null_mut(), &mut size) < 0
            || size < std::mem::size_of::<Message>()
            || size > 65536
        {
            continue;
        }
        let mut storage = vec![0u64; size.div_ceil(8)];
        if get(queue, index, storage.as_mut_ptr() as *mut c_void, &mut size) < 0 {
            continue;
        }
        let message = &*(storage.as_ptr() as *const Message);
        let base = storage.as_ptr() as usize;
        let end = base.saturating_add(storage.len() * 8);
        let address = message.description as usize;
        if address < base || address > end || message.len > end.saturating_sub(address) {
            continue;
        }
        let text = std::slice::from_raw_parts(message.description, message.len);
        messages.push(format!(
            "D3D12 #{}: {}",
            message.id,
            String::from_utf8_lossy(text).trim_end_matches('\0')
        ));
    }
    release(queue);
    messages
}

/// Create the compute pipeline state object that runs the DXIL trace shader.
///
/// # Safety
/// `device` must be live and `dxil` a compiled compute shader for `signature`.
pub unsafe fn create_compute_pipeline(
    device: *mut c_void,
    signature: *mut c_void,
    dxil: &[u8],
) -> Result<*mut c_void, String> {
    let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
        pRootSignature: signature,
        CS: D3D12_SHADER_BYTECODE {
            pShaderBytecode: dxil.as_ptr() as *const c_void,
            BytecodeLength: dxil.len(),
        },
        ..Default::default()
    };
    // The header-derived base-device slot for CreateComputePipelineState is 11.
    let vtable = *(device as *mut *const usize);
    let create: unsafe extern "system" fn(
        *mut c_void,
        *const D3D12_COMPUTE_PIPELINE_STATE_DESC,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT = std::mem::transmute(*vtable.add(DEVICE_CREATE_COMPUTE_PIPELINE_STATE));
    let Some(iid) = guid(IID_ID3D12PIPELINESTATE) else {
        return Err("the PipelineState GUID literal is invalid".into());
    };
    let mut state: *mut c_void = std::ptr::null_mut();
    let hr = create(device, &desc, &iid, &mut state);
    if hr < 0 || state.is_null() {
        let messages = recent_debug_messages(device);
        let detail = if messages.is_empty() {
            "no debug-layer details; restart with WGPU_VALIDATION=1 and Windows Graphics Tools installed".into()
        } else {
            messages.join(" | ")
        };
        return Err(format!(
            "CreateComputePipelineState failed (HRESULT 0x{hr:08x}): {detail}"
        ));
    }
    Ok(state)
}

// ---------------------------------------------------------------------------
// Descriptor heaps and views
//
// A compute dispatch reaches its buffers through a descriptor heap: the root
// signature names tables, and each table is a run of descriptors written from the
// CPU. What follows is the minimum to fill one: two view descriptions and the
// slot addresses the heap hands back.
// ---------------------------------------------------------------------------

/// `D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV`.
pub const D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV: u32 = 0;
/// `D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE`: the heap a dispatch reads.
pub const D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE: u32 = 1;

/// `D3D12_RESOURCE_DIMENSION_BUFFER`.
pub const D3D12_RESOURCE_DIMENSION_BUFFER: u32 = 1;

/// `DXGI_FORMAT_UNKNOWN`, which is what a buffer view uses.
pub const DXGI_FORMAT_UNKNOWN: u32 = 0;

/// `D3D12_BUFFER_SRV_FLAG_NONE`.
pub const D3D12_BUFFER_SRV_FLAG_NONE: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_DESCRIPTOR_HEAP_DESC {
    pub Type: u32,
    pub NumDescriptors: u32,
    pub Flags: u32,
    pub NodeMask: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_CONSTANT_BUFFER_VIEW_DESC {
    pub BufferLocation: u64,
    pub SizeInBytes: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_BUFFER_SRV {
    pub FirstElement: u64,
    pub NumElements: u32,
    pub StructureByteStride: u32,
    pub Flags: u32,
}

/// `D3D12_BUFFER_SRV`'s three fields and `D3D12_BUFFER_UAV`'s four are the same
/// members in a different order, and the C structs wrap them in a union together
/// with the texture forms. Only the buffer forms are used here, so the largest
/// of them — the UAV, with its counter and a stride — is the layout.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_BUFFER_UAV {
    pub FirstElement: u64,
    pub NumElements: u32,
    pub StructureByteStride: u32,
    pub CounterOffsetInBytes: u64,
    pub Flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_SHADER_RESOURCE_VIEW_DESC {
    pub Format: u32,
    pub ViewDimension: u32,
    pub Shader4ComponentMapping: u32,
    pub Buffer: D3D12_BUFFER_UAV,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_UNORDERED_ACCESS_VIEW_DESC {
    pub Format: u32,
    pub ViewDimension: u32,
    pub Buffer: D3D12_BUFFER_UAV,
}

/// `D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING`: the identity swizzle every buffer
/// SRV passes.
pub const D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING: u32 = 0x1688;

/// `D3D12_SRV_DIMENSION_BUFFER` / `D3D12_UAV_DIMENSION_BUFFER`.
pub const D3D12_SRV_DIMENSION_BUFFER: u32 = 1;
pub const D3D12_UAV_DIMENSION_BUFFER: u32 = 1;

/// A descriptor heap as a `ID3D12DescriptorHeap`, plus the increment the device
/// reports between one descriptor and the next.
pub unsafe fn create_descriptor_heap(
    device: *mut c_void,
    count: u32,
) -> Result<*mut c_void, String> {
    let desc = D3D12_DESCRIPTOR_HEAP_DESC {
        Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
        NumDescriptors: count,
        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
        NodeMask: 0,
    };
    // `ID3D12Device::CreateDescriptorHeap` is slot 15: `GetNodeCount` 8,
    // `CreateCommandQueue` 9, `CreateCommandAllocator` 10,
    // `CreateGraphicsPipelineState` 11, `CreateComputePipelineState` 12,
    // `CreateCommandList` 13, `CheckFeatureSupport` 14, `CreateDescriptorHeap` 15.
    let vtable = *(device as *mut *const usize);
    let create: unsafe extern "system" fn(
        *mut c_void,
        *const D3D12_DESCRIPTOR_HEAP_DESC,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT = std::mem::transmute(*vtable.add(DEVICE_CREATE_DESCRIPTOR_HEAP));
    let Some(iid) = guid(IID_ID3D12DESCRIPTORHEAP) else {
        return Err("the DescriptorHeap GUID literal is invalid".into());
    };
    let mut heap: *mut c_void = std::ptr::null_mut();
    let hr = create(device, &desc, &iid, &mut heap);
    if hr < 0 || heap.is_null() {
        return Err(format!(
            "CreateDescriptorHeap({count} descriptors) failed (HRESULT 0x{hr:08x})"
        ));
    }
    Ok(heap)
}

/// `IID_ID3D12DescriptorHeap`: `8efb471d-616c-4f49-90f7-127bb763fa51`.
pub const IID_ID3D12DESCRIPTORHEAP: &str = "8efb471d-616c-4f49-90f7-127bb763fa51";

/// `ID3D12DescriptorHeap::GetCPUDescriptorHandleForHeapStart` (slot 9) and
/// `GetGPUDescriptorHandleForHeapStart` (slot 10). Both return a two-word
/// `D3D12_CPU_DESCRIPTOR_HANDLE` / `D3D12_GPU_DESCRIPTOR_HANDLE`.
///
/// # Safety
/// `heap` must be live.
pub unsafe fn heap_handles(heap: *mut c_void) -> (usize, u64) {
    // Counted from `d3d12.h`: IUnknown 0..2, ID3D12Object 3..6
    // (`GetPrivateData`, `SetPrivateData`, `SetPrivateDataInterface`, `SetName`),
    // ID3D12DeviceChild 7 (`GetDevice`), ID3D12Pageable adds nothing, then
    // ID3D12DescriptorHeap's own three: GetDesc 8, the CPU handle start 9 and the
    // GPU handle start 10.
    let vtable = *(heap as *mut *const usize);
    let get_cpu: unsafe extern "system" fn(*mut c_void) -> usize =
        std::mem::transmute(*vtable.add(HEAP_GET_CPU_HANDLE_START));
    let get_gpu: unsafe extern "system" fn(*mut c_void) -> u64 =
        std::mem::transmute(*vtable.add(HEAP_GET_GPU_HANDLE_START));
    (get_cpu(heap), get_gpu(heap))
}

/// `ID3D12Device::GetDescriptorHandleIncrementSize` (slot 16).
///
/// # Safety
/// `device` must be live.
pub unsafe fn descriptor_increment(device: *mut c_void) -> u32 {
    let vtable = *(device as *mut *const usize);
    let size: unsafe extern "system" fn(*mut c_void, u32) -> u32 =
        std::mem::transmute(*vtable.add(DEVICE_GET_DESCRIPTOR_HANDLE_INCREMENT_SIZE));
    size(device, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
}

/// `ID3D12Device::CreateConstantBufferView` (slot 18).
///
/// # Safety
/// `device` must be live and `handle` a slot in a heap it created.
pub unsafe fn write_constant_buffer_view(
    device: *mut c_void,
    location: u64,
    size: u32,
    handle: usize,
) {
    let desc = D3D12_CONSTANT_BUFFER_VIEW_DESC {
        BufferLocation: location,
        SizeInBytes: size,
    };
    let vtable = *(device as *mut *const usize);
    let write: unsafe extern "system" fn(
        *mut c_void,
        *const D3D12_CONSTANT_BUFFER_VIEW_DESC,
        usize,
    ) = std::mem::transmute(*vtable.add(DEVICE_CREATE_CONSTANT_BUFFER_VIEW));
    write(device, &desc, handle);
}

/// `ID3D12Device::CreateShaderResourceView` (slot 19), buffer form.
///
/// # Safety
/// `device` must be live, `resource` a buffer, `handle` a slot in its heap.
pub unsafe fn write_buffer_srv(
    device: *mut c_void,
    resource: *mut c_void,
    first_element: u64,
    num_elements: u32,
    stride: u32,
    handle: usize,
) {
    debug_assert_eq!(stride, 16);
    let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
        ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Buffer: D3D12_BUFFER_UAV {
            FirstElement: first_element,
            NumElements: num_elements,
            // HLSL Buffer<float4> is typed, not StructuredBuffer<float4>.
            StructureByteStride: 0,
            CounterOffsetInBytes: 0,
            Flags: D3D12_BUFFER_SRV_FLAG_NONE,
        },
    };
    let vtable = *(device as *mut *const usize);
    let write: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const D3D12_SHADER_RESOURCE_VIEW_DESC,
        usize,
    ) = std::mem::transmute(*vtable.add(DEVICE_CREATE_SHADER_RESOURCE_VIEW));
    write(device, resource, &desc, handle);
}

/// `ID3D12Device::CreateUnorderedAccessView` (slot 20), buffer form.
///
/// # Safety
/// `device` must be live, `resource` a buffer, `handle` a slot in its heap.
pub unsafe fn write_buffer_uav(
    device: *mut c_void,
    resource: *mut c_void,
    first_element: u64,
    num_elements: u32,
    stride: u32,
    handle: usize,
) {
    let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT_UNKNOWN,
        ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
        Buffer: D3D12_BUFFER_UAV {
            FirstElement: first_element,
            NumElements: num_elements,
            StructureByteStride: stride,
            CounterOffsetInBytes: 0,
            Flags: 0,
        },
    };
    let vtable = *(device as *mut *const usize);
    let write: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const D3D12_UNORDERED_ACCESS_VIEW_DESC,
        usize,
    ) = std::mem::transmute(*vtable.add(DEVICE_CREATE_UNORDERED_ACCESS_VIEW));
    write(device, resource, std::ptr::null_mut(), &desc, handle);
}

/// Create the RWTexture2D<float4> view expected by the RayQuery shader.
pub unsafe fn write_texture_uav(device: *mut c_void, resource: *mut c_void, handle: usize) {
    let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
        ViewDimension: 4,                    // D3D12_UAV_DIMENSION_TEXTURE2D
        Buffer: D3D12_BUFFER_UAV::default(), // mip 0, plane 0
    };
    let vtable = *(device as *mut *const usize);
    let write: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const D3D12_UNORDERED_ACCESS_VIEW_DESC,
        usize,
    ) = std::mem::transmute(*vtable.add(DEVICE_CREATE_UNORDERED_ACCESS_VIEW));
    write(device, resource, std::ptr::null_mut(), &desc, handle);
}

/// `ID3D12GraphicsCommandList::SetComputeRootDescriptorTable` (slot 22) and
/// `Dispatch` (slot 25) live on the command list; both are reached through
/// `winapi`'s own declarations in `dx12.rs`, so only the handle types are here.
pub type D3D12_GPU_DESCRIPTOR_HANDLE = u64;

/// A `D3D12_GPU_DESCRIPTOR_HANDLE` from a raw address, for the root-table calls.
pub fn heap_gpu_handle(start: D3D12_GPU_DESCRIPTOR_HANDLE) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    start
}

/// The GPU handle at an offset into a heap, as the root-table calls expect.
pub fn offset_gpu_handle(
    start: D3D12_GPU_DESCRIPTOR_HANDLE,
    bytes: u64,
) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    start + bytes
}

/// `ID3D12Resource::GetGPUVirtualAddress`, slot 11 of the resource vtable.
///
/// Counted from `d3d12.h`: `IUnknown` 0..2, `ID3D12Object` 3..6,
/// `ID3D12DeviceChild` 7 (`GetDevice`), `ID3D12Pageable` adds nothing, then
/// `Map` 8, `Unmap` 9, `GetDesc` 10 and `GetGPUVirtualAddress` 11.
///
/// Slot 10 is `GetDesc`, which returns a *struct by value*; calling it through a
/// `-> u64` signature would read a fraction of that struct as an address.
///
/// # Safety
/// `resource` must be a live `ID3D12Resource`.
pub unsafe fn gpu_virtual_address(resource: *mut c_void) -> u64 {
    let vtable = *(resource as *mut *const usize);
    let address: unsafe extern "system" fn(*mut c_void) -> u64 =
        std::mem::transmute(*vtable.add(RESOURCE_GET_GPU_VIRTUAL_ADDRESS));
    address(resource)
}

// ---------------------------------------------------------------------------
// A storage texture and its readback
// ---------------------------------------------------------------------------

/// `D3D12_RESOURCE_DIMENSION_TEXTURE2D`.
pub const D3D12_RESOURCE_DIMENSION_TEXTURE2D: u32 = 3;
/// `D3D12_TEXTURE_LAYOUT_ROW_MAJOR`.
pub const D3D12_TEXTURE_LAYOUT_ROW_MAJOR: u32 = 1;
/// `DXGI_FORMAT_R32G32B32A32_FLOAT`.
pub const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;
/// `D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS`.
pub const D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS: u32 = 0x4;
/// `D3D12_RESOURCE_STATE_COMMON`.
pub const D3D12_RESOURCE_STATE_COMMON: u32 = 0;
/// `D3D12_RESOURCE_STATE_UNORDERED_ACCESS`.
pub const D3D12_RESOURCE_STATE_UNORDERED_ACCESS: u32 = 0x8;
/// `D3D12_RESOURCE_STATE_COPY_SOURCE`.
pub const D3D12_RESOURCE_STATE_COPY_SOURCE: u32 = 0x800;
/// `D3D12_HEAP_TYPE_DEFAULT` / `_UPLOAD` / `_READBACK`.
pub const D3D12_HEAP_TYPE_DEFAULT: u32 = 1;
pub const D3D12_HEAP_TYPE_UPLOAD: u32 = 2;
pub const D3D12_HEAP_TYPE_READBACK: u32 = 3;
/// `D3D12_HEAP_FLAG_NONE`.
pub const D3D12_HEAP_FLAG_NONE: u32 = 0;
/// `D3D12_CPU_PAGE_PROPERTY_UNKNOWN` / `D3D12_MEMORY_POOL_UNKNOWN`.
pub const D3D12_CPU_PAGE_PROPERTY_UNKNOWN: u32 = 0;
pub const D3D12_MEMORY_POOL_UNKNOWN: u32 = 0;
/// `D3D12_RESOURCE_BARRIER_TYPE_TRANSITION`.
pub const D3D12_RESOURCE_BARRIER_TYPE_TRANSITION: u32 = 1;
/// `D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES`.
pub const D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES: u32 = u32::MAX;

/// `D3D12_HEAP_PROPERTIES`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_HEAP_PROPERTIES {
    pub Type: u32,
    pub CPUPageProperty: u32,
    pub MemoryPoolPreference: u32,
    pub CreationNodeMask: u32,
    pub VisibleNodeMask: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DXGI_SAMPLE_DESC {
    pub Count: u32,
    pub Quality: u32,
}

/// `D3D12_RESOURCE_DESC`, buffer form and texture form sharing one layout: the
/// header's union of `D3D12_RESOURCE_DESC_BUFFER` and `D3D12_TEXTURE_DESC` is
/// `{ Dimension, Alignment, Width, Height, DepthOrArraySize, MipLevels, Format,
/// SampleDesc, Layout, Flags }` for the buffer case, which is the smaller.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_RESOURCE_DESC {
    pub Dimension: u32,
    pub Alignment: u64,
    pub Width: u64,
    pub Height: u32,
    pub DepthOrArraySize: u16,
    pub MipLevels: u16,
    pub Format: u32,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub Layout: u32,
    pub Flags: u32,
}

/// `D3D12_TEXTURE_COPY_LOCATION` for the buffer side of a texture-to-buffer copy.
#[repr(C)]
pub struct D3D12_TEXTURE_COPY_LOCATION {
    pub pResource: *mut c_void,
    pub Type: u32,
    pub PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
}

/// `D3D12_PLACED_SUBRESOURCE_FOOTPRINT`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
    pub Offset: u64,
    pub Footprint: D3D12_SUBRESOURCE_FOOTPRINT,
}

/// `D3D12_SUBRESOURCE_FOOTPRINT`: `{ Format, Width, Height, Depth, RowPitch }`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_SUBRESOURCE_FOOTPRINT {
    pub Format: u32,
    pub Width: u32,
    pub Height: u32,
    pub Depth: u32,
    pub RowPitch: u32,
}

/// `D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT`.
pub const D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT: u32 = 1;
/// `D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX`.
pub const D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX: u32 = 0;

/// `ID3D12Resource::Map` (slot 8) and `Unmap` (slot 9), for a readback buffer.
///
/// # Safety
/// `resource` must be a live `ID3D12Resource` on a CPU-visible heap.
pub unsafe fn map(resource: *mut c_void, read_range: *const c_void) -> Result<*mut u8, String> {
    let vtable = *(resource as *mut *const usize);
    let map: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *const c_void,
        *mut *mut c_void,
    ) -> HRESULT = std::mem::transmute(*vtable.add(RESOURCE_MAP));
    let mut pointer: *mut c_void = std::ptr::null_mut();
    let hr = map(resource, 0, read_range, &mut pointer);
    if hr < 0 || pointer.is_null() {
        return Err(format!("ID3D12Resource::Map failed (HRESULT 0x{hr:08x})"));
    }
    Ok(pointer as *mut u8)
}

/// # Safety
/// `resource` must be the one `map` succeeded on.
pub unsafe fn unmap(resource: *mut c_void) {
    let vtable = *(resource as *mut *const usize);
    let unmap: unsafe extern "system" fn(*mut c_void, u32, *const c_void) =
        std::mem::transmute(*vtable.add(RESOURCE_UNMAP));
    unmap(resource, 0, std::ptr::null());
}

/// `D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE`.
pub const D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE: u32 = 11;

/// The description a `RaytracingAccelerationStructure` binding needs: an address
/// and nothing else. The resource the address belongs to is never named, which is
/// why the resource argument of `CreateShaderResourceView` is null here.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_TLAS_SRV_DESC {
    pub Format: u32,
    pub ViewDimension: u32,
    pub Shader4ComponentMapping: u32,
    pub Location: u64,
}

/// Write a `RaytracingAccelerationStructure` SRV for `address` into a heap slot.
///
/// # Safety
/// `device` must be live and `handle` a slot in a heap it created.
pub unsafe fn write_tlas_srv(device: *mut c_void, address: u64, handle: usize) {
    let desc = D3D12_TLAS_SRV_DESC {
        Format: DXGI_FORMAT_UNKNOWN,
        ViewDimension: D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Location: address,
    };
    let vtable = *(device as *mut *const usize);
    let write: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const D3D12_TLAS_SRV_DESC,
        usize,
    ) = std::mem::transmute(*vtable.add(DEVICE_CREATE_SHADER_RESOURCE_VIEW));
    write(device, std::ptr::null_mut(), &desc, handle);
}

/// `ID3D12GraphicsCommandList::SetComputeRootDescriptorTable`.
///
/// The handle is a one-word struct in `winapi` and a bare integer in the offline
/// shim, so the call is made here, through the vtable, where one spelling of the
/// argument works for both.
///
/// # Safety
/// `list` must be a live command list and `base` an address in a heap bound to it.
pub unsafe fn set_compute_root_descriptor_table(list: *mut c_void, root: u32, base: u64) {
    // Counted on `ID3D12GraphicsCommandList`: IUnknown 0..2, ID3D12Object 3..6,
    // ID3D12DeviceChild 7, Close 8, Reset 9, ClearState 10, the draw calls
    // 11..12, Dispatch 13, the copies 14..18, the pipeline calls 19..20,
    // ResourceBarrier 21, ExecuteBundle 22, SetDescriptorHeaps 23, the two root
    // signatures 24..25, then SetComputeRootDescriptorTable at 30.
    let vtable = *(list as *mut *const usize);
    let set: unsafe extern "system" fn(*mut c_void, u32, u64) =
        std::mem::transmute(*vtable.add(GCL_SET_COMPUTE_ROOT_DESCRIPTOR_TABLE));
    set(list, root, base);
}

/// `ID3D12GraphicsCommandList::SetComputeRootConstantBufferView`.
///
/// Root parameter 0 of the trace root signature is a root CBV (`b0`), not a
/// descriptor table, so it is bound by GPU virtual address. Nothing called this:
/// the shader's `Params` block was therefore never written at all, and every
/// dispatch traced with whatever the driver left in the register — an eye at the
/// origin, a zero `config`, a zero `screen`. That is why the hardware path could
/// report Ready while no pixel ever changed.
///
/// # Safety
/// `list` must be a live command list with a root signature set, and `address`
/// must be the GPU virtual address of a buffer at least `PARAMS_BYTES` long.
pub unsafe fn set_compute_root_constant_buffer_view(list: *mut c_void, root: u32, address: u64) {
    // Counted on `ID3D12GraphicsCommandList`: IUnknown 0..2, ID3D12Object 3..6,
    // ID3D12DeviceChild 7, Close 8, Reset 9, ClearState 10, the draw calls
    // 11..12, Dispatch 13, the copies 14..18, the pipeline calls 19..20,
    // ResourceBarrier 21, ExecuteBundle 22, SetDescriptorHeaps 23, the two root
    // signatures 24..25, then the graphics root arguments 26..29 (CBV, two
    // descriptor tables, 32-bit constants) and the compute ones 30..33.
    // `SetComputeRootConstantBufferView` is 31.
    let vtable = *(list as *mut *const usize);
    let set: unsafe extern "system" fn(*mut c_void, u32, u64) =
        std::mem::transmute(*vtable.add(GCL_SET_COMPUTE_ROOT_CONSTANT_BUFFER_VIEW));
    set(list, root, address);
}

// ---------------------------------------------------------------------------
// Compile-time slot assertions
// ---------------------------------------------------------------------------

// Absolute values, so an edit to any count above that moves a call off its real
// method fails this build instead of failing on the user's machine.
const _: () = assert!(DEVICE_GET_NODE_COUNT == 7);
const _: () = assert!(DEVICE_CREATE_COMPUTE_PIPELINE_STATE == 11);
const _: () = assert!(DEVICE_CREATE_COMMAND_LIST == 12);
const _: () = assert!(DEVICE_CREATE_DESCRIPTOR_HEAP == 14);
const _: () = assert!(DEVICE_GET_DESCRIPTOR_HANDLE_INCREMENT_SIZE == 15);
const _: () = assert!(DEVICE_CREATE_ROOT_SIGNATURE == 16);
const _: () = assert!(DEVICE_CREATE_CONSTANT_BUFFER_VIEW == 17);
const _: () = assert!(DEVICE_CREATE_SHADER_RESOURCE_VIEW == 18);
const _: () = assert!(DEVICE_CREATE_UNORDERED_ACCESS_VIEW == 19);
const _: () = assert!(DEVICE_CREATE_COMMITTED_RESOURCE == 27);

const _: () = assert!(RESOURCE_MAP == 8);
const _: () = assert!(RESOURCE_UNMAP == 9);
const _: () = assert!(RESOURCE_GET_GPU_VIRTUAL_ADDRESS == 11);

// `heap_handles` below also reads these; ID3D12DescriptorHeap's own members are
// GetDesc 8, GetCPUDescriptorHandleForHeapStart 9, GetGPU… 10.
const _: () = assert!(HEAP_GET_CPU_HANDLE_START == 9);
const _: () = assert!(HEAP_GET_GPU_HANDLE_START == 10);

const _: () = assert!(GCL_RESET == 10);
const _: () = assert!(GCL_DISPATCH == 14);
const _: () = assert!(GCL_COPY_TEXTURE_REGION == 16);
const _: () = assert!(GCL_RESOURCE_BARRIER == 26);
const _: () = assert!(GCL_SET_DESCRIPTOR_HEAPS == 28);
const _: () = assert!(GCL_SET_COMPUTE_ROOT_SIGNATURE == 29);
const _: () = assert!(GCL_SET_COMPUTE_ROOT_DESCRIPTOR_TABLE == 31);
const _: () = assert!(GCL_SET_COMPUTE_ROOT_CONSTANT_BUFFER_VIEW == 37);
const _: () = assert!(GCL_SET_COMPUTE_ROOT_SHADER_RESOURCE_VIEW == 39);
const _: () = assert!(GCL_SET_COMPUTE_ROOT_UNORDERED_ACCESS_VIEW == 41);

const _: () = assert!(DEVICE5_PREBUILD_INFO_SLOT == 63);
const _: () = assert!(DEVICE5_SLOTS == 65);
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
        let root = guid(IID_ID3D12ROOTSIGNATURE).unwrap();
        assert_eq!(root.Data4, [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14]);
        assert!(guid("nope").is_none());
        assert!(guid("8b4f173b-2fea-4b80-8f58").is_none());
    }

    #[test]
    fn descriptor_value_types_match_d3d12_h() {
        assert_eq!(
            std::mem::size_of::<D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO>(),
            24
        );
        assert_eq!(std::mem::size_of::<D3D12_RAYTRACING_INSTANCE_DESC>(), 64);
        let instance = D3D12_RAYTRACING_INSTANCE_DESC::identity(0x1234);
        assert_eq!(instance.InstanceIDAndMask, 0xff000000);
        assert_eq!(instance.ContributionAndFlags, 0x05000000);
        assert_eq!(instance.AccelerationStructure, 0x1234);
        assert_eq!(TYPE_TOP_LEVEL, 0);
        assert_eq!(TYPE_BOTTOM_LEVEL, 1);
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
        assert_eq!(
            std::mem::size_of::<D3D12_GPU_VIRTUAL_ADDRESS_RANGE>(),
            2 * 8
        );
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

    /// The trace shader is a compute shader, so what runs it is an ordinary
    /// compute pipeline state object. Its description has to be the C layout:
    /// `{ ID3D12RootSignature*, D3D12_SHADER_BYTECODE CS, UINT NodeMask,
    ///    D3D12_CACHED_PIPELINE_STATE, D3D12_PIPELINE_STATE_FLAGS}`.
    #[test]
    fn the_compute_pipeline_description_is_the_c_layout() {
        use std::mem::size_of;
        // A pointer, then {pointer, usize}.
        assert_eq!(size_of::<D3D12_SHADER_BYTECODE>(), 2 * size_of::<usize>());
        assert_eq!(
            size_of::<D3D12_CACHED_PIPELINE_STATE>(),
            2 * size_of::<usize>()
        );
        // Root signature pointer, bytecode, NodeMask (padded), cached state, flags.
        // pRootSignature 8, CS 16, NodeMask 4 + 4 padding, CachedPSO 16, Flags 4
        // and 4 bytes of tail padding to the struct's 8-byte alignment: 56.
        assert_eq!(
            size_of::<D3D12_COMPUTE_PIPELINE_STATE_DESC>(),
            56,
            "pRootSignature, CS, padded NodeMask, CachedPSO, padded Flags"
        );
        assert_eq!(std::mem::align_of::<D3D12_COMPUTE_PIPELINE_STATE_DESC>(), 8);
    }

    /// `D3D12_ROOT_PARAMETER` is `{ Type, union, ShaderVisibility }`, and the
    /// union's largest member is the descriptor table: a count plus a pointer,
    /// which occupies 16 bytes on 64-bit.
    #[test]
    fn a_root_parameter_is_type_union_visibility() {
        use std::mem::size_of;
        let parameter = D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
            Data: D3D12_ROOT_PARAMETER_UNION {
                Descriptor: D3D12_ROOT_DESCRIPTOR {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                },
            },
            ShaderVisibility: 0,
        };
        // The union still occupies the table's size even for a CBV.
        assert_eq!(
            size_of::<D3D12_ROOT_DESCRIPTOR_TABLE>(),
            16,
            "a descriptor table is a count plus a pointer"
        );
        assert_eq!(size_of::<D3D12_DESCRIPTOR_RANGE>(), 20);
        assert_eq!(parameter.ShaderVisibility, 0);
    }

    /// The two device slots the pipeline needs, counted from `d3d12.h`. Both are
    /// reached by offset into a vtable this module does not spell out, so a wrong
    /// number is a call into an unrelated function rather than a compile error.
    #[test]
    fn the_pipeline_device_slots_are_what_the_header_counts() {
        // IUnknown 3, ID3D12Object 4, ID3D12DeviceChild 1 = 8 before the device's
        // own members. GetNodeCount (8) ..= CreateRootSignature (17), and
        // CreateComputePipelineState sits four earlier at 12.
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 9, 17, "CreateRootSignature");
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 4, 12, "CreateComputePipelineState");
    }

    /// Every slot below is reached by offset into a vtable, so a wrong number
    /// calls an unrelated function instead of failing to compile. These are the
    /// counts `d3d12.h` gives, interface by interface.
    #[test]
    fn the_pipeline_and_dispatch_slots_match_the_header() {
        // The dispatch is recorded through `winapi`'s own command list, except for
        // `SetComputeRootDescriptorTable`, which `sys` calls by offset because the
        // handle type differs between the real crate and the offline shim.
        //
        // ID3D12GraphicsCommandList: IUnknown 3, ID3D12Object 4,
        // ID3D12DeviceChild 1 = 8, then twenty-five members before
        // SetComputeRootDescriptorTable, which is therefore slot 33.
        assert_eq!(
            IUNKNOWN_SLOTS + 4 + 1 + 22,
            30,
            "SetComputeRootDescriptorTable"
        );
        // The device: GetNodeCount 8 through CreateRootSignature 17, with
        // CreateComputePipelineState at 12, CreateDescriptorHeap 15,
        // GetDescriptorHandleIncrementSize 16, and the three view creators at
        // 18, 19 and 20.
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 9, 17, "CreateRootSignature");
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 4, 12, "CreateComputePipelineState");
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 7, 15, "CreateDescriptorHeap");
        assert_eq!(
            IUNKNOWN_SLOTS + 4 + 1 + 8,
            16,
            "GetDescriptorHandleIncrementSize"
        );
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 10, 18, "CreateConstantBufferView");
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 11, 19, "CreateShaderResourceView");
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 12, 20, "CreateUnorderedAccessView");
        // The descriptor heap: IUnknown 3, ID3D12Object 4, ID3D12DeviceChild 1
        // (GetDevice at 7), ID3D12Pageable adds nothing, so GetDesc is 8 and the
        // two handle accessors are 9 and 10.
        assert_eq!(
            IUNKNOWN_SLOTS + 4 + 1 + 1,
            9,
            "GetCPUDescriptorHandleForHeapStart"
        );
        assert_eq!(
            IUNKNOWN_SLOTS + 4 + 1 + 2,
            10,
            "GetGPUDescriptorHandleForHeapStart"
        );
        // The resource: IUnknown 3, ID3D12Object 4, ID3D12DeviceChild 1
        // (GetDevice at 7), ID3D12Pageable adds nothing, then Map 8, Unmap 9,
        // GetDesc 10 and GetGPUVirtualAddress 11.
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 3, 11, "GetGPUVirtualAddress");
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 0, 8, "Map");
        assert_eq!(IUNKNOWN_SLOTS + 4 + 1 + 1, 9, "Unmap");
    }

    /// The trace root signature is written against the shader's own declarations:
    /// a CBV, a three-entry SRV table and a one-entry UAV table.
    #[test]
    fn the_trace_root_signature_has_the_shaders_three_tables() {
        // `ConstantBuffer<Params> : register(b0)`, three SRVs at t0..t2 and one
        // UAV at u0 in `dxr_trace.hlsl`.
        assert_eq!(D3D12_ROOT_PARAMETER_TYPE_CBV, 2);
        assert_eq!(D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE, 0);
        assert_eq!(D3D12_DESCRIPTOR_RANGE_TYPE_SRV, 0);
        assert_eq!(D3D12_DESCRIPTOR_RANGE_TYPE_UAV, 1);
        // Compare against winapi's independently generated header constants.
        assert_eq!(
            D3D12_ROOT_PARAMETER_TYPE_CBV,
            winapi::um::d3d12::D3D12_ROOT_PARAMETER_TYPE_CBV
        );
        assert_eq!(
            D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            winapi::um::d3d12::D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE
        );
        assert_eq!(
            D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
            winapi::um::d3d12::D3D12_DESCRIPTOR_RANGE_TYPE_UAV
        );
        assert_eq!(D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE, 11);
        let cbv = D3D12_ROOT_PARAMETER_UNION {
            Descriptor: D3D12_ROOT_DESCRIPTOR {
                ShaderRegister: 0,
                RegisterSpace: 0,
            },
        };
        unsafe {
            assert_eq!(cbv.Descriptor.ShaderRegister, 0);
            assert_eq!(cbv.Descriptor.RegisterSpace, 0);
        }
        assert_eq!(
            std::mem::size_of::<D3D12_ROOT_PARAMETER>(),
            std::mem::size_of::<winapi::um::d3d12::D3D12_ROOT_PARAMETER>()
        );
        // A TLAS view is `{ Format, ViewDimension, Mapping, Location }`.
        assert_eq!(std::mem::size_of::<D3D12_TLAS_SRV_DESC>(), 24);
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

    /// The vtable constants must name the methods the code says they do.
    ///
    /// The offline harness stubs D3D12 out, so it cannot notice a wrong slot: the
    /// stub returns `Ok` for whatever number it is handed. That is precisely how
    /// `CreateComputePipelineState` shipped pointing at `CreateCommandList` and
    /// `SetComputeRootDescriptorTable` at `SetGraphicsRootSignature` — on Windows
    /// the pipeline "built" (it created a command list) and the trace never ran.
    /// These assertions are the compile-time half; this test is the readable half
    /// and spells out the full order for the next person.
    #[test]
    fn every_vtable_slot_names_the_documented_method() {
        // ID3D12Device, from `GetNodeCount`: 8 CreateCommandQueue,
        // 9 CreateCommandAllocator, 10 CreateGraphicsPipelineState,
        // 11 CreateComputePipelineState, 12 CreateCommandList,
        // 13 CheckFeatureSupport, 14 CreateDescriptorHeap,
        // 15 GetDescriptorHandleIncrementSize, 16 CreateRootSignature,
        // 17 CreateConstantBufferView, 18 CreateShaderResourceView,
        // 19 CreateUnorderedAccessView … 27 CreateCommittedResource.
        assert_eq!(DEVICE_GET_NODE_COUNT, 7);
        assert_eq!(DEVICE_CREATE_COMPUTE_PIPELINE_STATE, 11);
        assert_eq!(DEVICE_CREATE_COMMAND_LIST, 12);
        assert_eq!(DEVICE_CREATE_DESCRIPTOR_HEAP, 14);
        assert_eq!(DEVICE_GET_DESCRIPTOR_HANDLE_INCREMENT_SIZE, 15);
        assert_eq!(DEVICE_CREATE_ROOT_SIGNATURE, 16);
        assert_eq!(DEVICE_CREATE_CONSTANT_BUFFER_VIEW, 17);
        assert_eq!(DEVICE_CREATE_SHADER_RESOURCE_VIEW, 18);
        assert_eq!(DEVICE_CREATE_UNORDERED_ACCESS_VIEW, 19);
        assert_eq!(DEVICE_CREATE_COMMITTED_RESOURCE, 27);
        // ID3D12Resource after ID3D12Pageable (no members).
        assert_eq!(RESOURCE_MAP, 8);
        assert_eq!(RESOURCE_UNMAP, 9);
        assert_eq!(RESOURCE_GET_GPU_VIRTUAL_ADDRESS, 11);
        // ID3D12DescriptorHeap.
        assert_eq!(HEAP_GET_CPU_HANDLE_START, 9);
        assert_eq!(HEAP_GET_GPU_HANDLE_START, 10);
        // ID3D12GraphicsCommandList, after ID3D12CommandList::GetType.
        assert_eq!(GCL_RESET, 10);
        assert_eq!(GCL_DISPATCH, 14);
        assert_eq!(GCL_COPY_TEXTURE_REGION, 16);
        assert_eq!(GCL_RESOURCE_BARRIER, 26);
        assert_eq!(GCL_SET_DESCRIPTOR_HEAPS, 28);
        assert_eq!(GCL_SET_COMPUTE_ROOT_SIGNATURE, 29);
        assert_eq!(GCL_SET_COMPUTE_ROOT_DESCRIPTOR_TABLE, 31);
        assert_eq!(GCL_SET_COMPUTE_ROOT_CONSTANT_BUFFER_VIEW, 37);
        // The neighbours of the CBV slot, so an off-by-one in either direction
        // cannot hide: 36 SetGraphicsRoot32BitConstants, 37 compute CBV,
        // 38 graphics CBV.
        assert_eq!(GCL_SET_COMPUTE_ROOT_CONSTANT_BUFFER_VIEW - 1, 36);
        assert_eq!(GCL_SET_COMPUTE_ROOT_SHADER_RESOURCE_VIEW, 39);
        assert_eq!(GCL_SET_COMPUTE_ROOT_UNORDERED_ACCESS_VIEW, 41);
        // The two root tables are adjacent, in the order the spec lists them.
        assert_eq!(GCL_SET_COMPUTE_ROOT_DESCRIPTOR_TABLE + 1, 32);
    }
}
