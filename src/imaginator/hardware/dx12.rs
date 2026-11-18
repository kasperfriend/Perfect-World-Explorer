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
use std::path::Path;
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

/// How long the render thread will wait for one acceleration-structure build.
///
/// A build for a captured map is a few hundred milliseconds at worst; anything
/// longer means the device is gone or wedged, and blocking the frame loop on it
/// is strictly worse than turning hardware tracing off and saying so.
pub const BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The DXIL blob's location, relative to the module's own folder
/// (`Modules/Imaginator`). `tools/imaginator-shaders.sh` writes exactly this
/// path under `Modules/Imaginator/`, which is the copy the module loads.
pub const DXIL_FILE: &str = "imaginator/dxr_trace.cso";

/// The compiled RayQuery shader, if it is installed in `directory`.
///
/// One function is the single source of truth for "is the blob there", because a
/// second, subtly different answer is how the module came to disagree with itself:
/// the probe looked in a working-directory-relative path while `trace_frame` read
/// from the module directory, so the page could say "missing" for a shader that
/// was about to be used, or "present" for one that was never going to be found.
pub fn ray_query_blob(directory: &Path) -> Option<(Vec<u8>, std::path::PathBuf)> {
    let path = directory.join(DXIL_FILE);
    match std::fs::read(&path) {
        Ok(blob) if blob.len() > 4 => Some((blob, path)),
        _ => None,
    }
}

/// Where the module directory may be, most specific first.
///
/// A portable install keeps `Modules/` beside the executable; a development run
/// keeps it beside the crate. The executable's own directory is what matters, not
/// the process's working directory.
pub fn module_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(MODULE_DIR));
        }
    }
    candidates.push(std::path::PathBuf::from(MODULE_DIR));
    candidates
}

/// The module's own folder, from the working directory. Spelled here rather than
/// imported so this file type-checks on its own; `imaginator::DIRECTORY` is the
/// same string.
pub const MODULE_DIR: &str = "Modules/Imaginator";

/// The shader's `Params` block in 32-bit floats: ten `float4`/`uint4` members,
/// i.e. 160 bytes, which is `PARAMS_BYTES` below.
pub const PARAMS_FLOATS: usize = 40;

/// The shader's `Params` block: ten `float4`/`uint4` members, 160 bytes. It is
/// declared in `tools/imaginator-shaders/dxr_trace.hlsl` and mirrored field for
/// field by `imaginator::gpu::GpuParams`. The root CBV points at a GPU buffer
/// containing these bytes.
pub const PARAMS_BYTES: usize = 160;

/// `DXGI_FORMAT_R32G32B32A32_FLOAT`, the trace output's format. `winapi` spells
/// this as an enum variant, which the offline shim types as a bare integer, so it
/// is written numerically here where both agree.
const R32G32B32A32_FLOAT: u32 = 2;

/// `Buffer<float4>` strides for the per-triangle records (`tri_normals`) and the
/// material table (`materials`).
pub const NORMALS_STRIDE: usize = 16;
pub const MATERIALS_STRIDE: usize = 16;

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
    /// The trace pipeline, built on the first frame that runs the hardware
    /// tracer and kept for the lifetime of the tracer.
    pipeline: Option<Pipeline>,
    /// Immutable shader records for the current acceleration structure.
    trace_normals: Option<Resource>,
    trace_materials: Option<Resource>,
}

/// The compute pipeline that runs the DXIL trace shader.
struct Pipeline {
    root_signature: *mut c_void,
    state: *mut c_void,
    /// Built from the DXIL blob, so it is the `RayQuery` shader and runs on the
    /// RT cores; the compute tracer's pipeline is `false`.
    ray_query: bool,
}

unsafe impl Send for Pipeline {}

unsafe impl Send for HardwareTracer {}

impl Drop for HardwareTracer {
    fn drop(&mut self) {
        // The pipeline was made by the device, so it is released first.
        if let Some(pipeline) = self.pipeline.take() {
            unsafe {
                sys::release(pipeline.state);
                sys::release(pipeline.root_signature);
            }
        }
        unsafe {
            sys::release(self.list4);
            sys::release(self.device5);
        }
        self.list4 = std::ptr::null_mut();
        self.device5 = std::ptr::null_mut();
    }
}

impl HardwareTracer {
    /// True when the RayQuery DXIL blob is installed, i.e. when a frame can be
    /// traced by the GPU's ray-tracing hardware rather than the software BVH
    /// walk the shared compute shader falls back to.
    ///
    /// `directory` is the module's own folder (`Modules/Imaginator`); the blob
    /// lives at `imaginator/dxr_trace.cso` beneath it.
    pub fn dxil_present(&self, directory: &Path) -> bool {
        ray_query_blob(directory).is_some()
            || module_candidates()
                .into_iter()
                .any(|candidate| ray_query_blob(&candidate).is_some())
    }

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
            pipeline: None,
            trace_normals: None,
            trace_materials: None,
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
        // DXR tier alone does not prove shader model 6.5 support. Ask the
        // driver separately so an unsupported shader is reported explicitly.
        let mut shader_model: u32 = 0x65; // D3D_SHADER_MODEL_6_5
        let hr = unsafe {
            self.device.CheckFeatureSupport(
                7 as win::D3D12_FEATURE, // D3D12_FEATURE_SHADER_MODEL
                &mut shader_model as *mut _ as *mut c_void,
                std::mem::size_of::<u32>() as u32,
            )
        };
        if !SUCCEEDED(hr) || shader_model < 0x65 {
            self.status = HardwareStatus::Unsupported(format!(
                "DXR inline RayQuery needs shader model 6.5; query returned 0x{shader_model:x} (HRESULT 0x{hr:08x})"
            ));
            return;
        }
        eprintln!(
            "Imaginator: DXR tier {} and shader model 0x{shader_model:x}",
            self.tier_label()
        );
        // 2. Device5 and CommandList4. Neither is declared by `winapi`.
        let mut device5: *mut c_void = std::ptr::null_mut();
        let Some(iid) = sys::guid(sys::IID_ID3D12DEVICE5) else {
            self.status = HardwareStatus::Failed("the Device5 GUID literal is invalid".into());
            return;
        };
        let hr = unsafe {
            sys::query_interface(
                self.device.as_ptr() as *mut c_void as *mut c_void,
                &iid,
                &mut device5,
            )
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
        // A freshly created command list is *open*. `Reset` on an open list
        // returns E_FAIL (0x80004005), which is exactly what the first build hit:
        // "command list Reset failed". Close it once here so every later Reset
        // starts from the recorded state.
        let hr = unsafe { (*(list4 as *mut win::ID3D12GraphicsCommandList)).Close() };
        if !SUCCEEDED(hr) {
            self.status = HardwareStatus::Failed(format!(
                "the build command list could not be closed (HRESULT 0x{hr:08x})"
            ));
            return;
        }
        self.list4 = list4;
        self.allocator = allocator;
        // 3. The one piece that cannot be produced in-tree. The probe has no
        //    module folder yet — it runs before the module page has one — so this
        //    only reports whether a blob exists where the module will look.
        // The blob's actual location, resolved the same way the module resolves
        // its own directory. This used to read a *current-working-directory*
        // relative path, so a module installed beside the executable reported the
        // shader missing whenever the process had been started from elsewhere —
        // which is most shortcuts on Windows.
        let found = module_candidates()
            .into_iter()
            .find_map(|dir| ray_query_blob(&dir));
        let note = match found {
            Some((blob, path)) => format!(
                "DXR tier {} on ID3D12Device5; {DXIL_FILE} present ({} KiB at {}), \
                 build runs on the first traced frame",
                self.tier_label(),
                blob.len() / 1024,
                path.display()
            ),
            None => format!(
                "DXR tier {} on ID3D12Device5; acceleration structures build on demand, but \
                 the RayQuery shader {DXIL_FILE} was not found in {} (run \
                 tools/imaginator-shaders.sh)",
                self.tier_label(),
                module_candidates()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" or ")
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
                self.trace_normals = None;
                self.trace_materials = None;
                self.built_for = Some(scene.clone());
                Ok(address)
            }
            Err(why) => {
                self.status = HardwareStatus::Failed(why.clone());
                Err(why)
            }
        }
    }

    /// Build the trace pipeline from the DXIL blob, once.
    ///
    /// The shader is a compute shader (`[numthreads(8,8,1)] cs`) whose traversal
    /// uses DXR 1.1 inline `RayQuery`, so the object that runs it is an ordinary
    /// compute pipeline state object — not a ray-tracing state object, and there
    /// is no `DispatchRays` anywhere in this path.
    fn ensure_pipeline(&mut self, dxil: &[u8]) -> Result<(), String> {
        if self.pipeline.is_some() {
            return Ok(());
        }
        if self.device5.is_null() {
            return Err("the DXR device is not ready".into());
        }
        if dxil.len() < 4 {
            return Err(
                "no compiled DXIL blob: imaginator/dxr_trace.cso is missing, and the \
                 shader needs dxc to compile"
                    .into(),
            );
        }
        // SAFETY: `device5` is live (checked above) and `dxil` outlives the call;
        // the runtime copies the bytecode during creation.
        let (signature, pipeline) = unsafe {
            let signature = sys::create_trace_root_signature(self.device5)?;
            match sys::create_compute_pipeline(self.device5, signature, dxil) {
                Ok(pipeline) => (signature, pipeline),
                Err(why) => {
                    sys::release(signature);
                    return Err(why);
                }
            }
        };
        self.pipeline = Some(Pipeline {
            root_signature: signature,
            state: pipeline,
            ray_query: true,
        });
        Ok(())
    }

    /// True once the trace pipeline exists.
    pub fn pipeline_ready(&self) -> bool {
        self.pipeline.is_some()
    }

    /// Whether the pipeline that exists is the DXIL `RayQuery` one.
    ///
    /// `pipeline_ready` alone is not enough to say "hardware ray tracing": the
    /// compute tracer builds a pipeline too, and reporting "RayQuery trace
    /// pipeline ready" for it is what made the module look like it was tracing
    /// on the RT cores when it was running an ordinary compute shader. Only a
    /// pipeline built from a non-empty DXIL blob uses `TraceRayInline` against
    /// the TLAS.
    pub fn ray_query(&self) -> bool {
        self.pipeline
            .as_ref()
            .is_some_and(|pipeline| pipeline.ray_query)
    }

    /// Run one hardware trace and hand back the four channels.
    ///
    /// The DXIL shader writes `rgba32float` into a UAV, so this traces into a
    /// texture of its own, copies it into a `READBACK` buffer and returns the
    /// pixels in the packed layout the composite stage already accepts from the
    /// CPU tracer. Nothing here depends on `wgpu` exposing a resource: the
    /// texture, the buffer and the views are all created on the raw device, which
    /// is what keeps this path independent of what the frame's own renderer does.
    ///
    /// `params` is the shader's 160-byte `Params` block; `bindings` are the four
    /// read-only buffers — the per-triangle records, the material table, and the
    /// two the trace reads for its tree and ground masks — in the order the root
    /// signature's SRV table declares them.
    #[allow(clippy::too_many_arguments)]
    pub fn trace_frame(
        &mut self,
        dxil: &[u8],
        tlas: sys::GpuAddress,
        width: u32,
        height: u32,
        params: &[u8],
        normals: &[u8],
        materials: &[u8],
    ) -> Result<Vec<f32>, String> {
        self.ensure_pipeline(dxil)?;
        if tlas == 0 {
            return Err("no acceleration structure to trace against".into());
        }
        if params.len() != PARAMS_BYTES {
            return Err(format!(
                "the trace parameters are {} bytes; the shader's Params block is {PARAMS_BYTES}",
                params.len()
            ));
        }
        if width == 0 || height == 0 {
            return Err("nothing to trace".into());
        }
        let Some(pipeline) = self.pipeline.as_ref() else {
            return Err("the trace pipeline is missing".into());
        };
        let state = pipeline.state;
        let root_signature = pipeline.root_signature;

        // 1. Every input lives in an upload buffer, and the output in a
        //    `DEFAULT` texture with a `READBACK` staging copy. Sizes are exact so
        //    the views can be written without a stride guess.
        let params_buffer = self.upload_bytes(params, "imaginator trace params")?;
        if self.trace_normals.is_none() {
            self.trace_normals = Some(self.upload_bytes(normals, "imaginator trace normals")?);
        }
        if self.trace_materials.is_none() {
            self.trace_materials =
                Some(self.upload_bytes(materials, "imaginator trace materials")?);
        }
        let normals_buffer = self.trace_normals.as_ref().expect("normals uploaded");
        let materials_buffer = self.trace_materials.as_ref().expect("materials uploaded");
        let output = self.create_output_texture(width, height)?;
        let row_pitch = trace_row_pitch(width);
        let readback = self.create_buffer_on(
            win::D3D12_HEAP_TYPE_READBACK,
            row_pitch as u64 * height as u64,
            "imaginator trace readback",
        )?;

        // 2. One shader-visible heap: an unused slot, three SRVs (TLAS,
        //    normals, materials) and a UAV. The CBV is a root descriptor.
        let heap = unsafe { sys::create_descriptor_heap(self.device.as_ptr() as *mut c_void, 5) }
            .map_err(|why| why.to_string())?;
        let (cpu_start, gpu_start) = unsafe { sys::heap_handles(heap) };
        let increment =
            unsafe { sys::descriptor_increment(self.device.as_ptr() as *mut c_void) } as usize;
        let slot = |index: usize| cpu_start + index * increment;
        // SAFETY: the heap has five slots and each write targets one of them; the
        // resources are alive for the whole call.
        unsafe {
            // The TLAS is a `RaytracingAccelerationStructure` on t0. Its view is
            // the structure's GPU address and nothing else — a buffer SRV over
            // the same bytes is not the same thing and the traversal would find
            // no geometry.
            sys::write_tlas_srv(self.device.as_ptr() as *mut c_void, tlas, slot(1));
            sys::write_buffer_srv(
                self.device.as_ptr() as *mut c_void,
                normals_buffer.as_mut_ptr() as *mut c_void,
                0,
                (normals.len() / NORMALS_STRIDE) as u32,
                NORMALS_STRIDE as u32,
                slot(2),
            );
            sys::write_buffer_srv(
                self.device.as_ptr() as *mut c_void,
                materials_buffer.as_mut_ptr() as *mut c_void,
                0,
                (materials.len() / MATERIALS_STRIDE) as u32,
                MATERIALS_STRIDE as u32,
                slot(3),
            );
            sys::write_texture_uav(
                self.device.as_ptr() as *mut c_void,
                output.as_mut_ptr() as *mut c_void,
                slot(4),
            );
        }

        // 3. Record and submit.
        let list = self.list4 as *mut win::ID3D12GraphicsCommandList;
        let hr = unsafe { self.allocator.Reset() };
        if !SUCCEEDED(hr) {
            unsafe { sys::release(heap) };
            return Err(format!(
                "command allocator Reset failed (HRESULT 0x{hr:08x})"
            ));
        }
        let hr = unsafe { (*list).Reset(self.allocator.as_mut_ptr(), std::ptr::null_mut()) };
        if !SUCCEEDED(hr) {
            unsafe { sys::release(heap) };
            return Err(format!("command list Reset failed (HRESULT 0x{hr:08x})"));
        }
        unsafe {
            (*list).SetPipelineState(state as *mut _);
            (*list).SetComputeRootSignature(root_signature as *mut _);
            let mut heap_pointer = heap as *mut win::ID3D12DescriptorHeap;
            (*list).SetDescriptorHeaps(1, &mut heap_pointer);
            // Root parameter 0 is the CBV, 1 the SRV table, 2 the UAV table.
            //
            // The CBV is a *root* descriptor, so it is bound by GPU address and
            // not through the heap. Nothing bound it before, so `Params` was read
            // from whatever the register happened to hold: a camera at the origin,
            // a zero `config` (so no bounces and an empty grid) and a zero `screen`.
            // The dispatch therefore always wrote an all-sky image, which the
            // composite then ignored pixel by pixel — the path looked ready and
            // changed nothing.
            sys::set_compute_root_constant_buffer_view(
                list as *mut c_void,
                0,
                sys::gpu_virtual_address(params_buffer.as_mut_ptr() as *mut c_void),
            );
            // Root parameter 1 is the SRV table (t0..t2, at heap slots 1..3) and
            // parameter 2 the UAV (slot 4).
            sys::set_compute_root_descriptor_table(
                list as *mut c_void,
                1,
                gpu_start + increment as u64,
            );
            sys::set_compute_root_descriptor_table(
                list as *mut c_void,
                2,
                gpu_start + 4 * increment as u64,
            );
            (*list).Dispatch(width.div_ceil(8), height.div_ceil(8), 1);
            let to_copy = transition_barrier(
                &output,
                win::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                win::D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            (*list).ResourceBarrier(1, &to_copy);
            // Texture to buffer: the destination is a placed footprint and the
            // source is a subresource index. D3D12 requires a 256-byte-aligned
            // row pitch even when the destination is a buffer.
            let footprint = sys::D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: 0,
                Footprint: sys::D3D12_SUBRESOURCE_FOOTPRINT {
                    Format: R32G32B32A32_FLOAT,
                    Width: width,
                    Height: height,
                    Depth: 1,
                    RowPitch: row_pitch,
                },
            };
            let destination = sys::D3D12_TEXTURE_COPY_LOCATION {
                pResource: readback.as_mut_ptr() as *mut c_void,
                Type: sys::D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                PlacedFootprint: footprint,
            };
            let source = sys::D3D12_TEXTURE_COPY_LOCATION {
                pResource: output.as_mut_ptr() as *mut c_void,
                Type: sys::D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                PlacedFootprint: footprint,
            };
            // `sys` declares these two locations; `winapi` names the same layout,
            // so they are passed through as the pointers the call takes.
            let destination: *const win::D3D12_TEXTURE_COPY_LOCATION =
                &destination as *const _ as *const win::D3D12_TEXTURE_COPY_LOCATION;
            let source: *const win::D3D12_TEXTURE_COPY_LOCATION =
                &source as *const _ as *const win::D3D12_TEXTURE_COPY_LOCATION;
            // The whole texture, one subresource, no source box.
            (*list).CopyTextureRegion(destination, 0, 0, 0, source, std::ptr::null());
            let hr = (*list).Close();
            if !SUCCEEDED(hr) {
                sys::release(heap);
                return Err(format!("command list Close failed (HRESULT 0x{hr:08x})"));
            }
        }
        let lists: [*mut win::ID3D12CommandList; 1] = [list as *mut _];
        unsafe { (*(self.queue.as_ptr())).ExecuteCommandLists(1, lists.as_ptr()) };
        let waited = self.wait_for_build();
        unsafe { sys::release(heap) };
        waited?;
        self.read_back(&readback, width, height)
    }

    /// A default-heap buffer holding `bytes`, written through a staging copy.
    pub fn upload_bytes(&mut self, bytes: &[u8], label: &str) -> Result<Resource, String> {
        self.upload(bytes.len().max(16) as u64, label, bytes)
    }

    /// The `rgba32float` texture the trace shader writes and the readback copies.
    fn create_output_texture(&self, width: u32, height: u32) -> Result<Resource, String> {
        self.create_texture(
            width,
            height,
            // `DXGI_FORMAT_R32G32B32A32_FLOAT` is 2 in every DXGI header.
            R32G32B32A32_FLOAT,
            win::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            "imaginator trace output",
        )
    }

    /// A 2-D resource on the default heap, for the trace output.
    fn create_texture(
        &self,
        width: u32,
        height: u32,
        format: u32,
        flags: u32,
        label: &str,
    ) -> Result<Resource, String> {
        let heap = win::D3D12_HEAP_PROPERTIES {
            Type: sys::D3D12_HEAP_TYPE_DEFAULT,
            CPUPageProperty: sys::D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: sys::D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        // winapi's own description: a texture is a `D3D12_RESOURCE_DESC` with
        // the 2-D dimension, and spelling it here avoids reinterpreting one
        // struct as another.
        let desc = win::D3D12_RESOURCE_DESC {
            Dimension: win::D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: width as u64,
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: win::D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: flags,
        };
        let mut resource: *mut win::ID3D12Resource = std::ptr::null_mut();
        // SAFETY: both descriptions outlive the call and the out-parameter is a
        // live local of exactly the interface the IID names.
        let hr = unsafe {
            self.device.CreateCommittedResource(
                &heap,
                win::D3D12_HEAP_FLAG_NONE,
                // The layouts are the same struct; `sys` declares it because a
                // texture's description has a different shape from a buffer's.
                &desc,
                sys::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                std::ptr::null(),
                &win::ID3D12Resource::uuidof(),
                &mut resource as *mut _ as *mut *mut c_void,
            )
        };
        if !SUCCEEDED(hr) || resource.is_null() {
            return Err(format!(
                "CreateCommittedResource({label}, {width}x{height}) failed (HRESULT 0x{hr:08x})"
            ));
        }
        // SAFETY: the call handed back a new reference on a live resource.
        Ok(unsafe { Resource::from_raw(resource) })
    }

    /// Copy the readback buffer's pixels into the packed channel order the
    /// composite stage expects.
    ///
    /// Strip the D3D12 footprint's row padding and return tightly packed RGBA
    /// pixels for the compositor.
    fn read_back(&self, readback: &Resource, width: u32, height: u32) -> Result<Vec<f32>, String> {
        let row_bytes = width as usize * 16;
        let row_pitch = trace_row_pitch(width) as usize;
        let pointer = unsafe { sys::map(readback.as_mut_ptr() as *mut c_void, std::ptr::null()) }?;
        let mut pixels = vec![0f32; width as usize * height as usize * 4];
        unsafe {
            for y in 0..height as usize {
                std::ptr::copy_nonoverlapping(
                    (pointer as *const u8).add(y * row_pitch),
                    (pixels.as_mut_ptr() as *mut u8).add(y * row_bytes),
                    row_bytes,
                );
            }
            sys::unmap(readback.as_mut_ptr() as *mut c_void);
        }
        Ok(pixels)
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
        let blas =
            self.create_as_buffer(blas_info.ResultDataMaxSizeInBytes, "imaginator blas", true)?;
        let scratch_size = blas_info
            .ScratchDataSizeInBytes
            .max(blas_info.UpdateScratchDataSizeInBytes)
            .max(1);
        let scratch = self.create_as_buffer(scratch_size, "imaginator blas scratch", false)?;
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
        let tlas =
            self.create_as_buffer(tlas_info.ResultDataMaxSizeInBytes, "imaginator tlas", true)?;
        let tlas_scratch_size = tlas_info
            .ScratchDataSizeInBytes
            .max(tlas_info.UpdateScratchDataSizeInBytes)
            .max(1);
        let tlas_scratch =
            self.create_as_buffer(tlas_scratch_size, "imaginator tlas scratch", false)?;
        let tlas_desc = sys::D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
            DestAccelerationStructureData: tlas.gpu_virtual_address(),
            Inputs: tlas_inputs,
            SourceAccelerationStructureData: 0,
            ScratchAccelerationStructureData: tlas_scratch.gpu_virtual_address(),
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
            let hr = self.allocator.Reset();
            if !SUCCEEDED(hr) {
                return Err(format!(
                    "command allocator Reset failed (HRESULT 0x{hr:08x})"
                ));
            }
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

    /// A committed ordinary buffer using the state required by its heap type.
    fn create_buffer_on(
        &self,
        heap_type: u32,
        bytes: u64,
        label: &str,
    ) -> Result<Resource, String> {
        let state = if heap_type == win::D3D12_HEAP_TYPE_UPLOAD {
            win::D3D12_RESOURCE_STATE_GENERIC_READ
        } else if heap_type == win::D3D12_HEAP_TYPE_READBACK {
            win::D3D12_RESOURCE_STATE_COPY_DEST
        } else {
            win::D3D12_RESOURCE_STATE_COMMON
        };
        self.create_buffer_config(
            heap_type,
            bytes,
            label,
            win::D3D12_RESOURCE_FLAG_NONE,
            state,
        )
    }

    fn create_as_buffer(
        &self,
        bytes: u64,
        label: &str,
        acceleration_structure: bool,
    ) -> Result<Resource, String> {
        let state = if acceleration_structure {
            0x400000
        } else {
            win::D3D12_RESOURCE_STATE_UNORDERED_ACCESS
        };
        self.create_buffer_config(
            win::D3D12_HEAP_TYPE_DEFAULT,
            bytes,
            label,
            win::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            state,
        )
    }

    fn create_buffer_config(
        &self,
        heap_type: u32,
        bytes: u64,
        label: &str,
        flags: win::D3D12_RESOURCE_FLAGS,
        state: win::D3D12_RESOURCE_STATES,
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
            Flags: flags,
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
                state,
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
        // reads is the one the compile-time assertion in `sys` pins to slot 63.
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
        let hr = self.allocator.Reset();
        if !SUCCEEDED(hr) {
            return Err(format!(
                "command allocator Reset failed (HRESULT 0x{hr:08x})"
            ));
        }
        let hr = (*base).Reset(self.allocator.as_mut_ptr(), std::ptr::null_mut());
        if !SUCCEEDED(hr) {
            return Err(format!("command list Reset failed (HRESULT 0x{hr:08x})"));
        }
        (vtbl.BuildRaytracingAccelerationStructure)(self.list4, desc, 0, std::ptr::null());
        // Publish AS writes before subsequent builds or ray queries read them.
        let mut barrier: win::D3D12_RESOURCE_BARRIER = std::mem::zeroed();
        barrier.Type = win::D3D12_RESOURCE_BARRIER_TYPE_UAV;
        barrier.Flags = win::D3D12_RESOURCE_BARRIER_FLAG_NONE;
        barrier.u.UAV_mut().pResource = std::ptr::null_mut();
        (*base).ResourceBarrier(1, &barrier);
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
        // Wait for a real deadline, not a spin count.
        //
        // This used to be `while GetCompletedValue() < 1 { spin_loop() }` bounded
        // by a count of 500 million iterations. A spin count says nothing about
        // time on a pre-empted thread: the loop pegs a core and, if the GPU has
        // not reached the build (or the driver never signals — a device removed,
        // a hung queue), the window never draws its first frame and the user sees
        // a white screen with nothing in the log. A deadline at least turns that
        // into a reported failure the module page can show.
        //
        // The yield matters for the same reason: this runs on the render thread,
        // so a plain spin also starves whatever wgpu needs to make progress.
        let deadline = std::time::Instant::now() + BUILD_TIMEOUT;
        // SAFETY: `fence` is the live fence this submit signalled.
        while unsafe { fence.GetCompletedValue() } < 1 {
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "the GPU did not finish the acceleration-structure build within {:?} \
                     (fence value still {}); hardware tracing is disabled for this session",
                    BUILD_TIMEOUT,
                    unsafe { fence.GetCompletedValue() }
                ));
            }
            std::thread::yield_now();
        }
        Ok(())
    }

    /// The ray-query shader source. Kept with the backend so the shader and the
    /// code that feeds it change together.
    pub const DXIL_PREPARE: &'static str = "tools/imaginator-shaders.sh compiles this to DXIL (shader model 6.5) with DXC; see dxr_trace.hlsl";
}

/// D3D12 texture-copy footprints require 256-byte row alignment.
fn trace_row_pitch(width: u32) -> u32 {
    (width * 16).div_ceil(256) * 256
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
    fn texture_readback_pitch_is_aligned() {
        assert_eq!(trace_row_pitch(1), 256);
        assert_eq!(trace_row_pitch(16), 256);
        assert_eq!(trace_row_pitch(17), 512);
    }

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
        // 48-byte transform + two packed bitfield words + 8-byte address.
        assert_eq!(
            std::mem::size_of::<sys::D3D12_RAYTRACING_INSTANCE_DESC>(),
            64,
            "D3D12_RAYTRACING_INSTANCE_DESC is 64 bytes, 16-byte aligned"
        );
        assert_eq!(
            std::mem::align_of::<sys::D3D12_RAYTRACING_INSTANCE_DESC>(),
            16
        );
    }

    /// The blob probe and the frame path must agree on where the shader is.
    ///
    /// They used to disagree: the probe read a working-directory-relative path
    /// while `trace_hardware` read the module's own directory, so a portable
    /// install launched from anywhere but its own folder reported the shader
    /// missing while the frame path would have found it — or the reverse.
    #[test]
    fn the_blob_probe_matches_the_path_the_trace_reads() {
        let dir =
            std::env::temp_dir().join(format!("imaginator-blob-probe-{}", std::process::id()));
        let shader_dir = dir.join("imaginator");
        std::fs::create_dir_all(&shader_dir).expect("a temp module directory");
        // Too small: a four-byte file is not DXIL and must not count.
        let small = dir.join(DXIL_FILE);
        std::fs::write(&small, b"abcd").expect("write the stub");
        assert!(
            ray_query_blob(&dir).is_none(),
            "a four-byte file is not a compiled shader"
        );
        // A plausible blob: the probe must find exactly what the trace reads.
        std::fs::write(&small, vec![0u8; 512]).expect("write the blob");
        let (blob, path) = ray_query_blob(&dir).expect("the blob must be found");
        assert_eq!(path, dir.join(DXIL_FILE));
        assert_eq!(blob.len(), 512);
        let _ = std::fs::remove_dir_all(&dir);

        // The candidates must include a path beside the executable, which is
        // where a portable install keeps the module.
        let candidates = module_candidates();
        assert!(!candidates.is_empty());
        assert!(
            candidates.iter().any(|c| c.ends_with(MODULE_DIR)),
            "one candidate must be Modules/Imaginator"
        );
    }
}
