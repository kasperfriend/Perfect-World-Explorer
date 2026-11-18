//! Independent world renderer. The host supplies indexed geometry/materials
//! and a frame snapshot, then either Lighter or the native world renderer
//! writes the scene and reverse-Z depth target, never both.
mod bvh;
pub(crate) mod model_feed;
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
    pub near_bounds: [f32; 4],
    pub extent: [u32; 4],
    pub options: [u32; 4],
    pub waves: [u32; 4],
    pub sky0: [u32; 4],
    pub sky1: [u32; 4],
    pub sky_tint: [f32; 4],
    pub sky_uv: [f32; 4],
    pub sun_disc: [f32; 4],
    pub moon_disc: [f32; 4],
}

pub const WATER_MIRROR: u32 = 8;
pub const CULL_BUILDINGS: u32 = 16;
pub const UNDERWATER: u32 = 32;

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
            source: wgpu::ShaderSource::Wgsl([BASE, SOFTWARE].concat().into()),
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
            output: Storage::new(device, "Lighter image", 16, wgpu::BufferUsages::empty()),
            revision: None,
            texture_len: 0,
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
        self.moving
            .upload(device, queue, "Lighter moving geometry", moving.bytes());
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
            source: wgpu::ShaderSource::Wgsl(include_str!("present.wgsl").into()),
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
            label: Some("Lighter output extent"),
            size: 16,
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
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
    ) {
        queue.write_buffer(&self.extent, 0, bytemuck::bytes_of(&extent));
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
    pub settings: Settings,
    armed: bool,
    adapter_supports_compute: bool,
    has_hardware_device: bool,
    native_backend: wgpu::Backend,
    pub world: Option<World>,
    revision: u64,
    scene_error: Option<String>,
    frame_error: Option<String>,
    persistence_error: Option<String>,
    status: Option<String>,
}

impl Default for Lighter {
    fn default() -> Self {
        Self {
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            hardware: None,
            software: None,
            present: None,
            settings: Settings::default(),
            armed: false,
            adapter_supports_compute: false,
            has_hardware_device: false,
            native_backend: wgpu::Backend::Empty,
            world: None,
            revision: 0,
            scene_error: None,
            frame_error: None,
            persistence_error: None,
            status: None,
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

    pub fn fail_frame(&mut self, error: String) {
        if self.frame_error.as_deref() != Some(&error) {
            eprintln!("Lighter: {error}");
        }
        self.frame_error = Some(error);
        self.status = None;
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
    ) -> Result<(), String> {
        if let Some(reason) = self.unavailable() {
            return Err(reason.into());
        }
        let world = self
            .world
            .as_ref()
            .ok_or("Lighter has no resident ray scene")?;
        let limits = device.limits();
        let storage = u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size);
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
        let [_, _, w, h] = frame.extent;
        let scaled = |n: u32| (n * self.settings.resolution / 100).max(1);
        let mut rw = scaled(w);
        let mut rh = scaled(h);
        while u64::from(rw) * u64::from(rh) * 16 > storage {
            if rw == 1 && rh == 1 {
                return Err("Lighter cannot allocate ray image".into());
            }
            rw = rw.div_ceil(2);
            rh = rh.div_ceil(2);
        }
        frame.extent = [rw, rh, w, h];
        frame.options[0] |= u32::from(self.settings.lighting)
            | u32::from(self.settings.shadows) << 1
            | u32::from(self.settings.reflections) << 2;
        frame.options[1] = self.settings.bounces;
        frame.options[2] = self.settings.samples;
        let bytes = u64::from(rw) * u64::from(rh) * 16;
        if self.present.is_none() {
            self.present = Some(checked_pipeline(device, || Present::new(device, format))?);
        }
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
                    if self.hardware.is_none() {
                        self.hardware =
                            Some(vulkan::Renderer::new(device, &[BASE, HARDWARE].concat())?);
                    }
                    self.hardware.as_mut().unwrap().trace(
                        device,
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
        self.present.as_ref().unwrap().draw(
            device,
            queue,
            encoder,
            image,
            frame.extent,
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
        Ok(())
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
        ui.checkbox(&mut self.settings.reflections, "Ray-traced reflections");
        ui.add(egui::Slider::new(&mut self.settings.bounces,0..=4).text("Secondary bounces"))
            .on_hover_text("0 traces primary visibility and direct shadows only. Reflection and water transmission need at least one bounce.");
        ui.add(egui::Slider::new(&mut self.settings.samples, 1..=4).text("Samples per pixel"));
        ui.add(
            egui::Slider::new(&mut self.settings.resolution, 25..=100)
                .text("Ray resolution")
                .suffix("%"),
        );
        if self.settings != old {
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
        ui.separator();
        ui.label("Lighter traces the streamed main world, including water, vegetation, buildings and animated world effects. It uses its own ray lighting and samples the client's sky textures. Map views and UI are unchanged.");
        ui.label("Hardware mode uses Vulkan, not DirectX 12. The whole-world scenery preload and temporal upscaling/frame generation are suspended while Lighter is active. See Modules/Lighter/README.md for compatibility notes.");
    }
}

#[cfg(test)]
mod tests;
