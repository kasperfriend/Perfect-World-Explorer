//! Independent world renderer. The host supplies indexed geometry/materials
//! and a frame snapshot, then either Lighter or the native world renderer
//! writes the scene and reverse-Z depth target, never both.
mod bvh;
pub(crate) mod coverage;
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
use std::time::Instant;
use textures::{suffix_segments, TexelPrefix, Textures};

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
    pub material_time: [f32; 4], // terrain night weight, grass fade enable, reserved
    pub grass_history: [[f32; 4]; scene::GRASS_WIND_SLOTS],
    pub shore_offsets: [f32; 4],
    pub resident_bounds: [f32; 4], // x min/max, z min/max
    pub water_effects: [u32; 4],   // simple-water tile, start body id (nonzero), reserved
}

// Matches Pixel in pixels.wgsl. Includes padding-free guide storage.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Pixel {
    radiance: [f32; 4],
    guide: [u32; 4],
}
const PIXEL_BYTES: u64 = std::mem::size_of::<Pixel>() as u64;
// Reserve a stable share even when a retained stage is toggled off. A
// resolution increase in another stage must not consume its reservation.
// Trace, two reprojection histories, and spatial filter.
const MAX_IMAGE_BYTES: u64 = (256 << 20) / 4;

pub const WATER_MIRROR: u32 = 8;
pub const CULL_BUILDINGS: u32 = 16;
pub const UNDERWATER: u32 = 32;
const SOFT_SUN: u32 = 64;
const ANISO_WIDE: u32 = 128;
const ANISO_ISOTROPIC: u32 = 256;
pub const BAKED_LIGHTMAPS: u32 = 512;
pub const GRASS_AREA_LIGHTMAPS: u32 = 1024;
const RELIGHT_BAKED: u32 = 2048;
pub const CLOUD_DETAIL: u32 = 4096;

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

fn image_limit(device: &wgpu::Device) -> u64 {
    (storage_limit(device) as u64).min(MAX_IMAGE_BYTES)
}

fn grown_capacity(needed: u64, current: u64, limit: u64) -> u64 {
    debug_assert!(needed <= limit);
    needed.max(current.saturating_mul(2).min(limit))
}

/// Bindings depend on actual resources, not pixels, scene revisions or the
/// addresses of Rust wrappers that may be moved by their owner.
struct Bindings<const N: usize, const CAP: usize = 1> {
    active: [Option<([wgpu::Id<wgpu::Buffer>; N], wgpu::BindGroup)>; CAP],
    created: u64,
    replace: usize,
}
impl<const N: usize, const CAP: usize> Default for Bindings<N, CAP> {
    fn default() -> Self {
        Self {
            active: std::array::from_fn(|_| None),
            created: 0,
            replace: 0,
        }
    }
}
impl<const N: usize, const CAP: usize> Bindings<N, CAP> {
    fn get(
        &mut self,
        device: &wgpu::Device,
        label: &str,
        layout: &wgpu::BindGroupLayout,
        buffers: [&wgpu::Buffer; N],
    ) -> &wgpu::BindGroup {
        let ids = buffers.map(wgpu::Buffer::global_id);
        let slot = self
            .active
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|(old, _)| *old == ids))
            .unwrap_or_else(|| {
                let i = self.replace;
                self.replace = (i + 1) % CAP;
                let entries: [_; N] = std::array::from_fn(|i| resource(i as u32, buffers[i]));
                self.active[i] = Some((
                    ids,
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some(label),
                        layout,
                        entries: &entries,
                    }),
                ));
                self.created += 1;
                i
            });
        &self.active[slot].as_ref().unwrap().1
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
    fn ensure(
        &mut self,
        device: &wgpu::Device,
        label: &'static str,
        needed: u64,
        limit: u64,
    ) -> bool {
        if self.0.size() >= needed {
            return false;
        }
        *self = Self::new(
            device,
            label,
            grown_capacity(needed, self.0.size(), limit),
            self.0.usage(),
        );
        true
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        data: &[u8],
        retained_prefix: usize,
    ) -> bool {
        let grew = self.ensure(
            device,
            label,
            data.len() as u64,
            storage_limit(device) as u64,
        );
        let offset = if grew || retained_prefix > data.len() {
            0
        } else {
            retained_prefix
        };
        if offset < data.len() {
            queue.write_buffer(&self.0, offset as u64, &data[offset..]);
        }
        grew
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
    binds: Bindings<5>,
    buffers_created: u64,
    revision: Option<u64>,
    texture_len: usize,
    texture_prefix: TexelPrefix,
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
            binds: Bindings::default(),
            buffers_created: 5,
            revision: None,
            texture_len: 0,
            texture_prefix: TexelPrefix::default(),
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
        self.buffers_created +=
            u64::from(
                self.output
                    .ensure(device, "Lighter image", bytes, image_limit(device)),
            );
        let new_world = self.revision != Some(world.revision);
        if new_world {
            self.buffers_created += u64::from(self.world.upload(
                device,
                queue,
                "Lighter world",
                world.packet.bytes(),
                0,
            ));
            self.revision = Some(world.revision);
        }
        let texels = &world.textures;
        let len = texels.byte_len();
        if new_world || self.texture_len != len {
            let grew = self.textures.ensure(
                device,
                "Lighter texels",
                len as u64,
                storage_limit(device) as u64,
            );
            let prefix = if grew {
                0
            } else if new_world {
                texels.shared_bytes(&self.texture_prefix)
            } else {
                self.texture_len
            };
            for (offset, part) in suffix_segments(texels.byte_segments(), prefix) {
                queue.write_buffer(&self.textures.0, offset as u64, part);
            }
            self.buffers_created += u64::from(grew);
            self.texture_len = len;
            if new_world {
                texels.remember_prefix(&mut self.texture_prefix);
            }
        }
        if self.last_moving != moving.words {
            self.buffers_created += u64::from(self.moving.upload(
                device,
                queue,
                "Lighter moving geometry",
                moving.bytes(),
                0,
            ));
            self.last_moving.clone_from(&moving.words);
            #[cfg(test)]
            {
                self.moving_uploads += 1;
            }
        }
        queue.write_buffer(&self.frame, 0, bytemuck::bytes_of(frame));
        let bind = self.binds.get(
            device,
            "Lighter ray data",
            &self.layout,
            [
                &self.frame,
                &self.world.0,
                &self.moving.0,
                &self.textures.0,
                &self.output.0,
            ],
        );
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Lighter primary and secondary rays (software)"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.dispatch_workgroups(frame.extent[0].div_ceil(8), frame.extent[1].div_ceil(8), 1);
        &self.output.0
    }
}

struct Present {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    extent: wgpu::Buffer,
    binds: Bindings<2, 2>,
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
            binds: Bindings::default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self,
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
        let bind = self.binds.get(
            device,
            "Lighter composition",
            &self.layout,
            [image, &self.extent],
        );
        let mut pass = output_pass(encoder, target, depth);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
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
    allocation_sample: Option<(Instant, [u64; 3], u64)>,
    pub(crate) moving_accel: scene::AccelCache,
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
    pub(crate) static_coverage: coverage::Inventory,
    pub(crate) moving_coverage: coverage::Inventory,
    pub(crate) skin_omissions: [crate::skin::SkinOmissions; 3],
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
            allocation_sample: None,
            moving_accel: Default::default(),
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
            static_coverage: Default::default(),
            moving_coverage: Default::default(),
            skin_omissions: Default::default(),
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

    pub fn texture_mips(&self) -> textures::MipStats {
        self.world
            .as_ref()
            .map_or_else(Default::default, |w| w.textures.mips)
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

    fn allocation_counts(&self) -> [u64; 3] {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let [mut buffers, mut bindings, structures] = self
            .hardware
            .as_ref()
            .map_or([0; 3], vulkan::Renderer::allocation_counts);
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let [mut buffers, mut bindings, structures] = [0; 3];
        if let Some(s) = &self.software {
            buffers += s.buffers_created;
            bindings += s.binds.created;
        }
        if let Some(p) = &self.present {
            buffers += 1;
            bindings += p.binds.created;
        }
        if let Some(p) = &self.post {
            let (b, g) = p.allocation_counts();
            buffers += b;
            bindings += g;
        }
        [buffers, bindings, structures]
    }

    fn log_allocations(&mut self) {
        if !crate::diagnostics::enabled(crate::diagnostics::Channel::TimingFrame) {
            self.allocation_sample = None;
            return;
        }
        let counts = self.allocation_counts();
        let (since, old, frames) = self
            .allocation_sample
            .get_or_insert_with(|| (Instant::now(), [0; 3], 0));
        *frames += 1;
        if since.elapsed().as_secs_f64() >= 1. {
            let [buffers, bindings, structures] = std::array::from_fn(|i| counts[i] - old[i]);
            crate::diag!(TimingFrame, "Lighter GPU creations in {} rendered frames: {buffers} buffers, 0 textures, {bindings} bindings, {structures} acceleration structures", *frames);
            *since = Instant::now();
            *old = counts;
            *frames = 0;
        }
    }

    fn ray_extent(&self, device: &wgpu::Device, w: u32, h: u32) -> Result<(u32, u32), String> {
        let pixel_limit = image_limit(device) / PIXEL_BYTES;
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

    /// Swapchain recreation may idle the native device, not just the wgpu
    /// queue. A raw image fill is not visible to wgpu's lifetime tracker.
    pub(crate) fn poll_hardware(&mut self) -> Result<bool, String> {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(rt) = &mut self.hardware {
            return rt.poll_frame();
        }
        Ok(true)
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
            ("textures", world.textures.byte_len()),
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
            | if self.settings.soft_sun { SOFT_SUN } else { 0 }
            | if self.settings.relight_baked {
                RELIGHT_BAKED
            } else {
                0
            }
            | match self.settings.anisotropy {
                16 => ANISO_WIDE,
                1 => ANISO_ISOTROPIC,
                _ => 0,
            };
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
                world.textures.byte_len(),
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
                        &world.textures,
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
        self.present.as_mut().unwrap().draw(
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
        let message = format!("{} active · {rw}×{rh} rays · {} unique resident / {} moving triangles · {} instances · {} unavailable texture(s) · mip_fallback {} ({} reduced)",
            self.settings.mode.label(),world.packet.triangles(),moving.triangles(),world.packet.instances.len(),world.textures.missing,world.textures.mips.fallback,world.textures.mips.reduced);
        if self.status.is_none() {
            eprintln!("Lighter: {message}");
        }
        self.status = Some(message);
        self.log_allocations();
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
            .on_hover_text("Reuse lighting samples in stable scenes. Reproject tiny view changes; camera movement, changed lighting or incompatible surfaces discard history.");
        ui.add(egui::Slider::new(&mut self.settings.bounces,0..=4).text("Secondary bounces"))
            .on_hover_text("0 traces primary visibility and direct shadows only. Reflection and water transmission need at least one bounce.");
        ui.add(egui::Slider::new(&mut self.settings.samples, 1..=4).text("Samples per pixel"));
        ui.add(
            egui::Slider::new(&mut self.settings.resolution, 25..=100)
                .text("Ray resolution")
                .suffix("%"),
        );
        egui::ComboBox::from_label("Ray texture anisotropy")
            .selected_text(format!("{}×", self.settings.anisotropy))
            .show_ui(ui, |ui| {
                for n in [1, 8, 16] {
                    ui.selectable_value(&mut self.settings.anisotropy, n, format!("{n}×"));
                }
            })
            .response
            .on_hover_text("1× uses isotropic trilinear filtering. 8× and 16× retain more detail along grazing surfaces.");
        ui.checkbox(&mut self.settings.relight_baked, "Relight baked scenery")
            .on_hover_text("Use authored reflectance rather than baked building vertex light or terrain/grass lightmaps. Ray lighting replaces the bake; the two are not multiplied together.");
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
            if self.error().is_none() && self.world.is_some() {
                ui.collapsing("Ray scene inventory", |ui| {
                    for entry in self.static_coverage.combine(&self.moving_coverage).labels() {
                        ui.label(entry);
                    }
                });
            }
            for (source, counts) in ["World", "Walker", "Populator"]
                .iter()
                .zip(self.skin_omissions)
            {
                if counts.files != 0 {
                    ui.label(format!(
                        "{source} unsupported skin records (seen): {counts}"
                    ));
                }
            }
        }
        ui.separator();
        ui.label("Lighter traces the streamed main world, including water, vegetation, buildings and animated world effects. It uses its own ray lighting and samples the client's sky textures. Map views and UI are unchanged.");
        ui.label("Hardware mode uses Vulkan, not DirectX 12. The native whole-world preload is suspended; Lighter has its own bounded BMD residency. Temporal upscaling/frame generation are also suspended. See Modules/Lighter/README.md for compatibility notes.");
    }
}

#[cfg(test)]
mod tests;
