//! Offline stand-in for the `winapi` crate: only the members of
//! `winapi::um::d3d12` that `src/imaginator/hardware/mod.rs` calls, plus the
//! `winerror`/`guiddef`/`unknwnbase` pieces its declarations need.
//!
//! This is a harness for `tools/imaginator-check.sh`, not part of the build.
//! The real crate (and the real `d3d12` wrapper) is what ships.
#![allow(non_snake_case, non_camel_case_types, dead_code, unused_imports)]

pub mod shared {
    pub mod minwindef {
        pub type UINT = u32;
        pub type BOOL = i32;
        pub type DWORD = u32;
    }
    pub mod winerror {
        pub type HRESULT = i32;
        pub const E_POINTER: HRESULT = 0x8000_4003_u32 as i32;
        pub const S_OK: HRESULT = 0;
        pub fn SUCCEEDED(hr: i32) -> bool {
            hr >= 0
        }
    }
    pub mod guiddef {
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct GUID {
            pub Data1: u32,
            pub Data2: u16,
            pub Data3: u16,
            pub Data4: [u8; 8],
        }
    }
    pub mod dxgitype {
        /// `DXGI_SAMPLE_DESC` lives here in the real crate and is what
        /// `D3D12_RESOURCE_DESC::SampleDesc` is typed as.
        #[repr(C)]
        pub struct DXGI_SAMPLE_DESC {
            pub Count: u32,
            pub Quality: u32,
        }
    }
}
pub use shared::minwindef::UINT;

pub mod um {
    pub mod unknwnbase {
        use crate::shared::{guiddef::GUID, winerror::HRESULT};
        use std::ffi::c_void;
        #[repr(C)]
        pub struct IUnknownVtbl {
            pub QueryInterface:
                unsafe extern "system" fn(*mut IUnknown, *const GUID, *mut *mut c_void) -> HRESULT,
            pub AddRef: unsafe extern "system" fn(*mut IUnknown) -> u32,
            pub Release: unsafe extern "system" fn(*mut IUnknown) -> u32,
        }
        #[repr(C)]
        pub struct IUnknown {
            pub lpVtbl: *mut IUnknownVtbl,
        }
    }

    pub mod d3d12 {
        use crate::shared::{guiddef::GUID, winerror::HRESULT};
        use std::ffi::c_void;

        pub use crate::shared::dxgitype::DXGI_SAMPLE_DESC;

        pub type D3D12_GPU_VIRTUAL_ADDRESS = u64;
        pub type D3D12_FEATURE = u32;
        pub type DXGI_FORMAT = u32;
        pub type D3D12_RESOURCE_STATES = u32;
        pub type D3D12_RESOURCE_FLAGS = u32;
        pub type D3D12_RESOURCE_BARRIER_TYPE = u32;
        pub type D3D12_RESOURCE_BARRIER_FLAGS = u32;
        pub type D3D12_RESOURCE_DIMENSION = u32;
        pub type D3D12_TEXTURE_LAYOUT = u32;
        pub type D3D12_HEAP_TYPE = u32;
        pub type D3D12_CPU_PAGE_PROPERTY = u32;
        pub type D3D12_MEMORY_POOL = u32;

        pub const D3D12_HEAP_TYPE_DEFAULT: D3D12_HEAP_TYPE = 1;
        pub const D3D12_HEAP_TYPE_UPLOAD: D3D12_HEAP_TYPE = 2;
        pub const D3D12_CPU_PAGE_PROPERTY_UNKNOWN: D3D12_CPU_PAGE_PROPERTY = 0;
        pub const D3D12_MEMORY_POOL_UNKNOWN: D3D12_MEMORY_POOL = 0;
        pub const D3D12_HEAP_FLAG_NONE: u32 = 0;
        pub const D3D12_RESOURCE_DIMENSION_BUFFER: D3D12_RESOURCE_DIMENSION = 1;
        pub const D3D12_TEXTURE_LAYOUT_ROW_MAJOR: D3D12_TEXTURE_LAYOUT = 1;
        pub const D3D12_RESOURCE_FLAG_NONE: D3D12_RESOURCE_FLAGS = 0;
        pub const D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS: D3D12_RESOURCE_FLAGS = 0x4;
        pub const D3D12_RESOURCE_STATE_COMMON: D3D12_RESOURCE_STATES = 0;
        pub const D3D12_RESOURCE_STATE_COPY_DEST: D3D12_RESOURCE_STATES = 0x400;
        pub const D3D12_RESOURCE_BARRIER_TYPE_TRANSITION: D3D12_RESOURCE_BARRIER_TYPE = 0;
        pub const D3D12_RESOURCE_BARRIER_FLAG_NONE: D3D12_RESOURCE_BARRIER_FLAGS = 0;
        pub const D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES: u32 = 0xffff_ffff;
        pub const DXGI_FORMAT_UNKNOWN: DXGI_FORMAT = 0;

        #[repr(C)]
        pub struct D3D12_RANGE {
            pub Begin: usize,
            pub End: usize,
        }

        #[repr(C)]
        pub struct D3D12_HEAP_PROPERTIES {
            pub Type: D3D12_HEAP_TYPE,
            pub CPUPageProperty: D3D12_CPU_PAGE_PROPERTY,
            pub MemoryPoolPreference: D3D12_MEMORY_POOL,
            pub CreationNodeMask: u32,
            pub VisibleNodeMask: u32,
        }
        #[repr(C)]
        pub struct D3D12_RESOURCE_DESC {
            pub Dimension: D3D12_RESOURCE_DIMENSION,
            pub Alignment: u64,
            pub Width: u64,
            pub Height: u32,
            pub DepthOrArraySize: u16,
            pub MipLevels: u16,
            pub Format: DXGI_FORMAT,
            pub SampleDesc: crate::shared::dxgitype::DXGI_SAMPLE_DESC,
            pub Layout: D3D12_TEXTURE_LAYOUT,
            pub Flags: D3D12_RESOURCE_FLAGS,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct D3D12_RESOURCE_TRANSITION_BARRIER {
            pub pResource: *mut ID3D12Resource,
            pub Subresource: u32,
            pub StateBefore: D3D12_RESOURCE_STATES,
            pub StateAfter: D3D12_RESOURCE_STATES,
        }
        #[repr(C)]
        pub union D3D12_RESOURCE_BARRIER_u {
            pub Transition: std::mem::ManuallyDrop<D3D12_RESOURCE_TRANSITION_BARRIER>,
            _pad: [u8; 32],
        }
        impl D3D12_RESOURCE_BARRIER_u {
            pub fn Transition_mut(&mut self) -> &mut D3D12_RESOURCE_TRANSITION_BARRIER {
                unsafe { &mut self.Transition }
            }
        }
        #[repr(C)]
        pub struct D3D12_RESOURCE_BARRIER {
            pub Type: D3D12_RESOURCE_BARRIER_TYPE,
            pub Flags: D3D12_RESOURCE_BARRIER_FLAGS,
            pub u: D3D12_RESOURCE_BARRIER_u,
        }

        #[repr(C)]
        pub struct D3D12_QUEUE_VTBL {
            pub ExecuteCommandLists: unsafe extern "system" fn(
                *mut ID3D12CommandQueue,
                u32,
                *const *mut ID3D12CommandList,
            ),
        }
        #[repr(C)]
        pub struct D3D12_FENCE_VTBL {
            pub GetCompletedValue: unsafe extern "system" fn(*mut ID3D12Fence) -> u64,
        }
        #[repr(C)]
        pub struct D3D12_GRAPHICS_COMMAND_LIST_VTBL {
            pub Reset: unsafe extern "system" fn(
                *mut ID3D12GraphicsCommandList,
                *mut ID3D12CommandAllocator,
                *mut ID3D12PipelineState,
            ) -> HRESULT,
            pub Close: unsafe extern "system" fn(*mut ID3D12GraphicsCommandList) -> HRESULT,
            pub ResourceBarrier: unsafe extern "system" fn(
                *mut ID3D12GraphicsCommandList,
                u32,
                *const D3D12_RESOURCE_BARRIER,
            ),
            pub CopyBufferRegion: unsafe extern "system" fn(
                *mut ID3D12GraphicsCommandList,
                *mut ID3D12Resource,
                u64,
                *mut ID3D12Resource,
                u64,
                u64,
            ),
        }

        #[repr(C)]
        pub struct ID3D12Resource {
            pub lpVtbl: *mut c_void,
        }
        impl ID3D12Resource {
            pub fn uuidof() -> GUID {
                GUID {
                    Data1: 0x696442be,
                    Data2: 0xa72e,
                    Data3: 0x4059,
                    Data4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad],
                }
            }
        }
        #[repr(C)]
        pub struct ID3D12CommandList {
            pub lpVtbl: *mut c_void,
        }
        #[repr(C)]
        pub struct ID3D12GraphicsCommandList {
            pub lpVtbl: *mut D3D12_GRAPHICS_COMMAND_LIST_VTBL,
        }
        impl ID3D12GraphicsCommandList {
            pub fn Reset(
                &self,
                allocator: *mut ID3D12CommandAllocator,
                pso: *mut ID3D12PipelineState,
            ) -> HRESULT {
                unsafe { ((*self.lpVtbl).Reset)(self as *const _ as *mut _, allocator, pso) }
            }
            pub fn Close(&self) -> HRESULT {
                unsafe { ((*self.lpVtbl).Close)(self as *const _ as *mut _) }
            }
            pub fn ResourceBarrier(&self, count: u32, barriers: *const D3D12_RESOURCE_BARRIER) {
                unsafe {
                    ((*self.lpVtbl).ResourceBarrier)(self as *const _ as *mut _, count, barriers)
                }
            }
            pub fn CopyBufferRegion(
                &self,
                dst: *mut ID3D12Resource,
                dst_offset: u64,
                src: *mut ID3D12Resource,
                src_offset: u64,
                bytes: u64,
            ) {
                unsafe {
                    ((*self.lpVtbl).CopyBufferRegion)(
                        self as *const _ as *mut _,
                        dst,
                        dst_offset,
                        src,
                        src_offset,
                        bytes,
                    )
                }
            }
        }
        #[repr(C)]
        pub struct ID3D12CommandQueue {
            pub lpVtbl: *mut D3D12_QUEUE_VTBL,
        }
        impl ID3D12CommandQueue {
            pub fn ExecuteCommandLists(&self, count: u32, lists: *const *mut ID3D12CommandList) {
                unsafe {
                    ((*self.lpVtbl).ExecuteCommandLists)(self as *const _ as *mut _, count, lists)
                }
            }
        }
        #[repr(C)]
        pub struct ID3D12CommandAllocator {
            pub lpVtbl: *mut c_void,
        }
        #[repr(C)]
        pub struct ID3D12Device {
            pub lpVtbl: *mut c_void,
        }
        #[repr(C)]
        pub struct ID3D12PipelineState {
            pub lpVtbl: *mut c_void,
        }
        #[repr(C)]
        pub struct ID3D12Fence {
            pub lpVtbl: *mut D3D12_FENCE_VTBL,
        }
        impl ID3D12Fence {
            pub fn GetCompletedValue(&self) -> u64 {
                unsafe { ((*self.lpVtbl).GetCompletedValue)(self as *const _ as *mut _) }
            }
        }
    }
}
