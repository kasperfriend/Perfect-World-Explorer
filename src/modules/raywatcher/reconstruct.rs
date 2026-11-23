//! Ray reconstruction: the vendor networks that replace the in-house denoiser's
//! output just before the upscaler.
//!
//! Two providers, one interface. Both take the same inputs — noisy ray signals,
//! the G-buffer, motion vectors, and the ray-specific guides (hit distances,
//! ray directions, normals packed with roughness) — and produce a cleaner image
//! at the render extent. Which one runs is a module setting:
//!
//! * **DLSS Ray Reconstruction** (`nvngx_dlssd.dll`, NGX feature
//!   `NVSDK_NGX_Feature_RayReconstruction`) on RTX hardware, through the same
//!   NGX loader the DLSS upscaler already uses,
//! * **FSR Ray Regeneration** (`amd_fidelityfx_denoiser_dx12.dll`, the FFX
//!   denoiser API) on DX12, RDNA hardware for the machine-learning path and any
//!   other DX12 GPU for the SDK's own spatial fallback.
//!
//! Both are *optional*: a missing DLL, a non-Windows build or an unsupported
//! adapter leaves the provider unavailable, the tab says so, and the in-house
//! denoiser keeps running. Nothing here is on the critical path of a frame that
//! does not select it.

use std::path::Path;

/// Which provider a frame will use. Mirrors
/// [`super::plan::RayReconstruction`] but keeps the module's own vocabulary out
/// of the FFI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    DlssRr,
    FsrRr,
}

/// What a provider reports about itself. `available` is the gate the plan
/// consults; `note` is what the tab shows either way.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub available: bool,
    pub note: String,
}

impl Probe {
    fn unavailable(note: impl Into<String>) -> Self {
        Self {
            available: false,
            note: note.into(),
        }
    }

    fn available(note: impl Into<String>) -> Self {
        Self {
            available: true,
            note: note.into(),
        }
    }
}

/// The DLL names the two providers load. Kept public so the packaging scripts,
/// the verifier and the tab all read one list.
pub const DLSS_RR_DLL: &str = "nvngx_dlssd.dll";
pub const FSR_RR_DLL: &str = "amd_fidelityfx_denoiser_dx12.dll";

/// Whether `dll` is present in `sdk_dir` (or beside the executable, where the
/// vendored loader looks first).
pub fn dll_present(sdk_dir: &Path, dll: &str) -> bool {
    if sdk_dir.join(dll).is_file() {
        return true;
    }
    // The FFX loader resolves the denoiser beside *itself*, so the loader's own
    // folder counts too.
    sdk_dir.join("amd_fidelityfx_loader_dx12.dll").is_file() && sdk_dir.join(dll).is_file()
}

/// Probe both providers without creating a device.
///
/// This is the decision the availability gate needs, and it is deliberately
/// separate from context creation: a DLL can be present and the adapter still
/// refuse the feature, which is the case the plan's `reconstruction_available`
/// predicate re-checks once a device exists.
pub fn probe_offline(sdk_dir: &Path, platform_supported: bool) -> (Probe, Probe) {
    if !platform_supported {
        let note = "Windows + DX12 only".to_string();
        return (Probe::unavailable(note.clone()), Probe::unavailable(note));
    }
    let dlss = if dll_present(sdk_dir, DLSS_RR_DLL) {
        Probe::available(format!("{DLSS_RR_DLL} found"))
    } else {
        Probe::unavailable(format!("{DLSS_RR_DLL} not found in {}", sdk_dir.display()))
    };
    let fsr = if dll_present(sdk_dir, FSR_RR_DLL) {
        Probe::available(format!("{FSR_RR_DLL} found"))
    } else {
        Probe::unavailable(format!("{FSR_RR_DLL} not found in {}", sdk_dir.display()))
    };
    (dlss, fsr)
}

/// Per-frame inputs for [`DlssRayReconstructor::evaluate`].
/// Check whether NVIDIA's NGX Core (`nvngx.dll`) is present in System32 or the
/// Windows DriverStore FileRepository. Used to diagnose DLSS Ray Reconstruction
/// runtime init failures in the UI.
pub fn driver_store_has_nvngx() -> bool {
    #[cfg(windows)]
    {
        if Path::new(r"C:\Windows\System32\nvngx.dll").is_file() {
            return true;
        }
        let repo = Path::new(r"C:\Windows\System32\DriverStore\FileRepository");
        if let Ok(entries) = std::fs::read_dir(repo) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && p.join("nvngx.dll").is_file() {
                    return true;
                }
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub struct DlssRrInputs<'a> {
    pub grid_colour: &'a wgpu::TextureView,
    pub grid_reflection: &'a wgpu::TextureView,
    pub grid_normals: &'a wgpu::TextureView,
    pub grid_albedo: &'a wgpu::TextureView,
    pub depth: &'a wgpu::TextureView,
    /// Untouched full raster world render (`None` only in opt-in replace mode).
    pub raster: Option<&'a wgpu::TextureView>,
    pub layered: bool,
    /// Per-frame scene-illumination albedo recovery gain (`1 / max(scene_illum, 0.08)`),
    /// keeping dark stone/wood dark instead of boosting every dark pixel to 0.75 grey.
    pub scene_albedo_gain: f32,
    pub motion: &'a wgpu::TextureView,
    /// Pre-multiplied `prev_unjittered_vp * current_unjittered_vp.inverse()` for
    /// exact full-screen camera motion vectors in DLSS RR.
    pub prev_vp_inv_vp: [[f32; 4]; 4],
    pub history_valid: bool,
    pub jitter: (f32, f32),
    pub reset: bool,
    pub world_to_view: [[f32; 4]; 4],
    pub view_to_clip: [[f32; 4]; 4],
}

#[cfg(windows)]
mod dlss_rr {
    //! NVIDIA DLSS Ray Reconstruction (`nvngx_dlssd.dll`, NGX feature
    //! `NVSDK_NGX_Feature_RayReconstruction = 13`) on DX12.

    use std::ffi::c_void;
    use std::mem::zeroed;
    use std::path::Path;
    use std::time::Instant;

    use winapi::shared::dxgiformat::{DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R32_FLOAT};
    use winapi::um::d3d12 as d12;

    use super::{DlssRrInputs, DLSS_RR_DLL};
    use crate::dlss::loader::{NgxApi, NGX_FEATURE_RAY_RECONSTRUCTION};
    use crate::dlss::params::{resource_slot_ladder, NgxParams, NGX_FAIL};
    use crate::fsr::{raw_dx12, NativeCommandRing, NativeTexture2D};

    // NGX DLSS-D parameter names from `sdk/include/nvidia/nvsdk_ngx_defs.h` and
    // `sdk/include/nvidia/nvsdk_ngx_defs_dlssd.h`.
    const P_CREATION_NODE_MASK: &str = "CreationNodeMask";
    const P_VISIBILITY_NODE_MASK: &str = "VisibilityNodeMask";
    const P_WIDTH: &str = "Width";
    const P_HEIGHT: &str = "Height";
    const P_OUT_WIDTH: &str = "OutWidth";
    const P_OUT_HEIGHT: &str = "OutHeight";
    const P_RESOURCE_WIDTH: &str = "ResourceWidth";
    const P_RESOURCE_HEIGHT: &str = "ResourceHeight";
    const P_RESOURCE_OUT_WIDTH: &str = "ResourceOutWidth";
    const P_RESOURCE_OUT_HEIGHT: &str = "ResourceOutHeight";
    const P_PERF_QUALITY: &str = "PerfQualityValue";
    const P_CREATE_FLAGS: &str = "DLSS.Feature.Create.Flags";
    const P_OUTPUT_SUBRECTS: &str = "DLSS.Enable.Output.Subrects";
    const P_DENOISE_MODE: &str = "DLSS.Denoise.Mode";
    const P_ROUGHNESS_MODE: &str = "DLSS.Roughness.Mode";
    const P_USE_HW_DEPTH: &str = "DLSS.Use.HW.Depth";

    const P_COLOR: &str = "Color";
    const P_OUTPUT: &str = "Output";
    const P_DEPTH: &str = "Depth";
    const P_MOTION_VECTORS: &str = "MotionVectors";
    const P_DIFFUSE_ALBEDO: &str = "DLSS.Input.DiffuseAlbedo";
    const P_SPECULAR_ALBEDO: &str = "DLSS.Input.SpecularAlbedo";
    const P_GBUFFER_NORMALS: &str = "GBuffer.Normals";
    const P_HIT_DISTANCE: &str = "RayTracingHitDistance";
    const P_WORLD_TO_VIEW: &str = "WorldToViewMatrix";
    const P_VIEW_TO_CLIP: &str = "ViewToClipMatrix";

    const P_JITTER_X: &str = "Jitter.Offset.X";
    const P_JITTER_Y: &str = "Jitter.Offset.Y";
    const P_RESET: &str = "Reset";
    const P_MV_SCALE_X: &str = "MV.Scale.X";
    const P_MV_SCALE_Y: &str = "MV.Scale.Y";
    const P_FRAME_TIME_MS: &str = "FrameTimeDeltaInMsec";
    const P_PRE_EXPOSURE: &str = "DLSS.Pre.Exposure";
    const P_EXPOSURE_SCALE: &str = "DLSS.Exposure.Scale";
    const P_TONEMAPPER_TYPE: &str = "TonemapperType";
    const P_INDICATOR_INVERT_X: &str = "DLSS.Indicator.Invert.X.Axis";
    const P_INDICATOR_INVERT_Y: &str = "DLSS.Indicator.Invert.Y.Axis";
    const P_RENDER_WIDTH: &str = "DLSS.Render.Subrect.Dimensions.Width";
    const P_RENDER_HEIGHT: &str = "DLSS.Render.Subrect.Dimensions.Height";

    const P_ZERO_SUBRECTS: &[&str] = &[
        "DLSS.Input.Color.Subrect.Base.X",
        "DLSS.Input.Color.Subrect.Base.Y",
        "DLSS.Input.Depth.Subrect.Base.X",
        "DLSS.Input.Depth.Subrect.Base.Y",
        "DLSS.Input.MV.Subrect.Base.X",
        "DLSS.Input.MV.Subrect.Base.Y",
        "DLSS.Output.Subrect.Base.X",
        "DLSS.Output.Subrect.Base.Y",
        "DLSS.Input.DiffuseAlbedo.Subrect.Base.X",
        "DLSS.Input.DiffuseAlbedo.Subrect.Base.Y",
        "DLSS.Input.SpecularAlbedo.Subrect.Base.X",
        "DLSS.Input.SpecularAlbedo.Subrect.Base.Y",
        "DLSS.Input.Normals.Subrect.Base.X",
        "DLSS.Input.Normals.Subrect.Base.Y",
        "DLSS.Input.Roughness.Subrect.Base.X",
        "DLSS.Input.Roughness.Subrect.Base.Y",
    ];

    // `NVSDK_NGX_PerfQuality_Value_DLAA = 5` (1:1 ray reconstruction at render extent).
    const NGX_PERF_QUALITY_DLAA: i32 = 5;
    // `IsHDR (1 << 0) | MVLowRes (1 << 1) | DepthInverted (1 << 3)`
    const NGX_DLSSD_CREATE_FLAGS: i32 = (1 << 0) | (1 << 1) | (1 << 3);
    // `NVSDK_NGX_DLSS_Denoise_Mode_DLUnified = 1`
    const NGX_DENOISE_MODE_DL_UNIFIED: i32 = 1;
    // `NVSDK_NGX_DLSS_Roughness_Mode_Packed = 1` (roughness in `GBuffer.Normals.w`)
    const NGX_ROUGHNESS_MODE_PACKED: u32 = 1;
    // `NVSDK_NGX_DLSS_Depth_Type_HW = 1`
    const NGX_DEPTH_TYPE_HW: u32 = 1;

    /// Fullscreen MRT prep pass: resamples the ray grid's noisy radiance,
    /// packed normals+roughness, diffuse/specular albedo, hit distance, and
    /// copies hardware `Depth32Float` into `R32Float` for DLSS-D in one pass.
    const PREP_SHADER: &str = r#"
@group(0) @binding(0) var src_colour: texture_2d<f32>;
@group(0) @binding(1) var src_reflection: texture_2d<f32>;
@group(0) @binding(2) var src_normals: texture_2d<f32>;
@group(0) @binding(3) var src_albedo: texture_2d<f32>;
@group(0) @binding(4) var src_depth: texture_depth_2d;
@group(0) @binding(5) var lin_samp: sampler;
@group(0) @binding(6) var src_raster: texture_2d<f32>;
struct PrepUniform {
    flags: vec4<f32>,
    prev_vp_inv_vp: mat4x4<f32>,
};
@group(0) @binding(7) var<uniform> prep_u: PrepUniform;
@group(0) @binding(8) var src_object_mv: texture_2d<f32>;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    var out: VOut;
    let xy = p[idx];
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5);
    return out;
}

struct FOut {
    @location(0) color: vec4<f32>,
    @location(1) depth: f32,
    @location(2) normals: vec4<f32>,
    @location(3) diffuse_albedo: vec4<f32>,
    @location(4) specular_albedo: vec4<f32>,
    @location(5) hit_distance: f32,
    @location(6) motion: vec4<f32>,
};

@fragment
fn fs(i: VOut) -> FOut {
    let depth_dim = vec2<f32>(textureDimensions(src_depth));
    let dcoord = vec2<i32>(clamp(i.uv * depth_dim, vec2<f32>(0.0), depth_dim - vec2<f32>(1.0)));
    let d = textureLoad(src_depth, dcoord, 0);

    let rdim = vec2<f32>(textureDimensions(src_raster));
    let rcoord = vec2<i32>(clamp(i.uv * rdim, vec2<f32>(0.0), rdim - vec2<f32>(1.0)));
    let raster_rgb = textureLoad(src_raster, rcoord, 0).rgb;

    // Compute exact full-screen camera + object motion vectors for BOTH sky/clouds
    // (`d <= 1e-6`) and world geometry so DLSS RR never ghosts rotating clouds or
    // smears buildings during camera motion / upscaler jitter.
    var mv_px = vec2<f32>(0.0);
    let obj_mv = textureLoad(src_object_mv, dcoord, 0);
    if (obj_mv.z > 0.5) {
        mv_px = obj_mv.xy;
    } else if (prep_u.flags.z > 0.5) {
        let uv_c = (vec2<f32>(dcoord) + vec2<f32>(0.5)) / depth_dim;
        let ndc = vec2<f32>(uv_c.x * 2.0 - 1.0, 1.0 - uv_c.y * 2.0);
        let z_reproj = select(d, 0.0, d <= 1e-6);
        let prev_clip = prep_u.prev_vp_inv_vp * vec4<f32>(ndc, z_reproj, 1.0);
        if (abs(prev_clip.w) > 1e-5) {
            let prev_ndc = prev_clip.xyz / prev_clip.w;
            let prev_uv = vec2<f32>(prev_ndc.x * 0.5 + 0.5, 0.5 - prev_ndc.y * 0.5);
            mv_px = (prev_uv - uv_c) * depth_dim;
        }
    }

    // Sky pixels pass through 100% untouched (with exact sky-dome motion vectors)
    // so clouds never ghost and rooftop GI never bleeds onto sky in DLSS RR.
    var out: FOut;
    if (d <= 1e-6) {
        out.color = vec4<f32>(raster_rgb, 1.0);
        out.depth = 0.0;
        out.normals = vec4<f32>(0.0, 1.0, 0.0, 1.0);
        out.diffuse_albedo = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        out.specular_albedo = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        out.hit_distance = 1.0;
        out.motion = vec4<f32>(mv_px, 0.0, 0.0);
        return out;
    }

    // Nearest texel fetch on ray-grid buffers prevents GI irradiance, shadow
    // light-factor, and surface normals from smearing across building silhouettes.
    // Opt #19: Removed redundant unused `src_albedo` load and dummy bilinear sample
    // (saves 2 full-screen RGBA16F texture fetches per pixel in DLSS RR prep).
    let gdim = vec2<f32>(textureDimensions(src_colour));
    let gcoord = vec2<i32>(clamp(i.uv * gdim, vec2<f32>(0.0), gdim - vec2<f32>(1.0)));
    let c = textureLoad(src_colour, gcoord, 0);
    let r = textureLoad(src_reflection, gcoord, 0);
    let n = textureLoad(src_normals, gcoord, 0);

    let layered = prep_u.flags.x > 0.5;
    let scene_gain = clamp(prep_u.flags.y, 1.0, 9.0);
    let full_albedo = clamp(raster_rgb * scene_gain, vec3<f32>(0.002), vec3<f32>(0.92));
    let is_water = n.w <= 0.12;
    let modulated_gi = select(full_albedo * c.rgb, c.rgb, is_water);
    let base_rgb = select(c.rgb, max(raster_rgb * clamp(c.a, 0.0, 1.0) + modulated_gi, vec3<f32>(0.0)), layered);
    out.color = vec4<f32>(max(base_rgb + r.rgb, vec3<f32>(0.0)), 1.0);
    let rough = clamp(n.w, 0.02, 1.0);
    out.depth = d;
    out.normals = vec4<f32>(n.xyz, rough);
    out.diffuse_albedo = vec4<f32>(full_albedo, 1.0);
    let spec_f0 = select(select(0.02, 0.06, rough <= 0.36), 0.18, is_water);
    out.specular_albedo = vec4<f32>(spec_f0, spec_f0, spec_f0, 1.0);
    out.hit_distance = r.a;
    out.motion = vec4<f32>(mv_px, 0.0, 0.0);
    return out;
}
"#;

    struct PrepPipeline {
        layout: wgpu::BindGroupLayout,
        pipeline: wgpu::RenderPipeline,
        sampler: wgpu::Sampler,
        flags_buf: wgpu::Buffer,
    }

    impl PrepPipeline {
        fn new(device: &wgpu::Device) -> Self {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("raywatcher dlss-rr prep"),
                source: wgpu::ShaderSource::Wgsl(PREP_SHADER.into()),
            });
            let tex = |binding: u32| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            };
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("raywatcher dlss-rr prep"),
                entries: &[
                    tex(0),
                    tex(1),
                    tex(2),
                    tex(3),
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    tex(6),
                    wgpu::BindGroupLayoutEntry {
                        binding: 7,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    tex(8),
                ],
            });
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("raywatcher dlss-rr prep"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let target = |fmt: wgpu::TextureFormat| {
                Some(wgpu::ColorTargetState {
                    format: fmt,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })
            };
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("raywatcher dlss-rr prep"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: "vs",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: "fs",
                    targets: &[
                        target(wgpu::TextureFormat::Rgba16Float),
                        target(wgpu::TextureFormat::R32Float),
                        target(wgpu::TextureFormat::Rgba16Float),
                        target(wgpu::TextureFormat::Rgba16Float),
                        target(wgpu::TextureFormat::Rgba16Float),
                        target(wgpu::TextureFormat::R32Float),
                        target(wgpu::TextureFormat::Rgba16Float),
                    ],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("raywatcher dlss-rr linear"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let flags_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("raywatcher dlss-rr prep uniform"),
                size: 80,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            Self {
                layout,
                pipeline,
                sampler,
                flags_buf,
            }
        }
    }

    struct RrNatives {
        color: NativeTexture2D,
        depth: NativeTexture2D,
        mv: NativeTexture2D,
        normals: NativeTexture2D,
        diffuse_albedo: NativeTexture2D,
        specular_albedo: NativeTexture2D,
        hit_distance: NativeTexture2D,
        output: NativeTexture2D,
        color_view: wgpu::TextureView,
        depth_view: wgpu::TextureView,
        normals_view: wgpu::TextureView,
        diffuse_view: wgpu::TextureView,
        specular_view: wgpu::TextureView,
        hit_view: wgpu::TextureView,
        mv_view: wgpu::TextureView,
    }

    impl RrNatives {
        fn new(
            device: &wgpu::Device,
            raw_device: &d3d12::Device,
            extent: (u32, u32),
        ) -> Result<Self, String> {
            let rt_flags = d12::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
            let rt_usage = wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST;
            let mk_rt = |label: &str, fmt: wgpu::TextureFormat, dxgi: u32| unsafe {
                NativeTexture2D::create(
                    device,
                    raw_device,
                    label,
                    fmt,
                    dxgi,
                    extent,
                    rt_flags,
                    rt_usage,
                    d12::D3D12_RESOURCE_STATE_COMMON,
                    false,
                )
            };
            let color = mk_rt(
                "dlss-rr color",
                wgpu::TextureFormat::Rgba16Float,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
            )?;
            let depth = mk_rt(
                "dlss-rr depth",
                wgpu::TextureFormat::R32Float,
                DXGI_FORMAT_R32_FLOAT,
            )?;
            let mv = mk_rt(
                "dlss-rr mv",
                wgpu::TextureFormat::Rgba16Float,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
            )?;
            let normals = mk_rt(
                "dlss-rr normals",
                wgpu::TextureFormat::Rgba16Float,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
            )?;
            let diffuse_albedo = mk_rt(
                "dlss-rr diffuse albedo",
                wgpu::TextureFormat::Rgba16Float,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
            )?;
            let specular_albedo = mk_rt(
                "dlss-rr specular albedo",
                wgpu::TextureFormat::Rgba16Float,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
            )?;
            let hit_distance = mk_rt(
                "dlss-rr hit distance",
                wgpu::TextureFormat::R32Float,
                DXGI_FORMAT_R32_FLOAT,
            )?;
            let output = unsafe {
                NativeTexture2D::create(
                    device,
                    raw_device,
                    "dlss-rr output",
                    wgpu::TextureFormat::Rgba16Float,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    extent,
                    d12::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                    wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING,
                    d12::D3D12_RESOURCE_STATE_COMMON,
                    false,
                )?
            };
            let color_view = color.texture.create_view(&Default::default());
            let depth_view = depth.texture.create_view(&Default::default());
            let normals_view = normals.texture.create_view(&Default::default());
            let diffuse_view = diffuse_albedo.texture.create_view(&Default::default());
            let specular_view = specular_albedo.texture.create_view(&Default::default());
            let hit_view = hit_distance.texture.create_view(&Default::default());
            let mv_view = mv.texture.create_view(&Default::default());
            Ok(Self {
                color,
                depth,
                mv,
                normals,
                diffuse_albedo,
                specular_albedo,
                hit_distance,
                output,
                color_view,
                depth_view,
                normals_view,
                diffuse_view,
                specular_view,
                hit_view,
                mv_view,
            })
        }
    }

    /// Live NVIDIA DLSS Ray Reconstruction (`nvngx_dlssd.dll`) session.
    pub struct DlssRayReconstructor {
        api: NgxApi,
        params: Box<NgxParams>,
        handle: *mut c_void,
        queue: d3d12::CommandQueue,
        ring: NativeCommandRing,
        extent: (u32, u32),
        natives: RrNatives,
        prep: PrepPipeline,
        prep_bg: Option<wgpu::BindGroup>,
        frame_index: u64,
        param_slot: usize,
        last_instant: Instant,
    }

    impl Drop for DlssRayReconstructor {
        fn drop(&mut self) {
            self.ring.wait_idle();
            if !self.handle.is_null() {
                unsafe {
                    (self.api.release_feature)(self.handle);
                }
            }
        }
    }

    impl DlssRayReconstructor {
        pub fn new(
            device: &wgpu::Device,
            sdk_dir: &Path,
            extent: (u32, u32),
        ) -> Result<Self, String> {
            let extent = (extent.0.max(1), extent.1.max(1));
            let (raw_device, raw_queue) = raw_dx12(device)?;
            let sdk_str = sdk_dir.to_string_lossy();
            let api = NgxApi::load(
                &sdk_str,
                DLSS_RR_DLL,
                raw_device.as_mut_ptr() as *mut c_void,
            )?;
            let mut params = api.new_parameters(false);
            params.set_u32(P_CREATION_NODE_MASK, 1);
            params.set_u32(P_VISIBILITY_NODE_MASK, 1);
            params.set_u32(P_WIDTH, extent.0);
            params.set_u32(P_HEIGHT, extent.1);
            params.set_u32(P_OUT_WIDTH, extent.0);
            params.set_u32(P_OUT_HEIGHT, extent.1);
            params.set_i32(P_PERF_QUALITY, NGX_PERF_QUALITY_DLAA);
            params.set_i32(P_CREATE_FLAGS, NGX_DLSSD_CREATE_FLAGS);
            params.set_i32(P_OUTPUT_SUBRECTS, 0);
            params.set_i32(P_DENOISE_MODE, NGX_DENOISE_MODE_DL_UNIFIED);
            params.set_u32(P_ROUGHNESS_MODE, NGX_ROUGHNESS_MODE_PACKED);
            params.set_u32(P_USE_HW_DEPTH, NGX_DEPTH_TYPE_HW);

            let mut ring = NativeCommandRing::new(&raw_device, "DLSS-RR")?;
            let slot_idx = ring.acquire("DLSS-RR-Create")?;
            let mut handle: *mut c_void = std::ptr::null_mut();
            let result = unsafe {
                let list = ring.slot_mut(slot_idx).list.as_mut_ptr() as *mut c_void;
                (api.create_feature)(
                    list,
                    NGX_FEATURE_RAY_RECONSTRUCTION,
                    params.as_ptr(),
                    &mut handle,
                )
            };
            ring.retire(slot_idx, &raw_queue, "DLSS-RR-Create")?;
            ring.wait_idle();
            if !crate::dlss::succeeded(result) || handle.is_null() {
                return Err(format!(
                    "DLSS Ray Reconstruction CreateFeature failed ({})",
                    crate::dlss::result_name(result)
                ));
            }
            let natives = RrNatives::new(device, &raw_device, extent)?;
            let prep = PrepPipeline::new(device);
            crate::dlss::diag(&format!(
                "[DLSS-RR] initialized nvngx_dlssd.dll ({}x{}, DLUnified + packed roughness)",
                extent.0, extent.1
            ));
            Ok(Self {
                api,
                params,
                handle,
                queue: raw_queue,
                ring,
                extent,
                natives,
                prep,
                prep_bg: None,
                frame_index: 0,
                param_slot: 0,
                last_instant: Instant::now(),
            })
        }

        pub fn extent(&self) -> (u32, u32) {
            self.extent
        }

        /// Drop the cached prep bind group so it is rebuilt on the next frame
        /// (e.g. after `grid_extent` changes while `render_extent` stays fixed).
        pub fn invalidate_prep(&mut self) {
            self.prep_bg = None;
        }

        pub fn evaluate(
            &mut self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            post_encoder: &mut wgpu::CommandEncoder,
            inputs: &DlssRrInputs<'_>,
            dst_output: &wgpu::Texture,
        ) -> Result<(), String> {
            let (w, h) = self.extent;
            // 1. Resample ray grid + hardware depth into our NativeTexture2D inputs.
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("raywatcher dlss-rr prep"),
            });
            #[repr(C)]
            #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
            struct PrepUniformData {
                flags: [f32; 4],
                prev_vp_inv_vp: [[f32; 4]; 4],
            }
            let uniform_data = PrepUniformData {
                flags: [
                    if inputs.layered { 1.0f32 } else { 0.0 },
                    inputs.scene_albedo_gain.clamp(1.0, 9.0),
                    if inputs.history_valid && !inputs.reset {
                        1.0f32
                    } else {
                        0.0
                    },
                    0.0,
                ],
                prev_vp_inv_vp: inputs.prev_vp_inv_vp,
            };
            queue.write_buffer(&self.prep.flags_buf, 0, bytemuck::bytes_of(&uniform_data));
            let raster_view = inputs.raster.unwrap_or(inputs.grid_colour);
            let bg = self.prep_bg.get_or_insert_with(|| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("raywatcher dlss-rr prep"),
                    layout: &self.prep.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(inputs.grid_colour),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(inputs.grid_reflection),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(inputs.grid_normals),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(inputs.grid_albedo),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(inputs.depth),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: wgpu::BindingResource::Sampler(&self.prep.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: wgpu::BindingResource::TextureView(raster_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 7,
                            resource: self.prep.flags_buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 8,
                            resource: wgpu::BindingResource::TextureView(inputs.motion),
                        },
                    ],
                })
            });
            fn attach(view: &wgpu::TextureView) -> Option<wgpu::RenderPassColorAttachment<'_>> {
                Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })
            }
            {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("raywatcher dlss-rr prep"),
                    color_attachments: &[
                        attach(&self.natives.color_view),
                        attach(&self.natives.depth_view),
                        attach(&self.natives.normals_view),
                        attach(&self.natives.diffuse_view),
                        attach(&self.natives.specular_view),
                        attach(&self.natives.hit_view),
                        attach(&self.natives.mv_view),
                    ],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                rp.set_pipeline(&self.prep.pipeline);
                rp.set_bind_group(0, bg, &[]);
                rp.draw(0..3, 0..1);
            }

            queue.submit(Some(encoder.finish()));

            // 2. Run NVSDK_NGX_D3D12_EvaluateFeature(RayReconstruction) on DX12.
            let now = Instant::now();
            let frame_time_ms = now
                .duration_since(self.last_instant)
                .as_secs_f32()
                .mul_add(1000.0, 0.0)
                .clamp(0.1, 1000.0);
            self.last_instant = now;

            let slot_idx = self.ring.acquire("DLSS-RR-Eval")?;
            let list = self.ring.slot_mut(slot_idx).list.as_mut_ptr();

            let rt_inputs = [
                self.natives.color.resource.as_mut_ptr(),
                self.natives.depth.resource.as_mut_ptr(),
                self.natives.normals.resource.as_mut_ptr(),
                self.natives.diffuse_albedo.resource.as_mut_ptr(),
                self.natives.specular_albedo.resource.as_mut_ptr(),
                self.natives.hit_distance.resource.as_mut_ptr(),
            ];
            let mv_res = self.natives.mv.resource.as_mut_ptr();
            let out_res = self.natives.output.resource.as_mut_ptr();

            unsafe {
                let mut before: [d12::D3D12_RESOURCE_BARRIER; 8] = zeroed();
                for (i, res) in rt_inputs.into_iter().enumerate() {
                    before[i].Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                    *before[i].u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: res,
                        Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: d12::D3D12_RESOURCE_STATE_RENDER_TARGET,
                        StateAfter: d12::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    };
                }
                before[6].Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                *before[6].u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: mv_res,
                    Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: d12::D3D12_RESOURCE_STATE_RENDER_TARGET,
                    StateAfter: d12::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                };
                let out_wgpu_state = if self.frame_index == 0 {
                    d12::D3D12_RESOURCE_STATE_COMMON
                } else {
                    d12::D3D12_RESOURCE_STATE_COPY_SOURCE
                };
                before[7].Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                *before[7].u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: out_res,
                    Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: out_wgpu_state,
                    StateAfter: d12::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                };
                (*list).ResourceBarrier(before.len() as u32, before.as_ptr());
            }

            let ladder: Vec<usize> = if self.frame_index == 0 {
                resource_slot_ladder(self.param_slot)
            } else {
                vec![self.param_slot]
            };
            let mut result = NGX_FAIL;
            let mut winning_slot = self.param_slot;
            let mut replacement: Option<Box<NgxParams>> = None;
            let mut w2v = inputs.world_to_view;
            let mut v2c = inputs.view_to_clip;
            for (attempt, slot) in ladder.into_iter().enumerate() {
                if attempt > 0 {
                    replacement = Some(self.api.new_parameters(false));
                }
                let params = match attempt {
                    0 => &mut self.params,
                    _ => replacement.as_mut().expect("replacement map"),
                };
                // Populate static dimensions, subrects, constants, and native
                // resource pointers once on frame 0 (or on a retry map).
                if self.frame_index == 0 || attempt > 0 {
                    params.set_resource_set_slot(slot);
                    params.set_f32(P_MV_SCALE_X, 1.0);
                    params.set_f32(P_MV_SCALE_Y, 1.0);
                    params.set_f32(P_PRE_EXPOSURE, 1.0);
                    params.set_f32(P_EXPOSURE_SCALE, 1.0);
                    params.set_u32(P_TONEMAPPER_TYPE, 0);
                    params.set_i32(P_INDICATOR_INVERT_X, 0);
                    params.set_i32(P_INDICATOR_INVERT_Y, 0);
                    for name in P_ZERO_SUBRECTS {
                        params.set_u32(name, 0);
                    }
                    params.set_u32(P_RENDER_WIDTH, w);
                    params.set_u32(P_RENDER_HEIGHT, h);
                    for (name, val) in [
                        (P_WIDTH, w),
                        (P_HEIGHT, h),
                        (P_OUT_WIDTH, w),
                        (P_OUT_HEIGHT, h),
                        (P_RESOURCE_WIDTH, w),
                        (P_RESOURCE_HEIGHT, h),
                        (P_RESOURCE_OUT_WIDTH, w),
                        (P_RESOURCE_OUT_HEIGHT, h),
                    ] {
                        params.set_u32(name, val);
                    }
                    params.set_d3d12_resource(P_COLOR, rt_inputs[0] as *mut c_void);
                    params.set_d3d12_resource(P_DEPTH, rt_inputs[1] as *mut c_void);
                    params.set_d3d12_resource(P_MOTION_VECTORS, mv_res as *mut c_void);
                    params.set_d3d12_resource(P_OUTPUT, out_res as *mut c_void);
                    params.set_d3d12_resource(P_GBUFFER_NORMALS, rt_inputs[2] as *mut c_void);
                    params.set_d3d12_resource(P_DIFFUSE_ALBEDO, rt_inputs[3] as *mut c_void);
                    params.set_d3d12_resource(P_SPECULAR_ALBEDO, rt_inputs[4] as *mut c_void);
                    params.set_d3d12_resource(P_HIT_DISTANCE, rt_inputs[5] as *mut c_void);
                }
                params.set_f32(P_JITTER_X, inputs.jitter.0);
                params.set_f32(P_JITTER_Y, inputs.jitter.1);
                params.set_i32(P_RESET, i32::from(inputs.reset || self.frame_index == 0));
                params.set_f32(P_FRAME_TIME_MS, frame_time_ms);
                params.set_void_pointer(P_WORLD_TO_VIEW, w2v.as_mut_ptr() as *mut c_void);
                params.set_void_pointer(P_VIEW_TO_CLIP, v2c.as_mut_ptr() as *mut c_void);

                result = unsafe {
                    (self.api.evaluate_feature)(
                        list as *mut c_void,
                        self.handle,
                        params.as_ptr(),
                        std::ptr::null(),
                    )
                };
                if crate::dlss::succeeded(result) {
                    winning_slot = slot;
                    break;
                }
            }

            unsafe {
                let mut after: [d12::D3D12_RESOURCE_BARRIER; 8] = zeroed();
                for (i, res) in rt_inputs.into_iter().enumerate() {
                    after[i].Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                    *after[i].u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: res,
                        Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: d12::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        StateAfter: d12::D3D12_RESOURCE_STATE_RENDER_TARGET,
                    };
                }
                after[6].Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                *after[6].u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: mv_res,
                    Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: d12::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    StateAfter: d12::D3D12_RESOURCE_STATE_RENDER_TARGET,
                };
                let out_wgpu_state = if self.frame_index == 0 {
                    d12::D3D12_RESOURCE_STATE_COMMON
                } else {
                    d12::D3D12_RESOURCE_STATE_COPY_SOURCE
                };
                after[7].Type = d12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                *after[7].u.Transition_mut() = d12::D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: out_res,
                    Subresource: d12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: d12::D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    StateAfter: out_wgpu_state,
                };
                (*list).ResourceBarrier(after.len() as u32, after.as_ptr());
            }

            self.ring.retire(slot_idx, &self.queue, "DLSS-RR-Eval")?;

            if !crate::dlss::succeeded(result) {
                return Err(format!(
                    "DLSS Ray Reconstruction EvaluateFeature failed ({})",
                    crate::dlss::result_name(result)
                ));
            }
            if let Some(map) = replacement {
                self.params = map;
            }
            self.param_slot = winning_slot;
            self.frame_index = self.frame_index.saturating_add(1);

            // 3. Record the output copy into the caller's post-RR encoder so it
            // batches with the final blit in a single queue submission.
            post_encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.natives.output.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: dst_output,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: w.min(dst_output.width()),
                    height: h.min(dst_output.height()),
                    depth_or_array_layers: 1,
                },
            );
            Ok(())
        }
    }
}

#[cfg(windows)]
pub use dlss_rr::DlssRayReconstructor;

#[cfg(not(windows))]
pub struct DlssRayReconstructor;

#[cfg(not(windows))]
impl DlssRayReconstructor {
    pub fn new(
        _device: &wgpu::Device,
        _sdk_dir: &Path,
        _extent: (u32, u32),
    ) -> Result<Self, String> {
        Err("DLSS Ray Reconstruction is Windows + DX12 only".into())
    }

    pub fn extent(&self) -> (u32, u32) {
        (0, 0)
    }

    pub fn invalidate_prep(&mut self) {}

    pub fn evaluate(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _post_encoder: &mut wgpu::CommandEncoder,
        _inputs: &DlssRrInputs<'_>,
        _dst_output: &wgpu::Texture,
    ) -> Result<(), String> {
        Err("DLSS Ray Reconstruction is Windows + DX12 only".into())
    }
}

#[cfg(windows)]
mod ffi {
    //! The FFX denoiser API, transcribed from `sdk/include/denoisers/ffx_denoiser.h`.
    //!
    //! The FFX API is versioned by descriptor type, not by symbol: every entry
    //! point is one of the five `ffx*` functions, and which effect runs is
    //! decided by the header `type` of the descriptor passed in. The denoiser is
    //! effect id `0x00050000`, so a descriptor type is
    //! `0x00050000 << ... | sub_id` — the same `FFX_API_MAKE_EFFECT_SUB_ID`
    //! macro the header expands.
    #![allow(dead_code)]

    use super::Probe;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    pub const FFX_DENOISER_VERSION: u32 = (1 << 22) | (2 << 12) | 1;
    pub const FFX_API_EFFECT_ID_DENOISER: u64 = 0x0005_0000;
    /// `FFX_API_MAKE_EFFECT_SUB_ID(effect, sub)` = `effect | sub << 16`.
    pub const fn sub_id(effect: u64, sub: u64) -> u64 {
        effect | (sub << 16)
    }
    pub const CREATE_DESC_TYPE_DENOISER: u64 = sub_id(FFX_API_EFFECT_ID_DENOISER, 0x01);
    pub const CONFIGURE_DESC_TYPE_DENOISER_KEYVALUE: u64 = sub_id(FFX_API_EFFECT_ID_DENOISER, 0x21);
    pub const DISPATCH_DESC_TYPE_DENOISER: u64 = sub_id(FFX_API_EFFECT_ID_DENOISER, 0x41);
    pub const DISPATCH_DESC_TYPE_DENOISER_AMBIENT_OCCLUSION: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x43);
    pub const DISPATCH_DESC_TYPE_DENOISER_DIRECT_DIFFUSE: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x44);
    pub const DISPATCH_DESC_TYPE_DENOISER_DIRECT_SPECULAR: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x45);
    pub const DISPATCH_DESC_TYPE_DENOISER_DOMINANT_LIGHT: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x46);
    pub const DISPATCH_DESC_TYPE_DENOISER_INDIRECT_DIFFUSE: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x47);
    pub const DISPATCH_DESC_TYPE_DENOISER_INDIRECT_SPECULAR: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x48);
    pub const DISPATCH_DESC_TYPE_DENOISER_SPECULAR_OCCLUSION: u64 =
        sub_id(FFX_API_EFFECT_ID_DENOISER, 0x49);

    pub const SIGNAL_AMBIENT_OCCLUSION: u32 = 1;
    pub const SIGNAL_DIRECT_DIFFUSE: u32 = 2;
    pub const SIGNAL_DIRECT_SPECULAR: u32 = 4;
    pub const SIGNAL_DOMINANT_LIGHT: u32 = 8;
    pub const SIGNAL_INDIRECT_DIFFUSE: u32 = 16;
    pub const SIGNAL_INDIRECT_SPECULAR: u32 = 32;
    pub const SIGNAL_SPECULAR_OCCLUSION: u32 = 64;

    pub const DISPATCH_FLAG_RESET: u32 = 1;
    pub const DISPATCH_FLAG_NON_ENCODED_ALBEDO: u32 = 2;

    pub const CREATE_FLAG_VALIDATION: u32 = 2;
    pub const RETURN_OK: u32 = 0;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Header {
        pub r#type: u64,
        pub p_next: *mut Header,
    }

    impl Header {
        pub fn new(r#type: u64) -> Self {
            Self {
                r#type,
                p_next: std::ptr::null_mut(),
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Dimensions2D {
        pub width: u32,
        pub height: u32,
    }

    /// `FfxApiFloatCoords3D`.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Coords3D {
        pub x: f32,
        pub y: f32,
        pub z: f32,
    }

    /// `FfxApiResource`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Resource {
        pub resource: *mut c_void,
        pub kind: u32,
        pub format: u32,
        pub width: u32,
        pub height: u32,
        pub depth: u32,
        pub mip_count: u32,
        pub flags: u32,
        pub usage: u32,
        pub state: u32,
    }

    impl Resource {
        pub const EMPTY: Resource = Resource {
            resource: std::ptr::null_mut(),
            kind: 0,
            format: 0,
            width: 0,
            height: 0,
            depth: 0,
            mip_count: 0,
            flags: 0,
            usage: 0,
            state: 0,
        };
    }

    pub const RESOURCE_TYPE_TEXTURE2D: u32 = 2;
    pub const RESOURCE_USAGE_UAV: u32 = 1 << 1;
    pub const RESOURCE_STATE_UNORDERED_ACCESS: u32 = 1 << 1;
    pub const RESOURCE_STATE_PIXEL_COMPUTE_READ: u32 = (1 << 2) | (1 << 3);
    pub const SURFACE_FORMAT_R16G16B16A16_FLOAT: u32 = 20;
    pub const SURFACE_FORMAT_R32_FLOAT: u32 = 28;
    pub const SURFACE_FORMAT_R8G8B8A8_UNORM: u32 = 10;
    pub const SURFACE_FORMAT_R16G16_FLOAT: u32 = 18;
    pub const SURFACE_FORMAT_R16_UINT: u32 = 41;

    #[repr(C)]
    pub struct CreateContextDescDenoiser {
        pub header: Header,
        pub version: u32,
        pub max_render_size: Dimensions2D,
        pub signal_flags: u32,
        pub checkerboard_signal_flags: u32,
        pub flags: u32,
    }

    #[repr(C)]
    pub struct DispatchDescDenoiser {
        pub header: Header,
        pub command_list: *mut c_void,
        pub linear_depth: Resource,
        pub motion_vectors: Resource,
        pub normals: Resource,
        pub specular_albedo: Resource,
        pub diffuse_albedo: Resource,
        pub motion_vector_scale: Coords3D,
        pub jitter_offsets: [f32; 2],
        pub camera_position_delta: Coords3D,
        pub view: [[f32; 4]; 4],
        pub projection: [[f32; 4]; 4],
        pub linear_depth_bounds: [f32; 2],
        pub render_size: Dimensions2D,
        pub frame_index: u32,
        pub flags: u32,
    }

    #[repr(C)]
    pub struct Signal {
        pub input: Resource,
        pub output: Resource,
        pub checkerboard_origin: u32,
    }

    #[repr(C)]
    pub struct DispatchDescSignal {
        pub header: Header,
        pub signal: Signal,
    }

    #[repr(C)]
    pub struct DispatchDescDominantLight {
        pub header: Header,
        pub signal: Signal,
        pub direction: Coords3D,
        pub emission: Coords3D,
        pub angular_radius: f32,
    }

    pub type PfnCreateContext =
        unsafe extern "C" fn(*mut *mut c_void, *mut Header, *const c_void) -> u32;
    pub type PfnDestroyContext = unsafe extern "C" fn(*mut *mut c_void, *const c_void) -> u32;
    pub type PfnQuery = unsafe extern "C" fn(*mut *mut c_void, *mut Header) -> u32;
    pub type PfnDispatch = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> u32;
    pub type PfnConfigure = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> u32;

    /// The five FFX entry points, loaded from the denoiser DLL.
    pub struct FfxDenoiser {
        handle: winapi::shared::minwindef::HMODULE,
        pub create_context: PfnCreateContext,
        pub destroy_context: PfnDestroyContext,
        pub query: PfnQuery,
        pub dispatch: PfnDispatch,
        pub configure: PfnConfigure,
    }

    impl Drop for FfxDenoiser {
        fn drop(&mut self) {
            unsafe { winapi::um::libloaderapi::FreeLibrary(self.handle) };
        }
    }

    impl FfxDenoiser {
        pub fn load(dir: &Path) -> Result<Self, String> {
            let candidate = dir.join(super::FSR_RR_DLL);
            if !candidate.is_file() {
                return Err(format!("{} not found", candidate.display()));
            }
            let wide: Vec<u16> = candidate
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let handle = unsafe { winapi::um::libloaderapi::LoadLibraryW(wide.as_ptr()) };
            if handle.is_null() {
                return Err(format!("LoadLibraryW({}) failed", candidate.display()));
            }
            let get = |name: &[u8]| -> Result<*const c_void, String> {
                let proc = unsafe {
                    winapi::um::libloaderapi::GetProcAddress(handle, name.as_ptr() as *const i8)
                };
                if proc.is_null() {
                    Err(format!(
                        "GetProcAddress({}) failed",
                        String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
                    ))
                } else {
                    Ok(proc as *const c_void)
                }
            };
            let loaded: Result<Self, String> = (|| {
                Ok(Self {
                    handle,
                    create_context: unsafe { std::mem::transmute(get(b"ffxCreateContext\0")?) },
                    destroy_context: unsafe { std::mem::transmute(get(b"ffxDestroyContext\0")?) },
                    query: unsafe { std::mem::transmute(get(b"ffxQuery\0")?) },
                    dispatch: unsafe { std::mem::transmute(get(b"ffxDispatch\0")?) },
                    configure: unsafe { std::mem::transmute(get(b"ffxConfigure\0")?) },
                })
            })();
            if loaded.is_err() {
                unsafe { winapi::um::libloaderapi::FreeLibrary(handle) };
            }
            loaded
        }
    }

    pub fn signal_flags(effects: super::super::plan::Effects) -> u32 {
        let mut flags = 0;
        if effects.ambient_occlusion {
            flags |= SIGNAL_AMBIENT_OCCLUSION;
        }
        if effects.lighting {
            flags |= SIGNAL_DIRECT_DIFFUSE;
        }
        if effects.reflections {
            flags |= SIGNAL_DIRECT_SPECULAR;
        }
        if effects.global_illumination {
            flags |= SIGNAL_INDIRECT_DIFFUSE;
        }
        if effects.shadows {
            flags |= SIGNAL_DOMINANT_LIGHT;
        }
        flags
    }

    pub fn probe(dir: &Path) -> Probe {
        match FfxDenoiser::load(dir) {
            Ok(_) => Probe::available(format!("{} loaded", super::FSR_RR_DLL)),
            Err(error) => Probe::unavailable(error),
        }
    }
}

#[cfg(windows)]
pub use ffi::FfxDenoiser;

#[cfg(not(windows))]
mod ffi_stub {
    use super::Probe;
    use std::path::Path;

    #[derive(Debug)]
    pub struct FfxDenoiser {
        pub _private: (),
    }

    impl FfxDenoiser {
        pub fn load(_dir: &Path) -> Result<Self, String> {
            Err("the FFX denoiser is Windows + DX12 only".to_string())
        }
    }

    pub fn probe(_dir: &Path) -> Probe {
        Probe::unavailable("the FFX denoiser is Windows + DX12 only".to_string())
    }
}

#[cfg(not(windows))]
use ffi_stub as ffi;
#[cfg(not(windows))]
pub use ffi_stub::FfxDenoiser;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_provider_dlls_are_named_exactly_as_the_sdks_ship_them() {
        assert_eq!(DLSS_RR_DLL, "nvngx_dlssd.dll");
        assert_eq!(FSR_RR_DLL, "amd_fidelityfx_denoiser_dx12.dll");
    }

    #[test]
    fn probing_on_an_unsupported_platform_explains_both_providers() {
        let (dlss, fsr) = probe_offline(Path::new("/nonexistent"), false);
        assert!(!dlss.available);
        assert!(!fsr.available);
        assert!(dlss.note.contains("Windows"));
        assert!(fsr.note.contains("Windows"));
    }

    #[test]
    fn a_missing_dll_is_reported_with_the_folder_that_was_searched() {
        let dir = std::env::temp_dir().join("raywatcher-reconstruct-probe-empty");
        let (_, fsr) = probe_offline(&dir, true);
        assert!(!fsr.available);
        assert!(fsr.note.contains("amd_fidelityfx_denoiser_dx12.dll"));
        assert!(fsr.note.contains(&dir.display().to_string()));
    }

    #[test]
    fn a_present_dll_is_reported_as_available() {
        let dir = std::env::temp_dir().join("raywatcher-reconstruct-probe-present");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(FSR_RR_DLL);
        std::fs::write(&path, b"placeholder").expect("write");
        let (_, fsr) = probe_offline(&dir, true);
        assert!(fsr.available, "{}", fsr.note);
        assert!(fsr.note.contains("found"));
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(all(test, windows))]
mod ffi_tests {
    use super::ffi::*;

    #[test]
    fn the_descriptor_types_match_the_sdk_header() {
        assert_eq!(CREATE_DESC_TYPE_DENOISER, 0x0005_0100);
        assert_eq!(CONFIGURE_DESC_TYPE_DENOISER_KEYVALUE, 0x0005_2100);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER, 0x0005_4100);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_AMBIENT_OCCLUSION, 0x0005_4300);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_DIRECT_DIFFUSE, 0x0005_4400);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_DIRECT_SPECULAR, 0x0005_4500);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_DOMINANT_LIGHT, 0x0005_4600);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_INDIRECT_DIFFUSE, 0x0005_4700);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_INDIRECT_SPECULAR, 0x0005_4800);
        assert_eq!(DISPATCH_DESC_TYPE_DENOISER_SPECULAR_OCCLUSION, 0x0005_4900);
        assert_eq!(FFX_DENOISER_VERSION, (1 << 22) | (2 << 12) | 1);
    }

    #[test]
    fn the_signal_flags_are_the_ones_the_header_defines() {
        assert_eq!(
            [
                SIGNAL_AMBIENT_OCCLUSION,
                SIGNAL_DIRECT_DIFFUSE,
                SIGNAL_DIRECT_SPECULAR,
                SIGNAL_DOMINANT_LIGHT,
                SIGNAL_INDIRECT_DIFFUSE,
                SIGNAL_INDIRECT_SPECULAR,
                SIGNAL_SPECULAR_OCCLUSION,
            ],
            [1, 2, 4, 8, 16, 32, 64]
        );
    }
}
