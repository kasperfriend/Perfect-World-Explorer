//! Offline stand-in for the `d3d12` crate (v0.19): `ComPtr`, the device/fence
//! constructors and the resource helpers `src/imaginator/hardware/mod.rs` uses.
//!
//! Harness for `tools/imaginator-check.sh`; the real crate ships.
#![allow(non_snake_case, non_camel_case_types, dead_code, unused_imports)]

use std::ffi::c_void;
use winapi::shared::guiddef::GUID;
use winapi::shared::winerror::HRESULT;
use winapi::um::d3d12 as raw;

pub trait Interface {}

impl Interface for raw::ID3D12Device {}
impl Interface for raw::ID3D12CommandQueue {}
impl Interface for raw::ID3D12CommandAllocator {}
impl Interface for raw::ID3D12PipelineState {}
impl Interface for raw::ID3D12Resource {}
impl Interface for raw::ID3D12GraphicsCommandList {}
impl Interface for raw::ID3D12Fence {}

/// The real `ComPtr` is `#[repr(transparent)]` over the raw pointer and adopts a
/// reference on `from_raw`; that transparency is what lets `hardware/mod.rs`
/// reclaim a resource from `CreateCommittedResource`'s out-parameter.
#[repr(transparent)]
pub struct ComPtr<T: Interface>(*mut T);


impl<T: Interface> ComPtr<T> {
    pub fn null() -> Self {
        ComPtr(std::ptr::null_mut())
    }
    pub fn as_ptr(&self) -> *const T {
        self.0
    }
    pub fn as_mut_ptr(&self) -> *mut T {
        self.0
    }
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

impl<T: Interface> std::ops::Deref for ComPtr<T> {
    type Target = T;
    /// Same as the real `ComPtr`: deref panics on null and yields the interface
    /// reference, which is how every `(*ptr).Method(...)` call in the module
    /// reaches its vtable.
    fn deref(&self) -> &T {
        assert!(!self.0.is_null(), "null COM pointer");
        unsafe { &*self.0 }
    }
}

impl<T: Interface> Clone for ComPtr<T> {
    fn clone(&self) -> Self {
        ComPtr(self.0)
    }
}

pub type D3DResult<T> = (T, HRESULT);

#[derive(Clone, Copy)]
pub enum CmdListType {
    Direct,
    Bundle,
    Compute,
    VideoDecode,
}

pub type Device = ComPtr<raw::ID3D12Device>;
pub type CommandQueue = ComPtr<raw::ID3D12CommandQueue>;
pub type CommandAllocator = ComPtr<raw::ID3D12CommandAllocator>;
pub type PipelineState = ComPtr<raw::ID3D12PipelineState>;
pub type Resource = ComPtr<raw::ID3D12Resource>;
pub type GraphicsCommandList = ComPtr<raw::ID3D12GraphicsCommandList>;
pub type Fence = ComPtr<raw::ID3D12Fence>;

impl Device {
    pub fn create_command_allocator(&self, _list_type: CmdListType) -> D3DResult<CommandAllocator> {
        (CommandAllocator::null(), 0)
    }
    pub fn create_graphics_command_list(
        &self,
        _list_type: CmdListType,
        _allocator: &CommandAllocator,
        _pso: PipelineState,
        _flags: u32,
    ) -> D3DResult<GraphicsCommandList> {
        (GraphicsCommandList::null(), 0)
    }
    pub fn create_fence(&self, _initial: u64) -> D3DResult<Fence> {
        (Fence::null(), 0)
    }
    pub fn CheckFeatureSupport(&self, _feature: u32, _data: *mut c_void, _size: u32) -> HRESULT {
        0
    }
    pub fn CreateCommittedResource(
        &self,
        _heap: &raw::D3D12_HEAP_PROPERTIES,
        _heap_flags: u32,
        _desc: &raw::D3D12_RESOURCE_DESC,
        _initial_state: raw::D3D12_RESOURCE_STATES,
        _optimized: *const c_void,
        _iid: *const GUID,
        _out: *mut *mut c_void,
    ) -> HRESULT {
        0
    }
}

impl CommandQueue {
    pub fn signal(&self, _fence: &Fence, _value: u64) -> HRESULT {
        0
    }
}

impl Resource {
    pub fn gpu_virtual_address(&self) -> u64 {
        0
    }
    pub fn Map(&self, _subresource: u32, _range: *const c_void, _out: *mut *mut c_void) -> HRESULT {
        0
    }
    pub fn Unmap(&self, _subresource: u32, _range: *const c_void) {}
}
