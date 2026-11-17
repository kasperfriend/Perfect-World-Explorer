//! Windows + DX12 implementation of the FSR upscaler. See the module-level
//! documentation in `super` for the overall design and its limits.

use super::mv::{FsrMotionVectorPass, FsrMvParams, COMPOSITE_SHADER};
use glam::Mat4;
use std::ffi::c_void;
use winapi::shared::dxgiformat::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_TYPELESS, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, DXGI_FORMAT_R16G16_FLOAT, DXGI_FORMAT_R32_FLOAT,
    DXGI_FORMAT_R8G8B8A8_TYPELESS, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
};
use winapi::um::d3d12 as d3d12_ty;

pub(crate) const FFX_UPSCALER_VERSION: u32 = (4 << 22) | (1 << 12) | 2;
pub(crate) const FFX_API_CREATE_CONTEXT_DESC_TYPE_UPSCALE: u64 = 0x00010000;
pub(crate) const FFX_API_DISPATCH_DESC_TYPE_UPSCALE: u64 = 0x00010001;
pub(crate) const FFX_API_QUERY_DESC_TYPE_UPSCALE_GETJITTERPHASECOUNT: u64 = 0x00010004;
pub(crate) const FFX_API_QUERY_DESC_TYPE_UPSCALE_GETJITTEROFFSET: u64 = 0x00010005;
pub(crate) const FFX_API_CREATE_CONTEXT_DESC_TYPE_BACKEND_DX12: u64 = 0x00000002;
pub(crate) const FFX_API_QUERY_DESC_TYPE_GET_VERSIONS: u64 = 4;
pub(crate) const FFX_API_DESC_TYPE_OVERRIDE_VERSION: u64 = 5;
pub(crate) const FFX_API_CONFIGURE_DESC_TYPE_GLOBALDEBUG: u64 = 7;
pub(crate) const FFX_API_CONFIGURE_GLOBALDEBUG_LEVEL_ERRORS: u32 = 0x1;
pub(crate) const FFX_API_CONFIGURE_GLOBALDEBUG_LEVEL_WARNINGS: u32 = 0x2;
pub(crate) const FFX_API_CREATE_CONTEXT_DESC_TYPE_UPSCALE_VERSION: u64 = 0x0001000b;
pub(crate) const FFX_API_RESOURCE_STATE_UNORDERED_ACCESS: u32 = 1 << 1;
pub(crate) const FFX_API_RESOURCE_STATE_PIXEL_COMPUTE_READ: u32 = (1 << 2) | (1 << 3);
/// `FFX_API_RESOURCE_STATE_RENDER_TARGET` (`ffx_api_types.h`): a resource the
/// application renders into. The frame-generation `HUDLessColor` is exactly
/// that — the composition target — and FFX wants the state it is in, not the
/// state FFX will use.
pub(crate) const FFX_API_RESOURCE_STATE_RENDER_TARGET: u32 = 1 << 8;
pub(crate) const FFX_API_RESOURCE_USAGE_READ_ONLY: u32 = 0;
pub(crate) const FFX_API_RESOURCE_USAGE_UAV: u32 = 1 << 1;
pub(crate) const FFX_API_RESOURCE_USAGE_DEPTHTARGET: u32 = 1 << 2;
pub(crate) const FFX_API_RESOURCE_TYPE_TEXTURE2D: u32 = 2;
pub(crate) const FFX_API_SURFACE_FORMAT_R16G16_FLOAT: u32 = 18;
pub(crate) const FFX_API_SURFACE_FORMAT_R32_FLOAT: u32 = 28;
pub(crate) const FFX_API_SURFACE_FORMAT_R8G8B8A8_UNORM: u32 = 10;
pub(crate) const FFX_API_SURFACE_FORMAT_B8G8R8A8_UNORM: u32 = 14;
pub(crate) const FFX_API_SURFACE_FORMAT_B8G8R8A8_SRGB: u32 = 15;
pub(crate) const FFX_API_SURFACE_FORMAT_R8G8B8A8_SRGB: u32 = 12;
const FFX_UPSCALE_ENABLE_DEPTH_INVERTED: u32 = 1 << 3;
const FFX_UPSCALE_ENABLE_NON_LINEAR_COLORSPACE: u32 = 1 << 8;
const FFX_UPSCALE_FLAG_NON_LINEAR_COLOR_SRGB: u32 = 1 << 1;

// D3D12 states the frame loop leaves the native targets in before the FFX
// command list runs, and the states it restores them to afterwards so wgpu's
// own tracker stays truthful on the following frame.
pub(crate) const D3D12_STATE_COMMON: u32 = d3d12_ty::D3D12_RESOURCE_STATE_COMMON;
pub(crate) const D3D12_STATE_COPY_DEST: u32 = d3d12_ty::D3D12_RESOURCE_STATE_COPY_DEST;
pub(crate) const D3D12_STATE_RENDER_TARGET: u32 = d3d12_ty::D3D12_RESOURCE_STATE_RENDER_TARGET;
pub(crate) const D3D12_STATE_UNORDERED_ACCESS: u32 =
    d3d12_ty::D3D12_RESOURCE_STATE_UNORDERED_ACCESS;
pub(crate) const D3D12_STATE_PIXEL_COMPUTE_READ: u32 =
    d3d12_ty::D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
        | d3d12_ty::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct FfxApiHeader {
    pub(crate) r#type: u64,
    pub(crate) p_next: *mut FfxApiHeader,
}

impl FfxApiHeader {
    pub(crate) fn new(r#type: u64) -> Self {
        Self {
            r#type,
            p_next: std::ptr::null_mut(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct FfxApiDimensions2D {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct FfxApiFloatCoords2D {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct FfxApiResourceDescription {
    pub(crate) r#type: u32,
    pub(crate) format: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) depth: u32,
    pub(crate) mip_count: u32,
    pub(crate) flags: u32,
    pub(crate) usage: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct FfxApiResource {
    pub(crate) resource: *mut c_void,
    pub(crate) description: FfxApiResourceDescription,
    pub(crate) state: u32,
}

impl FfxApiResource {
    pub(crate) const EMPTY: FfxApiResource = FfxApiResource {
        resource: std::ptr::null_mut(),
        description: FfxApiResourceDescription {
            r#type: 0,
            format: 0,
            width: 0,
            height: 0,
            depth: 0,
            mip_count: 0,
            flags: 0,
            usage: 0,
        },
        state: 0,
    };

    pub(crate) fn texture2d(
        resource: &d3d12::Resource,
        format: u32,
        width: u32,
        height: u32,
        usage: u32,
        state: u32,
    ) -> Self {
        Self {
            resource: resource.as_mut_ptr() as *mut c_void,
            description: FfxApiResourceDescription {
                r#type: FFX_API_RESOURCE_TYPE_TEXTURE2D,
                format,
                width,
                height,
                depth: 1,
                mip_count: 1,
                flags: 0,
                usage,
            },
            state,
        }
    }
}

#[repr(C)]
struct FfxCreateContextDescUpscale {
    header: FfxApiHeader,
    flags: u32,
    max_render_size: FfxApiDimensions2D,
    max_upscale_size: FfxApiDimensions2D,
    fp_message: *mut c_void,
}

#[repr(C)]
pub(crate) struct FfxCreateBackendDX12Desc {
    pub(crate) header: FfxApiHeader,
    pub(crate) device: *mut c_void,
}

#[repr(C)]
pub(crate) struct FfxOverrideVersion {
    pub(crate) header: FfxApiHeader,
    pub(crate) version_id: u64,
}

/// Required by every SDK 2.x upscaler provider ("As of SDK 2.1 it is
/// necessary to create and fill in a ffxCreateContextDescUpscaleVersion ...
/// and link this in the header.pNext field of ffxCreateContextDescUpscale").
/// Without it ffxCreateContext refuses to create the context.
#[repr(C)]
pub(crate) struct FfxCreateContextDescUpscaleVersion {
    pub(crate) header: FfxApiHeader,
    pub(crate) version: u32,
}

/// Registers the application-wide FFX diagnostic message callback. The
/// loader ignores effect_id for this descriptor type (the callback is
/// process-global), so a single call covers every effect.
#[repr(C)]
pub(crate) struct FfxConfigureDescGlobalDebug {
    pub(crate) header: FfxApiHeader,
    pub(crate) effect_id: u64,
    pub(crate) fp_message: *mut c_void,
    pub(crate) debug_level: u32,
}

#[repr(C)]
pub(crate) struct FfxQueryDescGetVersions {
    pub(crate) header: FfxApiHeader,
    pub(crate) create_desc_type: u64,
    pub(crate) device: *mut c_void,
    pub(crate) output_count: *mut u64,
    pub(crate) version_ids: *mut u64,
    pub(crate) version_names: *mut *const std::os::raw::c_char,
}

#[repr(C)]
struct FfxQueryDescUpscaleGetJitterPhaseCount {
    header: FfxApiHeader,
    render_width: u32,
    display_width: u32,
    out_phase_count: *mut i32,
}

#[repr(C)]
struct FfxQueryDescUpscaleGetJitterOffset {
    header: FfxApiHeader,
    index: i32,
    phase_count: i32,
    out_x: *mut f32,
    out_y: *mut f32,
}

#[repr(C)]
struct FfxDispatchDescUpscale {
    header: FfxApiHeader,
    command_list: *mut c_void,
    color: FfxApiResource,
    depth: FfxApiResource,
    motion_vectors: FfxApiResource,
    exposure: FfxApiResource,
    reactive: FfxApiResource,
    transparency_and_composition: FfxApiResource,
    output: FfxApiResource,
    jitter_offset: FfxApiFloatCoords2D,
    motion_vector_scale: FfxApiFloatCoords2D,
    render_size: FfxApiDimensions2D,
    upscale_size: FfxApiDimensions2D,
    enable_sharpening: bool,
    sharpness: f32,
    frame_time_delta: f32,
    pre_exposure: f32,
    reset: bool,
    camera_near: f32,
    camera_far: f32,
    camera_fov_angle_vertical: f32,
    view_space_to_meters_factor: f32,
    flags: u32,
}

pub(crate) type FfxReturnCode = u32;
pub(crate) const FFX_API_RETURN_OK: FfxReturnCode = 0;

pub(crate) type PfnFfxCreateContext =
    unsafe extern "C" fn(*mut *mut c_void, *mut FfxApiHeader, *const c_void) -> FfxReturnCode;
pub(crate) type PfnFfxDestroyContext =
    unsafe extern "C" fn(*mut *mut c_void, *const c_void) -> FfxReturnCode;
pub(crate) type PfnFfxQuery =
    unsafe extern "C" fn(*mut *mut c_void, *mut FfxApiHeader) -> FfxReturnCode;
pub(crate) type PfnFfxDispatch =
    unsafe extern "C" fn(*mut *mut c_void, *const FfxApiHeader) -> FfxReturnCode;
pub(crate) type PfnFfxConfigure =
    unsafe extern "C" fn(*mut *mut c_void, *const FfxApiHeader) -> FfxReturnCode;

extern "C" fn fsr_message_callback(message_type: u32, message: *const u16) {
    if message.is_null() {
        return;
    }
    let len = unsafe {
        let mut len = 0usize;
        while *message.add(len) != 0 && len < 4096 {
            len += 1;
        }
        len
    };
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(message, len) });
    super::diag(&format!(
        "[FSR {}] {text}",
        if message_type == 0 {
            "error"
        } else {
            "warning"
        }
    ));
}

pub(crate) struct FfxLoader {
    pub(crate) handle: winapi::shared::minwindef::HMODULE,
    pub(crate) create_context: PfnFfxCreateContext,
    pub(crate) destroy_context: PfnFfxDestroyContext,
    pub(crate) query: PfnFfxQuery,
    pub(crate) dispatch: PfnFfxDispatch,
    pub(crate) configure: PfnFfxConfigure,
}

impl Drop for FfxLoader {
    fn drop(&mut self) {
        unsafe {
            winapi::um::libloaderapi::FreeLibrary(self.handle);
        }
    }
}

impl FfxLoader {
    pub(crate) fn load(sdk_dir: &str) -> Result<FfxLoader, String> {
        use std::os::windows::ffi::OsStrExt;
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        let trimmed = sdk_dir.trim();
        if !trimmed.is_empty() {
            candidates.push(std::path::Path::new(trimmed).join("amd_fidelityfx_loader_dx12.dll"));
        }
        if let Some(dir) = std::env::var_os("PW_UPSCALER_SDK_DIR") {
            candidates.push(std::path::Path::new(&dir).join("amd_fidelityfx_loader_dx12.dll"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("sdk").join("amd_fidelityfx_loader_dx12.dll"));
            }
            // Cargo layout: `<repo>/target/<profile>/*.exe` -> `<repo>/sdk`.
            if let Some(repo) = exe
                .parent()
                .and_then(|dir| dir.parent())
                .and_then(|dir| dir.parent())
            {
                candidates.push(repo.join("sdk").join("amd_fidelityfx_loader_dx12.dll"));
            }
        }
        candidates.push(std::path::PathBuf::from("sdk").join("amd_fidelityfx_loader_dx12.dll"));
        let mut errors: Vec<String> = Vec::new();
        for candidate in candidates {
            if !candidate.is_file() {
                errors.push(format!("{}: not found", candidate.display()));
                continue;
            }
            let wide: Vec<u16> = candidate
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let handle = unsafe { winapi::um::libloaderapi::LoadLibraryW(wide.as_ptr()) };
            if handle.is_null() {
                errors.push(format!("{}: LoadLibraryW failed", candidate.display()));
                continue;
            }
            let get = |name: &[u8]| -> Result<*const c_void, String> {
                let proc = unsafe {
                    winapi::um::libloaderapi::GetProcAddress(handle, name.as_ptr() as *const i8)
                };
                if proc.is_null() {
                    Err(format!(
                        "GetProcAddress({}) failed",
                        String::from_utf8_lossy(name)
                    ))
                } else {
                    Ok(proc as *const c_void)
                }
            };
            let loaded: Result<FfxLoader, String> = (|| {
                Ok(FfxLoader {
                    handle,
                    create_context: unsafe { std::mem::transmute(get(b"ffxCreateContext\0")?) },
                    destroy_context: unsafe { std::mem::transmute(get(b"ffxDestroyContext\0")?) },
                    query: unsafe { std::mem::transmute(get(b"ffxQuery\0")?) },
                    dispatch: unsafe { std::mem::transmute(get(b"ffxDispatch\0")?) },
                    configure: unsafe { std::mem::transmute(get(b"ffxConfigure\0")?) },
                })
            })();
            match loaded {
                Ok(mut loader) => {
                    // The SDK only reports creation/provider problems through
                    // this callback; without it failures surface as bare
                    // return codes with no explanation.
                    let mut global_debug = FfxConfigureDescGlobalDebug {
                        header: FfxApiHeader::new(FFX_API_CONFIGURE_DESC_TYPE_GLOBALDEBUG),
                        effect_id: 0,
                        fp_message: fsr_message_callback as usize as *mut c_void,
                        debug_level: FFX_API_CONFIGURE_GLOBALDEBUG_LEVEL_ERRORS
                            | FFX_API_CONFIGURE_GLOBALDEBUG_LEVEL_WARNINGS,
                    };
                    let rc = unsafe {
                        (loader.configure)(
                            std::ptr::null_mut(),
                            &global_debug.header as *const FfxApiHeader,
                        )
                    };
                    if rc != FFX_API_RETURN_OK {
                        super::diag(&format!(
                            "[FSR] installing the FFX debug callback failed (0x{rc:x})"
                        ));
                    }
                    return Ok(loader);
                }
                Err(error) => {
                    unsafe { winapi::um::libloaderapi::FreeLibrary(handle) };
                    errors.push(format!("{}: {error}", candidate.display()));
                }
            }
        }
        Err(format!(
            "FFX loader could not be loaded ({})",
            errors.join("; ")
        ))
    }
}

pub(crate) fn is_bgra_format(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Bgra8Unorm
    )
}

/// DXGI format for the FSR *output* UAV. D3D12 forbids
/// D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS on _UNORM_SRGB formats
/// (E_INVALIDARG), so the output must use the plain UNORM twins. The output
/// is only ever sampled, so losing the sRGB view type costs nothing.
fn ffx_output_format(format: wgpu::TextureFormat) -> u32 {
    if is_bgra_format(format) {
        FFX_API_SURFACE_FORMAT_B8G8R8A8_UNORM
    } else {
        FFX_API_SURFACE_FORMAT_R8G8B8A8_UNORM
    }
}

pub(crate) fn dxgi_output_format(format: wgpu::TextureFormat) -> DXGI_FORMAT {
    if is_bgra_format(format) {
        DXGI_FORMAT_B8G8R8A8_UNORM
    } else {
        DXGI_FORMAT_R8G8B8A8_UNORM
    }
}

pub(crate) fn hr_ok(hr: winapi::shared::winerror::HRESULT) -> bool {
    hr >= 0
}

/// Committed texture + the wgpu import that mirrors it, so both the native
/// FFX command list and wgpu can address the same allocation.
pub(crate) struct NativeTexture2D {
    pub(crate) resource: d3d12::Resource,
    pub(crate) texture: wgpu::Texture,
}

/// One heap/format combination to attempt when the requested creation
/// fails, varying exactly one property from the documented default.
struct CreateFallback {
    name: &'static str,
    dxgi_format: Option<DXGI_FORMAT>,
    heap_type: d3d12_ty::D3D12_HEAP_TYPE,
    memory_pool: d3d12_ty::D3D12_MEMORY_POOL,
}

fn fallback_matrix(dxgi_format: DXGI_FORMAT, allow_format_twins: bool) -> Vec<CreateFallback> {
    const DEFAULT: d3d12_ty::D3D12_HEAP_TYPE = d3d12_ty::D3D12_HEAP_TYPE_DEFAULT;
    const CUSTOM: d3d12_ty::D3D12_HEAP_TYPE = d3d12_ty::D3D12_HEAP_TYPE_CUSTOM;
    const UNKNOWN: d3d12_ty::D3D12_MEMORY_POOL = d3d12_ty::D3D12_MEMORY_POOL_UNKNOWN;
    const L0: d3d12_ty::D3D12_MEMORY_POOL = d3d12_ty::D3D12_MEMORY_POOL_L0;
    const L1: d3d12_ty::D3D12_MEMORY_POOL = d3d12_ty::D3D12_MEMORY_POOL_L1;
    let (unorm_twin, typeless_twin) = if allow_format_twins {
        match dxgi_format {
            DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => (
                Some(DXGI_FORMAT_B8G8R8A8_UNORM),
                Some(DXGI_FORMAT_B8G8R8A8_TYPELESS),
            ),
            DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => (
                Some(DXGI_FORMAT_R8G8B8A8_UNORM),
                Some(DXGI_FORMAT_R8G8B8A8_TYPELESS),
            ),
            _ => (None, None),
        }
    } else {
        (None, None)
    };
    let mut matrix = vec![
        CreateFallback {
            name: "custom heap, L1 pool",
            dxgi_format: None,
            heap_type: CUSTOM,
            memory_pool: L1,
        },
        CreateFallback {
            name: "custom heap, L0 pool",
            dxgi_format: None,
            heap_type: CUSTOM,
            memory_pool: L0,
        },
        CreateFallback {
            name: "default heap, UNKNOWN pool",
            dxgi_format: None,
            heap_type: DEFAULT,
            memory_pool: UNKNOWN,
        },
    ];
    if let Some(twin) = unorm_twin {
        matrix.push(CreateFallback {
            name: "UNORM twin, default heap",
            dxgi_format: Some(twin),
            heap_type: DEFAULT,
            memory_pool: UNKNOWN,
        });
        matrix.push(CreateFallback {
            name: "UNORM twin, custom heap L1",
            dxgi_format: Some(twin),
            heap_type: CUSTOM,
            memory_pool: L1,
        });
        matrix.push(CreateFallback {
            name: "UNORM twin, custom heap L0",
            dxgi_format: Some(twin),
            heap_type: CUSTOM,
            memory_pool: L0,
        });
    }
    if let Some(twin) = typeless_twin {
        matrix.push(CreateFallback {
            name: "TYPELESS twin, default heap",
            dxgi_format: Some(twin),
            heap_type: DEFAULT,
            memory_pool: UNKNOWN,
        });
        matrix.push(CreateFallback {
            name: "TYPELESS twin, custom heap L1",
            dxgi_format: Some(twin),
            heap_type: CUSTOM,
            memory_pool: L1,
        });
    }
    matrix
}

impl NativeTexture2D {
    /// Which D3D12 heap this allocation actually lives in (diagnostics).
    ///
    /// NGX validates the resources it is handed, and a resource out of the
    /// custom-heap probe is not the same thing as a default-heap one — so the
    /// log names the heap that won instead of leaving it to inference.
    pub(crate) fn heap_kind(&self) -> &'static str {
        let mut properties: d3d12_ty::D3D12_HEAP_PROPERTIES = unsafe { std::mem::zeroed() };
        let mut flags: d3d12_ty::D3D12_HEAP_FLAGS = 0;
        let hr = unsafe {
            (*(self.resource.as_mut_ptr())).GetHeapProperties(&mut properties, &mut flags)
        };
        if !hr_ok(hr) {
            return "unknown";
        }
        match properties.Type {
            d3d12_ty::D3D12_HEAP_TYPE_DEFAULT => "default",
            d3d12_ty::D3D12_HEAP_TYPE_UPLOAD => "upload",
            d3d12_ty::D3D12_HEAP_TYPE_READBACK => "readback",
            d3d12_ty::D3D12_HEAP_TYPE_CUSTOM => "custom",
            _ => "unknown",
        }
    }

    /// The created resource's format and size, e.g. `0x00000057 2293x913`
    /// (diagnostics: what NGX will read off `GetDesc`).
    pub(crate) fn desc_summary(&self) -> String {
        // winapi declares `GetDesc(&self) -> D3D12_RESOURCE_DESC` (the COM
        // method returns the struct by value).
        let desc = unsafe { (*(self.resource.as_mut_ptr())).GetDesc() };
        format!(
            "0x{:08x} {}x{}",
            desc.Format as u32, desc.Width, desc.Height
        )
    }

    /// # Safety
    /// The caller guarantees the wgpu device was created by `raw_device`.
    pub(crate) unsafe fn create(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        label: &str,
        format: wgpu::TextureFormat,
        dxgi_format: DXGI_FORMAT,
        size: (u32, u32),
        resource_flags: d3d12_ty::D3D12_RESOURCE_FLAGS,
        wgpu_usage: wgpu::TextureUsages,
        initial_state: u32,
        allow_format_twins: bool,
    ) -> Result<NativeTexture2D, String> {
        let primary = Self::create_in_heap(
            device,
            raw_device,
            label,
            format,
            dxgi_format,
            size,
            resource_flags,
            wgpu_usage,
            initial_state,
            d3d12_ty::D3D12_HEAP_TYPE_DEFAULT,
            d3d12_ty::D3D12_MEMORY_POOL_UNKNOWN,
            false,
        );
        match primary {
            Ok(native) => Ok(native),
            Err(error) => {
                super::diag(&format!(
                    "[FSR] CreateCommittedResource failed for {label}: extent {}x{}, dxgi_format 0x{:08x}, flags 0x{:08x}, initial_state 0x{:08x}: {error}",
                    size.0,
                    size.1,
                    dxgi_format as u32,
                    resource_flags as u32,
                    initial_state
                ));
                // Driver-quirk fallback: retry the same request with one
                // property varied at a time, in order, until one succeeds.
                // The winning combination is used for real, so FSR can come
                // up on machines that reject the documented default.
                for fallback in fallback_matrix(dxgi_format, allow_format_twins) {
                    let (candidate_dxgi, candidate_format) = match fallback.dxgi_format {
                        Some(twin) => (twin, wgpu_format_for_dxgi(twin, format)),
                        None => (dxgi_format, format),
                    };
                    match Self::create_in_heap(
                        device,
                        raw_device,
                        label,
                        candidate_format,
                        candidate_dxgi,
                        size,
                        resource_flags,
                        wgpu_usage,
                        initial_state,
                        fallback.heap_type,
                        fallback.memory_pool,
                        true,
                    ) {
                        Ok(native) => {
                            super::diag(&format!(
                                "[FSR] {label}: \"{}\" succeeded; continuing with it",
                                fallback.name
                            ));
                            return Ok(native);
                        }
                        Err(error) => {
                            super::diag(&format!(
                                "[FSR] probe \"{}\" for {label} failed: {error}",
                                fallback.name
                            ));
                        }
                    }
                }
                Err(error)
            }
        }
    }

    /// # Safety
    /// The caller guarantees the wgpu device was created by `raw_device`.
    unsafe fn create_in_heap(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        label: &str,
        format: wgpu::TextureFormat,
        dxgi_format: DXGI_FORMAT,
        size: (u32, u32),
        resource_flags: d3d12_ty::D3D12_RESOURCE_FLAGS,
        wgpu_usage: wgpu::TextureUsages,
        initial_state: u32,
        heap_type: d3d12_ty::D3D12_HEAP_TYPE,
        memory_pool: d3d12_ty::D3D12_MEMORY_POOL,
        log_failure: bool,
    ) -> Result<NativeTexture2D, String> {
        use winapi::shared::dxgitype::DXGI_SAMPLE_DESC;
        use winapi::Interface;
        let desc = d3d12_ty::D3D12_RESOURCE_DESC {
            Dimension: d3d12_ty::D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0, // D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT
            Width: u64::from(size.0),
            Height: size.1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: dxgi_format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: d3d12_ty::D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: resource_flags,
        };
        // `D3D12_HEAP_PROPERTIES` has one validity rule that this function used
        // to break for every heap it ever asked for. From
        // `ns-d3d12-d3d12_heap_properties` ("Valid combinations of struct
        // member values"):
        //
        //   * when `Type` is `D3D12_HEAP_TYPE_CUSTOM`, `CPUPageProperty` and
        //     `MemoryPoolPreference` must *not* be `..._UNKNOWN`;
        //   * when `Type` is **not** `D3D12_HEAP_TYPE_CUSTOM`, both must
        //     `..._UNKNOWN`.
        //
        // This passed `D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE` (= 1, not 0) with
        // `D3D12_HEAP_TYPE_DEFAULT`, so `CreateCommittedResource` answered
        // `E_INVALIDARG` (`0x80070057`) for every single texture — all sizes,
        // all formats — and each one was then created through the custom-heap
        // probe below. Those resources are legal D3D12, but they are not the
        // default-heap resources NGX builds its views and kernels against, and
        // `NVSDK_NGX_D3D12_EvaluateFeature(DLSS)` rejected them with
        // `0xbad00005 FAIL_InvalidParameter`.
        //
        // `CreationNodeMask`/`VisibleNodeMask` are not part of the problem:
        // "passing zero is equivalent to passing one, in order to simplify the
        // usage of single-GPU adapters" (same page). They stay at 1 for clarity.
        let custom = heap_type == d3d12_ty::D3D12_HEAP_TYPE_CUSTOM;
        let heap = d3d12_ty::D3D12_HEAP_PROPERTIES {
            Type: heap_type,
            CPUPageProperty: if custom {
                d3d12_ty::D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE
            } else {
                d3d12_ty::D3D12_CPU_PAGE_PROPERTY_UNKNOWN
            },
            MemoryPoolPreference: memory_pool,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let mut resource_ptr: *mut d3d12_ty::ID3D12Resource = std::ptr::null_mut();
        let hr = raw_device.CreateCommittedResource(
            &heap,
            d3d12_ty::D3D12_HEAP_FLAG_NONE,
            &desc,
            initial_state,
            std::ptr::null(),
            &d3d12_ty::ID3D12Resource::uuidof(),
            &mut resource_ptr as *mut *mut d3d12_ty::ID3D12Resource as *mut *mut c_void,
        );
        if !hr_ok(hr) || resource_ptr.is_null() {
            if log_failure {
                super::diag(&format!(
                    "[FSR] create_in_heap failed for {label}: extent {}x{}, dxgi_format 0x{:08x}, flags 0x{:08x}, initial_state 0x{:08x} (hr 0x{hr:08x})",
                    size.0,
                    size.1,
                    dxgi_format as u32,
                    resource_flags as u32,
                    initial_state
                ));
            }
            return Err(format!(
                "CreateCommittedResource failed (HRESULT 0x{hr:08x})"
            ));
        }
        // ComPtr is transparent over the raw pointer; adopt the reference
        // CreateCommittedResource handed us instead of AddRef-ing a second.
        let resource: d3d12::Resource = std::mem::transmute(resource_ptr);
        let hal_texture = device
            .as_hal::<wgpu_hal::dx12::Api, _, _>(|maybe| {
                maybe.map(|_hal_device| {
                    let size3 = wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    };
                    // Associated function (not a method): creates the hal
                    // Texture wrapper around the committed resource.
                    unsafe {
                        wgpu_hal::dx12::Device::texture_from_raw(
                            resource.clone(),
                            format,
                            wgpu::TextureDimension::D2,
                            size3,
                            1,
                            1,
                        )
                    }
                })
            })
            .ok_or_else(|| format!("the wgpu device is not a DX12 device ({label})"))?
            .ok_or_else(|| format!("the wgpu device is not a DX12 device ({label})"))?;
        let import_desc = wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu_usage,
            view_formats: &[],
        };
        let texture = unsafe {
            device.create_texture_from_hal::<wgpu_hal::dx12::Api>(hal_texture, &import_desc)
        };
        Ok(NativeTexture2D { resource, texture })
    }
}

/// wgpu format that matches a plain-UNORM/TYPELESS DXGI twin of `fallback`.
fn wgpu_format_for_dxgi(dxgi: DXGI_FORMAT, reference: wgpu::TextureFormat) -> wgpu::TextureFormat {
    let bgra = is_bgra_format(reference);
    match dxgi {
        DXGI_FORMAT_B8G8R8A8_UNORM => wgpu::TextureFormat::Bgra8Unorm,
        DXGI_FORMAT_R8G8B8A8_UNORM => wgpu::TextureFormat::Rgba8Unorm,
        // TYPELESS twins import as the plain UNORM view; the sRGB bytes are
        // never re-encoded by wgpu (raw copy only), so the view type is safe.
        DXGI_FORMAT_B8G8R8A8_TYPELESS => wgpu::TextureFormat::Bgra8Unorm,
        DXGI_FORMAT_R8G8B8A8_TYPELESS => wgpu::TextureFormat::Rgba8Unorm,
        _ if bgra => wgpu::TextureFormat::Bgra8Unorm,
        _ => wgpu::TextureFormat::Rgba8Unorm,
    }
}

pub(crate) struct FsrComposite {
    /// Passthrough pipeline: source bytes are already in the target's encoding.
    pipeline: wgpu::RenderPipeline,
    /// Decoding pipeline: the target is sRGB and would encode the upscaler's
    /// sRGB bytes a second time (see `COMPOSITE_SHADER`).
    pipeline_decode: wgpu::RenderPipeline,
    decode_source: bool,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// `(strength, 1/width, 1/height, 0)`, written per draw.
    sharpen: wgpu::Buffer,
}

impl FsrComposite {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fsr composite layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fsr composite WGSL"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fsr composite pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fsr composite"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
        });
        let pipeline_decode = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fsr composite (sRGB decode)"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_decode",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fsr composite sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let sharpen = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fsr composite sharpen"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            pipeline_decode,
            // The upscaler's output carries the encoding of its input, and the
            // input is the sRGB world target; so anything that writes into an
            // sRGB target has to decode first.
            decode_source: format.is_srgb(),
            layout,
            sampler,
            sharpen,
        }
    }

    /// Blit `source` into `target`, optionally sharpening.
    ///
    /// `sharpness` is the viewer's own RCAS-style pass (0 = off, 1 = maximum).
    /// It exists for XeSS, whose SDK has no sharpening control at all; FSR and
    /// DLSS sharpen inside their own runtimes (`ffxDispatchDescUpscale.sharpness`
    /// and `NVSDK_NGX_Parameter_Sharpness`), so they pass 0 here and are never
    /// sharpened twice.
    pub(crate) fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Texture,
        target: &wgpu::TextureView,
        sharpness: f32,
    ) {
        // The slider's 0..1 maps to a 0..1.5 unsharp gain. 1.0 alone is already a
        // visible sharpen on a 1.5x reconstruction; allowing 1.5 at the top of
        // the range makes the control unmistakably active, and the
        // neighbourhood clamp keeps it halo-free.
        let strength = sharpness.clamp(0.0, 1.0) * 1.5;
        queue.write_buffer(
            &self.sharpen,
            0,
            bytemuck::cast_slice(&[
                strength,
                1.0 / source.width().max(1) as f32,
                1.0 / source.height().max(1) as f32,
                0.0f32,
            ]),
        );
        let view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fsr composite source"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.sharpen.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fsr composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if self.decode_source {
                pass.set_pipeline(&self.pipeline_decode);
            } else {
                pass.set_pipeline(&self.pipeline);
            }
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}

/// Pick the provider version id to force. Prefers the exact version the app
/// was built against; otherwise the newest enumerated provider wins.
struct FsrNatives {
    color: NativeTexture2D,
    depth: NativeTexture2D,
    mv: NativeTexture2D,
    output: NativeTexture2D,
}

impl FsrNatives {
    /// # Safety
    /// `raw_device` must be the device `device` was created from.
    pub(crate) unsafe fn create(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        render_extent: (u32, u32),
        display_extent: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Result<FsrNatives, String> {
        // The colour the upscaler reads is the world target's sRGB-encoded
        // bytes, and every provider is told exactly that: FSR takes
        // `NON_LINEAR_COLORSPACE` (perceptual input), XeSS takes
        // `LDR_INPUT_COLOR` ("disable tonemapping for input and output") and NGX
        // takes LDR (`IsHDR` clear). Providers create their own SRV from the
        // resource *description*, so an `_UNORM_SRGB` description would make the
        // hardware decode the bytes to linear before the upscaler sees them, and
        // each of them would then convert perceptual->linear a second time
        // (XeSS: visibly darker world). The description is therefore the plain
        // UNORM twin and the bytes the world pass wrote arrive unchanged.
        let color = NativeTexture2D::create(
            device,
            raw_device,
            "fsr color",
            format,
            dxgi_output_format(format),
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_NONE,
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            false,
        )?;
        let depth = NativeTexture2D::create(
            device,
            raw_device,
            "fsr depth",
            wgpu::TextureFormat::R32Float,
            DXGI_FORMAT_R32_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            D3D12_STATE_COMMON,
            false,
        )?;
        let mv = NativeTexture2D::create(
            device,
            raw_device,
            "fsr motion vectors",
            wgpu::TextureFormat::Rg16Float,
            DXGI_FORMAT_R16G16_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            false,
        )?;
        // The UAV flag forbids _UNORM_SRGB formats (E_INVALIDARG), so the
        // output resource is created with the plain UNORM twins, imported
        // with the matching non-sRGB wgpu format, and starts life in
        // COMMON (the one initial state D3D12 accepts for UAV resources
        // besides UNORDERED_ACCESS itself). The first dispatch barrier
        // promotes it to UNORDERED_ACCESS; steady state is unchanged.
        let output = NativeTexture2D::create(
            device,
            raw_device,
            "fsr output",
            format.remove_srgb_suffix(),
            dxgi_output_format(format),
            display_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            true,
        )?;
        Ok(FsrNatives {
            color,
            depth,
            mv,
            output,
        })
    }
}

/// One slot of a round-robin native command-ring. Each slot owns its own
/// `CommandAllocator`+`GraphicsCommandList` pair, so an in-flight slot is
/// never reset while the GPU may still be executing it.
pub(crate) struct NativeCommandSlot {
    pub(crate) allocator: d3d12::CommandAllocator,
    pub(crate) list: d3d12::GraphicsCommandList,
    /// Fence value the GPU reaches once this slot's latest submission retires.
    retired: u64,
}

/// Three (allocator, list) pairs rotated per submission, guarded by a single
/// fence. The previous single-allocator design forced a full CPU/GPU
/// rendezvous every frame: the CPU blocked until the GPU retired the
/// previous list before it could even start recording the next one. With a
/// 3-slot ring the GPU typically runs one frame behind, so the slot being
/// re-acquired has retired long ago and the fence check never waits; only
/// real back-pressure (or 5 s of pathological latency) blocks.
pub(crate) struct NativeCommandRing {
    slots: [NativeCommandSlot; 3],
    next: usize,
    fence: d3d12::Fence,
    fence_event: d3d12::Event,
    fence_value: u64,
}

impl NativeCommandRing {
    pub(crate) fn new(raw_device: &d3d12::Device, tag: &str) -> Result<Self, String> {
        let (fence, hr) = raw_device.create_fence(0);
        if hr < 0 {
            return Err(format!("{tag}: create_fence failed (HRESULT 0x{hr:08x})"));
        }
        let fence_event = d3d12::Event::create(false, false);
        let mut slots = Vec::with_capacity(3);
        for _ in 0..3 {
            let (allocator, hr) = raw_device.create_command_allocator(d3d12::CmdListType::Direct);
            if hr < 0 {
                return Err(format!(
                    "{tag}: create_command_allocator failed (HRESULT 0x{hr:08x})"
                ));
            }
            let (list, hr) = raw_device.create_graphics_command_list(
                d3d12::CmdListType::Direct,
                &allocator,
                d3d12::PipelineState::null(),
                0,
            );
            if hr < 0 {
                return Err(format!(
                    "{tag}: create_graphics_command_list failed (HRESULT 0x{hr:08x})"
                ));
            }
            // Close the still-open list so every use can follow the
            // acquire -> record -> retire pattern.
            let hr = list.close();
            if hr < 0 {
                return Err(format!(
                    "{tag}: ID3D12GraphicsCommandList::Close failed (HRESULT 0x{hr:08x})"
                ));
            }
            slots.push(NativeCommandSlot {
                allocator,
                list,
                retired: 0,
            });
        }
        let slots: [NativeCommandSlot; 3] = slots
            .try_into()
            .map_err(|_| format!("{tag}: internal ring size mismatch"))?;
        Ok(Self {
            slots,
            next: 0,
            fence,
            fence_event,
            fence_value: 0,
        })
    }

    /// Wait (bounded, only if the slot's previous work has not retired),
    /// reset the slot's allocator and list, and return its index.
    pub(crate) fn acquire(&mut self, tag: &str) -> Result<usize, String> {
        let index = self.next;
        self.next = (self.next + 1) % self.slots.len();
        let slot = &mut self.slots[index];
        if slot.retired != 0 && self.fence.get_value() < slot.retired {
            let _ = self
                .fence
                .set_event_on_completion(self.fence_event, slot.retired);
            self.fence_event.wait(5_000);
        }
        slot.allocator.reset();
        let hr = slot
            .list
            .reset(&slot.allocator, d3d12::PipelineState::null());
        if hr < 0 {
            return Err(format!(
                "{tag}: ID3D12GraphicsCommandList::Reset failed (HRESULT 0x{hr:08x})"
            ));
        }
        Ok(index)
    }

    pub(crate) fn slot_mut(&mut self, index: usize) -> &mut NativeCommandSlot {
        &mut self.slots[index]
    }

    /// Close the slot's list, submit it on `queue`, and stamp it with the new
    /// fence value so the next `acquire` of this slot waits only if needed.
    pub(crate) fn retire(
        &mut self,
        index: usize,
        queue: &d3d12::CommandQueue,
        tag: &str,
    ) -> Result<(), String> {
        let slot = &mut self.slots[index];
        let hr = slot.list.close();
        if hr < 0 {
            return Err(format!(
                "{tag}: ID3D12GraphicsCommandList::Close failed (HRESULT 0x{hr:08x})"
            ));
        }
        queue.execute_command_lists(std::slice::from_ref(&slot.list.as_list()));
        self.fence_value += 1;
        slot.retired = self.fence_value;
        let _ = queue.signal(&self.fence, self.fence_value);
        Ok(())
    }

    /// Block until every submitted slot has retired (Drop paths only).
    pub(crate) fn wait_idle(&self) {
        if self.fence.get_value() >= self.fence_value {
            return;
        }
        let _ = self
            .fence
            .set_event_on_completion(self.fence_event, self.fence_value);
        self.fence_event.wait(5_000);
    }
}

pub struct FsrUpscaler {
    loader: FfxLoader,
    queue: d3d12::CommandQueue,
    ring: NativeCommandRing,
    context: *mut c_void,
    format: wgpu::TextureFormat,
    render_extent: (u32, u32),
    display_extent: (u32, u32),
    natives: FsrNatives,
    mv_pass: FsrMotionVectorPass,
    composite: FsrComposite,
    frame_index: u64,
    phase_count: i32,
    current_jitter: (f32, f32),
    prev_view_proj: Option<Mat4>,
    reset_history: bool,
    last_instant: std::time::Instant,
    version_major: u32,
    /// The provider version the loader actually advertised and the override
    /// pinned (FFX version id: major << 22 | minor << 12 | patch). `None`
    /// when enumeration failed and the loader picked the provider itself.
    selected_version: Option<u64>,
}

impl Drop for FsrUpscaler {
    fn drop(&mut self) {
        self.ring.wait_idle();
        if !self.context.is_null() {
            unsafe {
                (self.loader.destroy_context)(&mut self.context, std::ptr::null());
            }
        }
    }
}

impl FsrUpscaler {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sdk_dir: &str,
        render_extent: (u32, u32),
        display_extent: (u32, u32),
        format: wgpu::TextureFormat,
        fsr_version_major: u32,
    ) -> Result<FsrUpscaler, String> {
        let _ = queue;
        let loader = FfxLoader::load(sdk_dir)?;
        // Owned clones of the hal handles; the originals belong to wgpu.
        let (raw_device, raw_queue) = raw_dx12(device)?;
        let device_ptr = raw_device.as_mut_ptr() as *mut c_void;

        // Enumerate the providers the loader can reach and force the newest
        // FSR 4.1-compatible one available; GetVersions needs only the device
        // pointer carried by the query desc itself.
        let mut count: u64 = 0;
        let mut probe = FfxQueryDescGetVersions {
            header: FfxApiHeader::new(FFX_API_QUERY_DESC_TYPE_GET_VERSIONS),
            create_desc_type: FFX_API_CREATE_CONTEXT_DESC_TYPE_UPSCALE,
            device: device_ptr,
            output_count: &mut count,
            version_ids: std::ptr::null_mut(),
            version_names: std::ptr::null_mut(),
        };
        let mut provider_note = String::new();
        let rc = unsafe { (loader.query)(std::ptr::null_mut(), &mut probe.header) };
        let mut chosen_version: Option<u64> = None;
        // Every advertised provider id, kept so context creation can fall
        // back down the list (e.g. an advertised FSR 4.1.2 that refuses to
        // create on this GPU) instead of giving up to bilinear upscaling.
        let mut enumerated_ids: Vec<u64> = Vec::new();
        if rc == FFX_API_RETURN_OK && count > 0 {
            let capacity = (count as usize).min(1024);
            let mut ids = vec![0u64; capacity];
            count = capacity as u64;
            let mut probe = FfxQueryDescGetVersions {
                header: FfxApiHeader::new(FFX_API_QUERY_DESC_TYPE_GET_VERSIONS),
                create_desc_type: FFX_API_CREATE_CONTEXT_DESC_TYPE_UPSCALE,
                device: device_ptr,
                output_count: &mut count,
                version_ids: ids.as_mut_ptr(),
                version_names: std::ptr::null_mut(),
            };
            let rc = unsafe { (loader.query)(std::ptr::null_mut(), &mut probe.header) };
            if rc == FFX_API_RETURN_OK {
                ids.truncate((count as usize).min(ids.len()));
                ids.retain(|&id| id != 0);
                enumerated_ids = ids.clone();
                chosen_version = choose_version_for_major(&ids, fsr_version_major);
                provider_note = format!(
                    "providers {:?}, selected 0x{:x}",
                    ids.iter().map(|i| format!("0x{i:x}")).collect::<Vec<_>>(),
                    chosen_version.unwrap_or(0)
                );
            }
        } else {
            super::diag(&format!(
                "[FSR] provider enumeration returned 0x{rc:x}; the loader will pick a provider"
            ));
        }

        // Try the selected provider first, then every other advertised
        // provider newest-first, and finally the loader's own default choice
        // when the enumeration came back empty. SDK 2.x providers require a
        // ffxCreateContextDescUpscaleVersion in the chain; every advertised
        // id is trusted to be create-able, but hardware the SDK under-filters
        // still reaches us here.
        let mut override_ids: Vec<u64> = Vec::new();
        if let Some(id) = chosen_version {
            override_ids.push(id);
        }
        for id in enumerated_ids.iter().copied() {
            if Some(id) != chosen_version {
                override_ids.push(id);
            }
        }
        let candidates: Vec<Option<u64>> = if override_ids.is_empty() {
            vec![None]
        } else {
            override_ids.into_iter().map(Some).collect()
        };
        let mut context: *mut c_void = std::ptr::null_mut();
        let mut last_rc: FfxReturnCode = FFX_API_RETURN_OK;
        let mut created_override: Option<u64> = None;
        for candidate in &candidates {
            let mut backend = FfxCreateBackendDX12Desc {
                header: FfxApiHeader::new(FFX_API_CREATE_CONTEXT_DESC_TYPE_BACKEND_DX12),
                device: device_ptr,
            };
            backend.header.p_next = std::ptr::null_mut();
            let mut upscale_version = FfxCreateContextDescUpscaleVersion {
                header: FfxApiHeader::new(FFX_API_CREATE_CONTEXT_DESC_TYPE_UPSCALE_VERSION),
                version: FFX_UPSCALER_VERSION,
            };
            upscale_version.header.p_next = &mut backend.header as *mut FfxApiHeader;
            let mut override_version = candidate.map(|id| {
                let mut ov = FfxOverrideVersion {
                    header: FfxApiHeader::new(FFX_API_DESC_TYPE_OVERRIDE_VERSION),
                    version_id: id,
                };
                ov.header.p_next = &mut upscale_version.header as *mut FfxApiHeader;
                ov
            });
            let mut upscale = FfxCreateContextDescUpscale {
                header: FfxApiHeader::new(FFX_API_CREATE_CONTEXT_DESC_TYPE_UPSCALE),
                flags: FFX_UPSCALE_ENABLE_DEPTH_INVERTED | FFX_UPSCALE_ENABLE_NON_LINEAR_COLORSPACE,
                max_render_size: FfxApiDimensions2D {
                    width: render_extent.0,
                    height: render_extent.1,
                },
                max_upscale_size: FfxApiDimensions2D {
                    width: display_extent.0,
                    height: display_extent.1,
                },
                fp_message: fsr_message_callback as usize as *mut c_void,
            };
            upscale.header.p_next = match override_version.as_mut() {
                Some(ov) => &mut ov.header as *mut FfxApiHeader,
                None => &mut upscale_version.header as *mut FfxApiHeader,
            };
            let rc = unsafe {
                (loader.create_context)(&mut context, &mut upscale.header, std::ptr::null())
            };
            if rc == FFX_API_RETURN_OK && !context.is_null() {
                created_override = *candidate;
                break;
            }
            super::diag(&format!(
                "[FSR] ffxCreateContext with override {}failed (0x{rc:x}); trying the next provider",
                candidate
                    .map(|id| format!("0x{id:x} "))
                    .unwrap_or_default()
            ));
            last_rc = rc;
            context = std::ptr::null_mut();
        }
        if created_override.is_none() && context.is_null() {
            return Err(format!(
                "ffxCreateContext failed (0x{last_rc:x}); is amd_fidelityfx_upscaler_dx12.dll next to the loader?"
            ));
        }
        chosen_version = created_override;
        provider_note = if let Some(id) = created_override {
            format!("{provider_note}; created with override 0x{id:x}")
        } else {
            format!("{provider_note}; created with the loader's default provider")
        };
        super::diag(&format!(
            "[FSR] upscaler context 0x{:x} created ({provider_note})",
            context as usize
        ));

        let mut phase_count: i32 = 0;
        let mut phase_query = FfxQueryDescUpscaleGetJitterPhaseCount {
            header: FfxApiHeader::new(FFX_API_QUERY_DESC_TYPE_UPSCALE_GETJITTERPHASECOUNT),
            render_width: render_extent.0,
            display_width: display_extent.0,
            out_phase_count: &mut phase_count,
        };
        let rc = unsafe { (loader.query)(&mut context, &mut phase_query.header) };
        if rc != FFX_API_RETURN_OK || phase_count <= 0 {
            unsafe { (loader.destroy_context)(&mut context, std::ptr::null()) };
            return Err(format!(
                "ffxQueryDescUpscaleGetJitterPhaseCount failed (0x{rc:x})"
            ));
        }

        let natives = match unsafe {
            FsrNatives::create(device, &raw_device, render_extent, display_extent, format)
        } {
            Ok(natives) => natives,
            Err(error) => {
                unsafe { (loader.destroy_context)(&mut context, std::ptr::null()) };
                return Err(error);
            }
        };
        let ring = NativeCommandRing::new(&raw_device, "FSR").map_err(|e| {
            unsafe { (loader.destroy_context)(&mut context, std::ptr::null()) };
            e
        })?;
        let mv_pass = FsrMotionVectorPass::new(device);
        let composite = FsrComposite::new(device, format);

        Ok(FsrUpscaler {
            loader,
            queue: raw_queue,
            ring,
            context,
            format,
            render_extent,
            display_extent,
            natives,
            mv_pass,
            composite,
            frame_index: 0,
            phase_count,
            current_jitter: (0.0, 0.0),
            prev_view_proj: None,
            reset_history: true,
            last_instant: std::time::Instant::now(),
            version_major: fsr_version_major,
            selected_version: chosen_version,
        })
    }

    pub fn render_extent(&self) -> (u32, u32) {
        self.render_extent
    }

    pub fn display_extent(&self) -> (u32, u32) {
        self.display_extent
    }

    /// The requested FFX provider major version (0 = newest advertised).
    pub(crate) fn version_major(&self) -> u32 {
        self.version_major
    }

    /// The full FFX version id of the selected provider, when enumeration
    /// succeeded. Decoded for display with `(id >> 22) & 0x3ff` (major),
    /// `(id >> 12) & 0xff` (minor) and `id & 0xfff` (patch).
    pub fn selected_provider(&self) -> Option<u64> {
        self.selected_version
    }

    pub fn next_jitter(&mut self) -> (f32, f32) {
        let mut x = 0.0f32;
        let mut y = 0.0f32;
        let mut query = FfxQueryDescUpscaleGetJitterOffset {
            header: FfxApiHeader::new(FFX_API_QUERY_DESC_TYPE_UPSCALE_GETJITTEROFFSET),
            index: (self.frame_index % self.phase_count.max(1) as u64) as i32,
            phase_count: self.phase_count,
            out_x: &mut x,
            out_y: &mut y,
        };
        let rc = unsafe { (self.loader.query)(&mut self.context, &mut query.header) };
        if rc != FFX_API_RETURN_OK {
            super::diag(&format!(
                "[FSR] GetJitterOffset failed (0x{rc:x}); using zero jitter"
            ));
            x = 0.0;
            y = 0.0;
        }
        self.current_jitter = (x, y);
        (x, y)
    }

    /// The jittered view-projection for this frame.
    ///
    /// The same raw offsets are handed to the FFX dispatch as `jitterOffset`;
    /// [`crate::fsr::jittered_clip_vp`] explains why the shear is built in the
    /// `w` column (a constant NDC offset) rather than as a clip-space
    /// translation.
    pub fn jittered_matrix(&self, matrix: Mat4) -> Mat4 {
        crate::fsr::jittered_clip_vp(matrix, self.current_jitter, self.render_extent)
    }
}

impl FsrUpscaler {
    /// On camera cuts, teleports and FSR re-enables the upscaler history is
    /// meaningless; the next dispatch runs with `reset = true`.
    pub fn reset_history(&mut self) {
        self.reset_history = true;
        self.prev_view_proj = None;
    }

    /// Copies the world color/depth into the native targets, runs the MV pass,
    /// then dispatches FSR on its own native command list.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color: &wgpu::Texture,
        depth: &wgpu::Texture,
        object_mv: &wgpu::TextureView,
        unjittered_vp: &Mat4,
        lens: crate::fsr::CameraLens,
        sharpness: f32,
    ) -> Result<(), String> {
        let now = std::time::Instant::now();
        let frame_time_ms = (now - self.last_instant).as_secs_f32() * 1000.0;
        self.last_instant = now;

        let (rw, rh) = self.render_extent;
        let size = wgpu::Extent3d {
            width: rw.max(1),
            height: rh.max(1),
            depth_or_array_layers: 1,
        };

        // --- wgpu side: copy world color into the committed texture the
        // native list will read (bit-exact; the target is COPY_DST) and
        // gather motion vectors + a raw R32_FLOAT copy of the reversed-Z
        // depth in one fullscreen pass.
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &self.natives.color.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            size,
        );
        // --- MV pass: fullscreen depth reprojection.
        let inv_vp = unjittered_vp.inverse();
        let prev_vp = self.prev_view_proj.unwrap_or(*unjittered_vp);
        let params = FsrMvParams {
            inv_vp: inv_vp.to_cols_array_2d(),
            prev_vp: prev_vp.to_cols_array_2d(),
            render_size: [rw as f32, rh as f32],
            // A reset frame has no usable history: the matrices on either
            // side of a cut describe unrelated views, so the reprojection would
            // produce garbage vectors. Zero them, exactly as the upscaler's own
            // `reset` tells it to expect.
            prev_valid: u32::from(self.prev_view_proj.is_some() && !self.reset_history),
            pad: 0,
        };
        queue.write_buffer(self.mv_pass.params_buffer(), 0, bytemuck::bytes_of(&params));
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor {
            aspect: wgpu::TextureAspect::All,
            ..Default::default()
        });
        let mv_view = self
            .natives
            .mv
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let depth_out_view = self
            .natives
            .depth
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fsr motion vector bind group"),
            layout: self.mv_pass.layout(),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.mv_pass.params_buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(object_mv),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fsr motion vector pass"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: &mv_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &depth_out_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(self.mv_pass.pipeline());
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));

        // --- Native FFX step. The command list runs on the same direct queue
        // wgpu uses, after the submit above, and restores every state it
        // moves so wgpu's tracker (and the next frame's copies) see the
        // states they expect.
        let slot_index = self.ring.acquire("FSR")?;
        let list = &mut self.ring.slot_mut(slot_index).list;
        let subresource = d3d12_ty::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
        let none = d3d12_ty::D3D12_RESOURCE_BARRIER_FLAG_NONE;
        let barrier = |resource: &d3d12::Resource, before: u32, after: u32| {
            d3d12::ResourceBarrier::transition(resource.clone(), subresource, before, after, none)
        };
        // The output resource starts in COMMON on frame 0 (the one extra
        // state D3D12 allows for UAV resources); afterwards the restore
        // barrier below parks it in PIXEL_COMPUTE_READ.
        let output_before = if self.frame_index == 0 {
            D3D12_STATE_COMMON
        } else {
            D3D12_STATE_PIXEL_COMPUTE_READ
        };
        let barriers = [
            barrier(
                &self.natives.color.resource,
                D3D12_STATE_COPY_DEST,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
            barrier(
                &self.natives.depth.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
            barrier(
                &self.natives.mv.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
            barrier(
                &self.natives.output.resource,
                output_before,
                D3D12_STATE_UNORDERED_ACCESS,
            ),
        ];
        list.resource_barrier(&barriers);

        let mut dispatch = FfxDispatchDescUpscale {
            header: FfxApiHeader::new(FFX_API_DISPATCH_DESC_TYPE_UPSCALE),
            command_list: list.as_mut_ptr() as *mut c_void,
            color: FfxApiResource::texture2d(
                &self.natives.color.resource,
                // The resource is the UNORM twin (see `FsrNatives::create`), so
                // the view FFX makes must be UNORM too; `NON_LINEAR_COLORSPACE`
                // plus the dispatch's `NON_LINEAR_COLOR_SRGB` then tell FFX to do
                // the sRGB conversion in its own shaders.
                ffx_output_format(self.format),
                rw,
                rh,
                FFX_API_RESOURCE_USAGE_READ_ONLY,
                FFX_API_RESOURCE_STATE_PIXEL_COMPUTE_READ,
            ),
            depth: FfxApiResource::texture2d(
                &self.natives.depth.resource,
                FFX_API_SURFACE_FORMAT_R32_FLOAT,
                rw,
                rh,
                FFX_API_RESOURCE_USAGE_DEPTHTARGET,
                FFX_API_RESOURCE_STATE_PIXEL_COMPUTE_READ,
            ),
            motion_vectors: FfxApiResource::texture2d(
                &self.natives.mv.resource,
                FFX_API_SURFACE_FORMAT_R16G16_FLOAT,
                rw,
                rh,
                FFX_API_RESOURCE_USAGE_READ_ONLY,
                FFX_API_RESOURCE_STATE_PIXEL_COMPUTE_READ,
            ),
            exposure: FfxApiResource::EMPTY,
            reactive: FfxApiResource::EMPTY,
            transparency_and_composition: FfxApiResource::EMPTY,
            output: FfxApiResource::texture2d(
                &self.natives.output.resource,
                ffx_output_format(self.format),
                self.display_extent.0,
                self.display_extent.1,
                FFX_API_RESOURCE_USAGE_UAV,
                FFX_API_RESOURCE_STATE_UNORDERED_ACCESS,
            ),
            jitter_offset: FfxApiFloatCoords2D {
                x: self.current_jitter.0,
                y: self.current_jitter.1,
            },
            motion_vector_scale: FfxApiFloatCoords2D { x: 1.0, y: 1.0 },
            render_size: FfxApiDimensions2D {
                width: rw,
                height: rh,
            },
            upscale_size: FfxApiDimensions2D {
                width: self.display_extent.0,
                height: self.display_extent.1,
            },
            // FFX sharpening pass (FSR 1 gets RCAS on the blit path, so the
            // two stay consistent). `sharpness` is the Graphics slider: 0
            // disables the pass entirely (FFX ignores the value unless
            // `enableSharpening` is set) and 1 is FFX's documented maximum.
            enable_sharpening: sharpness > 0.0,
            sharpness: sharpness.clamp(0.0, 1.0),
            frame_time_delta: frame_time_ms.clamp(0.1, 1000.0),
            pre_exposure: 1.0,
            reset: self.reset_history,
            // The same numbers the projection matrix was built from. FFX uses
            // them to linearise the depth it samples; a far plane that does not
            // match the matrix skews every depth-derivative and disocclusion
            // test (ghosting on edges, blur towards the horizon).
            camera_near: lens.near,
            camera_far: lens.far,
            camera_fov_angle_vertical: lens.fov_y_radians,
            // Pure World's world unit is a metre (`gfx` positions, the camera's
            // speed settings and the fog ranges are all in metres).
            view_space_to_meters_factor: 1.0,
            flags: FFX_UPSCALE_FLAG_NON_LINEAR_COLOR_SRGB,
        };
        let rc = unsafe { (self.loader.dispatch)(&mut self.context, &dispatch.header) };
        let restore = [
            barrier(
                &self.natives.color.resource,
                D3D12_STATE_PIXEL_COMPUTE_READ,
                D3D12_STATE_COPY_DEST,
            ),
            barrier(
                &self.natives.depth.resource,
                D3D12_STATE_PIXEL_COMPUTE_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(
                &self.natives.mv.resource,
                D3D12_STATE_PIXEL_COMPUTE_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(
                &self.natives.output.resource,
                D3D12_STATE_UNORDERED_ACCESS,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
        ];
        list.resource_barrier(&restore);
        self.ring.retire(slot_index, &self.queue, "FSR")?;

        if rc != FFX_API_RETURN_OK {
            return Err(format!("ffxDispatch upscale failed (0x{rc:x})"));
        }

        self.reset_history = false;
        self.prev_view_proj = Some(*unjittered_vp);
        self.frame_index += 1;
        Ok(())
    }

    /// Composites the upscaled frame into the composition view.
    pub fn composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
    ) {
        // FFX applies its own RCAS sharpening (`ffxDispatchDescUpscale.sharpness`).
        self.composite
            .draw(device, queue, &self.natives.output.texture, target, 0.0);
    }
}

/// Owned clones of the DX12 device and direct command queue wgpu created,
/// for code that records its own native command lists against the same GPU.
///
/// # Safety
/// The caller must keep the hal handles outliving the FFX objects that use
/// them; the originals belong to wgpu.
pub(crate) fn raw_dx12(
    device: &wgpu::Device,
) -> Result<(d3d12::Device, d3d12::CommandQueue), String> {
    let raw_device = unsafe {
        device
            .as_hal::<wgpu_hal::dx12::Api, _, _>(|maybe| {
                maybe.map(|hal_device| hal_device.raw_device().clone())
            })
            .ok_or_else(|| "the wgpu device is not a DX12 device".to_string())?
            .ok_or_else(|| "the wgpu device is not a DX12 device".to_string())?
    };
    let raw_queue = unsafe {
        device
            .as_hal::<wgpu_hal::dx12::Api, _, _>(|maybe| {
                maybe.map(|hal_device| hal_device.raw_queue().clone())
            })
            .ok_or_else(|| "the wgpu device is not a DX12 device".to_string())?
            .ok_or_else(|| "the wgpu device is not a DX12 device".to_string())?
    };
    Ok((raw_device, raw_queue))
}

/// Pick the provider version id to force. Prefers the provider whose major
/// version equals `wanted_major` (2, 3 or 4; 0 = automatic); within a major,
/// the newest wins. Falls back to the newest enumerated provider overall when
/// the requested major is not advertised by the DLL.
pub(crate) fn choose_version_for_major(ids: &[u64], wanted_major: u32) -> Option<u64> {
    if ids.is_empty() {
        return None;
    }
    let major = |id: u64| ((id >> 22) & 0x3ff) as u32;
    let real = |id: u64| id != 0;
    if wanted_major == 0 {
        return ids.iter().copied().filter(|&id| real(id)).max();
    }
    ids.iter()
        .copied()
        .filter(|&id| real(id) && major(id) == wanted_major)
        .max()
        .or_else(|| {
            super::diag(&format!(
                "[FSR] no FSR major.{} provider advertised (found {:?}); using the newest available",
                wanted_major,
                ids.iter().map(|i| format!("0x{i:x}")).collect::<Vec<_>>()
            ));
            ids.iter().copied().filter(|&id| real(id)).max()
        })
}

impl FsrUpscaler {
    /// Native depth (R32_FLOAT, reversed-Z) and motion vector (Rg16Float)
    /// resources, cloned for the frame-generation prepare dispatch.
    pub(crate) fn fg_native_resources(&self) -> (d3d12::Resource, d3d12::Resource) {
        (
            self.natives.depth.resource.clone(),
            self.natives.mv.resource.clone(),
        )
    }

    /// The jitter offset handed to the upscaler this frame.
    pub(crate) fn jitter(&self) -> (f32, f32) {
        self.current_jitter
    }

    /// `true` when this frame is a history reset frame (teleport/cut).
    pub(crate) fn wants_reset(&self) -> bool {
        self.reset_history
    }
}
