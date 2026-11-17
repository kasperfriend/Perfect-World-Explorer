//! Windows + DX12 implementation of DLSS super resolution. See the module
//! documentation in `super` for the overall design and limits.
//!
//! The shape matches the XeSS and FSR paths exactly: the world is rendered into
//! the scaled offscreen target, [`DlssUpscaler::record`] copies that colour,
//! the depth buffer and the reconstructed motion vectors into native textures
//! and runs `NVSDK_NGX_D3D12_EvaluateFeature` on its own D3D12 command list,
//! and [`DlssUpscaler::composite`] blits the full-resolution result into the
//! composition. The NGX call is given an `NVSDK_NGX_Parameter` table the viewer
//! owns (`super::params`), because the NGX DLLs export the feature entry points
//! but no parameter allocator.

use super::loader::{NgxApi, NGX_FEATURE_SUPER_SAMPLING, NGX_VERSION_API};
use super::params::NgxParams;
use crate::fsr::{
    dxgi_output_format, raw_dx12, FsrComposite, FsrMotionVectorPass, FsrMvParams,
    NativeCommandRing, NativeTexture2D, D3D12_STATE_COMMON, D3D12_STATE_COPY_DEST,
    D3D12_STATE_PIXEL_COMPUTE_READ, D3D12_STATE_RENDER_TARGET, D3D12_STATE_UNORDERED_ACCESS,
};
use glam::Mat4;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use winapi::shared::dxgiformat::{DXGI_FORMAT_R16G16_FLOAT, DXGI_FORMAT_R32_FLOAT};
use winapi::um::d3d12 as d3d12_ty;

/// Whether the NGX file logs have already been copied into `session.log`. One
/// copy is enough: the refusal repeats every frame, and each line would only
/// push the first frame's evidence out of the report.
static DUMPED_NGX_LOGS: AtomicBool = AtomicBool::new(false);

/// `D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE` — the state NGX wants its
/// inputs in (DLSS and DLSS-G both dispatch compute).
const D3D12_STATE_NON_PIXEL_READ: u32 = d3d12_ty::D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;

// `NVSDK_NGX_PerfQuality_Value` values for DLSS live in `crate::modes` (the
// provider × quality table) rather than here, so the value handed to NGX and
// the mode the UI advertises cannot drift apart. `UltraQuality` = 4 is
// deliberately absent from that table: the installed core
// (32.0.16.2114) rejects it with `0xbad00010`.

/// `NVSDK_NGX_DLSS_Feature_Flags_MVLowRes`: motion vectors are at render
/// resolution rather than display resolution.
const NGX_DLSS_FEATURE_MV_LOW_RES: i32 = 1 << 1;
/// `NVSDK_NGX_DLSS_Feature_Flags_DepthInverted`: the viewer's depth is
/// reversed-Z (near→1, far→0), the same convention FSR and XeSS are told.
const NGX_DLSS_FEATURE_DEPTH_INVERTED: i32 = 1 << 3;

// Parameter names, spelled as the vendored `nvsdk_ngx_defs.h` defines them.
const P_WIDTH: &str = "Width";
const P_HEIGHT: &str = "Height";
const P_OUT_WIDTH: &str = "OutWidth";
const P_OUT_HEIGHT: &str = "OutHeight";
const P_PERF_QUALITY: &str = "PerfQualityValue";
const P_CREATE_FLAGS: &str = "DLSS.Feature.Create.Flags";
/// `NVSDK_NGX_Parameter_CreationNodeMask` / `..._VisibilityNodeMask`.
///
/// `NGX_D3D12_CREATE_DLSS_EXT` (`nvsdk_ngx_helpers_d3d.h:283-284`) sets both on
/// every create, and NGX keeps them on the feature it hands back. Leaving them
/// unset means the feature was created with an adapter mask of `0`, which names
/// no adapter at all — the same mistake the D3D12 heaps used to make (see
/// `docs/blockers.md`, B-22). This viewer renders on one GPU, so both masks name
/// adapter 0.
const P_CREATION_NODE_MASK: &str = "CreationNodeMask";
const P_VISIBILITY_NODE_MASK: &str = "VisibilityNodeMask";
/// `NVSDK_NGX_Parameter_DLSS_Enable_Output_Subrects`, which the reference
/// wrapper also always writes. The viewer upscales the whole frame, so subrect
/// output stays off.
const P_OUTPUT_SUBRECTS: &str = "DLSS.Enable.Output.Subrects";
const P_COLOR: &str = "Color";
const P_DEPTH: &str = "Depth";
const P_MOTION_VECTORS: &str = "MotionVectors";
const P_OUTPUT: &str = "Output";
/// `NVSDK_NGX_Parameter_SuperSampling_Available` / `…FeatureInitResult`:
/// NGX's own verdicts, written into the map it was handed. Reading them back is
/// how the viewer knows whether DLSS is supported at all and whether the feature
/// really initialised, instead of trusting a `Success` HRESULT.
const P_SUPER_SAMPLING_AVAILABLE: &str = "SuperSampling.Available";
const P_FEATURE_INIT_RESULT: &str = "SuperSampling.FeatureInitResult";
/// `NVSDK_NGX_Parameter_Resource_*`: the dimensions of the *resources* holding
/// each image, which only differ from `Width`/`Height` when subrects are used.
/// This viewer hands over whole resources, so they repeat the extents.
const P_RESOURCE_WIDTH: &str = "ResourceWidth";
const P_RESOURCE_HEIGHT: &str = "ResourceHeight";
const P_RESOURCE_OUT_WIDTH: &str = "ResourceOutWidth";
const P_RESOURCE_OUT_HEIGHT: &str = "ResourceOutHeight";
const P_JITTER_X: &str = "Jitter.Offset.X";
const P_JITTER_Y: &str = "Jitter.Offset.Y";
const P_RESET: &str = "Reset";
const P_MV_SCALE_X: &str = "MV.Scale.X";
const P_MV_SCALE_Y: &str = "MV.Scale.Y";
const P_FRAME_TIME_MS: &str = "FrameTimeDeltaInMsec";
const P_RENDER_WIDTH: &str = "DLSS.Render.Subrect.Dimensions.Width";
const P_RENDER_HEIGHT: &str = "DLSS.Render.Subrect.Dimensions.Height";
const P_SHARPNESS: &str = "Sharpness";
/// `NVSDK_NGX_Parameter_DLSS_Pre_Exposure`. The reference wrapper
/// (`NGX_D3D12_EVALUATE_DLSS_EXT`, `nvsdk_ngx_helpers_d3d.h:355`) always sets
/// this and its sibling below, converting `0.0` to `1.0` — NGX divides by the
/// pre-exposure, so a missing/zero value is rejected with
/// `FAIL_InvalidParameter` (`0xbad00005`). That is exactly what every DLSS
/// evaluate returned until this was added: `NVSDK_NGX_D3D12_EvaluateFeature(DLSS)
/// failed (0xbad00005)`.
const P_PRE_EXPOSURE: &str = "DLSS.Pre.Exposure";
const P_EXPOSURE_SCALE: &str = "DLSS.Exposure.Scale";
const P_TONEMAPPER_TYPE: &str = "TonemapperType";
const P_INDICATOR_INVERT_X: &str = "DLSS.Indicator.Invert.X.Axis";
const P_INDICATOR_INVERT_Y: &str = "DLSS.Indicator.Invert.Y.Axis";
/// Subrect bases, all zero because the viewer renders and upscales full-frame.
/// The reference wrapper sets every one of them on every evaluate.
const P_ZERO_SUBRECTS: [&str; 12] = [
    "DLSS.Input.Color.Subrect.Base.X",
    "DLSS.Input.Color.Subrect.Base.Y",
    "DLSS.Input.Depth.Subrect.Base.X",
    "DLSS.Input.Depth.Subrect.Base.Y",
    "DLSS.Input.MV.Subrect.Base.X",
    "DLSS.Input.MV.Subrect.Base.Y",
    "DLSS.Input.Translucency.Subrect.Base.X",
    "DLSS.Input.Translucency.Subrect.Base.Y",
    "DLSS.Output.Subrect.Base.X",
    "DLSS.Output.Subrect.Base.Y",
    "DLSS.Input.Bias.Current.Color.Subrect.Base.X",
    "DLSS.Input.Bias.Current.Color.Subrect.Base.Y",
];
/// Optional resources the reference wrapper binds as `nullptr` on every
/// evaluate. NGX's parameter map is expected to carry the keys even when
/// nothing is bound, so they are set explicitly rather than left out.
const P_NULL_RESOURCES: [&str; 26] = [
    "TransparencyMask",
    "ExposureTexture",
    "DLSS.Input.Bias.Current.Color.Mask",
    "GBuffer.Albedo",
    "GBuffer.Roughness",
    "GBuffer.Metallic",
    "GBuffer.Specular",
    "GBuffer.Subsurface",
    "GBuffer.Normals",
    "GBuffer.ShadingModelId",
    "GBuffer.MaterialId",
    "GBuffer.Attrib.8",
    "GBuffer.Attrib.9",
    "GBuffer.Attrib.10",
    "GBuffer.Attrib.11",
    "GBuffer.Attrib.12",
    "GBuffer.Attrib.13",
    "GBuffer.Attrib.14",
    "GBuffer.Attrib.15",
    "MotionVectors3D",
    "IsParticleMask",
    "AnimatedTextureMask",
    "DepthHighRes",
    "Position.ViewSpace",
    "RayTracingHitDistance",
    "MotionVectorsReflection",
];

/// In-tree Halton jitter in pixels ([-0.5, 0.5)), 16 phases.
fn halton(mut index: u32, base: u32) -> f32 {
    let mut result = 0.0f32;
    let mut factor = 1.0f32 / base as f32;
    while index > 0 {
        result += factor * (index % base) as f32;
        index /= base;
        factor /= base as f32;
    }
    result
}

fn jitter_for_frame(frame: u64, phase_count: u64) -> (f32, f32) {
    let n = (frame % phase_count.max(1)) as u32 + 1;
    (halton(n, 2) - 0.5, halton(n, 3) - 0.5)
}

struct DlssNatives {
    color: NativeTexture2D,
    depth: NativeTexture2D,
    mv: NativeTexture2D,
    output: NativeTexture2D,
}

impl DlssNatives {
    /// # Safety
    /// `raw_device` must be the device `device` was created from.
    unsafe fn create(
        device: &wgpu::Device,
        raw_device: &d3d12::Device,
        render_extent: (u32, u32),
        display_extent: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> Result<Self, String> {
        // UNORM description, not `_UNORM_SRGB`: NGX builds its SRV from the
        // resource description, so an sRGB one would give DLSS linear pixels
        // while `IsHDR` is clear (i.e. "the input is sRGB-encoded LDR"), and it
        // would convert a second time. See `FsrNatives::create`.
        let color = NativeTexture2D::create(
            device,
            raw_device,
            "dlss color",
            format,
            dxgi_output_format(format),
            render_extent,
            // A render-target flag on top of the plain SRV the feature needs:
            // NGX is handed the viewer's colour buffer, which in every DLSS
            // sample is a render target, and a resource that cannot be a render
            // target at all is one more thing for its validation to reject.
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            false,
        )?;
        let depth = NativeTexture2D::create(
            device,
            raw_device,
            "dlss depth",
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
            "dlss motion vectors",
            wgpu::TextureFormat::Rg16Float,
            DXGI_FORMAT_R16G16_FLOAT,
            render_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            false,
        )?;
        // DLSS writes its result with a compute dispatch, so the output needs
        // UAV access — and a UAV cannot be an sRGB format, hence the plain
        // UNORM twin the FSR and XeSS outputs use as well.
        let output = NativeTexture2D::create(
            device,
            raw_device,
            "dlss output",
            format.remove_srgb_suffix(),
            dxgi_output_format(format),
            display_extent,
            d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                | d3d12_ty::D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            D3D12_STATE_COMMON,
            true,
        )?;
        crate::dlss::diag(&format!(
            "[DLSS] textures: colour {} {} heap, depth {} {}, motion vectors {} {}, output {} {}",
            color.desc_summary(),
            color.heap_kind(),
            depth.desc_summary(),
            depth.heap_kind(),
            mv.desc_summary(),
            mv.heap_kind(),
            output.desc_summary(),
            output.heap_kind()
        ));
        Ok(Self {
            color,
            depth,
            mv,
            output,
        })
    }
}

pub struct DlssUpscaler {
    api: NgxApi,
    params: Box<NgxParams>,
    handle: *mut c_void,
    queue: d3d12::CommandQueue,
    ring: NativeCommandRing,
    /// The extent DLSS actually evaluates at: NGX's own optimal-settings answer
    /// when the capability map could be asked, otherwise what the viewer asked
    /// for.
    render_extent: (u32, u32),
    /// What the settings table asked for. The caller compares *this* when it
    /// decides whether the upscaler still matches the settings, so NGX moving
    /// the render size by a pixel cannot make it rebuild every frame.
    requested_extent: (u32, u32),
    display_extent: (u32, u32),
    natives: DlssNatives,
    mv_pass: FsrMotionVectorPass,
    composite: FsrComposite,
    frame_index: u64,
    phase_count: u64,
    current_jitter: (f32, f32),
    prev_view_proj: Option<Mat4>,
    reset_history: bool,
    /// Which `NVSDK_NGX_Parameter::Set` overload this core's map answers the
    /// resource keys through, measured on the first evaluate (see `record`).
    /// It starts at the slot the header declares and stays there unless the
    /// feature itself disagrees.
    param_slot: usize,
    last_instant: std::time::Instant,
    quality_name: &'static str,
}

impl Drop for DlssUpscaler {
    fn drop(&mut self) {
        self.ring.wait_idle();
        if !self.handle.is_null() {
            unsafe {
                (self.api.release_feature)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

/// Everything one `NVSDK_NGX_D3D12_EvaluateFeature` call needs from the frame.
///
/// Kept as a plain value so the parameter block is written by one function, in
/// one order, from whichever map the evaluate attempt is using — the ladder in
/// `DlssUpscaler::record` writes the *same* block into a fresh map per attempt,
/// and a block written differently per attempt would make the attempts
/// incomparable.
struct EvaluateFrame {
    render: (u32, u32),
    display: (u32, u32),
    jitter: (f32, f32),
    reset: bool,
    frame_time_ms: f32,
}

/// Write the whole evaluate parameter block, resources included, through
/// `slot`.
///
/// This mirrors `NGX_D3D12_EVALUATE_DLSS_EXT` from `nvsdk_ngx_helpers_d3d.h`,
/// in the helper's order, plus the extent keys the header also names
/// (`Width`, `Height`, `OutWidth`, `OutHeight` and their `Resource*` twins):
/// NGX validates the map against the feature it created, and a key the helper
/// sets that the viewer leaves out is exactly how an evaluate ends up
/// answering `FAIL_InvalidParameter` while the create succeeds.
fn write_evaluate_parameters(
    params: &mut crate::dlss::params::NgxParams,
    slot: usize,
    frame: &EvaluateFrame,
    resources: [*mut c_void; 4],
) {
    // Which `Set` overload the pointer keys go through. Every write below the
    // numeric block uses this slot, so all four resources and the optional-null
    // entries agree with each other.
    params.set_resource_set_slot(slot);
    params.set_f32(P_JITTER_X, frame.jitter.0);
    params.set_f32(P_JITTER_Y, frame.jitter.1);
    params.set_i32(P_RESET, i32::from(frame.reset));
    // Motion vectors are in pixels, so the scale is 1:1.
    params.set_f32(P_MV_SCALE_X, 1.0);
    params.set_f32(P_MV_SCALE_Y, 1.0);
    // `InFrameTimeDeltaInMsec`: NGX scales how much it denoises and
    // anti-aliases by how far content moves per frame.
    params.set_f32(P_FRAME_TIME_MS, frame.frame_time_ms.clamp(0.1, 1000.0));
    // `Sharpness` stays 0 and the Graphics slider runs the viewer's own
    // RCAS-style pass in `DlssUpscaler::composite` instead: NGX's own
    // sharpening is deprecated ("Sharpness is not supported") and doing both
    // would double the halos.
    params.set_f32(P_SHARPNESS, 0.0);
    // NGX divides by the pre-exposure, so leaving it at the map's default 0
    // made every evaluate return `FAIL_InvalidParameter` — the feature created
    // fine and then never ran, which is why DLSS looked like "no upscaling at
    // all".
    params.set_f32(P_PRE_EXPOSURE, 1.0);
    params.set_f32(P_EXPOSURE_SCALE, 1.0);
    params.set_u32(P_TONEMAPPER_TYPE, 0);
    params.set_i32(P_INDICATOR_INVERT_X, 0);
    params.set_i32(P_INDICATOR_INVERT_Y, 0);
    for name in P_ZERO_SUBRECTS {
        params.set_u32(name, 0);
    }
    // The optional inputs stay absent rather than bogus: a null through the
    // resource overload is "not provided", which is what the helper does.
    for name in P_NULL_RESOURCES {
        params.set_d3d12_resource(name, std::ptr::null_mut());
    }
    params.set_u32(P_RENDER_WIDTH, frame.render.0.max(1));
    params.set_u32(P_RENDER_HEIGHT, frame.render.1.max(1));
    let (render_width, render_height) = frame.render;
    let (display_width, display_height) = frame.display;
    for (name, value) in [
        (P_WIDTH, render_width.max(1)),
        (P_HEIGHT, render_height.max(1)),
        (P_OUT_WIDTH, display_width.max(1)),
        (P_OUT_HEIGHT, display_height.max(1)),
        (P_RESOURCE_WIDTH, render_width.max(1)),
        (P_RESOURCE_HEIGHT, render_height.max(1)),
        (P_RESOURCE_OUT_WIDTH, display_width.max(1)),
        (P_RESOURCE_OUT_HEIGHT, display_height.max(1)),
    ] {
        params.set_u32(name, value);
    }
    // Colour, depth, motion vectors and output, in the helper's order.
    for (name, resource) in [
        (P_COLOR, resources[0]),
        (P_DEPTH, resources[1]),
        (P_MOTION_VECTORS, resources[2]),
        (P_OUTPUT, resources[3]),
    ] {
        params.set_d3d12_resource(name, resource);
    }
}

impl DlssUpscaler {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sdk_dir: &str,
        requested_extent: (u32, u32),
        display_extent: (u32, u32),
        format: wgpu::TextureFormat,
        quality: crate::UpscaleQuality,
    ) -> Result<Self, String> {
        let _ = queue;
        // The mode comes from the shared matrix (`crate::modes`), so the value
        // written to `PerfQualityValue` is the one the table advertises and a
        // mode DLSS does not have is refused here instead of being folded into
        // another one. `UltraQuality` (NGX value 4) is the case that matters:
        // the installed core answers `CreateFeature` with `0xbad00010`
        // (`FAIL_UnsupportedParameter`) for it — DLSS's native-resolution mode
        // is DLAA (value 5), which `NativeAa` maps to.
        let mode = crate::modes::require(crate::UpscaleMethod::Dlss, quality)?;
        let quality_setting = mode.vendor;
        let quality_name = mode.name;
        // §3.3.1 of the programming guide: "The minimum supported output
        // resolution for DLSS is 32x32. Attempting to initialize DLSS with a
        // smaller output resolution will fail with an Invalid Parameter error."
        // A minimised or collapsed window lands exactly there, and a real
        // failure there would spend the retry budget on a condition that fixes
        // itself the moment the window is restored; `src/gpu.rs` also skips
        // asking while the window is that small.
        if display_extent.0 < 32 || display_extent.1 < 32 {
            return Err(format!(
                "DLSS needs at least a 32x32 output; the window is {}x{}",
                display_extent.0, display_extent.1
            ));
        }
        let (raw_device, raw_queue) = raw_dx12(device)?;
        let device_ptr = raw_device.as_mut_ptr() as *mut c_void;

        // NGX Core (`nvngx.dll`, from the driver) initialises once for the
        // device and loads the vendored `nvngx_dlss.dll` itself; the snippet is never
        // called directly, because it refuses to initialise anything that is
        // not an NGX runtime (0xBAD00002).
        let api = NgxApi::load(sdk_dir, "nvngx_dlss.dll", device_ptr)?;
        // Two maps, because they are asked different questions. The capability
        // map carries the pre-populated verdicts and the optimal-settings
        // callback; the feature map is the one `CreateFeature` and
        // `EvaluateFeature` are handed, and it comes from the allocator the
        // header recommends for that job (B-27).
        let mut capabilities = api.new_parameters(true);
        let mut params = api.new_parameters(false);
        crate::dlss::diag(&format!(
            "[DLSS] NGX ready (API 0x{NGX_VERSION_API:x}, core {}), creating DLSS",
            api.core_path()
        ));

        match capabilities.get_u32(P_SUPER_SAMPLING_AVAILABLE) {
            Some(available) => crate::dlss::diag(&format!(
                "[DLSS] NGX reports SuperSampling.Available = {available}"
            )),
            None => crate::dlss::diag(
                "[DLSS] the capability map does not carry SuperSampling.Available",
            ),
        }
        // DLSS decides the render size, not the viewer. The programming guide
        // requires the values from the optimal-settings query to be passed to
        // create "exactly as given" (§3.2.2), and an evaluate subrect outside
        // the range those settings define is answered with
        // `FAIL_InvalidParameter` — the `0xbad00005` every run reported. When a
        // mode supports no dynamic resolution the range collapses to a single
        // pair, so even one pixel of the viewer's own ratio arithmetic is out of
        // range.
        let mut render_extent = requested_extent;
        match api.optimal_settings(&mut capabilities, display_extent, quality_setting) {
            Ok(settings) => {
                render_extent = settings.render;
                let range = match (settings.min, settings.max) {
                    (Some(min), Some(max)) => format!("{}x{}..{}x{}", min.0, min.1, max.0, max.1),
                    _ => format!("fixed {}x{}", settings.render.0, settings.render.1),
                };
                crate::dlss::diag(&format!(
                    "[DLSS] optimal settings: {quality_name} at {}x{} -> render {}x{} (range {range}; the viewer asked {}x{})",
                    display_extent.0,
                    display_extent.1,
                    render_extent.0,
                    render_extent.1,
                    requested_extent.0,
                    requested_extent.1
                ));
            }
            Err(error) => crate::dlss::diag(&format!(
                "[DLSS] optimal settings unavailable ({error}); using the viewer's {}x{}",
                requested_extent.0, requested_extent.1
            )),
        }
        params.set_u32(P_WIDTH, render_extent.0.max(1));
        params.set_u32(P_HEIGHT, render_extent.1.max(1));
        params.set_u32(P_OUT_WIDTH, display_extent.0.max(1));
        params.set_u32(P_OUT_HEIGHT, display_extent.1.max(1));
        // `SetI`, exactly as `NGX_D3D12_CREATE_DLSS_EXT` writes it
        // (`nvsdk_ngx_helpers_d3d.h`): `PerfQualityValue` is an int entry, and
        // the integer overloads are separate vtable slots, so a `SetUI` leaves
        // an entry NGX's own `GetI` may not see.
        params.set_i32(P_PERF_QUALITY, quality_setting as i32);
        let flags = NGX_DLSS_FEATURE_MV_LOW_RES | NGX_DLSS_FEATURE_DEPTH_INVERTED;
        params.set_i32(P_CREATE_FLAGS, flags);
        params.set_u32(P_CREATION_NODE_MASK, 1);
        params.set_u32(P_VISIBILITY_NODE_MASK, 1);
        params.set_i32(P_OUTPUT_SUBRECTS, 0);

        crate::dlss::diag(&format!(
            "[DLSS] create: {quality_name} (PerfQualityValue {quality_setting}), {}x{} -> {}x{}, flags 0x{flags:02x}, node masks 1/1, output subrects off",
            render_extent.0, render_extent.1, display_extent.0, display_extent.1
        ));
        let mut ring = NativeCommandRing::new(&raw_device, "DLSS")?;
        let mut handle: *mut c_void = std::ptr::null_mut();
        // Creation is allowed to record its own work (kernels/uploads) on the
        // list it is handed, so give it a real one and retire it afterwards.
        let slot_index = ring.acquire("DLSS")?;
        let result = {
            let list = &mut ring.slot_mut(slot_index).list;
            unsafe {
                (api.create_feature)(
                    list.as_mut_ptr() as *mut c_void,
                    NGX_FEATURE_SUPER_SAMPLING,
                    params.as_ptr(),
                    &mut handle,
                )
            }
        };
        if !crate::dlss::succeeded(result) || handle.is_null() {
            return Err(format!(
                "NVSDK_NGX_D3D12_CreateFeature(DLSS) failed ({})",
                crate::dlss::result_name(result)
            ));
        }
        let _ = ring.retire(slot_index, &raw_queue, "DLSS");
        match params.get_u32(P_FEATURE_INIT_RESULT) {
            Some(result) => crate::dlss::diag(&format!(
                "[DLSS] feature init result: {}",
                crate::dlss::result_name(result)
            )),
            None => {
                crate::dlss::diag("[DLSS] the parameter map has no SuperSampling.FeatureInitResult")
            }
        }

        crate::dlss::diag(&format!(
            "[DLSS] super resolution ready ({quality_name}, {}x{} -> {}x{})",
            render_extent.0, render_extent.1, display_extent.0, display_extent.1
        ));

        let natives = unsafe {
            DlssNatives::create(device, &raw_device, render_extent, display_extent, format)?
        };
        let mv_pass = FsrMotionVectorPass::new(device);
        let composite = FsrComposite::new(device, format);

        let ratio = display_extent.0.max(1) as f32 / render_extent.0.max(1) as f32;
        let phase_count = ((8.0 * ratio * ratio).ceil() as u64).clamp(16, 64);
        // The slot the map's own pointer round-trip reports, so the first
        // evaluate uses the measured overload rather than the declared one.
        let param_slot = params.resource_set_slot();
        Ok(Self {
            api,
            params,
            handle,
            queue: raw_queue,
            ring,
            render_extent,
            requested_extent,
            display_extent,
            natives,
            mv_pass,
            composite,
            frame_index: 0,
            phase_count,
            current_jitter: (0.0, 0.0),
            prev_view_proj: None,
            reset_history: true,
            param_slot,
            last_instant: std::time::Instant::now(),
            quality_name,
        })
    }

    /// The render extent the native passes use.
    pub fn render_extent(&self) -> (u32, u32) {
        self.render_extent
    }

    /// The display extent the output is produced at.
    pub fn display_extent(&self) -> (u32, u32) {
        self.display_extent
    }

    /// The render extent the settings asked for, before DLSS had its say.
    pub fn requested_extent(&self) -> (u32, u32) {
        self.requested_extent
    }

    /// Human-readable name of the active quality preset (UI badge).
    pub fn quality_name(&self) -> &'static str {
        self.quality_name
    }

    /// Halton jitter for the next frame, in pixels. Advances at record().
    pub fn next_jitter(&mut self) -> (f32, f32) {
        self.current_jitter = jitter_for_frame(self.frame_index, self.phase_count);
        self.current_jitter
    }

    /// The jitter currently in effect.
    pub fn jitter(&self) -> (f32, f32) {
        self.current_jitter
    }

    /// The jittered view-projection for this frame.
    ///
    /// DLSS wants the shear on the projection matrix *and* the same pixel
    /// offset in its evaluate call (`Jitter.Offset.X/Y`), so both describe the
    /// same sub-pixel sample position — see
    /// [`crate::fsr::jittered_clip_vp`].
    pub fn jittered_matrix(&self, matrix: Mat4) -> Mat4 {
        crate::fsr::jittered_clip_vp(matrix, self.current_jitter, self.render_extent)
    }

    /// On camera cuts, teleports and DLSS re-enables the accumulation history
    /// is meaningless; the next evaluate runs with `Reset`.
    pub fn reset_history(&mut self) {
        self.reset_history = true;
        self.prev_view_proj = None;
    }

    /// Copies the world colour/depth into the native targets, runs the motion
    /// vector pass, then evaluates DLSS on its own native command list.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color: &wgpu::Texture,
        depth: &wgpu::Texture,
        object_mv: &wgpu::TextureView,
        unjittered_vp: &Mat4,
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

        // --- wgpu side: world colour into the committed texture the native
        // list reads, plus motion vectors and a raw R32_FLOAT copy of the
        // reversed-Z depth in one fullscreen pass.
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
        let inv_vp = unjittered_vp.inverse();
        let prev_vp = self.prev_view_proj.unwrap_or(*unjittered_vp);
        let params = FsrMvParams {
            inv_vp: inv_vp.to_cols_array_2d(),
            prev_vp: prev_vp.to_cols_array_2d(),
            render_size: [rw as f32, rh as f32],
            // See the FSR pass: on a reset frame the previous matrices
            // describe an unrelated view, so the vectors must be zero.
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
            label: Some("dlss motion vector bind group"),
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
                label: Some("dlss motion vector pass"),
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

        // --- Native DLSS step on the same queue.
        let slot_index = self.ring.acquire("DLSS")?;
        let subresource = d3d12_ty::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
        let none = d3d12_ty::D3D12_RESOURCE_BARRIER_FLAG_NONE;
        let barrier = |resource: &d3d12::Resource, before: u32, after: u32| {
            d3d12::ResourceBarrier::transition(resource.clone(), subresource, before, after, none)
        };
        // The output starts in COMMON on frame 0; afterwards the restore
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
                D3D12_STATE_NON_PIXEL_READ,
            ),
            barrier(
                &self.natives.depth.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_NON_PIXEL_READ,
            ),
            barrier(
                &self.natives.mv.resource,
                D3D12_STATE_RENDER_TARGET,
                D3D12_STATE_NON_PIXEL_READ,
            ),
            barrier(
                &self.natives.output.resource,
                output_before,
                D3D12_STATE_UNORDERED_ACCESS,
            ),
        ];
        let list = &mut self.ring.slot_mut(slot_index).list;
        list.resource_barrier(&barriers);

        // The evaluate parameters, and the four resource pointers, whose `Set`
        // overload is measured with the feature itself.
        //
        // `nvsdk_ngx_params.h` declares eight `Set` overloads — five numeric, then
        // `ID3D11Resource *`, `ID3D12Resource *`, `void *` — and eight `Get`
        // overloads in the mirror order, and this viewer addressed the resource
        // ones by that declaration. A real session shows the integer half of the
        // order holding and the pointer half not: NGX's own feature log answers
        // every evaluate with `NGXDLAA::EvaluateFeature:788 error: could not find
        // Color parameter`, which is what a pointer stored under the wrong *type
        // tag* looks like from the feature's side.
        //
        // So the slot is found by asking the feature. On the first frame the four
        // resources are written through each candidate slot in turn and evaluated,
        // each attempt on a **fresh map**: a map can hold one value per key, and if
        // it keeps the type a key was first stored with, retrying the same key on
        // the same map with a different slot would test nothing (B-27). A rejected
        // evaluate validates before it records anything, so the command list is
        // still clean between attempts, and the winning slot and map are kept for
        // the session.
        let resources = [
            self.natives.color.resource.as_mut_ptr() as *mut c_void,
            self.natives.depth.resource.as_mut_ptr() as *mut c_void,
            self.natives.mv.resource.as_mut_ptr() as *mut c_void,
            self.natives.output.resource.as_mut_ptr() as *mut c_void,
        ];
        let evaluate = EvaluateFrame {
            render: self.render_extent,
            display: self.display_extent,
            jitter: self.current_jitter,
            reset: self.reset_history,
            frame_time_ms: frame_time_ms.clamp(0.1, 1000.0),
        };
        // A *real* resource pointer through every declared `Set` overload, read
        // back through every declared reader.
        //
        // The null probe cannot answer this: a null stored as a number and a
        // null stored as a resource both read back as zero, which is why the
        // map reported "a null pointer round-trips Set slot 6 / Get slot 14" in
        // a session where the feature could not find `Color` at all. A real
        // handle distinguishes the overloads — it survives only through the
        // resource one — and the log prints the address that went in, so the
        // address that comes back can be compared with it (`docs/blockers.md`
        // B-27).
        if self.frame_index == 0 {
            // The numeric half first: it identifies where this core's readers
            // sit, which is what the pointer half is then read against.
            crate::dlss::diag("[DLSS] numeric slot matrix: 0x123456789abcdef0 through every Set");
            for line in self.params.numeric_slot_matrix() {
                crate::dlss::diag(&format!("[DLSS]   {line}"));
            }
            let (found, lines) = self.params.resource_slot_matrix(P_COLOR, resources[0]);
            crate::dlss::diag(&format!(
                "[DLSS] resource slot matrix: Color = 0x{:x}, {} declared Set overloads against 4 \
                 readers",
                resources[0] as usize,
                lines.len()
            ));
            for line in &lines {
                crate::dlss::diag(&format!("[DLSS]   {line}"));
            }
            match found {
                Some(slot) => {
                    crate::dlss::diag(&format!(
                        "[DLSS] the ID3D12Resource reader keeps the pointer written through Set \
                         slot {slot}; the ladder starts there"
                    ));
                    self.param_slot = slot;
                }
                None => crate::dlss::diag(
                    "[DLSS] no declared Set overload leaves a resource pointer this map reads back \
                     unchanged; the ladder runs its candidates anyway",
                ),
            }
        }
        let ladder: Vec<usize> = if self.frame_index == 0 {
            crate::dlss::params::resource_slot_ladder(self.param_slot)
        } else {
            vec![self.param_slot]
        };
        let mut result = crate::dlss::params::NGX_FAIL;
        let mut winning_slot = self.param_slot;
        let mut replacement: Option<Box<crate::dlss::params::NgxParams>> = None;
        for (attempt, slot) in ladder.into_iter().enumerate() {
            if attempt > 0 {
                // A fresh map per attempt, so a key already stored as one type is
                // not silently kept that way.
                replacement = Some(self.api.new_parameters(false));
            }
            let params = match attempt {
                0 => &mut self.params,
                _ => replacement
                    .as_mut()
                    .expect("a map for every attempt after the first"),
            };
            write_evaluate_parameters(params, slot, &evaluate, resources);
            result = unsafe {
                (self.api.evaluate_feature)(
                    list.as_mut_ptr() as *mut c_void,
                    self.handle,
                    params.as_ptr(),
                    std::ptr::null(),
                )
            };
            if crate::dlss::succeeded(result) {
                if attempt > 0 {
                    crate::dlss::diag(&format!(
                        "[DLSS] resource parameters reach the feature through Set slot {slot} \
                         on a fresh map; slot {} was rejected",
                        self.param_slot
                    ));
                }
                winning_slot = slot;
                break;
            }
            // NGX names nothing in the HRESULT but names the key in its own log,
            // and the key is the experiment's result: the four were written
            // through one slot, so a complaint about one of them means that slot
            // is not the overload the feature reads through.
            let complaint = crate::dlss::loader::last_missing_parameter()
                .map(|line| format!("; its log says: {line}"))
                .unwrap_or_default();
            let described = params.describe_pointer_gets(P_COLOR);
            crate::dlss::diag(&format!(
                "[DLSS] evaluate rejected Set slot {slot} ({}){complaint}; Color reads back as \
                 {described}",
                crate::dlss::result_name(result)
            ));
        }
        if crate::dlss::succeeded(result) {
            if let Some(map) = replacement.take() {
                self.params = map;
            }
            self.param_slot = winning_slot;
            self.params.set_resource_set_slot(winning_slot);
        }

        let restore = [
            barrier(
                &self.natives.color.resource,
                D3D12_STATE_NON_PIXEL_READ,
                D3D12_STATE_COPY_DEST,
            ),
            barrier(
                &self.natives.depth.resource,
                D3D12_STATE_NON_PIXEL_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(
                &self.natives.mv.resource,
                D3D12_STATE_NON_PIXEL_READ,
                D3D12_STATE_RENDER_TARGET,
            ),
            barrier(
                &self.natives.output.resource,
                D3D12_STATE_UNORDERED_ACCESS,
                D3D12_STATE_PIXEL_COMPUTE_READ,
            ),
        ];
        list.resource_barrier(&restore);
        self.ring.retire(slot_index, &self.queue, "DLSS")?;

        if !crate::dlss::succeeded(result) {
            // `FAIL_InvalidParameter` names no parameter: the line that does is
            // in NGX's own log file, which is written whether or not the
            // logging callback is honoured.
            if !DUMPED_NGX_LOGS.swap(true, Ordering::Relaxed) {
                super::loader::dump_ngx_log_files();
            }
            return Err(format!(
                "NVSDK_NGX_D3D12_EvaluateFeature(DLSS) failed ({})",
                crate::dlss::result_name(result)
            ));
        }

        if self.frame_index == 0 {
            // One line per session that proves the feature really evaluated on
            // the buffers it was given (the extents are what a screen report
            // needs to be actionable).
            crate::dlss::diag(&format!(
                "[DLSS] first evaluate OK ({}x{} -> {}x{}, {})",
                self.render_extent.0,
                self.render_extent.1,
                self.display_extent.0,
                self.display_extent.1,
                self.quality_name
            ));
        }
        self.reset_history = false;
        self.prev_view_proj = Some(*unjittered_vp);
        self.frame_index += 1;
        Ok(())
    }

    /// Blit the DLSS result into the composition, sharpening it.
    ///
    /// `sharpness` is the Graphics tab's DLSS slider (0 = off, 1 = maximum). It
    /// runs the same clamped unsharp pass as the XeSS path: NGX only accepts
    /// `Sharpness` in the create parameter map, and the feature flag that used
    /// to enable its own sharpening is deprecated ("Sharpness is not
    /// supported", `nvsdk_ngx_defs.h`), so the viewer does it.
    pub fn composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        sharpness: f32,
    ) {
        self.composite.draw(
            device,
            queue,
            &self.natives.output.texture,
            target,
            sharpness,
        );
    }
}
