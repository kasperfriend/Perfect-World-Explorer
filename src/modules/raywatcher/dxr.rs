//! Hardware ray tracing backend using DirectX 12 DXR Tier 1.1 (`RayQuery`).
//!
//! Runs directly on GPU RT cores (NVIDIA RTX 20/30/40/50, AMD RDNA2+, Intel
//! Arc) through `wgpu-hal`'s underlying `ID3D12Device` and `ID3D12CommandQueue`
//! — without needing `wgpu` 0.20+ or any extra crate dependencies.
//!
//! * [`probe_hardware_rt`] queries `D3D12_FEATURE_D3D12_OPTIONS5` for
//!   `D3D12_RAYTRACING_TIER_1_1` (`11`) and checks that `ID3D12Device5` and the
//!   compiled `cs_6_5` DXIL shader (`pwe_dxr_shader`) are present.
//! * [`DxrTracer`] builds a hardware BLAS + TLAS from [`super::bvh::SceneGeometry`]
//!   via `ID3D12GraphicsCommandList4::BuildRaytracingAccelerationStructure` and
//!   dispatches `dxr_trace.hlsl` (`RayQuery<RAY_FLAG_NONE>`) into five shared
//!   `NativeTexture2D` planes (`colour`, `guide`, `reflection`, `normals`,
//!   `albedo`).

#[cfg(not(windows))]
use std::sync::Arc;

#[cfg(not(windows))]
use super::bvh::SceneGeometry;
#[cfg(not(windows))]
use super::gpu::RayParams;

/// Probe whether the active `wgpu::Device` supports real DXR Tier 1.1 hardware
/// ray tracing on RT cores. Returns `(supported, optional_reason)`.
pub fn probe_hardware_rt(device: &wgpu::Device) -> (bool, Option<String>) {
    #[cfg(windows)]
    {
        imp::probe_hardware_rt(device)
    }
    #[cfg(not(windows))]
    {
        let _ = device;
        (
            false,
            Some(
                "DXR 1.1 hardware ray tracing requires Windows + DX12; Software mode runs instead"
                    .into(),
            ),
        )
    }
}

#[cfg(windows)]
pub use imp::DxrTracer;

#[cfg(not(windows))]
pub struct DxrTracer;

#[cfg(not(windows))]
impl DxrTracer {
    pub fn new(
        _device: &wgpu::Device,
        _scene: &Arc<SceneGeometry>,
        _grid_extent: (u32, u32),
    ) -> Result<Self, String> {
        Err("DXR 1.1 hardware ray tracing requires Windows + DX12".into())
    }

    pub fn set_scene(&mut self, _scene: &Arc<SceneGeometry>) -> Result<(), String> {
        Err("DXR 1.1 hardware ray tracing requires Windows + DX12".into())
    }

    pub fn update_dynamic_character(&mut self, _verts: &[[f32; 3]]) -> Result<(), String> {
        Ok(())
    }

    pub fn resize(
        &mut self,
        _device: &wgpu::Device,
        _grid_extent: (u32, u32),
    ) -> Result<(), String> {
        Err("DXR 1.1 hardware ray tracing requires Windows + DX12".into())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn copy_gbuf_inputs(
        &mut self,
        _encoder: &mut wgpu::CommandEncoder,
        _src_pos: &wgpu::Texture,
        _src_norm: &wgpu::Texture,
        _src_raster: &wgpu::Texture,
        _src_sky_env: &wgpu::Texture,
        _copy_sky_env: bool,
        _src_terrain_map: &wgpu::Texture,
        _copy_terrain_map: bool,
    ) {
    }

    #[allow(clippy::too_many_arguments)]
    pub fn trace(
        &mut self,
        _encoder: &mut wgpu::CommandEncoder,
        _params: &RayParams,
        _grid_extent: (u32, u32),
        _dst_colour: &wgpu::Texture,
        _dst_guide: Option<&wgpu::Texture>,
        _dst_reflection: &wgpu::Texture,
        _dst_normals: &wgpu::Texture,
        _dst_albedo: Option<&wgpu::Texture>,
    ) -> Result<(), String> {
        Err("DXR 1.1 hardware ray tracing requires Windows + DX12".into())
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};
    use std::sync::Arc;

    use winapi::shared::dxgiformat::{
        DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R32G32B32A32_FLOAT,
        DXGI_FORMAT_R32G32B32_FLOAT, DXGI_FORMAT_UNKNOWN,
    };
    use winapi::shared::dxgitype::DXGI_SAMPLE_DESC;
    use winapi::shared::guiddef::GUID;
    use winapi::shared::minwindef::{BOOL, UINT};
    use winapi::shared::winerror::HRESULT;
    use winapi::um::d3d12 as d12;
    use winapi::um::d3dcommon::ID3DBlob;

    use crate::fsr::{raw_dx12, NativeCommandRing, NativeTexture2D};
    use crate::modules::raywatcher::bvh::{RayMaterial, RayTriangle, SceneGeometry};
    use crate::modules::raywatcher::gpu::{
        RayParams, RestirReservoir, COLOUR_FORMAT, GUIDE_FORMAT, SKY_ENV_HEIGHT, SKY_ENV_WIDTH,
        TERRAIN_MAP_SIZE,
    };

    const D3D12_FEATURE_D3D12_OPTIONS5: u32 = 27;
    const D3D12_RAYTRACING_TIER_1_1: u32 = 11;
    const D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE: u32 = 0x40_0000;
    const D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE: u32 = 11;

    const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL: u32 = 0;
    const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL: u32 = 1;
    const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE: u32 = 0x4;
    const D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_BUILD: u32 = 0x8;
    const D3D12_ELEMENTS_LAYOUT_ARRAY: u32 = 0;
    const D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES: u32 = 0;
    const D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE: u32 = 0x1;
    const D3D12_RAYTRACING_GEOMETRY_FLAG_NO_DUPLICATE_ANYHIT_INVOCATION: u32 = 0x2;

    // `MIDL_INTERFACE("8b4f173b-2fea-4b80-8f58-4307191ab95d")`
    const IID_ID3D12_DEVICE5: GUID = GUID {
        Data1: 0x8b4f_173b,
        Data2: 0x2fea,
        Data3: 0x4b80,
        Data4: [0x8f, 0x58, 0x43, 0x07, 0x19, 0x1a, 0xb9, 0x5d],
    };

    // `MIDL_INTERFACE("8754318e-d3a9-4541-98cf-645b50dc4874")`
    const IID_ID3D12_GRAPHICS_COMMAND_LIST4: GUID = GUID {
        Data1: 0x8754_318e,
        Data2: 0xd3a9,
        Data3: 0x4541,
        Data4: [0x98, 0xcf, 0x64, 0x5b, 0x50, 0xdc, 0x48, 0x74],
    };

    // `MIDL_INTERFACE("c54a6b66-72df-4ee8-8be5-a946a1429214")` — ID3D12RootSignature
    const IID_ID3D12_ROOT_SIGNATURE: GUID = GUID {
        Data1: 0xc54a_6b66,
        Data2: 0x72df,
        Data3: 0x4ee8,
        Data4: [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14],
    };

    // `MIDL_INTERFACE("765a30f3-f624-4c6f-a828-ace948622445")` — ID3D12PipelineState
    const IID_ID3D12_PIPELINE_STATE: GUID = GUID {
        Data1: 0x765a_30f3,
        Data2: 0xf624,
        Data3: 0x4c6f,
        Data4: [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45],
    };

    // `MIDL_INTERFACE("8efb471d-616c-4f49-90f7-127bb763fa51")` — ID3D12DescriptorHeap
    const IID_ID3D12_DESCRIPTOR_HEAP: GUID = GUID {
        Data1: 0x8efb_471d,
        Data2: 0x616c,
        Data3: 0x4f49,
        Data4: [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51],
    };

    // `MIDL_INTERFACE("696442be-a72e-4059-bc79-5b5c98040fad")` — ID3D12Resource
    const IID_ID3D12_RESOURCE: GUID = GUID {
        Data1: 0x6964_42be,
        Data2: 0xa72e,
        Data3: 0x4059,
        Data4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad],
    };

    // Exact COM vtable indices verified against Microsoft DirectX-Headers `d3d12.h`.
    const DEVICE5_SLOT_GET_PREBUILD_INFO: usize = 63;
    const CMDLIST4_SLOT_BUILD_AS: usize = 72;

    #[repr(C)]
    #[derive(Default)]
    struct D3d12Options5 {
        srv_only_tiled_resource_tier3: BOOL,
        render_passes_tier: u32,
        raytracing_tier: u32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct PrebuildInfo {
        result_data_max_size_in_bytes: u64,
        scratch_data_size_in_bytes: u64,
        update_scratch_data_size_in_bytes: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct GpuVaAndStride {
        start_address: u64,
        stride_in_bytes: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct GeometryTrianglesDesc {
        transform3x4: u64,
        index_format: u32,
        vertex_format: u32,
        index_count: UINT,
        vertex_count: UINT,
        index_buffer: u64,
        vertex_buffer: GpuVaAndStride,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct GeometryDesc {
        ty: u32,
        flags: u32,
        triangles: GeometryTrianglesDesc,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    union BuildInputsUnion {
        instance_descs: u64,
        p_geometry_descs: *const GeometryDesc,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct BuildAsInputs {
        ty: u32,
        flags: u32,
        num_descs: UINT,
        descs_layout: u32,
        u: BuildInputsUnion,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct BuildAsDesc {
        dest_as_data: u64,
        inputs: BuildAsInputs,
        source_as_data: u64,
        scratch_as_data: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct InstanceDesc {
        transform: [[f32; 4]; 3],
        instance_id_and_mask: u32,
        hit_group_and_flags: u32,
        acceleration_structure: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RawSrvDesc {
        format: u32,
        view_dimension: u32,
        shader4_component_mapping: u32,
        u: [u64; 3],
    }

    const _: () = {
        assert!(size_of::<RawSrvDesc>() == size_of::<d12::D3D12_SHADER_RESOURCE_VIEW_DESC>());
        assert!(size_of::<InstanceDesc>() == 64);
        assert!(size_of::<PrebuildInfo>() == 24);
    };

    #[cfg(pwe_dxr_shader)]
    const DXR_TRACE_DXIL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dxr_trace.dxil"));

    #[cfg(not(pwe_dxr_shader))]
    const DXR_TRACE_DXIL: &[u8] = &[];

    #[inline]
    unsafe fn com_release(ptr: *mut c_void) {
        if !ptr.is_null() {
            let vtbl = *(ptr as *const *const usize);
            let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(*vtbl.add(2));
            release(ptr);
        }
    }

    #[inline]
    unsafe fn com_query_interface(ptr: *mut c_void, iid: &GUID) -> Result<*mut c_void, HRESULT> {
        let mut out: *mut c_void = null_mut();
        let vtbl = *(ptr as *const *const usize);
        let qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT =
            std::mem::transmute(*vtbl);
        let hr = qi(ptr, iid as *const GUID, &mut out);
        if hr >= 0 && !out.is_null() {
            Ok(out)
        } else {
            Err(hr)
        }
    }

    #[inline]
    unsafe fn device5_get_prebuild_info(
        device5: *mut c_void,
        inputs: &BuildAsInputs,
    ) -> PrebuildInfo {
        let mut info = PrebuildInfo::default();
        let vtbl = *(device5 as *const *const usize);
        let f: unsafe extern "system" fn(*mut c_void, *const BuildAsInputs, *mut PrebuildInfo) =
            std::mem::transmute(*vtbl.add(DEVICE5_SLOT_GET_PREBUILD_INFO));
        f(device5, inputs as *const BuildAsInputs, &mut info);
        info
    }

    #[inline]
    unsafe fn cmdlist4_build_as(cmdlist4: *mut c_void, desc: &BuildAsDesc) {
        let vtbl = *(cmdlist4 as *const *const usize);
        let f: unsafe extern "system" fn(*mut c_void, *const BuildAsDesc, UINT, *const c_void) =
            std::mem::transmute(*vtbl.add(CMDLIST4_SLOT_BUILD_AS));
        f(cmdlist4, desc as *const BuildAsDesc, 0, null());
    }

    pub fn probe_hardware_rt(device: &wgpu::Device) -> (bool, Option<String>) {
        if DXR_TRACE_DXIL.is_empty() {
            return (
                false,
                Some("DXR 1.1 shader was not compiled into this build (dxc.exe missing); Software mode runs instead".into()),
            );
        }
        let (raw_device, _) = match raw_dx12(device) {
            Ok(pair) => pair,
            Err(err) => {
                return (false, Some(format!("{err}; Software mode runs instead")));
            }
        };
        let mut opts5 = D3d12Options5::default();
        let hr = unsafe {
            (*raw_device.as_mut_ptr()).CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS5,
                &mut opts5 as *mut D3d12Options5 as *mut c_void,
                size_of::<D3d12Options5>() as UINT,
            )
        };
        if hr < 0 {
            return (
                false,
                Some(format!(
                    "DX12 CheckFeatureSupport(OPTIONS5) failed (0x{hr:08x}); Software mode runs instead"
                )),
            );
        }
        if opts5.raytracing_tier < D3D12_RAYTRACING_TIER_1_1 {
            return (
                false,
                Some(format!(
                    "adapter reports DXR tier {} (Tier 1.1 required for RT cores); Software mode runs instead",
                    opts5.raytracing_tier
                )),
            );
        }
        match unsafe {
            com_query_interface(raw_device.as_mut_ptr() as *mut c_void, &IID_ID3D12_DEVICE5)
        } {
            Ok(dev5) => {
                unsafe { com_release(dev5) };
                (true, None)
            }
            Err(hr) => (
                false,
                Some(format!(
                    "ID3D12Device5 unavailable (0x{hr:08x}); Software mode runs instead"
                )),
            ),
        }
    }

    struct RawBuffer {
        resource: *mut d12::ID3D12Resource,
        gpu_va: u64,
        size: u64,
        mapped: *mut u8,
    }

    impl Drop for RawBuffer {
        fn drop(&mut self) {
            unsafe {
                if !self.mapped.is_null() {
                    (*self.resource).Unmap(0, null());
                }
                com_release(self.resource as *mut c_void);
            }
        }
    }

    impl RawBuffer {
        unsafe fn create(
            device: *mut d12::ID3D12Device,
            size: u64,
            heap_type: d12::D3D12_HEAP_TYPE,
            flags: d12::D3D12_RESOURCE_FLAGS,
            initial_state: d12::D3D12_RESOURCE_STATES,
        ) -> Result<Self, String> {
            let size = size.max(256);
            let heap = d12::D3D12_HEAP_PROPERTIES {
                Type: heap_type,
                CPUPageProperty: d12::D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
                MemoryPoolPreference: d12::D3D12_MEMORY_POOL_UNKNOWN,
                CreationNodeMask: 1,
                VisibleNodeMask: 1,
            };
            let desc = d12::D3D12_RESOURCE_DESC {
                Dimension: d12::D3D12_RESOURCE_DIMENSION_BUFFER,
                Alignment: 0,
                Width: size,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_UNKNOWN,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: d12::D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: flags,
            };
            let mut resource: *mut d12::ID3D12Resource = null_mut();
            let hr = (*device).CreateCommittedResource(
                &heap,
                d12::D3D12_HEAP_FLAG_NONE,
                &desc,
                initial_state,
                null(),
                &IID_ID3D12_RESOURCE,
                &mut resource as *mut *mut d12::ID3D12Resource as *mut *mut c_void,
            );
            if hr < 0 || resource.is_null() {
                return Err(format!(
                    "DXR CreateCommittedResource(size={size}) failed (0x{hr:08x})"
                ));
            }
            let gpu_va = (*resource).GetGPUVirtualAddress();
            Ok(Self {
                resource,
                gpu_va,
                size,
                mapped: null_mut(),
            })
        }

        /// Boost #11: Persistently map an upload heap buffer once to eliminate
        /// per-frame Map/Unmap kernel transitions on CBVs.
        unsafe fn map_persistent(&mut self) -> Result<(), String> {
            if !self.mapped.is_null() {
                return Ok(());
            }
            let mut ptr: *mut c_void = null_mut();
            let read_range = d12::D3D12_RANGE { Begin: 0, End: 0 };
            let hr = (*self.resource).Map(0, &read_range, &mut ptr);
            if hr < 0 || ptr.is_null() {
                return Err(format!("DXR persistent Map failed (0x{hr:08x})"));
            }
            self.mapped = ptr as *mut u8;
            Ok(())
        }

        unsafe fn write_upload(&self, bytes: &[u8]) -> Result<(), String> {
            if bytes.is_empty() {
                return Ok(());
            }
            if (bytes.len() as u64) > self.size {
                return Err(format!(
                    "DXR upload buffer overflow ({} > {})",
                    bytes.len(),
                    self.size
                ));
            }
            if !self.mapped.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.mapped, bytes.len());
                return Ok(());
            }
            let mut mapped: *mut c_void = null_mut();
            let read_range = d12::D3D12_RANGE { Begin: 0, End: 0 };
            let hr = (*self.resource).Map(0, &read_range, &mut mapped);
            if hr < 0 || mapped.is_null() {
                return Err(format!("DXR upload buffer Map failed (0x{hr:08x})"));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped as *mut u8, bytes.len());
            (*self.resource).Unmap(0, null());
            Ok(())
        }
    }

    struct DxrScene {
        tris: RawBuffer,
        materials: RawBuffer,
        _blas: RawBuffer,
        tlas: RawBuffer,
        tri_count: u32,
        opaque_tri_count: u32,
        mat_count: u32,
    }

    #[inline]
    fn align256(n: u64) -> u64 {
        (n.max(256) + 255) & !255
    }

    /// Small RAII guard for raw COM pointers during `DxrTracer::new` so early
    /// errors never leak D3D12 objects.
    struct ComGuard(*mut c_void);

    impl ComGuard {
        fn take(mut self) -> *mut c_void {
            let p = self.0;
            self.0 = null_mut();
            p
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { com_release(self.0) };
        }
    }

    struct DxrTargets {
        in_pos: NativeTexture2D,
        in_norm: NativeTexture2D,
        in_raster: NativeTexture2D,
        in_sky_env: NativeTexture2D,
        in_terrain_map: NativeTexture2D,
        colour: NativeTexture2D,
        guide: NativeTexture2D,
        reflection: NativeTexture2D,
        normals: NativeTexture2D,
        albedo: NativeTexture2D,
        extent: (u32, u32),
        /// True once `wgpu` has copied from the corresponding output target,
        /// leaving wgpu's DX12 state tracker expecting `COPY_SOURCE`.
        wgpu_tracked_copy_src: [bool; 5],
        /// True when an output plane is already resident in `UNORDERED_ACCESS`
        /// (unused planes stay in UAV across frames with zero pre/post barriers).
        uav_resident: [bool; 5],
        /// True once `wgpu` has copied into the G-buffer input targets,
        /// leaving wgpu's DX12 state tracker expecting `COPY_DEST`.
        wgpu_tracked_copy_dst: bool,
    }

    impl DxrTargets {
        fn new(
            device: &wgpu::Device,
            raw_device: &d3d12::Device,
            extent: (u32, u32),
        ) -> Result<Self, String> {
            let (w, h) = (extent.0.max(1), extent.1.max(1));
            let state = d12::D3D12_RESOURCE_STATE_COMMON;
            let in_usage = wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING;
            let in_pos = unsafe {
                NativeTexture2D::create(
                    device,
                    raw_device,
                    "raywatcher dxr in_pos",
                    wgpu::TextureFormat::Rgba32Float,
                    DXGI_FORMAT_R32G32B32A32_FLOAT,
                    (w, h),
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    in_usage,
                    state,
                    false,
                )?
            };
            let in_norm = unsafe {
                NativeTexture2D::create(
                    device,
                    raw_device,
                    "raywatcher dxr in_norm",
                    GUIDE_FORMAT,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    (w, h),
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    in_usage,
                    state,
                    false,
                )?
            };
            let in_raster = unsafe {
                NativeTexture2D::create(
                    device,
                    raw_device,
                    "raywatcher dxr in_raster",
                    COLOUR_FORMAT,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    (w, h),
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    in_usage,
                    state,
                    false,
                )?
            };
            let in_sky_env = unsafe {
                NativeTexture2D::create(
                    device,
                    raw_device,
                    "raywatcher dxr in sky env",
                    COLOUR_FORMAT,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    (SKY_ENV_WIDTH, SKY_ENV_HEIGHT),
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    in_usage,
                    state,
                    false,
                )?
            };
            let in_terrain_map = unsafe {
                NativeTexture2D::create(
                    device,
                    raw_device,
                    "raywatcher dxr in terrain map",
                    COLOUR_FORMAT,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    (TERRAIN_MAP_SIZE, TERRAIN_MAP_SIZE),
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    in_usage,
                    state,
                    false,
                )?
            };
            let flags = d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
            let usage = wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING;
            let mk = |label: &str, fmt: wgpu::TextureFormat| unsafe {
                NativeTexture2D::create(
                    device,
                    raw_device,
                    label,
                    fmt,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    (w, h),
                    flags,
                    usage,
                    state,
                    false,
                )
            };
            let colour = mk("raywatcher dxr colour", COLOUR_FORMAT)?;
            let guide = mk("raywatcher dxr guide", GUIDE_FORMAT)?;
            let reflection = mk("raywatcher dxr reflection", COLOUR_FORMAT)?;
            let normals = mk("raywatcher dxr normals", GUIDE_FORMAT)?;
            let albedo = mk("raywatcher dxr albedo", COLOUR_FORMAT)?;
            Ok(Self {
                in_pos,
                in_norm,
                in_raster,
                in_sky_env,
                in_terrain_map,
                colour,
                guide,
                reflection,
                normals,
                albedo,
                extent: (w, h),
                wgpu_tracked_copy_src: [false; 5],
                uav_resident: [false; 5],
                wgpu_tracked_copy_dst: false,
            })
        }
    }

    /// Real DXR Tier 1.1 hardware ray tracer running on GPU RT cores.
    pub struct DxrTracer {
        raw_device: d3d12::Device,
        device5: *mut c_void,
        raw_queue: d3d12::CommandQueue,
        ring: NativeCommandRing,
        root_sig: *mut d12::ID3D12RootSignature,
        pso: *mut d12::ID3D12PipelineState,
        desc_heap: *mut d12::ID3D12DescriptorHeap,
        desc_stride: u32,
        cbvs: [RawBuffer; 3],
        scene: DxrScene,
        /// Deferred retirement lists freed once `ring.completed_fence() >= fence`.
        retired_scenes: Vec<(u64, DxrScene)>,
        retired_buffers: Vec<(u64, Vec<RawBuffer>)>,
        /// Persistent 65,536-cell 3D world-space radiance cache (`u5`, 1 MB VRAM)
        /// updated and queried by hardware RT cores for low-noise multi-bounce GI.
        radiance_cache: RawBuffer,
        /// Two-frame ReSTIR GI reservoir history, shared with the DXR compute shader at u6.
        restir_reservoirs: RawBuffer,
        restir_reservoirs_enabled: bool,
        restir_history_valid: bool,
        targets: DxrTargets,
        /// Opt #22: Reusable grow-only DXR AS scratch buffer (avoids per-rebuild WDDM allocs).
        reusable_scratch: Option<(u64, RawBuffer)>,
        /// Opt #22: Reusable persistent TLAS instance descriptor upload buffer (2 instances).
        reusable_instance_desc: Option<RawBuffer>,
        /// Feature #1: 3-slot race-free ring of reusable dynamic character/NPC buffers
        /// (Walker + Populator) avoiding per-frame WDDM allocations and CPU/GPU races.
        dyn_verts: [Option<(u64, RawBuffer)>; 3],
        dyn_blas: [Option<(u64, RawBuffer)>; 3],
        dyn_tlas: [Option<(u64, RawBuffer)>; 3],
        dyn_inst_desc: [Option<RawBuffer>; 3],
        dyn_slot: usize,
        dyn_active_tlas_va: Option<u64>,
        dyn_last_hash: u64,
        has_dyn_instance: bool,
    }

    // Descriptor heap slots:
    // 0: t0 TLAS SRV
    // 1: t1 tris StructuredBuffer SRV
    // 2: t2 materials StructuredBuffer SRV
    // 3: t3 in_pos Texture2D SRV
    // 4: t4 in_norm Texture2D SRV
    // 5: t5 in_raster Texture2D SRV
    // 6: u0 out_colour UAV
    // 7: u1 out_guide UAV
    // 8: u2 out_reflection UAV
    // 9: u3 out_normals UAV
    // 10: u4 out_albedo UAV
    // 11: u5 radiance cache UAV
    // 12: u6 two-frame ReSTIR GI reservoir UAV
    const RADIANCE_CACHE_CELLS: u32 = 65_536;
    const HEAP_SLOTS: u32 = 15;

    impl Drop for DxrTracer {
        fn drop(&mut self) {
            self.ring.wait_idle();
            unsafe {
                com_release(self.desc_heap as *mut c_void);
                com_release(self.pso as *mut c_void);
                com_release(self.root_sig as *mut c_void);
                com_release(self.device5);
            }
        }
    }

    impl DxrTracer {
        pub fn new(
            device: &wgpu::Device,
            scene: &Arc<SceneGeometry>,
            grid_extent: (u32, u32),
        ) -> Result<Self, String> {
            let (ok, reason) = probe_hardware_rt(device);
            if !ok {
                return Err(reason.unwrap_or_else(|| "DXR 1.1 unavailable".into()));
            }
            let (raw_device, raw_queue) = raw_dx12(device)?;
            let dev_ptr = raw_device.as_mut_ptr();
            let device5_guard = ComGuard(unsafe {
                com_query_interface(dev_ptr as *mut c_void, &IID_ID3D12_DEVICE5)
                    .map_err(|hr| format!("ID3D12Device5 QueryInterface failed (0x{hr:08x})"))?
            });
            let mut ring = NativeCommandRing::new(&raw_device, "DXR")?;
            let (root_sig, pso, desc_heap, desc_stride) =
                unsafe { Self::create_pipeline(dev_ptr)? };
            let root_sig_guard = ComGuard(root_sig as *mut c_void);
            let pso_guard = ComGuard(pso as *mut c_void);
            let desc_heap_guard = ComGuard(desc_heap as *mut c_void);
            let cbv_size = align256(size_of::<RayParams>() as u64);
            let mk_cbv = || unsafe {
                let mut buf = RawBuffer::create(
                    dev_ptr,
                    cbv_size,
                    d12::D3D12_HEAP_TYPE_UPLOAD,
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    d12::D3D12_RESOURCE_STATE_GENERIC_READ,
                )?;
                buf.map_persistent()?;
                Ok::<RawBuffer, String>(buf)
            };
            let cbvs = [mk_cbv()?, mk_cbv()?, mk_cbv()?];
            let mut reusable_scratch: Option<(u64, RawBuffer)> = None;
            let mut reusable_instance_desc: Option<RawBuffer> = None;
            let (dxr_scene, staging, build_fence) = Self::build_scene(
                dev_ptr,
                device5_guard.0,
                &mut ring,
                &raw_queue,
                scene,
                &mut reusable_scratch,
                &mut reusable_instance_desc,
            )?;
            let radiance_cache = unsafe {
                RawBuffer::create(
                    dev_ptr,
                    (RADIANCE_CACHE_CELLS as u64) * 16,
                    d12::D3D12_HEAP_TYPE_DEFAULT,
                    d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                    d12::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                )?
            };
            let targets = DxrTargets::new(device, &raw_device, grid_extent)?;
            let reservoir_bytes = u64::from(grid_extent.0.max(1))
                .saturating_mul(u64::from(grid_extent.1.max(1)))
                .saturating_mul(2)
                .saturating_mul(size_of::<RestirReservoir>() as u64);
            let restir_reservoirs_enabled = reservoir_bytes <= device.limits().max_buffer_size;
            let restir_buffer_bytes = if restir_reservoirs_enabled {
                reservoir_bytes.max(64)
            } else {
                64
            };
            let restir_reservoirs = unsafe {
                RawBuffer::create(
                    dev_ptr,
                    restir_buffer_bytes,
                    d12::D3D12_HEAP_TYPE_DEFAULT,
                    d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                    d12::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                )?
            };

            let tracer = Self {
                raw_device,
                device5: device5_guard.take(),
                raw_queue,
                ring,
                root_sig: root_sig_guard.take() as *mut d12::ID3D12RootSignature,
                pso: pso_guard.take() as *mut d12::ID3D12PipelineState,
                desc_heap: desc_heap_guard.take() as *mut d12::ID3D12DescriptorHeap,
                desc_stride,
                cbvs,
                scene: dxr_scene,
                retired_scenes: Vec::new(),
                retired_buffers: vec![(build_fence, staging)],
                radiance_cache,
                restir_reservoirs,
                restir_reservoirs_enabled,
                restir_history_valid: false,
                targets,
                reusable_scratch,
                reusable_instance_desc,
                dyn_verts: [None, None, None],
                dyn_blas: [None, None, None],
                dyn_tlas: [None, None, None],
                dyn_inst_desc: [None, None, None],
                dyn_slot: 0,
                dyn_active_tlas_va: None,
                dyn_last_hash: 0,
                has_dyn_instance: false,
            };
            tracer.write_scene_descriptors();
            tracer.write_target_descriptors();
            Ok(tracer)
        }

        unsafe fn create_pipeline(
            dev_ptr: *mut d12::ID3D12Device,
        ) -> Result<
            (
                *mut d12::ID3D12RootSignature,
                *mut d12::ID3D12PipelineState,
                *mut d12::ID3D12DescriptorHeap,
                u32,
            ),
            String,
        > {
            // Root parameter 0: CBV at b0
            // Root parameter 1: Descriptor table with 6 SRVs (t0..t5)
            // Root parameter 2: Descriptor table with 7 UAVs (u0..u6)
            let srv_range = d12::D3D12_DESCRIPTOR_RANGE {
                RangeType: d12::D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                NumDescriptors: 8,
                BaseShaderRegister: 0,
                RegisterSpace: 0,
                OffsetInDescriptorsFromTableStart: 0,
            };
            let uav_range = d12::D3D12_DESCRIPTOR_RANGE {
                RangeType: d12::D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
                NumDescriptors: 7,
                BaseShaderRegister: 0,
                RegisterSpace: 0,
                OffsetInDescriptorsFromTableStart: 0,
            };
            let mut params: [d12::D3D12_ROOT_PARAMETER; 3] = zeroed();
            params[0].ParameterType = d12::D3D12_ROOT_PARAMETER_TYPE_CBV;
            params[0].ShaderVisibility = d12::D3D12_SHADER_VISIBILITY_ALL;
            *params[0].u.Descriptor_mut() = d12::D3D12_ROOT_DESCRIPTOR {
                ShaderRegister: 0,
                RegisterSpace: 0,
            };
            params[1].ParameterType = d12::D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE;
            params[1].ShaderVisibility = d12::D3D12_SHADER_VISIBILITY_ALL;
            *params[1].u.DescriptorTable_mut() = d12::D3D12_ROOT_DESCRIPTOR_TABLE {
                NumDescriptorRanges: 1,
                pDescriptorRanges: &srv_range,
            };
            params[2].ParameterType = d12::D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE;
            params[2].ShaderVisibility = d12::D3D12_SHADER_VISIBILITY_ALL;
            *params[2].u.DescriptorTable_mut() = d12::D3D12_ROOT_DESCRIPTOR_TABLE {
                NumDescriptorRanges: 1,
                pDescriptorRanges: &uav_range,
            };

            let rs_desc = d12::D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: params.len() as UINT,
                pParameters: params.as_ptr(),
                NumStaticSamplers: 0,
                pStaticSamplers: null(),
                Flags: d12::D3D12_ROOT_SIGNATURE_FLAG_NONE,
            };
            let mut blob: *mut ID3DBlob = null_mut();
            let mut err_blob: *mut ID3DBlob = null_mut();
            let hr = d12::D3D12SerializeRootSignature(
                &rs_desc,
                d12::D3D_ROOT_SIGNATURE_VERSION_1_0,
                &mut blob,
                &mut err_blob,
            );
            if !err_blob.is_null() {
                com_release(err_blob as *mut c_void);
            }
            if hr < 0 || blob.is_null() {
                return Err(format!(
                    "DXR D3D12SerializeRootSignature failed (0x{hr:08x})"
                ));
            }
            let mut root_sig: *mut d12::ID3D12RootSignature = null_mut();
            let hr = (*dev_ptr).CreateRootSignature(
                0,
                (*blob).GetBufferPointer(),
                (*blob).GetBufferSize(),
                &IID_ID3D12_ROOT_SIGNATURE,
                &mut root_sig as *mut *mut d12::ID3D12RootSignature as *mut *mut c_void,
            );
            com_release(blob as *mut c_void);
            if hr < 0 || root_sig.is_null() {
                return Err(format!("DXR CreateRootSignature failed (0x{hr:08x})"));
            }

            let pso_desc = d12::D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: root_sig,
                CS: d12::D3D12_SHADER_BYTECODE {
                    pShaderBytecode: DXR_TRACE_DXIL.as_ptr() as *const c_void,
                    BytecodeLength: DXR_TRACE_DXIL.len(),
                },
                NodeMask: 0,
                CachedPSO: d12::D3D12_CACHED_PIPELINE_STATE {
                    pCachedBlob: null(),
                    CachedBlobSizeInBytes: 0,
                },
                Flags: d12::D3D12_PIPELINE_STATE_FLAG_NONE,
            };
            let mut pso: *mut d12::ID3D12PipelineState = null_mut();
            let hr = (*dev_ptr).CreateComputePipelineState(
                &pso_desc,
                &IID_ID3D12_PIPELINE_STATE,
                &mut pso as *mut *mut d12::ID3D12PipelineState as *mut *mut c_void,
            );
            if hr < 0 || pso.is_null() {
                com_release(root_sig as *mut c_void);
                return Err(format!(
                    "DXR CreateComputePipelineState failed (0x{hr:08x})"
                ));
            }

            let heap_desc = d12::D3D12_DESCRIPTOR_HEAP_DESC {
                Type: d12::D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: HEAP_SLOTS,
                Flags: d12::D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            };
            let mut desc_heap: *mut d12::ID3D12DescriptorHeap = null_mut();
            let hr = (*dev_ptr).CreateDescriptorHeap(
                &heap_desc,
                &IID_ID3D12_DESCRIPTOR_HEAP,
                &mut desc_heap as *mut *mut d12::ID3D12DescriptorHeap as *mut *mut c_void,
            );
            if hr < 0 || desc_heap.is_null() {
                com_release(pso as *mut c_void);
                com_release(root_sig as *mut c_void);
                return Err(format!("DXR CreateDescriptorHeap failed (0x{hr:08x})"));
            }
            let stride = (*dev_ptr)
                .GetDescriptorHandleIncrementSize(d12::D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
            Ok((root_sig, pso, desc_heap, stride))
        }

        fn build_scene(
            dev_ptr: *mut d12::ID3D12Device,
            device5: *mut c_void,
            ring: &mut NativeCommandRing,
            raw_queue: &d3d12::CommandQueue,
            scene: &Arc<SceneGeometry>,
            reusable_scratch: &mut Option<(u64, RawBuffer)>,
            reusable_instance_desc: &mut Option<RawBuffer>,
        ) -> Result<(DxrScene, Vec<RawBuffer>, u64), String> {
            // Ensure at least 1 degenerate triangle so BLAS/TLAS build always
            // succeeds even on an empty scene.
            let dummy_tri = [RayTriangle {
                a: [0.0, 0.0, 0.0, 0.0],
                b: [0.0, 0.0, 0.0, 0.0],
                c: [0.0, 0.0, 0.0, 0.0],
                normal: [0.0, 1.0, 0.0, 0.0],
            }];
            let dummy_mat = [RayMaterial {
                albedo: [0.5, 0.5, 0.5, 1.0],
                surface: [0.8, 0.0, 0.0, 0.0],
            }];
            let raw_tris: &[RayTriangle] = if scene.triangles.is_empty() {
                &dummy_tri
            } else {
                &scene.triangles
            };
            let mats_data: &[RayMaterial] = if scene.materials.is_empty() {
                &dummy_mat
            } else {
                &scene.materials
            };
            // Count opaque vs alpha-tested triangles in one pass, then write
            // partitioned RayTriangles and flat [f32; 3] BLAS vertices directly
            // into mapped DX12 upload heaps (zero intermediate Vec allocations).
            // Boost #12: Precompute opaque-material table once + integer cast per tri.
            let mat_max = mats_data.len().saturating_sub(1);
            let mat_opaque: Vec<bool> = mats_data.iter().map(|m| m.surface[3] >= 0.99).collect();
            let mut opaque_tri_count = 0u32;
            for t in raw_tris {
                let mat_idx = ((t.b[3] + 0.5).max(0.0) as usize).min(mat_max);
                if mat_opaque[mat_idx] && t.c[3].to_bits() == 0 && t.normal[3].to_bits() == 0 {
                    opaque_tri_count += 1;
                }
            }
            let tri_count = raw_tris.len() as u32;
            let alpha_tri_count = tri_count - opaque_tri_count;
            let mat_count = mats_data.len() as u32;

            unsafe {
                let verts_bytes = (raw_tris.len() * 3 * size_of::<[f32; 3]>()) as u64;
                let vertices = RawBuffer::create(
                    dev_ptr,
                    verts_bytes,
                    d12::D3D12_HEAP_TYPE_UPLOAD,
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    d12::D3D12_RESOURCE_STATE_GENERIC_READ,
                )?;
                let tris_bytes = (raw_tris.len() * size_of::<RayTriangle>()) as u64;
                let tris_upload = RawBuffer::create(
                    dev_ptr,
                    tris_bytes,
                    d12::D3D12_HEAP_TYPE_UPLOAD,
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    d12::D3D12_RESOURCE_STATE_GENERIC_READ,
                )?;
                {
                    let read_range = d12::D3D12_RANGE { Begin: 0, End: 0 };
                    let mut v_mapped: *mut c_void = null_mut();
                    let hr_v = (*vertices.resource).Map(0, &read_range, &mut v_mapped);
                    if hr_v < 0 || v_mapped.is_null() {
                        return Err(format!("DXR vertex upload Map failed (0x{hr_v:08x})"));
                    }
                    let mut t_mapped: *mut c_void = null_mut();
                    let hr_t = (*tris_upload.resource).Map(0, &read_range, &mut t_mapped);
                    if hr_t < 0 || t_mapped.is_null() {
                        (*vertices.resource).Unmap(0, null());
                        return Err(format!("DXR tri upload Map failed (0x{hr_t:08x})"));
                    }
                    let v_out = v_mapped as *mut [f32; 3];
                    let t_out = t_mapped as *mut RayTriangle;
                    let mut o_idx = 0usize;
                    let mut a_idx = opaque_tri_count as usize;
                    for &t in raw_tris {
                        let mat_idx = ((t.b[3] + 0.5).max(0.0) as usize).min(mat_max);
                        let is_op = mat_opaque[mat_idx]
                            && t.c[3].to_bits() == 0
                            && t.normal[3].to_bits() == 0;
                        let dst = if is_op {
                            let i = o_idx;
                            o_idx += 1;
                            i
                        } else {
                            let i = a_idx;
                            a_idx += 1;
                            i
                        };
                        t_out.add(dst).write(t);
                        let vbase = dst * 3;
                        v_out.add(vbase).write([t.a[0], t.a[1], t.a[2]]);
                        v_out.add(vbase + 1).write([t.b[0], t.b[1], t.b[2]]);
                        v_out.add(vbase + 2).write([t.c[0], t.c[1], t.c[2]]);
                    }
                    (*tris_upload.resource).Unmap(0, null());
                    (*vertices.resource).Unmap(0, null());
                }
                let tris = RawBuffer::create(
                    dev_ptr,
                    tris_bytes,
                    d12::D3D12_HEAP_TYPE_DEFAULT,
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    d12::D3D12_RESOURCE_STATE_COPY_DEST,
                )?;

                let mats_bytes = (mats_data.len() * size_of::<RayMaterial>()) as u64;
                let mats_upload = RawBuffer::create(
                    dev_ptr,
                    mats_bytes,
                    d12::D3D12_HEAP_TYPE_UPLOAD,
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    d12::D3D12_RESOURCE_STATE_GENERIC_READ,
                )?;
                mats_upload.write_upload(bytemuck::cast_slice(&mats_data))?;
                let materials = RawBuffer::create(
                    dev_ptr,
                    mats_bytes,
                    d12::D3D12_HEAP_TYPE_DEFAULT,
                    d12::D3D12_RESOURCE_FLAG_NONE,
                    d12::D3D12_RESOURCE_STATE_COPY_DEST,
                )?;

                let vert_stride = size_of::<[f32; 3]>() as u64;
                let mut geoms: Vec<GeometryDesc> = Vec::with_capacity(2);
                if opaque_tri_count > 0 {
                    geoms.push(GeometryDesc {
                        ty: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
                        flags: D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE,
                        triangles: GeometryTrianglesDesc {
                            transform3x4: 0,
                            index_format: DXGI_FORMAT_UNKNOWN,
                            vertex_format: DXGI_FORMAT_R32G32B32_FLOAT,
                            index_count: 0,
                            vertex_count: opaque_tri_count * 3,
                            index_buffer: 0,
                            vertex_buffer: GpuVaAndStride {
                                start_address: vertices.gpu_va,
                                stride_in_bytes: vert_stride,
                            },
                        },
                    });
                }
                if alpha_tri_count > 0 {
                    geoms.push(GeometryDesc {
                        ty: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
                        flags: D3D12_RAYTRACING_GEOMETRY_FLAG_NO_DUPLICATE_ANYHIT_INVOCATION, // Alpha-tested foliage only (1 any-hit max)
                        triangles: GeometryTrianglesDesc {
                            transform3x4: 0,
                            index_format: DXGI_FORMAT_UNKNOWN,
                            vertex_format: DXGI_FORMAT_R32G32B32_FLOAT,
                            index_count: 0,
                            vertex_count: alpha_tri_count * 3,
                            index_buffer: 0,
                            vertex_buffer: GpuVaAndStride {
                                start_address: vertices.gpu_va
                                    + (opaque_tri_count as u64) * 3 * vert_stride,
                                stride_in_bytes: vert_stride,
                            },
                        },
                    });
                }
                let blas_inputs = BuildAsInputs {
                    ty: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                    flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
                    num_descs: geoms.len() as UINT,
                    descs_layout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                    u: BuildInputsUnion {
                        p_geometry_descs: geoms.as_ptr(),
                    },
                };
                let blas_info = device5_get_prebuild_info(device5, &blas_inputs);
                if blas_info.result_data_max_size_in_bytes == 0 {
                    return Err(
                        "DXR BLAS GetRaytracingAccelerationStructurePrebuildInfo returned 0 bytes"
                            .into(),
                    );
                }
                let blas = RawBuffer::create(
                    dev_ptr,
                    align256(blas_info.result_data_max_size_in_bytes),
                    d12::D3D12_HEAP_TYPE_DEFAULT,
                    d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                    D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
                )?;

                let instance = InstanceDesc {
                    transform: [
                        [1.0, 0.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0, 0.0],
                    ],
                    instance_id_and_mask: 0xffu32 << 24,
                    hit_group_and_flags: 0,
                    acceleration_structure: blas.gpu_va,
                };
                // Opt #22: Reuse persistent mapped instance_desc upload buffer across rebuilds.
                if reusable_instance_desc.is_none() {
                    let mut buf = RawBuffer::create(
                        dev_ptr,
                        (size_of::<InstanceDesc>() * 2) as u64,
                        d12::D3D12_HEAP_TYPE_UPLOAD,
                        d12::D3D12_RESOURCE_FLAG_NONE,
                        d12::D3D12_RESOURCE_STATE_GENERIC_READ,
                    )?;
                    buf.map_persistent()?;
                    *reusable_instance_desc = Some(buf);
                }
                let instance_desc_ref = reusable_instance_desc.as_ref().unwrap();
                let inst_bytes = std::slice::from_raw_parts(
                    &instance as *const InstanceDesc as *const u8,
                    size_of::<InstanceDesc>(),
                );
                instance_desc_ref.write_upload(inst_bytes)?;

                let tlas_inputs = BuildAsInputs {
                    ty: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
                    flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
                    num_descs: 1,
                    descs_layout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                    u: BuildInputsUnion {
                        instance_descs: instance_desc_ref.gpu_va,
                    },
                };
                let tlas_info = device5_get_prebuild_info(device5, &tlas_inputs);
                if tlas_info.result_data_max_size_in_bytes == 0 {
                    return Err(
                        "DXR TLAS GetRaytracingAccelerationStructurePrebuildInfo returned 0 bytes"
                            .into(),
                    );
                }
                let tlas = RawBuffer::create(
                    dev_ptr,
                    align256(tlas_info.result_data_max_size_in_bytes),
                    d12::D3D12_HEAP_TYPE_DEFAULT,
                    d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                    D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
                )?;

                let scratch_size = align256(
                    blas_info
                        .scratch_data_size_in_bytes
                        .max(tlas_info.scratch_data_size_in_bytes),
                );
                let mut staging = vec![vertices, tris_upload, mats_upload];
                // Opt #22: Reuse grow-only persistent scratch buffer across rebuilds (zero WDDM alloc).
                let need_new_scratch = reusable_scratch
                    .as_ref()
                    .is_none_or(|(cap, _)| *cap < scratch_size);
                if need_new_scratch {
                    if let Some((_, old_scratch)) = reusable_scratch.take() {
                        staging.push(old_scratch);
                    }
                    let new_cap = align256(scratch_size + (scratch_size / 4));
                    let buf = RawBuffer::create(
                        dev_ptr,
                        new_cap,
                        d12::D3D12_HEAP_TYPE_DEFAULT,
                        d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                        d12::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    )?;
                    *reusable_scratch = Some((new_cap, buf));
                }
                let scratch_gpu_va = reusable_scratch.as_ref().unwrap().1.gpu_va;

                // Record and submit BLAS + TLAS build on ID3D12GraphicsCommandList4.
                let slot_idx = ring.acquire("DXR-AS")?;
                let list_ptr = ring.slot_mut(slot_idx).list.as_mut_ptr();
                let list4 = com_query_interface(
                    list_ptr as *mut c_void,
                    &IID_ID3D12_GRAPHICS_COMMAND_LIST4,
                )
                .map_err(|hr| format!("ID3D12GraphicsCommandList4 unavailable (0x{hr:08x})"))?;

                // Opt #23: Enqueue DMA buffer copies BEFORE BLAS build so GPU copy/compute engines
                // overlap vertex/material transfers with RT-core BVH construction, and merge all
                // post-build UAV + Transition barriers into a single ResourceBarrier call.
                (*list_ptr).CopyBufferRegion(tris.resource, 0, staging[1].resource, 0, tris_bytes);
                (*list_ptr).CopyBufferRegion(
                    materials.resource,
                    0,
                    staging[2].resource,
                    0,
                    mats_bytes,
                );

                let blas_desc = BuildAsDesc {
                    dest_as_data: blas.gpu_va,
                    inputs: blas_inputs,
                    source_as_data: 0,
                    scratch_as_data: scratch_gpu_va,
                };
                cmdlist4_build_as(list4, &blas_desc);

                let mut uav_barrier: d12::D3D12_RESOURCE_BARRIER = zeroed();
                uav_barrier.Type = d12::D3D12_RESOURCE_BARRIER_TYPE_UAV;
                // Null resource barriers both `blas` and the reused `scratch` UAV.
                uav_barrier.u.UAV_mut().pResource = null_mut();
                (*list_ptr).ResourceBarrier(1, &uav_barrier);

                let tlas_desc = BuildAsDesc {
                    dest_as_data: tlas.gpu_va,
                    inputs: tlas_inputs,
                    source_as_data: 0,
                    scratch_as_data: scratch_gpu_va,
                };
                cmdlist4_build_as(list4, &tlas_desc);

                let mut final_barriers: [d12::D3D12_RESOURCE_BARRIER; 3] = zeroed();
                final_barriers[0].Type = d12::D3D12_RESOURCE_BARRIER_TYPE_UAV;
                final_barriers[0].u.UAV_mut().pResource = tlas.resource;
                for (b, res) in final_barriers[1..]
                    .iter_mut()
                    .zip([tris.resource, materials.resource])
                {
                    b.Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                    *b.u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: res,
                        Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: d12::D3D12_RESOURCE_STATE_COPY_DEST,
                        StateAfter: d12::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    };
                }
                (*list_ptr).ResourceBarrier(3, final_barriers.as_ptr());

                com_release(list4);
                ring.retire(slot_idx, raw_queue, "DXR-AS")?;
                let build_fence = ring.current_fence();

                Ok((
                    DxrScene {
                        tris,
                        materials,
                        _blas: blas,
                        tlas,
                        tri_count,
                        opaque_tri_count,
                        mat_count,
                    },
                    staging,
                    build_fence,
                ))
            }
        }

        fn cpu_handle(&self, slot: u32) -> d12::D3D12_CPU_DESCRIPTOR_HANDLE {
            let base = unsafe { (*self.desc_heap).GetCPUDescriptorHandleForHeapStart() };
            d12::D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: base.ptr + (slot as usize) * (self.desc_stride as usize),
            }
        }

        fn gpu_handle(&self, slot: u32) -> d12::D3D12_GPU_DESCRIPTOR_HANDLE {
            let base = unsafe { (*self.desc_heap).GetGPUDescriptorHandleForHeapStart() };
            d12::D3D12_GPU_DESCRIPTOR_HANDLE {
                ptr: base.ptr + (slot as u64) * (self.desc_stride as u64),
            }
        }

        /// Update only the changing TLAS SRV at descriptor slot 0.
        /// The static triangle/material SRVs at slots 1 and 2 are unchanged by actor updates.
        fn write_tlas_descriptor(&self) {
            unsafe {
                let active_tlas_va = self.dyn_active_tlas_va.unwrap_or(self.scene.tlas.gpu_va);
                let tlas_srv = RawSrvDesc {
                    format: DXGI_FORMAT_UNKNOWN,
                    view_dimension: D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE,
                    shader4_component_mapping: 0x0000_1688,
                    u: [active_tlas_va, 0, 0],
                };
                (*self.raw_device.as_mut_ptr()).CreateShaderResourceView(
                    null_mut(),
                    &tlas_srv as *const RawSrvDesc as *const d12::D3D12_SHADER_RESOURCE_VIEW_DESC,
                    self.cpu_handle(0),
                );
            }
        }

        fn write_scene_descriptors(&self) {
            self.write_tlas_descriptor();
            unsafe {
                let dev = self.raw_device.as_mut_ptr();
                // Slot 1: tris StructuredBuffer SRV.
                let mut tri_srv: d12::D3D12_SHADER_RESOURCE_VIEW_DESC = zeroed();
                tri_srv.Format = DXGI_FORMAT_UNKNOWN;
                tri_srv.ViewDimension = d12::D3D12_SRV_DIMENSION_BUFFER;
                tri_srv.Shader4ComponentMapping = 0x0000_1688;
                *tri_srv.u.Buffer_mut() = d12::D3D12_BUFFER_SRV {
                    FirstElement: 0,
                    NumElements: self.scene.tri_count,
                    StructureByteStride: size_of::<RayTriangle>() as UINT,
                    Flags: d12::D3D12_BUFFER_SRV_FLAG_NONE,
                };
                (*dev).CreateShaderResourceView(
                    self.scene.tris.resource,
                    &tri_srv,
                    self.cpu_handle(1),
                );

                // Slot 2: materials StructuredBuffer SRV.
                let mut mat_srv: d12::D3D12_SHADER_RESOURCE_VIEW_DESC = zeroed();
                mat_srv.Format = DXGI_FORMAT_UNKNOWN;
                mat_srv.ViewDimension = d12::D3D12_SRV_DIMENSION_BUFFER;
                mat_srv.Shader4ComponentMapping = 0x0000_1688;
                *mat_srv.u.Buffer_mut() = d12::D3D12_BUFFER_SRV {
                    FirstElement: 0,
                    NumElements: self.scene.mat_count,
                    StructureByteStride: size_of::<RayMaterial>() as UINT,
                    Flags: d12::D3D12_BUFFER_SRV_FLAG_NONE,
                };
                (*dev).CreateShaderResourceView(
                    self.scene.materials.resource,
                    &mat_srv,
                    self.cpu_handle(2),
                );
            }
        }

        fn write_target_descriptors(&self) {
            unsafe {
                let dev = self.raw_device.as_mut_ptr();
                // Slots 3..5: t3..t5 G-buffer input Texture2D SRVs.
                let in_specs = [
                    (
                        self.targets.in_pos.resource.as_mut_ptr(),
                        DXGI_FORMAT_R32G32B32A32_FLOAT,
                    ),
                    (
                        self.targets.in_norm.resource.as_mut_ptr(),
                        DXGI_FORMAT_R16G16B16A16_FLOAT,
                    ),
                    (
                        self.targets.in_raster.resource.as_mut_ptr(),
                        DXGI_FORMAT_R16G16B16A16_FLOAT,
                    ),
                    (
                        self.targets.in_sky_env.resource.as_mut_ptr(),
                        DXGI_FORMAT_R16G16B16A16_FLOAT,
                    ),
                    (
                        self.targets.in_terrain_map.resource.as_mut_ptr(),
                        DXGI_FORMAT_R16G16B16A16_FLOAT,
                    ),
                ];
                for (i, (res, fmt)) in in_specs.into_iter().enumerate() {
                    let mut srv: d12::D3D12_SHADER_RESOURCE_VIEW_DESC = zeroed();
                    srv.Format = fmt;
                    srv.ViewDimension = d12::D3D12_SRV_DIMENSION_TEXTURE2D;
                    srv.Shader4ComponentMapping = 0x0000_1688;
                    *srv.u.Texture2D_mut() = d12::D3D12_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                        PlaneSlice: 0,
                        ResourceMinLODClamp: 0.0,
                    };
                    (*dev).CreateShaderResourceView(res, &srv, self.cpu_handle(3 + i as u32));
                }

                // Slots 6..10: u0..u4 output Texture2D UAVs.
                let mut uav: d12::D3D12_UNORDERED_ACCESS_VIEW_DESC = zeroed();
                uav.Format = DXGI_FORMAT_R16G16B16A16_FLOAT;
                uav.ViewDimension = d12::D3D12_UAV_DIMENSION_TEXTURE2D;
                let resources = [
                    self.targets.colour.resource.as_mut_ptr(),
                    self.targets.guide.resource.as_mut_ptr(),
                    self.targets.reflection.resource.as_mut_ptr(),
                    self.targets.normals.resource.as_mut_ptr(),
                    self.targets.albedo.resource.as_mut_ptr(),
                ];
                for (i, res) in resources.into_iter().enumerate() {
                    (*dev).CreateUnorderedAccessView(
                        res,
                        null_mut(),
                        &uav,
                        self.cpu_handle(8 + i as u32),
                    );
                }

                // Slot 13: u5 persistent 3D world-space radiance cache (`RWStructuredBuffer<float4>`).
                let mut rc_uav: d12::D3D12_UNORDERED_ACCESS_VIEW_DESC = zeroed();
                rc_uav.Format = DXGI_FORMAT_UNKNOWN;
                rc_uav.ViewDimension = d12::D3D12_UAV_DIMENSION_BUFFER;
                *rc_uav.u.Buffer_mut() = d12::D3D12_BUFFER_UAV {
                    FirstElement: 0,
                    NumElements: RADIANCE_CACHE_CELLS,
                    StructureByteStride: 16,
                    CounterOffsetInBytes: 0,
                    Flags: d12::D3D12_BUFFER_UAV_FLAG_NONE,
                };
                (*dev).CreateUnorderedAccessView(
                    self.radiance_cache.resource,
                    null_mut(),
                    &rc_uav,
                    self.cpu_handle(13),
                );

                // Slot 14: u6 ReSTIR GI reservoirs (two pixel-sized frames, 64-byte records).
                let mut restir_uav: d12::D3D12_UNORDERED_ACCESS_VIEW_DESC = zeroed();
                restir_uav.Format = DXGI_FORMAT_UNKNOWN;
                restir_uav.ViewDimension = d12::D3D12_UAV_DIMENSION_BUFFER;
                *restir_uav.u.Buffer_mut() = d12::D3D12_BUFFER_UAV {
                    FirstElement: 0,
                    NumElements: if self.restir_reservoirs_enabled {
                        (self.targets.extent.0 as u64 * self.targets.extent.1 as u64 * 2) as u32
                    } else {
                        1
                    },
                    StructureByteStride: size_of::<RestirReservoir>() as UINT,
                    CounterOffsetInBytes: 0,
                    Flags: d12::D3D12_BUFFER_UAV_FLAG_NONE,
                };
                (*dev).CreateUnorderedAccessView(
                    self.restir_reservoirs.resource,
                    null_mut(),
                    &restir_uav,
                    self.cpu_handle(14),
                );
            }
        }

        /// Copy the wgpu-generated 1:1 depth-derived G-buffer planes into the
        /// shared DX12 input textures. Call on `encoder` and submit `encoder`
        /// to `queue` before calling [`Self::trace`].
        #[allow(clippy::too_many_arguments)]
        pub fn copy_gbuf_inputs(
            &mut self,
            encoder: &mut wgpu::CommandEncoder,
            src_pos: &wgpu::Texture,
            src_norm: &wgpu::Texture,
            src_raster: &wgpu::Texture,
            src_sky_env: &wgpu::Texture,
            copy_sky_env: bool,
            src_terrain_map: &wgpu::Texture,
            copy_terrain_map: bool,
        ) {
            let (tw, th) = self.targets.extent;
            let force_sky = !self.targets.wgpu_tracked_copy_dst || copy_sky_env;
            let force_terrain = !self.targets.wgpu_tracked_copy_dst || copy_terrain_map;
            let pairs = [
                (src_pos, &self.targets.in_pos.texture, tw, th, true),
                (src_norm, &self.targets.in_norm.texture, tw, th, true),
                (src_raster, &self.targets.in_raster.texture, tw, th, true),
                (
                    src_sky_env,
                    &self.targets.in_sky_env.texture,
                    SKY_ENV_WIDTH,
                    SKY_ENV_HEIGHT,
                    force_sky,
                ),
                (
                    src_terrain_map,
                    &self.targets.in_terrain_map.texture,
                    TERRAIN_MAP_SIZE,
                    TERRAIN_MAP_SIZE,
                    force_terrain,
                ),
            ];
            for (src, dst, mw, mh, should_copy) in pairs {
                if !should_copy {
                    continue;
                }
                let copy_extent = wgpu::Extent3d {
                    width: mw.min(src.width()).min(dst.width()),
                    height: mh.min(src.height()).min(dst.height()),
                    depth_or_array_layers: 1,
                };
                encoder.copy_texture_to_texture(
                    wgpu::ImageCopyTexture {
                        texture: src,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyTexture {
                        texture: dst,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    copy_extent,
                );
            }
            self.targets.wgpu_tracked_copy_dst = true;
        }

        fn gc_retired(&mut self) {
            // Opt #24: Skip DX12 ID3D12Fence::GetCompletedValue COM call when no retired resources exist.
            if self.retired_buffers.is_empty() && self.retired_scenes.is_empty() {
                return;
            }
            let done = self.ring.completed_fence();
            self.retired_buffers.retain(|(f, _)| *f > done);
            self.retired_scenes.retain(|(f, _)| *f > done);
        }

        pub fn set_scene(&mut self, scene: &Arc<SceneGeometry>) -> Result<(), String> {
            self.gc_retired();
            let prev_fence = self.ring.current_fence();
            let (new_scene, staging, build_fence) = Self::build_scene(
                self.raw_device.as_mut_ptr(),
                self.device5,
                &mut self.ring,
                &self.raw_queue,
                scene,
                &mut self.reusable_scratch,
                &mut self.reusable_instance_desc,
            )?;
            let old_scene = std::mem::replace(&mut self.scene, new_scene);
            self.retired_scenes.push((prev_fence, old_scene));
            self.retired_buffers.push((build_fence, staging));
            self.has_dyn_instance = false;
            self.dyn_active_tlas_va = None;
            self.dyn_last_hash = 0;
            self.write_scene_descriptors();
            Ok(())
        }

        /// Feature #1: Stream posed dynamic character & Populator NPC triangles into a
        /// race-free 3-slot fast-build dynamic BLAS + 2-instance TLAS (`InstanceID = 1`)
        /// without rebuilding the static world BLAS or allocating per-frame D3D12 buffers.
        pub fn update_dynamic_character(&mut self, verts: &[[f32; 3]]) -> Result<(), String> {
            let tri_count = (verts.len() / 3) as u32;
            if tri_count == 0 {
                if self.has_dyn_instance {
                    self.has_dyn_instance = false;
                    self.dyn_active_tlas_va = None;
                    self.dyn_last_hash = 0;
                    self.write_tlas_descriptor();
                }
                return Ok(());
            }

            // Fast fingerprint of dynamic vertices: skip BLAS/TLAS rebuild if unchanged.
            let vcount = (tri_count * 3) as usize;
            let step = (vcount / 64).max(1);
            let mut h: u64 = (vcount as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            let mut idx = 0usize;
            while idx < vcount {
                let v = verts[idx];
                h ^= (v[0].to_bits() as u64)
                    | ((v[1].to_bits() as u64) << 21)
                    | ((v[2].to_bits() as u64) << 42);
                h = h.rotate_left(13).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                idx += step;
            }
            if self.has_dyn_instance && h == self.dyn_last_hash {
                return Ok(());
            }

            let dev_ptr = self.raw_device.as_mut_ptr();
            let s = self.dyn_slot;
            self.dyn_slot = (self.dyn_slot + 1) % 3;

            unsafe {
                self.gc_retired();
                let mut retired_now: Vec<RawBuffer> = Vec::new();
                if self.dyn_inst_desc[s].is_none() {
                    let mut buf = RawBuffer::create(
                        dev_ptr,
                        (size_of::<InstanceDesc>() * 2) as u64,
                        d12::D3D12_HEAP_TYPE_UPLOAD,
                        d12::D3D12_RESOURCE_FLAG_NONE,
                        d12::D3D12_RESOURCE_STATE_GENERIC_READ,
                    )?;
                    buf.map_persistent()?;
                    self.dyn_inst_desc[s] = Some(buf);
                }

                let vbytes = (vcount * size_of::<[f32; 3]>()) as u64;
                if self.dyn_verts[s]
                    .as_ref()
                    .is_none_or(|(cap, _)| *cap < vbytes)
                {
                    if let Some((_, old)) = self.dyn_verts[s].take() {
                        retired_now.push(old);
                    }
                    let cap = align256((vbytes + vbytes / 4).max(32 * 1024));
                    let mut buf = RawBuffer::create(
                        dev_ptr,
                        cap,
                        d12::D3D12_HEAP_TYPE_UPLOAD,
                        d12::D3D12_RESOURCE_FLAG_NONE,
                        d12::D3D12_RESOURCE_STATE_GENERIC_READ,
                    )?;
                    buf.map_persistent()?;
                    self.dyn_verts[s] = Some((cap, buf));
                }
                let vbuf = &self.dyn_verts[s].as_ref().unwrap().1;
                vbuf.write_upload(bytemuck::cast_slice(&verts[..vcount]))?;

                let geom = GeometryDesc {
                    ty: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
                    flags: D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE,
                    triangles: GeometryTrianglesDesc {
                        transform3x4: 0,
                        index_format: DXGI_FORMAT_UNKNOWN,
                        vertex_format: DXGI_FORMAT_R32G32B32_FLOAT,
                        index_count: 0,
                        vertex_count: tri_count * 3,
                        index_buffer: 0,
                        vertex_buffer: GpuVaAndStride {
                            start_address: vbuf.gpu_va,
                            stride_in_bytes: size_of::<[f32; 3]>() as u64,
                        },
                    },
                };
                let blas_inputs = BuildAsInputs {
                    ty: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                    flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_BUILD,
                    num_descs: 1,
                    descs_layout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                    u: BuildInputsUnion {
                        p_geometry_descs: &geom as *const GeometryDesc,
                    },
                };
                let info = device5_get_prebuild_info(self.device5, &blas_inputs);
                if info.result_data_max_size_in_bytes == 0 {
                    return Ok(());
                }
                let bsize = align256(info.result_data_max_size_in_bytes);
                if self.dyn_blas[s]
                    .as_ref()
                    .is_none_or(|(cap, _)| *cap < bsize)
                {
                    if let Some((_, old)) = self.dyn_blas[s].take() {
                        retired_now.push(old);
                    }
                    let cap = align256(bsize + bsize / 4);
                    let buf = RawBuffer::create(
                        dev_ptr,
                        cap,
                        d12::D3D12_HEAP_TYPE_DEFAULT,
                        d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                        D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
                    )?;
                    self.dyn_blas[s] = Some((cap, buf));
                }
                let blas_gpu_va = self.dyn_blas[s].as_ref().unwrap().1.gpu_va;
                let mut scratch_needed = align256(info.scratch_data_size_in_bytes);

                let inst0 = InstanceDesc {
                    transform: [
                        [1.0, 0.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0, 0.0],
                    ],
                    instance_id_and_mask: 0xffu32 << 24,
                    hit_group_and_flags: 0,
                    acceleration_structure: self.scene._blas.gpu_va,
                };
                let inst1 = InstanceDesc {
                    transform: [
                        [1.0, 0.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0, 0.0],
                    ],
                    instance_id_and_mask: (0xffu32 << 24) | 1u32,
                    hit_group_and_flags: 0,
                    acceleration_structure: blas_gpu_va,
                };
                let insts = [inst0, inst1];
                let inst_bytes = std::slice::from_raw_parts(
                    insts.as_ptr() as *const u8,
                    size_of::<InstanceDesc>() * 2,
                );
                let inst_ref = self.dyn_inst_desc[s].as_ref().unwrap();
                inst_ref.write_upload(inst_bytes)?;

                let tlas_inputs = BuildAsInputs {
                    ty: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
                    flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_BUILD,
                    num_descs: 2,
                    descs_layout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                    u: BuildInputsUnion {
                        instance_descs: inst_ref.gpu_va,
                    },
                };
                let tlas_info = device5_get_prebuild_info(self.device5, &tlas_inputs);
                if tlas_info.result_data_max_size_in_bytes == 0 {
                    return Ok(());
                }
                scratch_needed = scratch_needed.max(align256(tlas_info.scratch_data_size_in_bytes));
                if self
                    .reusable_scratch
                    .as_ref()
                    .is_none_or(|(cap, _)| *cap < scratch_needed)
                {
                    if let Some((_, old)) = self.reusable_scratch.take() {
                        retired_now.push(old);
                    }
                    let cap = align256(scratch_needed + scratch_needed / 4);
                    let buf = RawBuffer::create(
                        dev_ptr,
                        cap,
                        d12::D3D12_HEAP_TYPE_DEFAULT,
                        d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                        d12::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    )?;
                    self.reusable_scratch = Some((cap, buf));
                }
                let tsize = align256(tlas_info.result_data_max_size_in_bytes);
                if self.dyn_tlas[s]
                    .as_ref()
                    .is_none_or(|(cap, _)| *cap < tsize)
                {
                    if let Some((_, old)) = self.dyn_tlas[s].take() {
                        retired_now.push(old);
                    }
                    let cap = align256(tsize + tsize / 4);
                    let buf = RawBuffer::create(
                        dev_ptr,
                        cap,
                        d12::D3D12_HEAP_TYPE_DEFAULT,
                        d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                        D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
                    )?;
                    self.dyn_tlas[s] = Some((cap, buf));
                }
                let tlas_gpu_va = self.dyn_tlas[s].as_ref().unwrap().1.gpu_va;
                let scratch_va = self.reusable_scratch.as_ref().unwrap().1.gpu_va;

                let slot_idx = self.ring.acquire("DXR-DynAS")?;
                let list_ptr = self.ring.slot_mut(slot_idx).list.as_mut_ptr();
                let list4 = com_query_interface(
                    list_ptr as *mut c_void,
                    &IID_ID3D12_GRAPHICS_COMMAND_LIST4,
                )
                .map_err(|hr| format!("ID3D12GraphicsCommandList4 unavailable (0x{hr:08x})"))?;

                let blas_desc = BuildAsDesc {
                    dest_as_data: blas_gpu_va,
                    inputs: blas_inputs,
                    source_as_data: 0,
                    scratch_as_data: scratch_va,
                };
                cmdlist4_build_as(list4, &blas_desc);

                let mut uav_b: d12::D3D12_RESOURCE_BARRIER = zeroed();
                uav_b.Type = d12::D3D12_RESOURCE_BARRIER_TYPE_UAV;
                uav_b.u.UAV_mut().pResource = null_mut();
                (*list_ptr).ResourceBarrier(1, &uav_b);

                let tlas_desc = BuildAsDesc {
                    dest_as_data: tlas_gpu_va,
                    inputs: tlas_inputs,
                    source_as_data: 0,
                    scratch_as_data: scratch_va,
                };
                cmdlist4_build_as(list4, &tlas_desc);
                (*list_ptr).ResourceBarrier(1, &uav_b);

                com_release(list4);
                self.ring.retire(slot_idx, &self.raw_queue, "DXR-DynAS")?;
                let fence = self.ring.current_fence();
                if !retired_now.is_empty() {
                    self.retired_buffers.push((fence, retired_now));
                }
                self.has_dyn_instance = true;
                self.dyn_active_tlas_va = Some(tlas_gpu_va);
                self.dyn_last_hash = h;
                self.write_tlas_descriptor();
            }
            Ok(())
        }

        /// Invalidate temporal ReSTIR history when the renderer switches tracing backends.
        pub fn invalidate_restir_history(&mut self) {
            self.restir_history_valid = false;
        }

        pub fn resize(
            &mut self,
            device: &wgpu::Device,
            grid_extent: (u32, u32),
        ) -> Result<(), String> {
            let target = (grid_extent.0.max(1), grid_extent.1.max(1));
            if self.targets.extent == target {
                return Ok(());
            }
            self.ring.wait_idle();
            self.targets = DxrTargets::new(device, &self.raw_device, target)?;
            let reservoir_bytes = u64::from(target.0)
                .saturating_mul(u64::from(target.1))
                .saturating_mul(2)
                .saturating_mul(size_of::<RestirReservoir>() as u64);
            self.restir_reservoirs_enabled = reservoir_bytes <= device.limits().max_buffer_size;
            self.restir_reservoirs = unsafe {
                RawBuffer::create(
                    self.raw_device.as_mut_ptr(),
                    if self.restir_reservoirs_enabled { reservoir_bytes.max(64) } else { 64 },
                    d12::D3D12_HEAP_TYPE_DEFAULT,
                    d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                    d12::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                )?
            };
            self.restir_history_valid = false;
            self.write_target_descriptors();
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        pub fn trace(
            &mut self,
            encoder: &mut wgpu::CommandEncoder,
            params: &RayParams,
            grid_extent: (u32, u32),
            dst_colour: &wgpu::Texture,
            dst_guide: Option<&wgpu::Texture>,
            dst_reflection: &wgpu::Texture,
            dst_normals: &wgpu::Texture,
            dst_albedo: Option<&wgpu::Texture>,
        ) -> Result<(), String> {
            let (w, h) = (grid_extent.0.max(1), grid_extent.1.max(1));
            unsafe {
                self.gc_retired();
                let slot_idx = self.ring.acquire("DXR-Trace")?;
                let mut dxr_params = *params;
                dxr_params.fog[3] = self.scene.opaque_tri_count as f32;
                dxr_params.restir[0] = if self.restir_reservoirs_enabled { 1.0 } else { 0.0 };
                dxr_params.frame[3] = if self.restir_history_valid && params.frame[3] > 0.5 {
                    1.0
                } else {
                    0.0
                };
                self.cbvs[slot_idx].write_upload(bytemuck::bytes_of(&dxr_params))?;
                let list = self.ring.slot_mut(slot_idx).list.as_mut_ptr();

                let in_resources = [
                    self.targets.in_pos.resource.as_mut_ptr(),
                    self.targets.in_norm.resource.as_mut_ptr(),
                    self.targets.in_raster.resource.as_mut_ptr(),
                    self.targets.in_sky_env.resource.as_mut_ptr(),
                    self.targets.in_terrain_map.resource.as_mut_ptr(),
                ];
                let in_wgpu_state = if self.targets.wgpu_tracked_copy_dst {
                    d12::D3D12_RESOURCE_STATE_COPY_DEST
                } else {
                    d12::D3D12_RESOURCE_STATE_COMMON
                };
                let resources = [
                    self.targets.colour.resource.as_mut_ptr(),
                    self.targets.guide.resource.as_mut_ptr(),
                    self.targets.reflection.resource.as_mut_ptr(),
                    self.targets.normals.resource.as_mut_ptr(),
                    self.targets.albedo.resource.as_mut_ptr(),
                ];
                // Boost #10: Only transition output UAV planes that will actually be copied
                // back to wgpu; unused planes stay in COMMON/UAV without barrier flushes.
                let out_used = [true, dst_guide.is_some(), true, true, dst_albedo.is_some()];
                let mut pre_barriers: [d12::D3D12_RESOURCE_BARRIER; 11] = zeroed();
                let mut pre_count = 0usize;
                for res in in_resources {
                    let b = &mut pre_barriers[pre_count];
                    b.Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                    *b.u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: res,
                        Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: in_wgpu_state,
                        StateAfter: d12::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    };
                    pre_count += 1;
                }
                for i in 0..5 {
                    if self.targets.uav_resident[i] {
                        continue;
                    }
                    let b = &mut pre_barriers[pre_count];
                    b.Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                    *b.u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: resources[i],
                        Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: if self.targets.wgpu_tracked_copy_src[i] {
                            d12::D3D12_RESOURCE_STATE_COPY_SOURCE
                        } else {
                            d12::D3D12_RESOURCE_STATE_COMMON
                        },
                        StateAfter: d12::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    };
                    self.targets.uav_resident[i] = true;
                    pre_count += 1;
                }
                // Order the previous frame's UAV writes before the next dispatch
                // reads the previous parity and overwrites the current one.
                let reservoir_barrier = &mut pre_barriers[pre_count];
                reservoir_barrier.Type = d12::D3D12_RESOURCE_BARRIER_TYPE_UAV;
                *reservoir_barrier.u.UAV_mut() = d12::D3D12_RESOURCE_UAV_BARRIER {
                    pResource: self.restir_reservoirs.resource,
                };
                pre_count += 1;
                (*list).ResourceBarrier(pre_count as UINT, pre_barriers.as_ptr());

                let mut heaps = [self.desc_heap];
                (*list).SetDescriptorHeaps(1, heaps.as_mut_ptr());
                (*list).SetComputeRootSignature(self.root_sig);
                (*list).SetPipelineState(self.pso);
                (*list).SetComputeRootConstantBufferView(0, self.cbvs[slot_idx].gpu_va);
                (*list).SetComputeRootDescriptorTable(1, self.gpu_handle(0));
                (*list).SetComputeRootDescriptorTable(2, self.gpu_handle(8));
                (*list).Dispatch(w.div_ceil(8), h.div_ceil(8), 1); // Opt #13: 8x8 threadgroup

                let mut post_barriers: [d12::D3D12_RESOURCE_BARRIER; 10] = zeroed();
                let mut post_count = 0usize;
                for res in in_resources {
                    let b = &mut post_barriers[post_count];
                    b.Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                    *b.u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: res,
                        Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: d12::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        StateAfter: in_wgpu_state,
                    };
                    post_count += 1;
                }
                for i in 0..5 {
                    if !out_used[i] {
                        // Leave unused plane in UNORDERED_ACCESS so future frames skip
                        // both pre- and post-barriers until a mode toggle copies it.
                        continue;
                    }
                    let b = &mut post_barriers[post_count];
                    b.Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                    *b.u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: resources[i],
                        Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: d12::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                        // Return to whatever state wgpu's DX12 tracker expects:
                        // COMMON until wgpu has copied from this target once,
                        // COPY_SOURCE thereafter.
                        StateAfter: if self.targets.wgpu_tracked_copy_src[i] {
                            d12::D3D12_RESOURCE_STATE_COPY_SOURCE
                        } else {
                            d12::D3D12_RESOURCE_STATE_COMMON
                        },
                    };
                    self.targets.uav_resident[i] = false;
                    post_count += 1;
                }
                (*list).ResourceBarrier(post_count as UINT, post_barriers.as_ptr());

                self.ring.retire(slot_idx, &self.raw_queue, "DXR-Trace")?;
            }
            self.restir_history_valid = self.restir_reservoirs_enabled;

            let copy_extent = wgpu::Extent3d {
                width: w.min(self.targets.extent.0).min(dst_colour.width()),
                height: h.min(self.targets.extent.1).min(dst_colour.height()),
                depth_or_array_layers: 1,
            };
            // Copy only the planes the active denoiser/reconstructor consumes,
            // marking each copied plane as tracked in COPY_SOURCE by wgpu.
            let pairs = [
                (&self.targets.colour.texture, Some(dst_colour)),
                (&self.targets.guide.texture, dst_guide),
                (&self.targets.reflection.texture, Some(dst_reflection)),
                (&self.targets.normals.texture, Some(dst_normals)),
                (&self.targets.albedo.texture, dst_albedo),
            ];
            for (idx, (src, dst_opt)) in pairs.into_iter().enumerate() {
                let Some(dst) = dst_opt else { continue };
                self.targets.wgpu_tracked_copy_src[idx] = true;
                encoder.copy_texture_to_texture(
                    wgpu::ImageCopyTexture {
                        texture: src,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyTexture {
                        texture: dst,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    copy_extent,
                );
            }
            Ok(())
        }
    }
}
