//! Independent world renderer. The host supplies indexed geometry/materials
//! and a frame snapshot, then either Lighter or the native world renderer
//! writes the scene and reverse-Z depth target, never both.
mod bvh;
pub(crate) mod model_feed;
mod post;
pub(crate) mod scene;
mod settings;
pub(crate) mod sky;
pub(crate) mod textures;
#[cfg(any(target_os = "windows", target_os = "linux"))]
mod vulkan;

use bytemuck::{Pod, Zeroable};
use scene::Packet;
pub(crate) use settings::{Mode, Settings};
use textures::Textures;

const PIXELS: &str = include_str!("pixels.wgsl");
const BASE: &str = include_str!("trace.wgsl");
const SOFTWARE: &str = include_str!("software.wgsl");
#[cfg(any(target_os = "windows", target_os = "linux"))]
const HARDWARE: &str = include_str!("hardware.wgsl");

pub fn supports_hardware(adapter: &wgpu::Adapter) -> bool {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        vulkan::supported(adapter)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = adapter;
        false
    }
}

pub fn device_features(adapter: &wgpu::Adapter, startup: Mode) -> wgpu::Features {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    if startup != Mode::Off && supports_hardware(adapter) {
        return vulkan::FEATURES;
    }
    let _ = (adapter, startup);
    wgpu::Features::empty()
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Frame {
    pub vp: [[f32; 4]; 4],
    pub inv_vp: [[f32; 4]; 4],
    pub eye: [f32; 4],
    pub sun: [f32; 4],
    pub ambient: [f32; 4],
    pub sun_color: [f32; 4],
    pub air: [f32; 4],
    pub air_color: [f32; 4],
    pub water: [f32; 4],
    pub water_color: [f32; 4],
    pub terrain: [f32; 4],
    pub extent: [u32; 4],
    pub options: [u32; 4],
    pub waves: [u32; 4],
    pub sky0: [u32; 4],
    pub sky1: [u32; 4],
    pub sky_tint: [f32; 4],
    pub sky_uv: [f32; 4],
    pub sun_disc: [f32; 4],
    pub moon_disc: [f32; 4],
    pub sampling: [u32; 4],
}

// Matches Pixel in pixels.wgsl. Includes padding-free guide storage.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Pixel {
    radiance: [f32; 4],
    guide: [u32; 4],
}
const PIXEL_BYTES: u64 = std::mem::size_of::<Pixel>() as u64;
// Keep trace + history + spatial scratch bounded at high output resolutions.
const MAX_RAY_IMAGE_BYTES: u64 = 256 << 20;

pub const WATER_MIRROR: u32 = 8;
pub const CULL_BUILDINGS: u32 = 16;
pub const UNDERWATER: u32 = 32;
const SOFT_SUN: u32 = 64;

pub(crate) fn storage_limit(device: &wgpu::Device) -> usize {
    let limits = device.limits();
    u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size) as usize
}

pub struct World {
    pub packet: Packet,
    pub textures: Textures,
    revision: u64,
}

fn binding(
    binding: u32,
    visibility: wgpu::ShaderStages,
    ty: wgpu::BufferBindingType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn resource(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

fn checked_pipeline<T>(device: &wgpu::Device, create: impl FnOnce() -> T) -> Result<T, String> {
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipeline = create();
    if let Some(error) = pollster::block_on(device.pop_error_scope()) {
        Err(format!("Lighter pipeline unavailable: {error}"))
    } else {
        Ok(pipeline)
    }
}

struct Storage(wgpu::Buffer);
impl Storage {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        size: u64,
        extra: wgpu::BufferUsages,
    ) -> Self {
        Self(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::STORAGE | extra,
            mapped_at_creation: false,
        }))
    }
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        data: &[u8],
    ) {
        let len = data.len() as u64;
        if self.0.size() < len {
            let size = len
                .next_power_of_two()
                .min(u64::from(device.limits().max_storage_buffer_binding_size));
            *self = Self::new(device, label, size, wgpu::BufferUsages::COPY_DST);
        }
        queue.write_buffer(&self.0, 0, data);
    }
}

struct Software {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    frame: wgpu::Buffer,
    world: Storage,
    moving: Storage,
    textures: Storage,
    output: Storage,
    revision: Option<u64>,
    texture_len: usize,
    last_moving: Vec<u32>,
    #[cfg(test)]
    moving_uploads: u64,
}

impl Software {
    fn new(device: &wgpu::Device) -> Self {
        let stage = wgpu::ShaderStages::COMPUTE;
        let read = wgpu::BufferBindingType::Storage { read_only: true };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Lighter software bindings"),
            entries: &[
                binding(0, stage, wgpu::BufferBindingType::Uniform),
                binding(1, stage, read),
                binding(2, stage, read),
                binding(3, stage, read),
                binding(
                    4,
                    stage,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Lighter software pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Lighter software BVH"),
            source: wgpu::ShaderSource::Wgsl([PIXELS, BASE, SOFTWARE].concat().into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Lighter compute ray tracer"),
            layout: Some(&pl),
            module: &shader,
            entry_point: "render",
        });
        let frame = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Lighter frame"),
            size: std::mem::size_of::<Frame>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let upload = |label| Storage::new(device, label, 16, wgpu::BufferUsages::COPY_DST);
        Self {
            layout,
            pipeline,
            frame,
            world: upload("Lighter world"),
            moving: upload("Lighter moving geometry"),
            textures: upload("Lighter texels"),
            output: Storage::new(
                device,
                "Lighter image",
                PIXEL_BYTES,
                wgpu::BufferUsages::empty(),
            ),
            revision: None,
            texture_len: 0,
            last_moving: Vec::new(),
            #[cfg(test)]
            moving_uploads: 0,
        }
    }

    fn trace(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Frame,
        bytes: u64,
        world: &World,
        moving: &Packet,
    ) -> &wgpu::Buffer {
        if self.output.0.size() != bytes {
            self.output = Storage::new(device, "Lighter image", bytes, wgpu::BufferUsages::empty());
        }
        let new_world = self.revision != Some(world.revision);
        if new_world {
            self.world
                .upload(device, queue, "Lighter world", world.packet.bytes());
            self.revision = Some(world.revision);
        }
        let texels = world.textures.bytes();
        if new_world || self.texture_len != texels.len() {
            self.textures
                .upload(device, queue, "Lighter texels", texels);
            self.texture_len = texels.len();
        }
        if self.last_moving != moving.words {
            self.moving
                .upload(device, queue, "Lighter moving geometry", moving.bytes());
            self.last_moving.clone_from(&moving.words);
            #[cfg(test)]
            {
                self.moving_uploads += 1;
            }
        }
        queue.write_buffer(&self.frame, 0, bytemuck::bytes_of(frame));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Lighter ray data"),
            layout: &self.layout,
            entries: &[
                resource(0, &self.frame),
                resource(1, &self.world.0),
                resource(2, &self.moving.0),
                resource(3, &self.textures.0),
                resource(4, &self.output.0),
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Lighter primary and secondary rays (software)"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(frame.extent[0].div_ceil(8), frame.extent[1].div_ceil(8), 1);
        &self.output.0
    }
}

struct Present {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    extent: wgpu::Buffer,
}
impl Present {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Lighter composition bindings"),
            entries: &[
                binding(
                    0,
                    fragment,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                binding(1, fragment, wgpu::BufferBindingType::Uniform),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Lighter composition layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Lighter scene and depth blit"),
            source: wgpu::ShaderSource::Wgsl(
                [PIXELS, include_str!("present.wgsl")].concat().into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Lighter scene and depth output"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vertex",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fragment",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            multisample: Default::default(),
            multiview: None,
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
        });
        let extent = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Lighter output parameters"),
            size: 32,
            mapped_at_creation: false,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Self {
            layout,
            pipeline,
            extent,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        image: &wgpu::Buffer,
        extent: [u32; 4],
        guided: bool,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
    ) {
        queue.write_buffer(
            &self.extent,
            0,
            bytemuck::bytes_of(&[extent, [u32::from(guided), 0, 0, 0]]),
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Lighter composition"),
            layout: &self.layout,
            entries: &[resource(0, image), resource(1, &self.extent)],
        });
        let mut pass = output_pass(encoder, target, depth);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn output_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    target: &'a wgpu::TextureView,
    depth: &'a wgpu::TextureView,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Lighter output (no raster world)"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(0.),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}

pub struct Lighter {
    // Must be dropped before the wgpu Device (owns native Vulkan objects).
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    hardware: Option<vulkan::Renderer>,
    software: Option<Software>,
    present: Option<Present>,
    post: Option<post::Post>,
    history: post::History,
    pub settings: Settings,
    armed: bool,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    capture: bool,
    adapter_supports_compute: bool,
    has_hardware_device: bool,
    native_backend: wgpu::Backend,
    pub world: Option<World>,
    revision: u64,
    scene_error: Option<String>,
    frame_error: Option<String>,
    persistence_error: Option<String>,
    status: Option<String>,
    pub(crate) scenery_coverage: Option<String>,
}

impl Default for Lighter {
    fn default() -> Self {
        Self {
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            hardware: None,
            software: None,
            present: None,
            post: None,
            history: post::History::default(),
            settings: Settings::default(),
            armed: false,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            capture: false,
            adapter_supports_compute: false,
            has_hardware_device: false,
            native_backend: wgpu::Backend::Empty,
            world: None,
            revision: 0,
            scene_error: None,
            frame_error: None,
            persistence_error: None,
            status: None,
            scenery_coverage: None,
        }
    }
}

impl Lighter {
    pub fn new(settings: Settings, adapter: &wgpu::Adapter, device: &wgpu::Device) -> Self {
        let rt = device_features(adapter, settings.mode);
        Self {
            armed: settings.mode != Mode::Off,
            settings,
            native_backend: adapter.get_info().backend,
            adapter_supports_compute: adapter.get_downlevel_capabilities().flags.contains(
                wgpu::DownlevelFlags::COMPUTE_SHADERS | wgpu::DownlevelFlags::FRAGMENT_STORAGE,
            ),
            has_hardware_device: !rt.is_empty() && device.features().contains(rt),
            ..Self::default()
        }
    }

    /// Explicit one-shot preview or test. Windowed Lighter always probes and
    /// returns to the event loop instead of waiting for native commands.
    pub(crate) fn for_capture(self) -> Self {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        return Self {
            capture: true,
            ..self
        };
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        self
    }

    pub fn armed(&self) -> bool {
        self.armed
    }
    pub fn replaces_world(&self) -> bool {
        self.armed && self.settings.mode != Mode::Off
    }
    pub fn requested(&self) -> bool {
        self.settings.mode != Mode::Off
    }

    pub fn install(&mut self, scene: Result<(Packet, Textures), String>) {
        self.revision = self.revision.wrapping_add(1);
        self.frame_error = None;
        match scene {
            Ok((packet, textures)) => {
                self.world = Some(World {
                    packet,
                    textures,
                    revision: self.revision,
                });
                self.scene_error = None;
            }
            Err(error) => {
                self.world = None;
                self.scene_error = Some(error);
            }
        }
    }

    fn unavailable(&self) -> Option<&str> {
        if !self.adapter_supports_compute {
            Some("This adapter cannot run Lighter's compute and storage-buffer passes.")
        } else if self.settings.mode == Mode::Hardware && !self.has_hardware_device {
            if self.native_backend != wgpu::Backend::Vulkan {
                Some("Hardware RT needs Vulkan 1.2 with ray queries. Restart after selecting Hardware; remove any conflicting WGPU_BACKEND override.")
            } else {
                Some("No hardware ray-query support on the selected Vulkan adapter. Choose Software or select an RT-capable GPU.")
            }
        } else {
            None
        }
    }

    pub fn error(&self) -> Option<&str> {
        if !self.replaces_world() {
            return None;
        }
        self.unavailable()
            .or(self.scene_error.as_deref())
            .or(self.frame_error.as_deref())
    }

    pub fn clear(
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
    ) {
        drop(output_pass(encoder, target, depth));
    }

    pub(crate) fn reset_history(&mut self) {
        self.history.reset();
    }

    pub fn fail_frame(&mut self, error: String) {
        self.reset_history();
        if self.frame_error.as_deref() != Some(&error) {
            eprintln!("Lighter: {error}");
        }
        self.frame_error = Some(error);
        self.status = None;
    }

    fn ray_extent(&self, device: &wgpu::Device, w: u32, h: u32) -> Result<(u32, u32), String> {
        let storage = storage_limit(device) as u64;
        let image_count =
            1 + u64::from(self.settings.denoiser) + u64::from(self.settings.progressive);
        let pixel_limit = storage.min(MAX_RAY_IMAGE_BYTES / image_count) / PIXEL_BYTES;
        if pixel_limit == 0 {
            return Err("Lighter cannot allocate ray image".into());
        }
        let scaled = |n: u32| (n * self.settings.resolution / 100).max(1);
        let mut rw = scaled(w);
        let mut rh = scaled(h);
        while u64::from(rw) * u64::from(rh) > pixel_limit {
            let fit = (pixel_limit as f64 / (f64::from(rw) * f64::from(rh))).sqrt();
            rw = ((f64::from(rw) * fit) as u32).max(1);
            rh = ((f64::from(rh) * fit) as u32).max(1);
        }
        Ok((rw, rh))
    }

    /// Check before acquiring a swapchain image. False means retry on a later
    /// redraw without evolving history, submitting stale pixels or reporting
    /// backpressure as an RT failure.
    pub(crate) fn prepare_frame(
        &mut self,
        device: &wgpu::Device,
        w: u32,
        h: u32,
    ) -> Result<bool, String> {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if self.settings.mode == Mode::Hardware && self.unavailable().is_none() {
            let (rw, rh) = self.ray_extent(device, w, h)?;
            let bytes = u64::from(rw) * u64::from(rh) * PIXEL_BYTES;
            if self.hardware.is_none() {
                self.hardware = Some(vulkan::Renderer::new(
                    device,
                    &[PIXELS, BASE, HARDWARE].concat(),
                )?);
            }
            let rt = self.hardware.as_mut().unwrap();
            if self.capture {
                crate::gpu::wait_for_capture(device, || {
                    rt.try_prepare_image(device, bytes)
                        .map(|ready| ready.then_some(true))
                })
            } else {
                rt.try_prepare_image(device, bytes)
            }
        } else {
            Ok(true)
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        {
            let _ = (device, w, h);
            Ok(true)
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        mut frame: Frame,
        moving: &Packet,
    ) -> Result<bool, String> {
        if let Some(reason) = self.unavailable() {
            return Err(reason.into());
        }
        let [_, _, w, h] = frame.extent;
        if !self.prepare_frame(device, w, h)? {
            return Ok(false);
        }
        let world = self
            .world
            .as_ref()
            .ok_or("Lighter has no resident ray scene")?;
        let storage = storage_limit(device) as u64;
        for (label, len) in [
            ("resident world", world.packet.bytes().len()),
            ("moving geometry", moving.bytes().len()),
            ("textures", world.textures.bytes().len()),
        ] {
            if len as u64 > storage {
                return Err(format!(
                    "Lighter {label} exceeds adapter storage limit ({} MiB)",
                    storage >> 20
                ));
            }
        }
        let (rw, rh) = self.ray_extent(device, w, h)?;
        frame.extent = [rw, rh, w, h];
        frame.options[0] |= u32::from(self.settings.lighting)
            | u32::from(self.settings.shadows) << 1
            | u32::from(self.settings.reflections) << 2
            | if self.settings.soft_sun { SOFT_SUN } else { 0 };
        frame.options[1] = self.settings.bounces;
        frame.options[2] = self.settings.samples;
        let bytes = u64::from(rw) * u64::from(rh) * PIXEL_BYTES;
        if self.present.is_none() {
            self.present = Some(checked_pipeline(device, || Present::new(device, format))?);
        }
        if (self.settings.denoiser || self.settings.progressive) && self.post.is_none() {
            self.post = Some(checked_pipeline(device, || post::Post::new(device))?);
        }
        let history = if self.settings.progressive {
            let moving_unchanged = match self.settings.mode {
                Mode::Software => self
                    .software
                    .as_ref()
                    .is_some_and(|s| s.last_moving == moving.words),
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                Mode::Hardware => self
                    .hardware
                    .as_ref()
                    .is_some_and(|h| h.moving_unchanged(moving)),
                _ => false,
            };
            Some(self.history.prepare(
                &mut frame,
                world.revision,
                world.textures.bytes().len(),
                self.settings.mode,
                moving_unchanged,
            ))
        } else {
            self.history.reset();
            frame.sampling = [0; 4];
            None
        };
        let image = match self.settings.mode {
            Mode::Software => {
                if self.software.is_none() {
                    self.software = Some(checked_pipeline(device, || Software::new(device))?);
                }
                self.software
                    .as_mut()
                    .unwrap()
                    .trace(device, queue, encoder, &frame, bytes, world, moving)
            }
            Mode::Hardware => {
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                {
                    self.hardware.as_mut().unwrap().trace(
                        &frame,
                        bytes,
                        &world.packet,
                        moving,
                        world.textures.bytes(),
                        world.revision,
                    )?
                }
                #[cfg(not(any(target_os = "windows", target_os = "linux")))]
                {
                    return Err("Hardware RT is not implemented on this OS".into());
                }
            }
            Mode::Off => return Err("Lighter render requested while off".into()),
        };
        let image = if let Some(post) = &mut self.post {
            post.process(
                device,
                queue,
                encoder,
                image,
                frame.extent,
                history,
                self.settings.denoiser,
            )
        } else {
            image
        };
        self.present.as_ref().unwrap().draw(
            device,
            queue,
            encoder,
            image,
            frame.extent,
            self.settings.denoiser,
            target,
            depth,
        );
        self.frame_error = None;
        let message = format!("{} active · {rw}×{rh} rays · {} unique resident / {} moving triangles · {} instances · {} unavailable texture(s)",
            self.settings.mode.label(),world.packet.triangles(),moving.triangles(),world.packet.instances.len(),world.textures.missing);
        if self.status.is_none() {
            eprintln!("Lighter: {message}");
        }
        self.status = Some(message);
        Ok(true)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let old = self.settings;
        egui::ComboBox::from_label("World renderer")
            .selected_text(self.settings.mode.label())
            .show_ui(ui, |ui| {
                for mode in Mode::ALL {
                    ui.selectable_value(&mut self.settings.mode, mode, mode.label());
                }
            });
        ui.checkbox(
            &mut self.settings.lighting,
            "Ray-traced lighting / indirect bounce",
        );
        ui.checkbox(&mut self.settings.shadows, "Ray-traced shadows");
        ui.add_enabled(
            self.settings.shadows,
            egui::Checkbox::new(&mut self.settings.soft_sun, "Soft sun shadows"),
        )
        .on_hover_text(
            "Sample across the sun's angular disc. Point lights still cast sharp shadows.",
        );
        ui.checkbox(&mut self.settings.reflections, "Ray-traced reflections");
        ui.checkbox(&mut self.settings.denoiser, "Edge-aware denoiser")
            .on_hover_text("Filter linear radiance using primary depth, normals and albedo. Never changes output depth.");
        ui.checkbox(&mut self.settings.progressive, "Progressive sampling while still")
            .on_hover_text("Accumulate new lighting samples while the camera and scene are stable. Movement or changed lighting discards incompatible history.");
        ui.add(egui::Slider::new(&mut self.settings.bounces,0..=4).text("Secondary bounces"))
            .on_hover_text("0 traces primary visibility and direct shadows only. Reflection and water transmission need at least one bounce.");
        ui.add(egui::Slider::new(&mut self.settings.samples, 1..=4).text("Samples per pixel"));
        ui.add(
            egui::Slider::new(&mut self.settings.resolution, 25..=100)
                .text("Ray resolution")
                .suffix("%"),
        );
        if self.settings != old {
            self.reset_history();
            self.persistence_error = self.settings.save().err();
            self.frame_error = None;
            self.status = None;
        }
        if let Some(error) = &self.persistence_error {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                format!("Lighter settings were not saved: {error}"),
            );
        }
        if !self.requested() {
            ui.label("Active: native renderer (Lighter off).");
        } else if !self.armed {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Restart Explorer to load ray geometry. Active now: native renderer.",
            );
        } else if let Some(error) = self.error() {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        } else if let Some(status) = &self.status {
            ui.label(status);
        } else {
            ui.label("Preparing the first ray-traced frame…");
        }
        if self.replaces_world() {
            if let Some(coverage) = &self.scenery_coverage {
                ui.label(coverage);
            }
        }
        ui.separator();
        ui.label("Lighter traces the streamed main world, including water, vegetation, buildings and animated world effects. It uses its own ray lighting and samples the client's sky textures. Map views and UI are unchanged.");
        ui.label("Hardware mode uses Vulkan, not DirectX 12. The native whole-world preload is suspended; Lighter has its own bounded BMD residency. Temporal upscaling/frame generation are also suspended. See Modules/Lighter/README.md for compatibility notes.");
    }
}

#[cfg(test)]
mod tests;
