//! The hardware tracer: DXR acceleration structures on wgpu's own D3D12 device.
//!
//! ## What it does
//!
//! 1. **Probe.** The device wgpu created is asked, through
//!    `CheckFeatureSupport(D3D12_OPTIONS5)`, for its `RaytracingTier`. Anything
//!    below tier 1.1 — the tier that makes inline `RayQuery` available in a
//!    compute shader — is reported as unavailable, with the tier named.
//! 2. **Build.** The module's triangle soup (the same one the CPU tracer walks)
//!    is uploaded into committed buffers and built into a **BLAS** with
//!    `BuildRaytracingAccelerationStructure`, then wrapped in a **TLAS** over one
//!    identity instance. What the shader then walks is the driver's traversal
//!    structure, evaluated by the GPU's ray-tracing units.
//! 3. **Trace.** A compute shader using `RayQuery` does the primary hit, the
//!    shadow rays, the reflection ray and the indirect bounces: the same terms
//!    the compute tracer walks by hand, evaluated by fixed-function traversal.
//!
//! ## What has to be true for step 3 to run
//!
//! `RayQuery` needs **DXIL for shader model 6.5, built by DXC**. The project
//! vendors no DXC and cannot add one as a Rust dependency, so the blob is built
//! out of tree by `tools/imaginator-shaders.sh` and loaded from
//! `imaginator/dxr_trace.cso` beside the module. Everything before that point is
//! live without a shader: the probe and the acceleration-structure build both
//! run. When the blob is missing, `status()` says exactly that — it
//! distinguishes "no DXR tier", "no Device5", "no build" and "no DXIL" — and the
//! compute tracer keeps producing the image. The module never claims hardware
//! traversal while it is walking the BVH in software.

use super::sys;
use crate::imaginator::bvh::{Bvh, SharedScene};
use d3d12::{CmdListType, CommandQueue, Device, Resource};
use std::ffi::c_void;
use std::sync::Arc;
use winapi::shared::dxgiformat::DXGI_FORMAT_UNKNOWN;
use winapi::shared::dxgitype::DXGI_SAMPLE_DESC;
use winapi::shared::guiddef::GUID;
use winapi::shared::winerror::SUCCEEDED;
use winapi::um::d3d12 as win;
// `ID3D12Resource::uuidof()` is a method of `winapi::Interface`, not an inherent
// one, exactly as `fsr::imp` imports it for its own `CreateCommittedResource`.
use winapi::Interface;

/// Bytes per triangle record the BLAS is built from: three `f32x3` positions.
pub const GEOMETRY_STRIDE: u64 = 36;

/// The DXIL blob's location, relative to the module's own folder.
pub const DXIL_FILE: &str = "imaginator/dxr_trace.cso";

/// Why the hardware tracer is not running, or what it is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HardwareStatus {
    /// Nothing has been probed yet.
    Untried,
    /// The device and the build path work; the DXIL blob is still missing.
    Ready(String),
    /// The device (or its driver) cannot do DXR at the required tier.
    Unsupported(String),
    /// A build step failed; the string is the failure, not a guess.
    Failed(String),
}

impl HardwareStatus {
    pub fn available(&self) -> bool {
        matches!(self, HardwareStatus::Ready(_))
    }

    pub fn label(&self) -> String {
        match self {
            HardwareStatus::Untried => "not probed yet".into(),
            HardwareStatus::Ready(note) => format!("acceleration structures ready: {note}"),
            HardwareStatus::Unsupported(why) => format!("unavailable: {why}"),
            HardwareStatus::Failed(why) => format!("failed: {why}"),
        }
    }
}

/// Owned acceleration-structure memory. The buffers have to outlive every view
/// into them, so they live and die together.
struct Structures {
    _geometry: Resource,
    _instances: Resource,
    _scratch: Resource,
    _blas: Resource,
    _tlas: Resource,
    tlas_address: sys::GpuAddress,
    triangles: usize,
}

pub struct HardwareTracer {
    /// `ID3D12Device5`, owned. Queried once at probe time.
    device5: *mut c_void,
    /// `ID3D12GraphicsCommandList4` used only for builds, owned.
    list4: *mut c_void,
    /// Its allocator; a command list cannot be reset without one.
    allocator: d3d12::CommandAllocator,
    device: Device,
    queue: CommandQueue,
    /// Released only on drop; `d3d12::ComPtr` is opaque, so this is kept as the
    /// raw pointer the builds were validated against.
    tier: u32,
    built_for: Option<SharedScene>,
    structures: Option<Structures>,
    pub status: HardwareStatus,
    /// Triangles in the live structures, for the status line.
    pub built_triangles: usize,
}

unsafe impl Send for HardwareTracer {}

impl Drop for HardwareTracer {
    fn drop(&mut self) {
        unsafe {
            sys::release(self.list4);
            sys::release(self.device5);
        }
        self.list4 = std::ptr::null_mut();
        self.device5 = std::ptr::null_mut();
    }
}

impl HardwareTracer {
    /// Probe `device` for DXR and prepare the command list the builds record on.
    ///
    /// Never fails outward: every problem becomes a [`HardwareStatus`], because
    /// this runs on the render thread and a missing feature must not abort a
    /// frame.
    pub fn probe(device: &wgpu::Device) -> Self {
        let mut tracer = Self {
            device5: std::ptr::null_mut(),
            list4: std::ptr::null_mut(),
            allocator: d3d12::CommandAllocator::null(),
            device: Device::null(),
            queue: CommandQueue::null(),
            tier: 0,
            built_for: None,
            structures: None,
            status: HardwareStatus::Untried,
            built_triangles: 0,
        };
        let Ok((raw_device, raw_queue)) = crate::fsr::raw_dx12(device) else {
            tracer.status = HardwareStatus::Unsupported(
                "this wgpu device is not D3D12; acceleration structures need the DX12 backend"
                    .into(),
            );
            return tracer;
        };
        tracer.device = raw_device;
        tracer.queue = raw_queue;
        tracer.probe_raw();
        tracer
    }

    fn probe_raw(&mut self) {
        // 1. The tier. `CheckFeatureSupport` is a plain `ID3D12Device` member,
        //    so `winapi`'s own declaration is used.
        let mut options = sys::D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
        // SAFETY: `options` is a live, correctly-typed local and the size matches
        // the feature constant being queried.
        let hr = unsafe {
            self.device.CheckFeatureSupport(
                sys::FEATURE_D3D12_OPTIONS5 as win::D3D12_FEATURE,
                &mut options as *mut _ as *mut c_void,
                std::mem::size_of::<sys::D3D12_FEATURE_DATA_D3D12_OPTIONS5>() as u32,
            )
        };
        if !SUCCEEDED(hr) {
            self.status = HardwareStatus::Unsupported(format!(
                "CheckFeatureSupport(D3D12_OPTIONS5) failed (HRESULT 0x{hr:08x}); the driver exposes no ray-tracing tier"
            ));
            return;
        }
        self.tier = options.RaytracingTier;
        if options.RaytracingTier < sys::RAYTRACING_TIER_1_1 {
            let tier = match options.RaytracingTier {
                sys::RAYTRACING_TIER_NOT_SUPPORTED => "none".to_string(),
                sys::RAYTRACING_TIER_1_0 => "1.0".to_string(),
                sys::RAYTRACING_TIER_1_1 => "1.1".to_string(),
                sys::RAYTRACING_TIER_1_2 => "1.2".to_string(),
                other => other.to_string(),
            };
            self.status = HardwareStatus::Unsupported(format!(
                "DXR tier {tier}: inline RayQuery needs tier 1.1; the software tracer covers this GPU"
            ));
            return;
        }
        // 2. Device5 and CommandList4. Neither is declared by `winapi`.
        let mut device5: *mut c_void = std::ptr::null_mut();
        let Some(iid) = sys::guid(sys::IID_ID3D12DEVICE5) else {
            self.status = HardwareStatus::Failed("the Device5 GUID literal is invalid".into());
            return;
        };
        let hr = unsafe {
            sys::query_interface(self.device.as_ptr() as *mut c_void, &iid, &mut device5)
        };
        if !SUCCEEDED(hr) || device5.is_null() {
            self.status = HardwareStatus::Unsupported(format!(
                "QueryInterface(ID3D12Device5) failed (HRESULT 0x{hr:08x})"
            ));
            return;
        }
        self.device5 = device5;
        let (allocator, hr) = self.device.create_command_allocator(CmdListType::Direct);
        if !SUCCEEDED(hr) {
            self.status = HardwareStatus::Failed(format!(
                "CreateCommandAllocator failed (HRESULT 0x{hr:08x})"
            ));
            return;
        }
        let (list, hr) = self.device.create_graphics_command_list(
            CmdListType::Direct,
            &allocator,
            d3d12::PipelineState::null(),
            0,
        );
        if !SUCCEEDED(hr) {
            self.status =
                HardwareStatus::Failed(format!("CreateCommandList failed (HRESULT 0x{hr:08x})"));
            return;
        }
        let mut list4: *mut c_void = std::ptr::null_mut();
        let Some(list_iid) = sys::guid(sys::IID_ID3D12GRAPHICSCOMMANDLIST4) else {
            self.status = HardwareStatus::Failed("the CommandList4 GUID literal is invalid".into());
            return;
        };
        let hr =
            unsafe { sys::query_interface(list.as_ptr() as *mut c_void, &list_iid, &mut list4) };
        if !SUCCEEDED(hr) || list4.is_null() {
            self.status = HardwareStatus::Unsupported(format!(
                "QueryInterface(ID3D12GraphicsCommandList4) failed (HRESULT 0x{hr:08x})"
            ));
            return;
        }
        self.list4 = list4;
        self.allocator = allocator;
        // 3. The one piece that cannot be produced in-tree.
        let note = match std::fs::read(DXIL_FILE) {
            Ok(blob) if blob.len() > 4 => format!(
                "DXR tier {} on ID3D12Device5; {DXIL_FILE} present ({} KiB), build runs on the first traced frame",
                self.tier_label(),
                blob.len() / 1024
            ),
            _ => format!(
                "DXR tier {} on ID3D12Device5; acceleration structures build on demand, but the RayQuery shader {DXIL_FILE} is missing (run tools/imaginator-shaders.sh)",
                self.tier_label()
            ),
        };
        self.status = HardwareStatus::Ready(note);
    }

    fn tier_label(&self) -> &'static str {
        match self.tier {
            sys::RAYTRACING_TIER_1_2 => "1.2",
            sys::RAYTRACING_TIER_1_1 => "1.1",
            sys::RAYTRACING_TIER_1_0 => "1.0",
            _ => "none",
        }
    }

    /// Build the BLAS and TLAS for `scene` unless the live structures are already
    /// built from exactly that scene.
    ///
    /// Returns the TLAS GPU address, which is what a RayQuery shader binds, or the
    /// reason the build did not happen.
    pub fn ensure_built(&mut self, scene: &SharedScene) -> Result<sys::GpuAddress, String> {
        if let (Some(built), Some(structures)) = (&self.built_for, &self.structures) {
            if Arc::ptr_eq(built, scene) {
                return Ok(structures.tlas_address);
            }
        }
        if self.device5.is_null() || self.list4.is_null() {
            return Err("the DXR device is not ready".into());
        }
        let Some(bvh) = scene.bvh.as_ref() else {
            return Err("the scene has no geometry".into());
        };
        match self.build(bvh) {
            Ok(structures) => {
                self.built_triangles = structures.triangles;
                let address = structures.tlas_address;
                self.structures = Some(structures);
                self.built_for = Some(scene.clone());
                Ok(address)
            }
            Err(why) => {
                self.status = HardwareStatus::Failed(why.clone());
                Err(why)
            }
        }
    }

    fn build(&mut self, bvh: &Bvh) -> Result<Structures, String> {
        if bvh.triangles.is_empty() {
            return Err("nothing to build".into());
        }
        // 1. Vertices, three f32x3 per triangle, zipped directly from the BVH.
        let mut vertices = Vec::with_capacity(bvh.triangles.len() * 9);
        for t in &bvh.triangles {
            for p in [t.a, t.b, t.c] {
                vertices.extend_from_slice(&[p.x, p.y, p.z]);
            }
        }
        let geometry = self.upload(
            (vertices.len() * 4) as u64,
            "imaginator as vertices",
            bytemuck_cast(&vertices),
        )?;
        let blas_inputs = sys::D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: sys::TYPE_BOTTOM_LEVEL,
            Flags: sys::BUILD_FLAG_PREFER_FAST_TRACE,
            NumDescs: 1,
            DescsLayout: sys::ELEMENTS_LAYOUT_ARRAY,
            Descs: sys::D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_DESCS {
                // `geometry_desc` lives on this stack frame until the build below
                // has been recorded *and executed*; `build_on_list` waits.
                pGeometryDescs: std::ptr::null(),
            },
        };
        // The geometry descriptor is filled after the upload so it can point at
        // the buffer's GPU address.
        let geometry_desc = sys::D3D12_RAYTRACING_GEOMETRY_DESC {
            Type: sys::GEOMETRY_TYPE_TRIANGLES,
            Flags: sys::GEOMETRY_FLAG_OPAQUE,
            Triangles: sys::D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                Transform3x4: 0,
                IndexFormat: DXGI_FORMAT_UNKNOWN,
                VertexFormat: sys::DXGI_FORMAT_R32G32B32_FLOAT,
                IndexCount: 0,
                VertexCount: (bvh.triangles.len() * 3) as u32,
                IndexBuffer: 0,
                VertexBuffer: sys::D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                    StartAddress: geometry.gpu_virtual_address(),
                    StrideInBytes: 12,
                },
            },
        };
        let mut blas_inputs = blas_inputs;
        blas_inputs.Descs.pGeometryDescs = &geometry_desc;
        let blas_info = self.prebuild_info(&blas_inputs)?;
        let blas = self.upload_raw(blas_info.ResultDataMaxSizeInBytes, "imaginator blas")?;
        let scratch_size = blas_info
            .ScratchDataSizeInBytes
            .max(blas_info.UpdateScratchDataSizeInBytes)
            .max(1);
        let scratch = self.upload_raw(scratch_size, "imaginator blas scratch")?;
        let blas_desc = sys::D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
            DestAccelerationStructureData: blas.gpu_virtual_address(),
            Inputs: blas_inputs,
            SourceAccelerationStructureData: 0,
            ScratchAccelerationStructureData: scratch.gpu_virtual_address(),
        };
        unsafe { self.record_and_wait(&blas_desc)? };
        // 2. One opaque instance over that BLAS.
        let instance = sys::D3D12_RAYTRACING_INSTANCE_DESC::identity(blas.gpu_virtual_address());
        let mut instance_bytes =
            vec![0u8; std::mem::size_of::<sys::D3D12_RAYTRACING_INSTANCE_DESC>()];
        // SAFETY: `instance` is a `Copy` plain-data struct of exactly this size,
        // and the destination is a byte buffer of that size.
        unsafe {
            std::ptr::copy_nonoverlapping(
                &instance as *const _ as *const u8,
                instance_bytes.as_mut_ptr(),
                instance_bytes.len(),
            );
        }
        let instances = self.upload(
            instance_bytes.len() as u64,
            "imaginator instances",
            &instance_bytes,
        )?;
        let tlas_inputs = sys::D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: sys::TYPE_TOP_LEVEL,
            Flags: sys::BUILD_FLAG_PREFER_FAST_TRACE,
            NumDescs: 1,
            DescsLayout: sys::ELEMENTS_LAYOUT_ARRAY,
            Descs: sys::D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_DESCS {
                InstanceDescs: instances.gpu_virtual_address(),
            },
        };
        let tlas_info = self.prebuild_info(&tlas_inputs)?;
        let tlas = self.upload_raw(tlas_info.ResultDataMaxSizeInBytes, "imaginator tlas")?;
        let tlas_desc = sys::D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
            DestAccelerationStructureData: tlas.gpu_virtual_address(),
            Inputs: tlas_inputs,
            SourceAccelerationStructureData: 0,
            ScratchAccelerationStructureData: scratch.gpu_virtual_address(),
        };
        unsafe { self.record_and_wait(&tlas_desc)? };
        Ok(Structures {
            _geometry: geometry,
            _instances: instances,
            _scratch: scratch,
            _blas: blas,
            tlas_address: tlas.gpu_virtual_address(),
            _tlas: tlas,
            triangles: bvh.triangles.len(),
        })
    }

    /// A default-heap buffer filled from `data` through a mapped upload heap.
    ///
    /// `wgpu` never sees these buffers: the bytes are written on the CPU into a
    /// `D3D12_HEAP_TYPE_UPLOAD` resource and copied with `CopyBufferRegion` on
    /// the module's own command list. That keeps every acceleration-structure
    /// input in this file and costs one submit at load time.
    fn upload(&mut self, bytes: u64, label: &str, data: &[u8]) -> Result<Resource, String> {
        let destination = self.create_buffer(bytes, label)?;
        if data.is_empty() {
            return Ok(destination);
        }
        let length = (data.len() as u64).min(bytes);
        let staging = self.create_buffer_on(
            win::D3D12_HEAP_TYPE_UPLOAD,
            length,
            &format!("{label} staging"),
        )?;
        // SAFETY: an upload-heap resource is persistently mapped, and the write is
        // clamped to the resource's own size above.
        unsafe {
            let mut mapped: *mut c_void = std::ptr::null_mut();
            let hr = staging.Map(0, std::ptr::null(), &mut mapped);
            if !SUCCEEDED(hr) || mapped.is_null() {
                return Err(format!("{label}: staging Map failed (HRESULT 0x{hr:08x})"));
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), mapped as *mut u8, length as usize);
            staging.Unmap(0, std::ptr::null());
        }
        self.copy_and_wait(&staging, &destination, length)?;
        Ok(destination)
    }

    /// Record one `CopyBufferRegion` with the barriers around it, execute it and
    /// wait, so the destination is back in `COMMON` and fully written.
    fn copy_and_wait(
        &mut self,
        source: &Resource,
        destination: &Resource,
        bytes: u64,
    ) -> Result<(), String> {
        let list = self.list4 as *mut win::ID3D12GraphicsCommandList;
        // SAFETY: `list` is the module's own live command list and both resources
        // are buffers on the same device.
        unsafe {
            let hr = (*list).Reset(self.allocator.as_mut_ptr(), std::ptr::null_mut());
            if !SUCCEEDED(hr) {
                return Err(format!("command list Reset failed (HRESULT 0x{hr:08x})"));
            }
            let to_copy = transition_barrier(
                destination,
                win::D3D12_RESOURCE_STATE_COMMON,
                win::D3D12_RESOURCE_STATE_COPY_DEST,
            );
            (*list).ResourceBarrier(1, &to_copy);
            (*list).CopyBufferRegion(destination.as_mut_ptr(), 0, source.as_mut_ptr(), 0, bytes);
            let back = transition_barrier(
                destination,
                win::D3D12_RESOURCE_STATE_COPY_DEST,
                win::D3D12_RESOURCE_STATE_COMMON,
            );
            (*list).ResourceBarrier(1, &back);
            let hr = (*list).Close();
            if !SUCCEEDED(hr) {
                return Err(format!("command list Close failed (HRESULT 0x{hr:08x})"));
            }
        }
        let lists: [*mut win::ID3D12CommandList; 1] = [self.list4 as *mut _];
        // SAFETY: the queue is wgpu's own direct queue and the list is closed.
        unsafe { (*self.queue.as_ptr()).ExecuteCommandLists(1, lists.as_ptr()) };
        self.wait_for_build()
    }

    fn upload_raw(&self, bytes: u64, label: &str) -> Result<Resource, String> {
        self.create_buffer(bytes, label)
    }

    fn create_buffer(&self, bytes: u64, label: &str) -> Result<Resource, String> {
        self.create_buffer_on(win::D3D12_HEAP_TYPE_DEFAULT, bytes, label)
    }

    /// A committed buffer on `heap_type`. Acceleration-structure buffers are
    /// plain default-heap resources in `COMMON`; the build call is what gives
    /// them meaning.
    fn create_buffer_on(
        &self,
        heap_type: u32,
        bytes: u64,
        label: &str,
    ) -> Result<Resource, String> {
        let heap = win::D3D12_HEAP_PROPERTIES {
            Type: heap_type,
            CPUPageProperty: win::D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: win::D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let desc = win::D3D12_RESOURCE_DESC {
            Dimension: win::D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: bytes.max(1),
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: win::D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: win::D3D12_RESOURCE_FLAG_NONE,
        };
        let mut resource: *mut win::ID3D12Resource = std::ptr::null_mut();
        // SAFETY: `heap`/`desc` outlive the call, the IID is `ID3D12Resource`'s and
        // the out-parameter is a live local of exactly that type. Mirrors
        // `fsr::imp`'s call.
        let hr = unsafe {
            self.device.CreateCommittedResource(
                &heap,
                win::D3D12_HEAP_FLAG_NONE,
                &desc,
                win::D3D12_RESOURCE_STATE_COMMON,
                std::ptr::null(),
                &win::ID3D12Resource::uuidof(),
                &mut resource as *mut _ as *mut *mut c_void,
            )
        };
        if !SUCCEEDED(hr) || resource.is_null() {
            return Err(format!(
                "CreateCommittedResource({label}, {bytes} bytes) failed (HRESULT 0x{hr:08x})"
            ));
        }
        // Adopt the reference the call handed us; `Resource` is transparent over
        // the raw pointer, exactly as `fsr::imp` does for its texture imports.
        Ok(unsafe { std::mem::transmute::<*mut win::ID3D12Resource, Resource>(resource) })
    }

    fn prebuild_info(
        &self,
        inputs: &sys::D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS,
    ) -> Result<sys::D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO, String> {
        if self.device5.is_null() {
            return Err("the DXR device is not ready".into());
        }
        let device5 = self.device5 as *mut sys::ID3D12Device5;
        // SAFETY: `device5` is a live `ID3D12Device5`; the vtable field this
        // reads is the one the compile-time assertion in `sys` pins to slot 68.
        let vtbl = unsafe { (*device5).lpVtbl.as_ref() }.ok_or("the Device5 vtable is gone")?;
        let mut info = sys::D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
        unsafe {
            (vtbl.GetRaytracingAccelerationStructurePrebuildInfo)(self.device5, inputs, &mut info);
        }
        if info.ResultDataMaxSizeInBytes == 0 {
            return Err("the driver reported a zero-byte acceleration structure".into());
        }
        Ok(info)
    }

    /// Record one build on the module's own command list and wait for it.
    ///
    /// # Safety
    /// `self.list4` must be a live `ID3D12GraphicsCommandList4` on the same device
    /// as the resources the descriptor points at, and the descriptor's scratch and
    /// destination buffers must still be alive when this returns (they are: every
    /// caller holds them across the call).
    unsafe fn record_and_wait(
        &mut self,
        desc: &sys::D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC,
    ) -> Result<(), String> {
        let list4 = self.list4 as *mut sys::ID3D12GraphicsCommandList4;
        let vtbl = (*list4).lpVtbl.as_ref().ok_or("the command list is gone")?;
        let base = self.list4 as *mut win::ID3D12GraphicsCommandList;
        let hr = (*base).Reset(self.allocator.as_mut_ptr(), std::ptr::null_mut());
        if !SUCCEEDED(hr) {
            return Err(format!("command list Reset failed (HRESULT 0x{hr:08x})"));
        }
        (vtbl.BuildRaytracingAccelerationStructure)(self.list4, desc, 0, std::ptr::null());
        let hr = (*base).Close();
        if !SUCCEEDED(hr) {
            return Err(format!("command list Close failed (HRESULT 0x{hr:08x})"));
        }
        // Executed on wgpu's own direct queue, so the build is ordered with every
        // frame wgpu submits and no second queue can reorder it.
        let lists: [*mut win::ID3D12CommandList; 1] = [self.list4 as *mut _];
        (*self.queue.as_ptr()).ExecuteCommandLists(1, lists.as_ptr());
        self.wait_for_build()
    }

    fn wait_for_build(&self) -> Result<(), String> {
        let (fence, hr) = self.device.create_fence(0);
        if !SUCCEEDED(hr) {
            return Err(format!("CreateFence failed (HRESULT 0x{hr:08x})"));
        }
        let hr = self.queue.signal(&fence, 1);
        if !SUCCEEDED(hr) {
            return Err(format!("queue Signal failed (HRESULT 0x{hr:08x})"));
        }
        // A bounded spin: builds happen when the captured scene changes, which is
        // a load-time event, and the alternative (an event object plus a wait) is
        // more Win32 surface for the same effect.
        let mut spins = 0u64;
        // SAFETY: `fence` is the live fence this submit signalled.
        while unsafe { fence.GetCompletedValue() } < 1 {
            std::hint::spin_loop();
            spins += 1;
            if spins > 500_000_000 {
                return Err("the GPU never finished the acceleration-structure build".into());
            }
        }
        Ok(())
    }

    /// The ray-query shader source. Kept with the backend so the shader and the
    /// code that feeds it change together.
    pub const DXIL_PREPARE: &'static str = "tools/imaginator-shaders.sh compiles this to DXIL (shader model 6.5) with DXC; see dxr_trace.hlsl";
}

/// A `D3D12_RESOURCE_BARRIER` transitioning a whole resource.
fn transition_barrier(
    resource: &Resource,
    before: win::D3D12_RESOURCE_STATES,
    after: win::D3D12_RESOURCE_STATES,
) -> win::D3D12_RESOURCE_BARRIER {
    // SAFETY: `D3D12_RESOURCE_BARRIER` is a plain C aggregate; all-zero is the
    // same "no barrier" value the header's `{}` produces.
    let mut barrier: win::D3D12_RESOURCE_BARRIER = unsafe { std::mem::zeroed() };
    barrier.Type = win::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
    barrier.Flags = win::D3D12_RESOURCE_BARRIER_FLAG_NONE;
    // SAFETY: `winapi`'s union accessors are `unsafe` because the caller has to
    // know which member is live. `Type` is set to `TRANSITION` immediately above,
    // so `Transition` is the active member, and the local is exclusively borrowed.
    let transition = unsafe { barrier.u.Transition_mut() };
    transition.pResource = resource.as_mut_ptr();
    transition.Subresource = win::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
    transition.StateBefore = before;
    transition.StateAfter = after;
    barrier
}

/// Reinterpret a `[f32]` slice as bytes without a `bytemuck` dependency.
fn bytemuck_cast(floats: &[f32]) -> &[u8] {
    // SAFETY: `f32` has no padding and the slice is immutable for the call.
    unsafe {
        std::slice::from_raw_parts(floats.as_ptr() as *const u8, std::mem::size_of_val(floats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reports_each_distinct_reason() {
        assert!(!HardwareStatus::Untried.available());
        assert!(!HardwareStatus::Unsupported("no tier".into()).available());
        assert!(!HardwareStatus::Failed("boom".into()).available());
        assert!(HardwareStatus::Ready("ok".into()).available());
        assert!(HardwareStatus::Unsupported("no tier".into())
            .label()
            .contains("no tier"));
        assert!(HardwareStatus::Ready("ok".into())
            .label()
            .contains("acceleration structures ready"));
    }

    #[test]
    fn triangle_vertices_are_three_f32x3_with_no_padding() {
        assert_eq!(GEOMETRY_STRIDE, 3 * 3 * 4);
        let floats = [1.0f32, 2., 3., 4., 5., 6., 7., 8., 9.];
        assert_eq!(bytemuck_cast(&floats).len() as u64, GEOMETRY_STRIDE);
    }

    #[test]
    fn the_instance_record_is_the_size_the_driver_expects() {
        // Transform (12 floats = 48) + InstanceID, contribution, flags (12) +
        // 4 bytes of alignment padding before the 8-byte address + address (8).
        assert_eq!(
            std::mem::size_of::<sys::D3D12_RAYTRACING_INSTANCE_DESC>(),
            80,
            "D3D12_RAYTRACING_INSTANCE_DESC is 80 bytes, 16-byte aligned"
        );
        assert_eq!(
            std::mem::align_of::<sys::D3D12_RAYTRACING_INSTANCE_DESC>(),
            16
        );
    }
}
