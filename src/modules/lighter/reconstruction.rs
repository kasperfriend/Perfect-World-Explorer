//! Opt-in same-device Vulkan reconstruction. Native upscalers and frame
//! generation remain paused whenever the ray renderer owns the scene.
mod bootstrap;
mod ffx;
mod ngx;

use super::{
    checked_pipeline, image_limit,
    temporal::{self, Inputs, Plane, Prepared},
    Mode, Settings, PIXEL_BYTES,
};
use ash::vk;
use std::{ffi::CString, path::PathBuf, sync::Arc};
use wgpu::hal::{api::Vulkan, vulkan::InstanceShared};

type R<T> = Result<T, String>;

// Both optional runtimes use the audited release bytes. Do not load from
// PATH, a feature DLL in place of the bridge, or an unvalidated SDK path.
fn verified_binary(path: &std::path::Path, size: u64, sha256: &str) -> R<PathBuf> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let path = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != size {
        return Err(format!(
            "{}: unexpected SDK binary length (expected {size})",
            path.display()
        ));
    }
    let mut hash = Sha256::new();
    let mut chunk = [0; 32 * 1024];
    loop {
        let n = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&chunk[..n]);
    }
    if format!("{:x}", hash.finalize()) != sha256 {
        return Err(format!(
            "{}: SDK binary does not match the pinned SHA-256",
            path.display()
        ));
    }
    Ok(path)
}

fn check_adapter(props: &vk::PhysicalDeviceProperties, rr: bool) -> R<()> {
    if props.device_type == vk::PhysicalDeviceType::CPU {
        Err("vendor ray reconstruction requires a physical Vulkan GPU".into())
    } else if rr && props.vendor_id != 0x10de {
        Err("DLSS RR requires a supported NVIDIA adapter".into())
    } else {
        Ok(())
    }
}

pub(super) enum Request {
    Fsr(Arc<ffx::Api>),
    Rr(Arc<ngx::Api>),
    #[cfg(test)]
    Probe {
        rr: bool,
    },
}

/// One stage per process so two captures differ by exactly one omission.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Bisect {
    #[default]
    None,
    Bootstrap,
    RawDevice,
    FfxLoader,
    NgxLoader,
    Interop,
}

impl Bisect {
    fn parse(value: &str) -> R<Self> {
        match value {
            "" | "off" => Ok(Self::None),
            "bootstrap" => Ok(Self::Bootstrap),
            "raw-device" => Ok(Self::RawDevice),
            "ffx-loader" => Ok(Self::FfxLoader),
            "ngx-loader" => Ok(Self::NgxLoader),
            "interop" => Ok(Self::Interop),
            _ => Err("PW_RAY_RECONSTRUCTION_SKIP must name a single diagnostic stage".into()),
        }
    }

    fn from_env() -> R<Self> {
        match std::env::var("PW_RAY_RECONSTRUCTION_SKIP") {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::None),
            Err(_) => Err("invalid PW_RAY_RECONSTRUCTION_SKIP".into()),
        }
    }

    fn before_load(self, provider: &str) -> R<()> {
        if self == Self::Bootstrap
            || matches!(
                (self, provider),
                (Self::FfxLoader, "fsr") | (Self::NgxLoader, "rr")
            )
        {
            Err(format!(
                "diagnostic skipped {self:?} before loading {provider}"
            ))
        } else {
            Ok(())
        }
    }
}

impl Request {
    fn load(skip: Bisect) -> R<Option<Self>> {
        let path = |var: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .ok_or_else(|| format!("set {var} to enable the requested Vulkan SDK"))
        };
        Self::load_with(
            skip,
            std::env::var("PW_RAY_RECONSTRUCTION").as_deref(),
            path,
        )
    }

    fn load_with(
        skip: Bisect,
        provider: Result<&str, &std::env::VarError>,
        path: impl Fn(&str) -> R<PathBuf>,
    ) -> R<Option<Self>> {
        let provider = match provider {
            Ok("" | "off") | Err(std::env::VarError::NotPresent) => return Ok(None),
            Ok(value @ ("fsr" | "rr")) => value,
            _ => return Err("PW_RAY_RECONSTRUCTION must be off, fsr or rr".into()),
        };
        skip.before_load(provider)?;
        if provider == "fsr" {
            Ok(Some(Self::Fsr(Arc::new(ffx::Api::load(&path(
                "PW_RAY_FSR_VK_DLL",
            )?)?))))
        } else {
            Ok(Some(Self::Rr(Arc::new(ngx::Api::load(
                &path("PW_RAY_RR_BRIDGE")?,
                &path("PW_RAY_RR_SDK_DIR")?,
            )?))))
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Fsr(_) => "Vulkan temporal FSR",
            Self::Rr(_) => "Vulkan DLSS Ray Reconstruction",
            #[cfg(test)]
            Self::Probe { .. } => "Vulkan reconstruction interop fixture",
        }
    }
    fn is_fsr(&self) -> bool {
        matches!(self, Self::Fsr(_))
    }
    fn is_rr(&self) -> bool {
        match self {
            Self::Rr(_) => true,
            #[cfg(test)]
            Self::Probe { rr } => *rr,
            _ => false,
        }
    }
    fn is_probe(&self) -> bool {
        match self {
            #[cfg(test)]
            Self::Probe { .. } => true,
            _ => false,
        }
    }

    fn device_extensions(
        &self,
        instance: &InstanceShared,
        physical: vk::PhysicalDevice,
        props: &vk::PhysicalDeviceProperties,
    ) -> R<Vec<CString>> {
        if !self.is_probe() {
            check_adapter(props, self.is_rr())?;
        }
        match self {
            Self::Rr(api) => {
                let enabled = instance.extensions();
                let required = api.extensions(vk::Instance::null(), vk::PhysicalDevice::null())?;
                if required
                    .iter()
                    .any(|ext| !enabled.contains(&ext.as_c_str()))
                {
                    return Err("NGX instance requirements were not enabled at creation".into());
                }
                api.extensions(instance.raw_instance().handle(), physical)
            }
            _ => Ok(Vec::new()),
        }
    }
}

#[derive(Default)]
struct Fallback(Option<PathBuf>);

impl Fallback {
    fn new(skip: Bisect) -> Self {
        let requested = skip == Bisect::None
            && std::env::var("PW_RAY_RECONSTRUCTION").as_deref() == Ok("rr")
            && std::env::var("PW_RAY_RECONSTRUCTION_FALLBACK").as_deref() == Ok("rr-fsr-portable");
        if !requested {
            return Self::default();
        }
        let path = std::env::var_os("PW_RAY_FSR_VK_DLL").map(PathBuf::from);
        if path.is_none() {
            eprintln!("Lighter fallback FSR unavailable: set PW_RAY_FSR_VK_DLL");
        }
        Self(path)
    }

    fn try_fsr(&mut self, reason: &str) -> Option<Request> {
        let path = self.0.take()?;
        eprintln!("Lighter reconstruction fallback: RR rejected ({reason}); trying FSR");
        match ffx::Api::load(&path) {
            Ok(api) => Some(Request::Fsr(Arc::new(api))),
            Err(error) => {
                eprintln!("Lighter reconstruction fallback: FSR unavailable; portable: {error}");
                None
            }
        }
    }
}

/// Owned by startup, not global; a failed optional path leaves ordinary wgpu
/// device selection and reload/recovery available.
#[derive(Default)]
pub(crate) struct Startup(Option<Request>, Bisect, Fallback);

impl Startup {
    pub(crate) fn new(mode: Mode) -> Self {
        if mode == Mode::Off {
            if matches!(
                std::env::var("PW_RAY_RECONSTRUCTION").as_deref(),
                Ok("fsr" | "rr")
            ) {
                eprintln!("Native ignores PW_RAY_RECONSTRUCTION; no Vulkan ray SDK initialized");
            }
            return Self::default();
        }
        let skip = match Bisect::from_env() {
            Ok(skip) => skip,
            Err(error) => {
                eprintln!("Lighter reconstruction unavailable; using portable resolve: {error}");
                return Self::default();
            }
        };
        let mut fallback = Fallback::new(skip);
        let request = match Request::load(skip) {
            Ok(request) => request,
            Err(error) => fallback.try_fsr(&error).or_else(|| {
                eprintln!("Lighter reconstruction unavailable; using portable resolve: {error}");
                None
            }),
        };
        Self(request, skip, fallback)
    }

    fn optional_import_failed(&mut self, reason: &str) {
        if self.0.as_ref().is_some_and(Request::is_rr) {
            if let Some(next) = self.2.try_fsr(reason) {
                self.0 = Some(next);
                return;
            }
        }
        self.disable(reason);
    }

    pub(crate) fn instance(&mut self, backends: wgpu::Backends) -> wgpu::Instance {
        let bits = if self.0.is_some() && std::env::var_os("WGPU_BACKEND").is_none() {
            wgpu::Backends::VULKAN
        } else {
            backends
        };
        let mut desc = wgpu::InstanceDescriptor {
            backends: bits,
            ..Default::default()
        };
        if self.0.is_some() {
            desc.flags = desc.flags.with_env();
        }
        if self.1 == Bisect::RawDevice {
            self.disable("diagnostic skipped raw-device (instance and device import)");
        }
        if let Some(Request::Rr(api)) = &self.0 {
            if bits.contains(wgpu::Backends::VULKAN) {
                let optional = api
                    .extensions(vk::Instance::null(), vk::PhysicalDevice::null())
                    .and_then(|extensions| bootstrap::instance(&extensions, desc.flags));
                match optional {
                    Ok(instance) => return instance,
                    Err(error) => self.optional_import_failed(&error),
                }
            }
        }
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: if self.0.is_some() { bits } else { backends },
            ..desc
        })
    }

    pub(crate) fn disable(&mut self, reason: &str) {
        self.2 = Fallback::default();
        if let Some(req) = self.0.take() {
            eprintln!(
                "Lighter: {} unavailable; using portable resolve: {reason}",
                req.label()
            );
        }
    }

    pub(crate) fn device(
        &mut self,
        adapter: &wgpu::Adapter,
        desc: &wgpu::DeviceDescriptor<'_>,
    ) -> Option<(wgpu::Device, wgpu::Queue)> {
        if self.1 == Bisect::RawDevice {
            self.disable("diagnostic skipped raw-device (device import)");
        }
        let request = self.0.as_ref()?;
        match bootstrap::device(adapter, desc, request) {
            Ok(device) => Some(device),
            Err(error) => {
                // FSR uses the base Vulkan extensions; the normal wgpu
                // device returned by the caller is sufficient after an RR
                // raw import failure. Never retry a lost Vulkan device.
                if crate::gpu::reconstruction_device_lost(&error) {
                    self.disable(&error);
                } else {
                    self.optional_import_failed(&error);
                }
                None
            }
        }
    }

    pub(crate) fn install(self, lighter: &mut super::Lighter, device: &wgpu::Device) {
        if let Some(req) = self.0 {
            match Raw::new(device) {
                Ok(raw) => lighter.reconstruction = Some(Session::new(req, raw, self.1, self.2)),
                Err(error) => eprintln!(
                    "Lighter: {} unavailable; using portable resolve: {error}",
                    req.label()
                ),
            }
        }
    }
}

struct Raw {
    instance: ash::Instance,
    device: ash::Device,
    physical: vk::PhysicalDevice,
    gipa: vk::PFN_vkGetInstanceProcAddr,
    gdpa: vk::PFN_vkGetDeviceProcAddr,
    queue: vk::Queue,
    family: u32,
}

impl Raw {
    fn new(device: &wgpu::Device) -> R<Self> {
        unsafe {
            device
                .as_hal::<Vulkan, _, _>(|hal| {
                    let hal = hal.ok_or("not a Vulkan device")?;
                    let shared = hal.shared_instance();
                    let instance = shared.raw_instance();
                    Ok(Self {
                        instance: instance.clone(),
                        device: hal.raw_device().clone(),
                        physical: hal.raw_physical_device(),
                        gipa: shared.entry().static_fn().get_instance_proc_addr,
                        gdpa: instance.fp_v1_0().get_device_proc_addr,
                        queue: hal.raw_queue(),
                        family: hal.queue_family_index(),
                    })
                })
                .ok_or("no Vulkan HAL")?
        }
    }
}

pub(super) fn extent2([rw, rh, w, h]: [u32; 4]) -> ([u32; 2], [u32; 2]) {
    ([rw, rh], [w, h])
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Image {
    image: vk::Image,
    view: vk::ImageView,
    format: vk::Format,
    size: [u32; 2],
}

#[repr(C)]
struct Images {
    color: Image,
    depth: Image,
    velocity: Image,
    output: Image,
    guides: [Image; 4],
}
impl Images {
    fn reads(&self) -> impl Iterator<Item = Image> + '_ {
        [self.color, self.depth, self.velocity].into_iter().chain(
            self.guides
                .iter()
                .copied()
                .filter(|g| g.image != vk::Image::null()),
        )
    }
}
const COLOR_RANGE: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

struct Views {
    device: ash::Device,
    owned: Vec<vk::ImageView>,
}
impl Views {
    fn image(&mut self, plane: &Plane, make_view: bool) -> R<Image> {
        let mut image = vk::Image::null();
        unsafe {
            plane.texture.as_hal::<Vulkan, _>(|t| {
                if let Some(t) = t {
                    image = t.raw_handle();
                }
            })
        };
        if image == vk::Image::null() {
            return Err("reconstruction plane is not Vulkan".into());
        }
        let format = match plane.texture.format() {
            wgpu::TextureFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
            wgpu::TextureFormat::Rg32Float => vk::Format::R32G32_SFLOAT,
            wgpu::TextureFormat::R32Float => vk::Format::R32_SFLOAT,
            _ => return Err("unexpected Vulkan reconstruction format".into()),
        };
        let size = [plane.texture.width(), plane.texture.height()];
        let view = if make_view {
            let view = unsafe {
                self.device.create_image_view(
                    &vk::ImageViewCreateInfo::builder()
                        .image(image)
                        .format(format)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .subresource_range(COLOR_RANGE),
                    None,
                )
            }
            .map_err(vkerr("create SDK image view"))?;
            self.owned.push(view);
            view
        } else {
            vk::ImageView::null()
        };
        Ok(Image {
            image,
            view,
            format,
            size,
        })
    }
}
impl Drop for Views {
    fn drop(&mut self) {
        for view in self.owned.drain(..) {
            unsafe { self.device.destroy_image_view(view, None) };
        }
    }
}

fn vkerr(step: &str) -> impl FnOnce(vk::Result) -> String + '_ {
    move |error| format!("Lighter Vulkan reconstruction {step}: {error:?}")
}

/// Reused command list; one-in-flight initially. Live redraw polls rather
/// than idling Vulkan or waiting for a fence. Drop waits only at teardown.
struct Commands {
    device: ash::Device,
    queue: vk::Queue,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    pending: bool,
    timer: Option<(vk::QueryPool, u32, f32)>,
    #[cfg(test)]
    fail_submit: bool,
}
impl Commands {
    fn new(raw: &Raw, measure: bool) -> R<Self> {
        let device = raw.device.clone();
        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::builder().queue_family_index(raw.family),
                None,
            )
        }
        .map_err(vkerr("command pool"))?;
        let mut owned = Self {
            device,
            queue: raw.queue,
            pool,
            cmd: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            pending: false,
            timer: None,
            #[cfg(test)]
            fail_submit: false,
        };
        owned.cmd = unsafe {
            owned.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(vkerr("allocate"))?[0];
        owned.fence = unsafe {
            owned
                .device
                .create_fence(&vk::FenceCreateInfo::default(), None)
        }
        .map_err(vkerr("fence"))?;
        if measure {
            let queues = unsafe {
                raw.instance
                    .get_physical_device_queue_family_properties(raw.physical)
            };
            let props = unsafe { raw.instance.get_physical_device_properties(raw.physical) };
            let bits = queues[raw.family as usize].timestamp_valid_bits;
            if bits != 0 {
                // Timestamp availability is not required for SDK admission.
                owned.timer = unsafe {
                    owned.device.create_query_pool(
                        &vk::QueryPoolCreateInfo::builder()
                            .query_type(vk::QueryType::TIMESTAMP)
                            .query_count(2),
                        None,
                    )
                }
                .ok()
                .map(|pool| (pool, bits, props.limits.timestamp_period));
            }
        }
        Ok(owned)
    }
    fn idle(&mut self) -> R<bool> {
        if self.pending {
            self.pending =
                !unsafe { self.device.get_fence_status(self.fence) }.map_err(vkerr("poll"))?;
        }
        Ok(!self.pending)
    }
    fn begin(&self) -> R<()> {
        assert!(!self.pending);
        unsafe {
            self.device
                .reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
                .map_err(vkerr("reset"))?;
            self.device
                .begin_command_buffer(
                    self.cmd,
                    &vk::CommandBufferBeginInfo::builder()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(vkerr("begin"))?;
            if let Some((pool, _, _)) = self.timer {
                self.device.cmd_reset_query_pool(self.cmd, pool, 0, 2);
                self.device.cmd_write_timestamp(
                    self.cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    pool,
                    0,
                );
            }
        }
        Ok(())
    }
    fn elapsed_ms(&self) -> Option<f64> {
        let (pool, bits, period) = self.timer?;
        if self.pending {
            return None;
        }
        let mut stamps = [0u64; 2];
        unsafe {
            self.device
                .get_query_pool_results(pool, 0, 2, &mut stamps, vk::QueryResultFlags::TYPE_64)
                .ok()?;
        }
        Some(super::vulkan::timestamp_ms(
            stamps[0], stamps[1], bits, period,
        ))
    }
    fn submit(&mut self) -> R<()> {
        unsafe {
            if let Some((pool, _, _)) = self.timer {
                self.device.cmd_write_timestamp(
                    self.cmd,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    pool,
                    1,
                );
            }
            self.device
                .end_command_buffer(self.cmd)
                .map_err(vkerr("end"))?;
            #[cfg(test)]
            if std::mem::take(&mut self.fail_submit) {
                return Err(vkerr("submit (injected)")(
                    vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
                ));
            }
            self.device
                .reset_fences(&[self.fence])
                .map_err(vkerr("reset fence"))?;
            self.device
                .queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::builder()
                        .command_buffers(std::slice::from_ref(&self.cmd))
                        .build()],
                    self.fence,
                )
                .map_err(vkerr("submit"))?;
        }
        self.pending = true;
        Ok(())
    }
}
impl Drop for Commands {
    fn drop(&mut self) {
        unsafe {
            if self.pending {
                let _ = self.device.wait_for_fences(&[self.fence], true, u64::MAX);
            }
            if let Some((pool, _, _)) = self.timer {
                self.device.destroy_query_pool(pool, None);
            }
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
        }
    }
}

enum Context {
    Fsr(ffx::Context),
    Rr(ngx::Context),
    #[cfg(test)]
    Probe,
}

struct Epoch {
    // Command retirement precedes the vendor context, then the raw views,
    // then the wgpu allocations referenced by those native commands.
    commands: Commands,
    context: Context,
    _views: Views,
    input: Inputs,
    output: Plane,
    fsr_masks: Option<[Image; 2]>,
    images: Images,
    extent: [u32; 4],
    dispatched: bool,
    #[cfg(test)]
    last_reset: bool,
    #[cfg(test)]
    last_jitter: [f32; 2],
}

impl Epoch {
    fn new(
        device: &wgpu::Device,
        raw: &Raw,
        req: &Request,
        extent: [u32; 4],
        skip: Bisect,
    ) -> R<Self> {
        let (render, display) = extent2(extent);
        let rr = req.is_rr();
        let context = match req {
            Request::Fsr(api) => Context::Fsr(ffx::Context::new(api, raw, extent)?),
            Request::Rr(api) => Context::Rr(ngx::Context::new(api, raw, extent)?),
            #[cfg(test)]
            Request::Probe { .. } => Context::Probe,
        };
        if skip == Bisect::Interop {
            // Drop the just-opened provider before any image, sampled view, bind
            // group or command allocation. Never pass a dummy Vulkan handle.
            return Err("diagnostic skipped SDK image/view and descriptor setup".into());
        }
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let made = checked_pipeline(device, || {
            (
                Inputs::new(device, render, rr),
                Plane::new(
                    device,
                    "Vendor ray reconstruction output",
                    display,
                    wgpu::TextureFormat::Rgba16Float,
                    wgpu::TextureUsages::RENDER_ATTACHMENT
                        | if cfg!(test) {
                            wgpu::TextureUsages::COPY_SRC
                        } else {
                            wgpu::TextureUsages::empty()
                        }
                        | if req.is_probe() {
                            wgpu::TextureUsages::COPY_DST
                        } else {
                            wgpu::TextureUsages::empty()
                        },
                ),
            )
        });
        let oom = pollster::block_on(device.pop_error_scope());
        if let Some(oom) = oom {
            return Err(oom.to_string());
        }
        let (input, output) = made?;
        let mut views = Views {
            device: raw.device.clone(),
            owned: Vec::new(),
        };
        let out = views.image(&output, rr)?;
        let color = views.image(&input.color, rr)?;
        let velocity = views.image(&input.velocity, rr)?;
        let mut guides = [Image::default(); 4];
        if let Some(planes) = &input.guides {
            for (g, plane) in guides.iter_mut().zip(planes) {
                *g = views.image(plane, rr)?;
            }
        }
        let fsr_masks = input
            .fsr_masks
            .as_ref()
            .map(|p| -> R<_> { Ok([views.image(&p[0], false)?, views.image(&p[1], false)?]) })
            .transpose()?;
        let images = Images {
            color,
            velocity,
            output: out,
            guides,
            depth: views.image(&input.depth, rr)?,
        };
        Ok(Self {
            commands: Commands::new(
                raw,
                std::env::var("PW_LIGHTER_TRAVEL_SYNC").as_deref() == Ok("1"),
            )?,
            context,
            _views: views,
            input,
            output,
            fsr_masks,
            images,
            extent,
            dispatched: false,
            #[cfg(test)]
            last_reset: false,
            #[cfg(test)]
            last_jitter: [0.; 2],
        })
    }

    fn output_init(&self, encoder: &mut wgpu::CommandEncoder) {
        if !self.dispatched {
            // Make wgpu's resource tracker initialize the output BEFORE raw
            // Vulkan writes it. Otherwise the first sampled use would clear it.
            let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Initialize reconstruction output"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
    }

    fn dispatch(&mut self, frame: &Prepared) -> R<()> {
        use vk::{AccessFlags as A, ImageLayout as L};
        let images = &self.images;
        let output_layout = if self.dispatched {
            L::SHADER_READ_ONLY_OPTIMAL
        } else {
            L::COLOR_ATTACHMENT_OPTIMAL
        };
        let ms = frame.camera.frame_ms.clamp(0.1, 1000.);
        self.commands.begin()?;
        let d = &self.commands.device;
        let cmd = self.commands.cmd;
        let barrier = |image: Image, old, new, src, dst| {
            vk::ImageMemoryBarrier::builder()
                .image(image.image)
                .old_layout(old)
                .new_layout(new)
                .src_access_mask(src)
                .dst_access_mask(dst)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(COLOR_RANGE)
                .build()
        };
        let wide = A::MEMORY_READ | A::MEMORY_WRITE;
        let transition = |b: &[vk::ImageMemoryBarrier]| unsafe {
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                b,
            );
        };
        let read = L::SHADER_READ_ONLY_OPTIMAL;
        let write = L::GENERAL;
        let reads = || images.reads().chain(self.fsr_masks.into_iter().flatten());
        let mut barriers: Vec<_> = reads()
            .map(|i| barrier(i, write, read, wide, A::SHADER_READ))
            .collect();
        barriers.push(barrier(
            images.output,
            output_layout,
            write,
            wide,
            A::SHADER_READ | A::SHADER_WRITE,
        ));
        transition(&barriers);
        match &mut self.context {
            Context::Fsr(ctx) => {
                ctx.record(cmd, images, self.fsr_masks.expect("FSR masks"), frame, ms)?
            }
            Context::Rr(ctx) => ctx.record(cmd, images, frame, ms)?,
            #[cfg(test)]
            Context::Probe => unsafe {
                // The production command/submission/layout protocol, with a
                // deterministic write instead of any vendor reconstruction.
                transition(&[barrier(
                    images.output,
                    write,
                    write,
                    wide,
                    A::TRANSFER_WRITE,
                )]);
                d.cmd_clear_color_image(
                    cmd,
                    images.output.image,
                    write,
                    &vk::ClearColorValue {
                        float32: [0.25, 0.5, 0.75, 1.],
                    },
                    &[COLOR_RANGE],
                );
            },
        }
        // Restore the inputs/outputs to the application's layouts (DLSS
        // Programming Guide §3.4, and FFX's unregister transitions). This
        // submission and its wgpu consumers run on the same graphics queue.
        barriers.clear();
        barriers.extend(reads().map(|i| barrier(i, read, write, A::SHADER_READ, wide)));
        barriers.push(barrier(images.output, write, output_layout, wide, wide));
        transition(&barriers);
        self.commands.submit()?;
        self.dispatched = true;
        #[cfg(test)]
        {
            self.last_reset = frame.reset;
            self.last_jitter = frame.jitter;
        }
        Ok(())
    }
}

pub(super) struct Session {
    epoch: Option<Epoch>,
    request: Request,
    raw: Raw,
    history: temporal::History,
    rejected: Option<[u32; 4]>,
    active: bool,
    skip: Bisect,
    fallback: Fallback,
}

impl Session {
    fn new(request: Request, raw: Raw, skip: Bisect, fallback: Fallback) -> Self {
        Self {
            epoch: None,
            request,
            raw,
            history: temporal::History::default(),
            rejected: None,
            active: false,
            skip,
            fallback,
        }
    }
    pub(super) fn poll(&mut self) -> R<bool> {
        self.epoch
            .as_mut()
            .map_or(Ok(true), |epoch| epoch.commands.idle())
    }
    pub(super) fn reset(&mut self) {
        self.history.reset();
        self.active = false;
    }
    pub(super) fn sdk_ms(&self) -> Option<f64> {
        self.active.then_some(())?;
        self.epoch.as_ref()?.commands.elapsed_ms()
    }
    pub(super) fn status(&self) -> Option<&'static str> {
        self.active.then(|| self.request.label())
    }
    pub(super) fn extent_ready(&self, extent: [u32; 4]) -> bool {
        self.epoch
            .as_ref()
            .is_some_and(|epoch| epoch.extent == extent)
    }
    pub(super) fn bytes(&self, extent: [u32; 4]) -> u64 {
        u64::from(extent[0])
            * u64::from(extent[1])
            * PIXEL_BYTES
            * if self.request.is_rr() { 3 } else { 2 }
    }
    pub(super) fn prepare(
        &mut self,
        device: &wgpu::Device,
        extent: [u32; 4],
        settings: Settings,
    ) -> R<bool> {
        if !self.poll()? {
            return Ok(false);
        }
        // Never combine two histories/jitter/disocclusion owners. Keep modes
        // requiring different primary sampling as independent controls.
        if settings.mode == Mode::Off
            || settings.reference
            || settings.samples != 1
            || settings.dynamic_resolution
            || (settings.mode == Mode::Hardware && settings.hybrid_primary)
        {
            self.epoch = None;
            self.reset();
            return Ok(true);
        }
        if self.extent_ready(extent) || self.rejected == Some(extent) {
            return Ok(true);
        }
        if let Some(old) = &self.epoch {
            debug_assert!(!old.commands.pending);
            eprintln!(
                "Lighter: {} retiring {:?} after the prior submission fence before resize to {:?}",
                self.request.label(),
                old.extent,
                extent
            );
        }
        self.epoch = None;
        self.reset();
        let [rw, rh, w, h] = extent;
        let input_output_bytes =
            u64::from(rw) * u64::from(rh) * if self.request.is_rr() { 68 } else { 48 }
                + u64::from(w) * u64::from(h) * 8;
        if self.bytes(extent) > image_limit(device)
            || input_output_bytes > 192 << 20
            || rw == 0
            || rh == 0
            || w > device.limits().max_texture_dimension_2d
            || h > device.limits().max_texture_dimension_2d
            || (self.request.is_fsr() && (rw.saturating_mul(3) < w || rh.saturating_mul(3) < h))
        {
            if self.reject(
                extent,
                "unsupported render/display size or ray input budget",
            ) {
                return self.prepare(device, extent, settings);
            }
            return Ok(true);
        }
        match Epoch::new(device, &self.raw, &self.request, extent, self.skip) {
            Ok(epoch) => {
                self.epoch = Some(epoch);
                self.rejected = None;
            }
            Err(error) if crate::gpu::reconstruction_device_lost(&error) => return Err(error),
            Err(error) => {
                if self.reject(extent, &error) {
                    // No frame recorded yet: safe to prepare the new inputs
                    // here. Never recurse past the one-shot fallback slot.
                    return self.prepare(device, extent, settings);
                }
            }
        }
        Ok(true)
    }
    fn reject(&mut self, extent: [u32; 4], error: &str) -> bool {
        // Called only after observing the preceding native fence (or before
        // any new native submit). wgpu tracks its own resource references.
        self.epoch = None;
        self.rejected = Some(extent);
        self.reset();
        if self.request.is_rr() {
            if let Some(next) = self.fallback.try_fsr(error) {
                self.request = next;
                self.rejected = None;
                return true;
            }
        }
        eprintln!(
            "Lighter: {} unavailable; using portable resolve: {error}",
            self.request.label()
        );
        false
    }

    pub(super) fn begin(
        &mut self,
        frame: &mut super::Frame,
        camera: Option<temporal::Camera>,
        world: &super::World,
        moving: &super::Packet,
        mode: Mode,
        device: &wgpu::Device,
    ) -> Option<Prepared> {
        let prepared = camera
            .filter(|_| self.extent_ready(frame.extent))
            .and_then(|camera| {
                self.history.prepare(
                    frame,
                    camera,
                    temporal::Continuity {
                        revision: world.revision,
                        texture_bytes: world.textures.byte_len(),
                        mode,
                        rr: self.request.is_rr(),
                    },
                    moving,
                    super::storage_limit(device),
                )
            });
        if prepared.is_none() {
            self.reset();
        }
        prepared
    }

    pub(super) fn resolve(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        raw: &wgpu::Buffer,
        prepared: Prepared,
    ) -> R<bool> {
        let epoch = self.epoch.as_mut().expect("prepared reconstruction extent");
        epoch.input.encode(device, queue, encoder, raw, &prepared);
        epoch.output_init(encoder);
        // Queue writes, Software tracing (when selected), and the packed ray
        // inputs become visible before the same-queue SDK dispatch.
        queue.submit(Some(
            std::mem::replace(encoder, device.create_command_encoder(&Default::default())).finish(),
        ));
        match epoch.dispatch(&prepared) {
            Ok(()) => {
                epoch.input.advance();
                self.history.commit(prepared);
                if !self.active {
                    eprintln!(
                        "Lighter: {} evaluated at {:?}",
                        self.request.label(),
                        epoch.extent
                    );
                    if let Context::Fsr(ctx) = &epoch.context {
                        let (name, id) = ctx.provider;
                        eprintln!(
                            "Lighter Vulkan FSR evaluation submitted: {name} (provider {id:#x})"
                        );
                    }
                }
                self.active = true;
                Ok(true)
            }
            Err(err) if crate::gpu::reconstruction_device_lost(&err) => Err(err),
            Err(err) => {
                let extent = epoch.extent;
                if self.reject(extent, &err) {
                    eprintln!("Lighter reconstruction fallback: portable for failed RR dispatch; FSR prepares next frame");
                }
                Ok(false)
            }
        }
    }

    pub(super) fn output(&self) -> (&wgpu::TextureView, &wgpu::TextureView) {
        let epoch = self.epoch.as_ref().unwrap();
        (&epoch.output.view, &epoch.input.mask.view)
    }
}

#[cfg(test)]
#[path = "reconstruction/tests.rs"]
mod tests;
