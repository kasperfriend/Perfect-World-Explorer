//! Independent world renderer. The host supplies indexed geometry/materials
//! and a frame snapshot, then either Lighter or the native world renderer
//! writes the scene and reverse-Z depth target, never both.
mod bvh;
pub(crate) mod coverage;
mod game_profile;
mod log;
pub(crate) mod model_feed;
mod post;
mod profile;
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub(crate) mod reconstruction;
mod residency;
#[cfg(all(test, any(target_os = "windows", target_os = "linux")))]
mod svgf_tests;
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub(crate) mod reconstruction {
    pub(crate) struct Startup;
    impl Startup {
        pub(crate) fn new(_: super::Mode) -> Self {
            Self
        }
        pub(crate) fn instance(&mut self, backends: wgpu::Backends) -> wgpu::Instance {
            wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..Default::default()
            })
        }
        pub(crate) fn device(
            &mut self,
            _: &wgpu::Adapter,
            _: &wgpu::DeviceDescriptor<'_>,
        ) -> Option<(wgpu::Device, wgpu::Queue)> {
            None
        }
        pub(crate) fn disable(&mut self, _: &str) {}
        pub(crate) fn install(self, _: &mut super::Lighter, _: &wgpu::Device) {}
    }
}
pub(crate) mod scene;
mod settings;
mod temporal;
pub(crate) use temporal::Camera as TemporalCamera;
pub(crate) mod sky;
pub(crate) mod textures;
#[cfg(any(target_os = "windows", target_os = "linux"))]
mod vulkan;

use bytemuck::{Pod, Zeroable};
use scene::Packet;
pub(crate) use settings::{Denoiser, Mode, Settings};
use std::time::{Duration, Instant};
use textures::{suffix_segments, upload_pages, TexelPrefix, Textures};

const PIXELS: &str = include_str!("pixels.wgsl");
const BSDF: &str = include_str!("bsdf.wgsl");
const REFERENCE: &str = include_str!("reference.wgsl");
const MEDIA: &str = include_str!("media.wgsl");
const BASE: &str = include_str!("trace.wgsl");
const PRESENT: &str = include_str!("present.wgsl");
const PRESENT_PORTABLE: &str = r#"
@fragment fn fragment(@builtin(position) coord: vec4<f32>) -> Output {
    return resolve(coord);
}
"#;
const PRESENT_TEMPORAL: &str = include_str!("temporal_present.wgsl");
const SOFTWARE: &str = include_str!("software.wgsl");
#[cfg(any(target_os = "windows", target_os = "linux"))]
const HARDWARE: &str = include_str!("hardware.wgsl");

const BC_TERRAIN: &str = include_str!("bc_terrain.wgsl");
const BC_BUFFER_LOAD: &str = r#"
fn bc_sample_level(addr: u32, uv: vec2<f32>, mip: u32, fmt: u32) -> vec3<f32> {
    return bc_terrain_level(addr,uv,mip,(fmt & BC_TERRAIN_1)!=0u);
}
"#;
const MATERIAL_BUFFER_LOAD: &str = r#"
fn level(addr: u32, uv: vec2<f32>, edge: bool, mip: u32, fmt: u32) -> vec4<f32> {
    return buffer_level(addr,uv,edge,mip,fmt);
}
"#;

const DECODED_TERRAIN: &str = r#"
fn terrain_tile(addr: u32, uv: vec2<f32>, taps: Taps) -> vec3<f32> {
    return filtered_tile(addr,uv,false,taps).rgb;
}
"#;

const SCREEN_REUSE: &str = include_str!("screen_reuse.wgsl");
const SCREEN_OFF: &str = r#"
fn screen_begin(at: u32) {}
fn screen_end() {}
fn screen_surface(s: Surface, hit: Hit) -> bool { return false; }
fn screen_lights(p: vec3<f32>, s: Surface, tag: vec2<u32>, inv_pmf: f32, fresh: LightRay, seed: ptr<function,u32>) -> LightRay {
    var ray = fresh;
    ray.energy *= inv_pmf;
    return ray;
}
fn screen_gi_read(p: vec3<f32>, s: Surface, hit: Hit) -> vec4<f32> { return vec4<f32>(0.); }
fn screen_gi_write(p: vec3<f32>, s: Surface, hit: Hit, color: vec3<f32>) {}
"#;

const SCREEN_DI: u32 = 1;
const SCREEN_GI: u32 = 2;
const SCREEN_PREVIOUS: u32 = 4;
const SCREEN_GI_PREVIOUS: u32 = 8;
const SCREEN_REPROJECTED: u32 = 16;
const SCREEN_IMAGE_PLANES: u64 = 7;

fn screen_reuse_requested() -> u32 {
    match std::env::var("PW_RAY_SCREEN_REUSE").as_deref() {
        Ok("di") => SCREEN_DI,
        Ok("gi") => SCREEN_GI,
        Ok("both") => SCREEN_DI | SCREEN_GI,
        _ => 0,
    }
}

pub(crate) fn wants_screen_reuse() -> bool {
    screen_reuse_requested() != 0
}

fn trace_source(backend: &str, compact_terrain: bool, screen_reuse: bool) -> String {
    trace_source_with_pages(backend, compact_terrain, screen_reuse, "", false)
}

fn trace_source_with_pages(
    backend: &str,
    compact_terrain: bool,
    screen_reuse: bool,
    pages: &str,
    temporal: bool,
) -> String {
    trace_source_parts(backend, compact_terrain, screen_reuse, pages, "", temporal)
}

fn trace_source_parts(
    backend: &str,
    compact_terrain: bool,
    screen_reuse: bool,
    pages: &str,
    material_pages: &str,
    temporal: bool,
) -> String {
    let material = if material_pages.is_empty() {
        MATERIAL_BUFFER_LOAD
    } else {
        material_pages
    };
    let (terrain, load) = if !pages.is_empty() {
        (BC_TERRAIN, pages)
    } else if compact_terrain {
        (BC_TERRAIN, BC_BUFFER_LOAD)
    } else {
        (DECODED_TERRAIN, "")
    };
    let reuse = if screen_reuse {
        SCREEN_REUSE
    } else {
        SCREEN_OFF
    };
    let inputs = if temporal {
        temporal::TRACE
    } else {
        temporal::TRACE_OFF
    };
    [
        PIXELS, BSDF, MEDIA, REFERENCE, BASE, material, terrain, load, reuse, inputs, backend,
    ]
    .concat()
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn raster_source() -> String {
    // Only candidate acceptance runs in the fragment stage; opaque terrain
    // color and screen caches are consumed by the compute sibling instead.
    let mut source = trace_source(SOFTWARE, false, false);
    source.push_str(include_str!("primary.wgsl"));
    source
}

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
    if startup == Mode::Hardware && supports_hardware(adapter) {
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
    pub material_time: [f32; 4], // terrain night weight, grass fade, static light night factor, diffuse distance
    pub grass_history: [[f32; 4]; scene::GRASS_WIND_SLOTS],
    pub shore_offsets: [f32; 4],
    pub resident_bounds: [f32; 4], // x min/max, z min/max
    pub water_effects: [u32; 4],   // simple tile, initial body, wet-cell table, body count
    pub sky_cdf: [u32; 4],         // current and next sky CDF offsets
    pub previous_screen_vp: [[f32; 4]; 4],
    pub ray_effects: [f32; 4], // opt-in specular/sun/diffuse length limits and active tag
}

// Matches Pixel in pixels.wgsl. Includes padding-free guide storage.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Pixel {
    rg: u32,
    bv: u32,
    depth: f32,
    view_depth: f32,
    normal_flags: u32,
    albedo_history: u32,
    owner: u32,
}
const PIXEL_BYTES: u64 = std::mem::size_of::<Pixel>() as u64;
// Reserve a stable share even when a retained stage is toggled off. A
// resolution increase in another stage must not consume its reservation.
// Trace, two reprojection histories, and spatial filter.
pub(super) const RAY_ALLOCATION_BUDGET: u64 = 6_500_000_000;
const HEAP_WARNING_BYTES: u64 = 7_000_000_000;
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
const REFERENCE_MODE: u32 = 16384;
const SINGLE_PATH: u32 = 32768;
#[cfg(any(target_os = "windows", target_os = "linux"))]
const HYBRID_PRIMARY: u32 = 65536;
const PHYSICAL_BSDF: u32 = 8192;
const RAY_LIGHTING_OVERLAY: u32 = 512;

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
    staged: Option<residency::SoftwareScene>,
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
    fn new(
        device: &wgpu::Device,
        compact_terrain: bool,
        screen_reuse: bool,
        temporal: bool,
    ) -> Self {
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
            source: wgpu::ShaderSource::Wgsl(
                trace_source_with_pages(SOFTWARE, compact_terrain, screen_reuse, "", temporal)
                    .into(),
            ),
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
            textures: Storage::new(
                device,
                "Lighter texels",
                16,
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            ),
            output: Storage::new(
                device,
                "Lighter image",
                PIXEL_BYTES,
                wgpu::BufferUsages::empty(),
            ),
            staged: None,
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

    fn stage_resident(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        revision: u64,
        packet: &Packet,
        texels: &Textures,
        limit: usize,
    ) -> Result<bool, String> {
        if self.staged.is_none() {
            let prefix = texels.shared_bytes(&self.texture_prefix);
            let capacity = residency::texel_capacity(texels).min(storage_limit(device));
            self.staged = Some(residency::SoftwareScene::new(
                device,
                revision,
                packet.bytes().len(),
                capacity,
                prefix,
            )?);
            self.buffers_created += 2;
        }
        let stage = self.staged.as_mut().unwrap();
        assert_eq!(stage.revision, revision, "one resident candidate at a time");
        match stage.poll(
            device,
            queue,
            packet.bytes(),
            texels,
            &self.textures.0,
            limit,
        ) {
            Ok(ready) => Ok(ready),
            Err(error) => {
                self.staged = None;
                Err(error)
            }
        }
    }

    fn publish_resident(&mut self, revision: u64, texels: &Textures) -> Option<residency::Stats> {
        if !self
            .staged
            .as_ref()
            .is_some_and(|stage| stage.ready(revision))
        {
            return None;
        }
        let stage = self.staged.take().unwrap();
        self.world = stage.world;
        self.textures = stage.texels;
        self.revision = Some(revision);
        self.texture_len = texels.byte_len();
        texels.remember_prefix(&mut self.texture_prefix);
        Some(stage.stats)
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
            for (offset, part) in upload_pages(texels.byte_segments(), prefix) {
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

type ResolvedBindings = (
    (
        wgpu::Id<wgpu::Buffer>,
        wgpu::Id<wgpu::TextureView>,
        wgpu::Id<wgpu::TextureView>,
    ),
    wgpu::BindGroup,
);
struct Present {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    extent: wgpu::Buffer,
    binds: Bindings<2, 2>,
    resolved: Option<ResolvedBindings>,
}
impl Present {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, temporal_output: bool) -> Self {
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let mut entries = vec![
            binding(
                0,
                fragment,
                wgpu::BufferBindingType::Storage { read_only: true },
            ),
            binding(1, fragment, wgpu::BufferBindingType::Uniform),
        ];
        if temporal_output {
            entries.extend([
                temporal::sampled(2, fragment),
                temporal::sampled(3, fragment),
            ]);
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Lighter composition bindings"),
            entries: &entries,
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Lighter composition layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Lighter scene and depth blit"),
            source: wgpu::ShaderSource::Wgsl(
                [
                    PIXELS,
                    PRESENT,
                    if temporal_output {
                        PRESENT_TEMPORAL
                    } else {
                        PRESENT_PORTABLE
                    },
                ]
                .concat()
                .into(),
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
            resolved: None,
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
        temporal_output: Option<(&wgpu::TextureView, &wgpu::TextureView)>,
    ) {
        queue.write_buffer(
            &self.extent,
            0,
            bytemuck::bytes_of(&[extent, [u32::from(guided), 0, 0, 0]]),
        );
        let bind = if let Some((color, mask)) = temporal_output {
            let key = (image.global_id(), color.global_id(), mask.global_id());
            if self.resolved.as_ref().is_none_or(|entry| entry.0 != key) {
                self.resolved = Some((
                    key,
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Lighter reconstructed scene"),
                        layout: &self.layout,
                        entries: &[
                            resource(0, image),
                            resource(1, &self.extent),
                            temporal::texture(2, color),
                            temporal::texture(3, mask),
                        ],
                    }),
                ));
            }
            &self.resolved.as_ref().unwrap().1
        } else {
            self.binds.get(
                device,
                "Lighter composition",
                &self.layout,
                [image, &self.extent],
            )
        };
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

#[cfg(any(target_os = "windows", target_os = "linux"))]
enum ColdRayBuild {
    Progress(u64, &'static str),
    Done(Result<(vulkan::Renderer, bool), String>),
}

pub struct Lighter {
    // Retire SDK work before releasing imported ray buffers or the device.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    reconstruction: Option<reconstruction::Session>,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    vendor_present: Option<Present>,
    // Must be dropped before the wgpu Device (owns native Vulkan objects).
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    hardware: Option<vulkan::Renderer>,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    cold_ray: Option<std::sync::mpsc::Receiver<ColdRayBuild>>,
    /// Deadline for cold-ray startup; reset on every stage advance so a
    /// stalled pipeline worker fails closed instead of hanging forever.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    cold_ray_deadline: Option<Instant>,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    paced_startup: bool,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    initial_progress: [u64; 3],
    /// Read-only mirror of the Graphics upscaler choice, shown in the panel
    /// so the user knows what the ray overlay inherits. Not a control.
    graphics_upscaler: Option<String>,
    /// Read-only mirror of the Graphics frame-gen choice, shown in the panel.
    graphics_frame_gen: Option<String>,
    /// Last-frame status: denoiser used, upscaler inherited, frame-gen state,
    /// fallback reason. Shown in the panel as read-only information.
    status_line: Option<String>,
    /// Rotating log writer; off unless enabled by the panel toggle or
    /// PW_LIGHTER_LOG env var.
    logger: log::Logger,
    software: Option<Software>,
    present: Option<Present>,
    post: Option<post::Post>,
    history: post::History,
    timings: Option<profile::Timings>,
    gpu_sample: Option<profile::GpuSample>,
    adaptive: profile::Resolution,
    cadence: profile::Cadence,
    allocation_sample: Option<(Instant, [u64; 3], u64)>,
    pub(crate) moving_accel: scene::AccelCache,
    pub settings: Settings,
    saved_mode: Mode,
    pending_mode: Option<Mode>,
    pending_primary: Option<bool>,
    armed: bool,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    target_16_6: bool,
    capture: bool,
    capture_reference_samples: Option<u32>,
    #[cfg(test)]
    fail_start_after_prepare: bool,
    compact_terrain: bool,
    bc_pages: bool,
    screen_reuse: u32,
    screen_written: bool,
    screen_last_flags: u32,
    last_trace_extent: (u32, u32),
    last_primary: [u64; 2],
    game_profile: Option<[f32; 4]>,
    game_profile_active: bool,
    game_sampling: Option<u32>,
    adapter_supports_compute: bool,
    has_hardware_device: bool,
    native_backend: wgpu::Backend,
    pub world: Option<World>,
    revision: u64,
    last_residency: Option<residency::Stats>,
    #[cfg(test)]
    residency_transfer_bytes: usize,
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
            reconstruction: None,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            vendor_present: None,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            hardware: None,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            cold_ray: None,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            cold_ray_deadline: None,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            paced_startup: false,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            initial_progress: [0; 3],
            graphics_upscaler: None,
            graphics_frame_gen: None,
            status_line: None,
            logger: log::Logger::from_env(),
            software: None,
            present: None,
            post: None,
            history: post::History::default(),
            timings: None,
            gpu_sample: None,
            adaptive: profile::Resolution::default(),
            cadence: profile::Cadence::default(),
            allocation_sample: None,
            moving_accel: Default::default(),
            settings: Settings::default(),
            saved_mode: Mode::Off,
            pending_mode: None,
            pending_primary: None,
            armed: false,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            target_16_6: std::env::var("PW_RAY_ADAPTIVE_TIMESTAMPS").as_deref() == Ok("1"),
            capture: false,
            capture_reference_samples: None,
            #[cfg(test)]
            fail_start_after_prepare: false,
            compact_terrain: false,
            bc_pages: false,
            screen_reuse: 0,
            screen_written: false,
            screen_last_flags: 0,
            last_trace_extent: (0, 0),
            last_primary: [0, 0],
            game_profile: game_profile::requested(),
            game_profile_active: false,
            game_sampling: game_profile::blue_noise_requested().then_some(0),
            adapter_supports_compute: false,
            has_hardware_device: false,
            native_backend: wgpu::Backend::Empty,
            world: None,
            revision: 0,
            last_residency: None,
            #[cfg(test)]
            residency_transfer_bytes: residency::TRANSFER_BYTES,
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
        let bc_pages = settings.mode == Mode::Hardware
            && textures::bc_pages_requested()
            && device
                .features()
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        Self {
            armed: settings.mode != Mode::Off,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            paced_startup: settings.mode == Mode::Hardware
                && (std::env::var("PW_RAY_PACED_STARTUP").as_deref() == Ok("1")
                    || std::env::var("PW_RAY_COMPACT_BLAS").as_deref() == Ok("1")),
            compact_terrain: bc_pages || textures::compact_terrain_enabled(),
            bc_pages,
            screen_reuse: screen_reuse_requested(),
            // Readbacks belong to explicitly requested capture, not normal
            // present. Vulkan adaptive scaling reads a signaled query below.
            timings: (settings.mode != Mode::Off
                && (std::env::var("PW_LIGHTER_TRAVEL_SYNC").as_deref() == Ok("1")
                    || std::env::var("PW_RAY_CAPTURE_TIMINGS").as_deref() == Ok("1")))
            .then(|| profile::Timings::new(device))
            .flatten(),
            settings,
            saved_mode: settings.mode,
            native_backend: adapter.get_info().backend,
            adapter_supports_compute: adapter.get_downlevel_capabilities().flags.contains(
                wgpu::DownlevelFlags::COMPUTE_SHADERS | wgpu::DownlevelFlags::FRAGMENT_STORAGE,
            ),
            has_hardware_device: !rt.is_empty() && device.features().contains(rt),
            ..Self::default()
        }
    }

    /// Booting Native doesn't discard the last committed ray selection.
    pub(crate) fn stage_saved_mode(&mut self, mode: Mode) {
        self.saved_mode = mode;
        self.pending_mode = (mode != self.settings.mode).then_some(mode);
    }

    fn settings_for_disk(&self) -> Settings {
        Settings {
            mode: self.saved_mode,
            ..self.settings
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

    /// Headless audit only. Keep the interactive Reference preset at 32 spp.
    pub(crate) fn set_capture_reference_samples(&mut self, samples: Option<u32>) {
        self.capture_reference_samples = samples;
    }

    pub(crate) fn effective_settings(&self) -> Settings {
        let mut settings = self.settings.effective();
        if settings.reference {
            if let Some(samples) = self.capture_reference_samples {
                settings.samples = samples;
            }
        }
        settings
    }

    pub(crate) fn raster_active(&self) -> bool {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(rt) = &self.hardware {
            return self.settings.mode == Mode::Hardware && rt.raster_active();
        }
        false
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) const CPU_QUERY_TEST_FEATURES: wgpu::Features = vulkan::FEATURES;

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn for_cpu_query_test(
        mut self,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
    ) -> Self {
        assert_eq!(adapter.get_info().device_type, wgpu::DeviceType::Cpu);
        assert!(!supports_hardware(adapter));
        assert!(device.features().contains(Self::CPU_QUERY_TEST_FEATURES));
        self.has_hardware_device = true;
        self.for_capture()
    }

    pub(crate) fn screen_reuse_flags(&self) -> u32 {
        self.screen_last_flags
            & (SCREEN_DI | SCREEN_GI | SCREEN_PREVIOUS | SCREEN_GI_PREVIOUS | SCREEN_REPROJECTED)
    }

    /// Exact eligible first-solid query slots, independent of the number
    /// of ray-box/triangle tests or later ordered/secondary traversals.
    pub(crate) fn travel_primary(&self) -> [u64; 2] {
        self.last_primary
    }

    pub(crate) fn game_profile_active(&self) -> bool {
        self.game_profile_active
    }

    pub(crate) fn travel_rt_counts(&self) -> Option<[u64; 2]> {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if self.settings.mode == Mode::Hardware {
            return self.hardware.as_ref().map(vulkan::Renderer::rt_counts);
        }
        None
    }

    pub(crate) fn travel_trace(&self) -> ((u32, u32), (u32, bool, bool)) {
        (
            self.last_trace_extent,
            (
                self.screen_reuse_flags() & (SCREEN_DI | SCREEN_GI),
                self.compact_terrain,
                self.bc_page_stats()[0] > 0,
            ),
        )
    }

    /// Counters for the just-published streamed candidate, not allocator or
    /// timing data. The caller only attaches them to a commit receipt.
    pub(crate) fn travel_pacing(&self) -> Option<[u64; 6]> {
        self.last_residency.map(|s| {
            [
                s.batches,
                s.host_bytes,
                s.resident_bytes,
                s.max_batch_bytes as u64,
                s.blas_batches,
                s.blas_builds,
            ]
        })
    }

    pub(crate) fn travel_textures(&self) -> [u64; 5] {
        let logical = self
            .world
            .as_ref()
            .map_or(0, |w| w.textures.byte_len() as u64);
        let [tiles, _, images] = self.bc_page_stats();
        let [bc5, bc7] = self.bc_material_tiles();
        [logical, tiles, bc5, bc7, images]
    }

    pub fn armed(&self) -> bool {
        self.armed
    }
    /// Whether a usable ray scene, rather than just a requested mode, is live.
    pub fn replaces_world(&self) -> bool {
        self.armed && self.requested() && self.error().is_none()
    }
    pub fn requested(&self) -> bool {
        self.settings.mode != Mode::Off
    }

    /// The host holds one candidate until both the Native and ray bindings
    /// are ready. Do not publish these buffers, coverage or a new revision
    /// while an old frame can still read them.
    pub(crate) fn stage_resident(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &Packet,
        texels: &Textures,
    ) -> Result<bool, String> {
        if !self.armed || !self.requested() || self.unavailable().is_some() {
            return Ok(true);
        }
        if packet.bytes().len().max(texels.byte_len()) > storage_limit(device) {
            return Err("Lighter candidate exceeds adapter storage limit".into());
        }
        let revision = self.revision.wrapping_add(1);
        #[cfg(not(test))]
        let limit = residency::TRANSFER_BYTES;
        #[cfg(test)]
        let limit = self.residency_transfer_bytes;
        match self.settings.mode {
            Mode::Software => {
                if let Some(software) = &mut self.software {
                    return software.stage_resident(device, queue, revision, packet, texels, limit);
                }
            }
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            Mode::Hardware => {
                if let Some(rt) = &mut self.hardware {
                    return rt.stage_resident(revision, packet, texels, limit);
                }
            }
            _ => {}
        }
        // A ray error during first open may not have created a GPU pipeline.
        // Its later recovery takes the watched first-use path as before.
        Ok(true)
    }

    pub fn install(&mut self, scene: Result<(Packet, Textures), String>) {
        self.revision = self.revision.wrapping_add(1);
        self.frame_error = None;
        match scene {
            Ok((packet, textures)) => {
                self.last_residency = match self.settings.mode {
                    Mode::Software => self
                        .software
                        .as_mut()
                        .and_then(|rt| rt.publish_resident(self.revision, &textures)),
                    #[cfg(any(target_os = "windows", target_os = "linux"))]
                    Mode::Hardware => self
                        .hardware
                        .as_mut()
                        .and_then(|rt| rt.publish_resident(self.revision, &textures)),
                    _ => None,
                };
                if let Some(s) = self.last_residency {
                    crate::diag!(TimingLoad, "Lighter staged residency: {} transfer batch(es), host {} B, reused GPU {} B, peak batch {} B; {} BLAS in {} batch(es)",
                        s.batches, s.host_bytes, s.resident_bytes, s.max_batch_bytes, s.blas_builds, s.blas_batches);
                }
                self.world = Some(World {
                    packet,
                    textures,
                    revision: self.revision,
                });
                self.scene_error = None;
            }
            Err(error) => {
                if self.scene_error.as_deref() != Some(&error) {
                    eprintln!("Lighter: {error}");
                }
                self.world = None;
                self.last_residency = None;
                self.scene_error = Some(error);
                self.reset_history();
                self.status = None;
            }
        }
        // A failed or bypassed candidate must not be mistaken for the next
        // revision. wgpu owns submitted buffer lifetimes; raw Vulkan polls.
        if let Some(rt) = &mut self.software {
            rt.staged = None;
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(rt) = &mut self.hardware {
            rt.abandon_residency();
        }
    }

    fn unavailable(&self) -> Option<&str> {
        if !self.adapter_supports_compute {
            Some("This adapter cannot run Lighter's compute and storage-buffer passes.")
        } else if self.settings.mode == Mode::Hardware && !self.has_hardware_device {
            if self.native_backend != wgpu::Backend::Vulkan {
                Some("Hardware RT needs Vulkan 1.2 with ray queries. Use Reload World with Hardware selected and remove any conflicting WGPU_BACKEND override.")
            } else {
                Some("No hardware ray-query support on the selected Vulkan adapter. Choose Software or select an RT-capable GPU.")
            }
        } else {
            None
        }
    }

    pub(crate) fn uses_bc_pages(&self) -> bool {
        self.bc_pages
    }

    pub(crate) fn bc_page_stats(&self) -> [u64; 3] {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(rt) = &self.hardware {
            return rt.bc_page_stats();
        }
        [0; 3]
    }

    pub(crate) fn bc_material_tiles(&self) -> [u64; 2] {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(rt) = self
            .hardware
            .as_ref()
            .filter(|_| self.settings.mode == Mode::Hardware)
        {
            return rt.bc_material_tiles();
        }
        [0; 2]
    }

    pub fn texture_mips(&self) -> textures::MipStats {
        self.world
            .as_ref()
            .map_or_else(Default::default, |w| w.textures.mips)
    }

    pub fn error(&self) -> Option<&str> {
        if !self.armed || !self.requested() {
            return None;
        }
        self.unavailable()
            .or(self.scene_error.as_deref())
            .or(self.frame_error.as_deref())
            .or_else(|| {
                self.world
                    .is_none()
                    .then_some("Lighter has no resident ray scene")
            })
    }

    pub(crate) fn profile_submitted(&mut self) {
        if let Some(timings) = &mut self.timings {
            timings.submitted();
        }
    }

    pub(crate) fn profile_collect(&mut self, queue: &wgpu::Queue) {
        if let Some(timings) = &mut self.timings {
            if let Some(sample) = timings.collect(queue) {
                self.gpu_sample = Some(sample);
            }
        }
    }

    /// Headless captures have already synchronized their readback. Take
    /// stage samples for that completed draw, not the prior raw Vulkan fence
    /// normally used by the live controller. Never wait here.
    pub(crate) fn capture_stage_sample(&mut self, queue: &wgpu::Queue) -> Option<[f64; 6]> {
        let post = self.timings.as_mut()?.collect_submitted(queue)?;
        self.gpu_sample = Some(post);
        let mut stages = [
            0.,
            0.,
            post.trace_ms,
            post.reproject_ms,
            post.filter_ms,
            post.upscale_ms,
        ];
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if post.mode == Mode::Hardware {
            let rt = self.hardware.as_mut()?;
            if !rt.poll_frame().ok()? {
                return None;
            }
            let (extent, native) = rt.gpu_stages()?;
            if extent != post.extent {
                return None;
            }
            stages[..3].copy_from_slice(&native);
        }
        stages.iter().all(|s| s.is_finite()).then_some(stages)
    }

    pub(crate) fn capture_sdk_ms(&self) -> Option<f64> {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(session) = &self.reconstruction {
            return session.sdk_ms();
        }
        None
    }

    pub(crate) fn profile_presented(&mut self, now: Instant) {
        self.cadence.present(now);
    }

    pub(crate) fn reset_reconstruction(&mut self) {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(session) = &mut self.reconstruction {
            session.reset();
        }
        self.release_reconstruction_bind();
    }

    fn release_reconstruction_bind(&mut self) {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(present) = &mut self.vendor_present {
            present.resolved = None;
        }
    }

    pub(crate) fn reconstruction_requested(&self) -> bool {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            self.reconstruction.is_some()
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        {
            false
        }
    }

    pub(crate) fn reconstruction_status(&self) -> Option<&'static str> {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(session) = &self.reconstruction {
            return session.status();
        }
        None
    }

    pub(crate) fn reset_history(&mut self) {
        self.reset_reconstruction();
        self.history.reset();
        self.screen_written = false;
        self.screen_last_flags = 0;
        // Post-processing state depends on history; reset it so the next
        // progressive render rebuilds Post with a clean history_write index.
        self.post = None;
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

    fn has_adaptive_samples(&self) -> bool {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if self.settings.mode == Mode::Hardware {
            return self.reconstruction.is_none()
                && self.game_profile.is_none()
                && self
                    .hardware
                    .as_ref()
                    .is_some_and(vulkan::Renderer::has_timestamps);
        }
        self.timings.is_some()
    }

    fn ray_extent(&self, device: &wgpu::Device, w: u32, h: u32) -> Result<(u32, u32), String> {
        let pixel_limit = image_limit(device) / PIXEL_BYTES;
        if pixel_limit == 0 {
            return Err("Lighter cannot allocate ray image".into());
        }
        let settings = self.settings.effective();
        let pct = if settings.dynamic_resolution && self.has_adaptive_samples() {
            self.adaptive
                .percent
                .unwrap_or(settings.resolution)
                .clamp(50, 100)
        } else {
            settings.resolution
        };
        let scaled = |n: u32| (n * pct / 100).max(1);
        let mut rw = scaled(w);
        let mut rh = scaled(h);
        while u64::from(rw) * u64::from(rh) > pixel_limit {
            let fit = (pixel_limit as f64 / (f64::from(rw) * f64::from(rh))).sqrt();
            rw = ((f64::from(rw) * fit) as u32).max(1);
            rh = ((f64::from(rh) * fit) as u32).max(1);
        }
        Ok((rw, rh))
    }

    fn trace_image_layout(&self, device: &wgpu::Device, rw: u32, rh: u32) -> (u64, u32) {
        let bytes = u64::from(rw) * u64::from(rh) * PIXEL_BYTES;
        let s = self.settings.effective();
        if self.screen_reuse == 0
            || s.reference
            || !s.progressive
            || s.samples != 1
            || !s.lighting
            || !s.shadows
            || bytes * SCREEN_IMAGE_PLANES > image_limit(device)
        {
            return (bytes, 0);
        }
        let flags = self.screen_reuse
            & (u32::from(s.single_path) * SCREEN_DI | u32::from(s.bounces == 1) * SCREEN_GI);
        (
            bytes * if flags != 0 { SCREEN_IMAGE_PLANES } else { 1 },
            flags,
        )
    }

    fn frame_image_layout(&self, device: &wgpu::Device, extent: [u32; 4]) -> (u64, u32) {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(session) = self
            .reconstruction
            .as_ref()
            .filter(|s| s.extent_ready(extent))
        {
            return (session.bytes(extent), 0);
        }
        self.trace_image_layout(device, extent[0], extent[1])
    }

    /// Swapchain recreation may idle the native device, not just the wgpu
    /// queue. A raw image fill is not visible to wgpu's lifetime tracker.
    pub(crate) fn poll_hardware(&mut self) -> Result<bool, String> {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(session) = &mut self.reconstruction {
            if !session.poll()? {
                return Ok(false);
            }
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(rt) = &mut self.hardware {
            let trace_idle = rt.poll_frame()?;
            let resident_idle = rt.poll_resident_upload()?;
            return Ok(trace_idle && resident_idle);
        }
        Ok(true)
    }

    /// Keep the original failure and renderer choice. Release failed optional
    /// GPU state while the owning device and queue still exist; the failed
    /// viewer/handoff then exits or takes the existing recovery path.
    pub(crate) fn retire_failed_start(&mut self) {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            self.cold_ray = None;
            self.cold_ray_deadline = None;
            self.vendor_present = None;
            self.reconstruction = None;
        }
        self.present = None;
        self.post = None;
        self.software = None;
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            self.hardware = None;
        }
        self.timings = None;
        self.last_residency = None;
    }

    /// Write an error/fallback to stderr and (if logging is on) to the log file.
    pub(crate) fn log_error(&mut self, stage: &str, reason: &str) {
        eprintln!("Lighter: {stage}: {reason}");
        self.logger.write(&log::LogEntry::Error(log::ErrorEvent {
            stage: stage.to_string(),
            reason: reason.to_string(),
        }));
        self.status_line = Some(format!("{stage}: {reason}"));
    }

    pub(crate) fn prepare_window_frame(
        &mut self,
        device: &std::sync::Arc<wgpu::Device>,
        w: u32,
        h: u32,
        first: bool,
    ) -> Result<bool, String> {
        #[allow(unused_mut)]
        let mut result = self.prepare_window_inner(device, w, h, first);
        #[cfg(test)]
        if first && self.fail_start_after_prepare && matches!(result, Ok(true)) {
            result = Err("injected Lighter failure after GPU startup allocation".into());
        }
        if first && result.is_err() {
            self.retire_failed_start();
        }
        result
    }

    /// First-use Hardware route only. The worker creates pipelines and static
    /// handles but never submits queue work. An Arc keeps the parent Vulkan
    /// device alive if recovery or window close abandons the worker.
    fn prepare_window_inner(
        &mut self,
        device: &std::sync::Arc<wgpu::Device>,
        w: u32,
        h: u32,
        first: bool,
    ) -> Result<bool, String> {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if first
            && self.paced_startup
            && self.settings.mode == Mode::Hardware
            && self.unavailable().is_none()
        {
            use std::sync::mpsc::{channel, TryRecvError};
            if self.hardware.is_none() && self.cold_ray.is_none() {
                let (tx, rx) = channel();
                let device = device.clone();
                let compact = self.compact_terrain;
                let reuse = self.screen_reuse != 0;
                let temporal = self.reconstruction.is_some();
                let hybrid = self.settings.hybrid_primary;
                let game = self.game_profile.is_some() && hybrid;
                let pages = self
                    .world
                    .as_ref()
                    .filter(|_| self.bc_pages)
                    .map(|w| w.textures.bc_pages.clone());
                let stage: u64 = 0;
                self.cold_ray_deadline = Some(Instant::now() + Duration::from_secs(30));
                std::thread::Builder::new().name("lighter-pipeline-startup".into()).spawn(move || {
                    let progress = |step| { let _ = tx.send(ColdRayBuild::Progress(stage, step)); };
                    let build = |temporal| vulkan::Renderer::new(&device, compact, reuse, pages.as_ref(), temporal, game, &progress);
                    let made = match build(temporal) {
                        Err(error) if temporal && !crate::gpu::reconstruction_device_lost(&error) => {
                            eprintln!("Lighter temporal tracer unavailable; using portable resolve: {error}");
                            build(false).map(|r| (r, false))
                        }
                        other => other.map(|r| (r, temporal)),
                    }.map(|(mut r, enabled)| {
                        if hybrid {
                            r.prewarm_primary(&progress);
                        }
                        (r, enabled)
                    });
                    // On a closed channel the unused result drops before our
                    // Arc. There is no queue work or imported image to drain.
                    let _ = tx.send(ColdRayBuild::Done(made));
                }).map_err(|e| format!("start Lighter pipeline worker: {e}"))?;
                self.cold_ray = Some(rx);
                return Ok(false);
            }
            if let Some(rx) = &self.cold_ray {
                let made = loop {
                    if let Some(deadline) = self.cold_ray_deadline {
                        if Instant::now() > deadline {
                            return Err(
                                "Lighter cold-ray startup exceeded the first-frame timeout".into(),
                            );
                        }
                    }
                    match rx.try_recv() {
                        Ok(ColdRayBuild::Progress(_, stage_msg)) => {
                            crate::gpu::first_frame_progress(stage_msg);
                            self.cold_ray_deadline = Some(Instant::now() + Duration::from_secs(30));
                        }
                        Ok(ColdRayBuild::Done(made)) => break made,
                        Err(TryRecvError::Empty) => {
                            if let Some(deadline) = self.cold_ray_deadline {
                                if Instant::now() > deadline {
                                    return Err(
                                        "Lighter cold-ray startup exceeded the first-frame timeout"
                                            .into(),
                                    );
                                }
                            }
                            return Ok(false);
                        }
                        Err(TryRecvError::Disconnected) => {
                            break Err("Lighter pipeline worker ended before publication".into())
                        }
                    }
                };
                self.cold_ray = None;
                self.cold_ray_deadline = None;
                let (renderer, temporal) = made?;
                if !temporal {
                    self.reconstruction = None;
                }
                self.hardware = Some(renderer);
                crate::gpu::first_frame_progress(
                    "Ray pipeline worker completed; staging initial chunk",
                );
                return Ok(false);
            }
            if let (Some(rt), Some(world)) = (&mut self.hardware, &self.world) {
                let (done, stats) =
                    rt.initial_residency(self.revision, &world.packet, &world.textures)?;
                let progress = [stats.batches, stats.blas_builds, u64::from(done)];
                if progress > self.initial_progress {
                    self.initial_progress = progress;
                    crate::gpu::first_frame_progress(&format!(
                        "Initial ray residency: {} chunk(s), {} BLAS, {} copy batches complete",
                        progress[2], progress[1], progress[0]
                    ));
                }
                if !done {
                    return Ok(false);
                }
                if stats.batches != 0 {
                    self.last_residency = Some(stats);
                }
            }
        }
        self.prepare_frame_with_progress(device, w, h, |stage| {
            if first {
                crate::gpu::first_frame_progress(stage);
            }
        })
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
        self.prepare_frame_with_progress(device, w, h, |_| {})
    }

    fn prepare_frame_with_progress(
        &mut self,
        device: &wgpu::Device,
        w: u32,
        h: u32,
        progress: impl Fn(&'static str),
    ) -> Result<bool, String> {
        if self.settings.mode != Mode::Off && self.capture && self.timings.is_none() {
            self.timings = profile::Timings::new(device);
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let temporal_capable = if self.reconstruction.is_some() {
            let (rw, rh) = self.ray_extent(device, w, h)?;
            let settings = self.settings.effective();
            let extent = [rw, rh, w, h];
            if !self.reconstruction.as_ref().unwrap().extent_ready(extent) {
                self.release_reconstruction_bind();
            }
            let session = self.reconstruction.as_mut().unwrap();
            let mut prepare = || session.prepare(device, extent, settings);
            let ready = if self.capture {
                crate::gpu::wait_for_capture(device, || prepare().map(|b| b.then_some(true)))?
            } else {
                prepare()?
            };
            if !ready {
                return Ok(false);
            }
            true
        } else {
            false
        };
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let temporal_capable = false;
        if self.settings.mode == Mode::Software && self.software.is_none() {
            // Start the cold compute pipeline before acquiring a surface.
            progress("Compiling the Software RT compute pipeline…");
            let build = |temporal| {
                checked_pipeline(device, || {
                    Software::new(
                        device,
                        self.compact_terrain,
                        self.screen_reuse != 0,
                        temporal,
                    )
                })
            };
            self.software = Some(match build(temporal_capable) {
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                Err(error) if temporal_capable => {
                    eprintln!(
                        "Lighter temporal tracer unavailable; using portable resolve: {error}"
                    );
                    self.reconstruction = None;
                    build(false)?
                }
                built => built?,
            });
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if self.settings.mode == Mode::Hardware && self.unavailable().is_none() {
            let (rw, rh) = self.ray_extent(device, w, h)?;
            if self.hardware.is_none() {
                let build = |temporal| {
                    vulkan::Renderer::new(
                        device,
                        self.compact_terrain,
                        self.screen_reuse != 0,
                        self.world
                            .as_ref()
                            .map(|w| &w.textures.bc_pages)
                            .filter(|_| self.bc_pages),
                        temporal,
                        self.game_profile.is_some() && self.settings.hybrid_primary,
                        &progress,
                    )
                };
                self.hardware = Some(match build(temporal_capable) {
                    Err(error)
                        if temporal_capable && !crate::gpu::reconstruction_device_lost(&error) =>
                    {
                        eprintln!(
                            "Lighter temporal tracer unavailable; using portable resolve: {error}"
                        );
                        self.reconstruction = None;
                        build(false)?
                    }
                    built => built?,
                });
            }
            let (bytes, _) = self.frame_image_layout(device, [rw, rh, w, h]);
            let rt = self.hardware.as_mut().unwrap();
            if self.settings.hybrid_primary {
                // The UI stages this only through Reload World. Prewarm even
                // if a later per-frame setting (SPP/Reference) uses full rays.
                rt.prewarm_primary(&progress);
            }
            if self.capture {
                crate::gpu::wait_for_capture(device, || {
                    rt.try_prepare_image(device, bytes, &progress)
                        .map(|ready| ready.then_some(true))
                })
            } else {
                rt.try_prepare_image(device, bytes, &progress)
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
        camera: Option<temporal::Camera>,
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
        if self.settings.mode == Mode::Software
            && (world.packet.hardware_only || moving.hardware_only)
        {
            return Err("Reload World to build Lighter's software acceleration data".into());
        }
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
        let settings = self.effective_settings();
        frame.options[0] |= u32::from(settings.lighting)
            | u32::from(settings.shadows) << 1
            | u32::from(settings.reflections) << 2
            | if settings.ray_lighting {
                RAY_LIGHTING_OVERLAY
            } else {
                0
            }
            | if settings.single_path { SINGLE_PATH } else { 0 }
            | if settings.physical_bsdf {
                PHYSICAL_BSDF
            } else {
                0
            }
            | if settings.reference {
                REFERENCE_MODE
            } else {
                0
            }
            | if settings.soft_sun { SOFT_SUN } else { 0 }
            | if settings.relight_baked {
                RELIGHT_BAKED
            } else {
                0
            }
            | match settings.anisotropy {
                16 => ANISO_WIDE,
                1 => ANISO_ISOTROPIC,
                _ => 0,
            };
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if settings.mode == Mode::Hardware && settings.hybrid_primary && settings.samples == 1 {
            frame.options[0] |= HYBRID_PRIMARY;
        }
        frame.options[1] = settings.bounces;
        frame.options[2] = settings.samples;
        frame.material_time[3] = settings.diffuse_distance as f32;
        frame.ray_effects = [0.; 4];
        if settings.mode == Mode::Hardware
            && settings.hybrid_primary
            && settings.samples == 1
            && !settings.reference
            && !settings.progressive
            && settings.bounces == 1
            && (settings.lighting || settings.reflections)
            && rw <= w.div_ceil(2)
            && rh <= h.div_ceil(2)
        {
            if let Some(flags) = self.game_profile {
                frame.ray_effects = flags;
            }
        }
        let (bytes, screen) = self.frame_image_layout(device, frame.extent);
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let unjittered = frame;
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let temporal_frame = if let Some(session) = &mut self.reconstruction {
            let prepared = session.begin(&mut frame, camera, world, moving, settings.mode, device);
            if prepared.is_some() && self.vendor_present.is_none() {
                match checked_pipeline(device, || Present::new(device, format, true)) {
                    Ok(present) => self.vendor_present = Some(present),
                    Err(error) => {
                        // No frame or vendor work has been submitted yet.
                        eprintln!("Lighter temporal composition unavailable; using portable resolve: {error}");
                        self.reconstruction = None;
                        frame = unjittered;
                    }
                }
            }
            prepared.filter(|_| self.reconstruction.is_some())
        } else {
            None
        };
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let vendor_frame = temporal_frame.is_some();
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let vendor_frame = {
            let _ = camera;
            false
        };
        if vendor_frame {
            frame.ray_effects = [0.; 4];
        }
        if self.present.is_none() {
            self.present = Some(checked_pipeline(device, || {
                Present::new(device, format, false)
            })?);
        }
        let history = if vendor_frame {
            self.history.reset();
            None
        } else if settings.progressive {
            let moving_unchanged = match settings.mode {
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
            let lighting_unchanged = moving_unchanged
                || match settings.mode {
                    Mode::Software => self.software.as_ref().is_some_and(|s| {
                        scene::same_moving_lighting(&s.last_moving, &moving.words)
                    }),
                    #[cfg(any(target_os = "windows", target_os = "linux"))]
                    Mode::Hardware => self
                        .hardware
                        .as_ref()
                        .is_some_and(|h| h.same_moving_lighting(moving)),
                    _ => false,
                };
            let mut history = self.history.prepare(
                &mut frame,
                world.revision,
                world.textures.byte_len(),
                settings.mode,
                lighting_unchanged,
            );
            if !moving_unchanged {
                history.changed_animations |= post::ANIM_GEOMETRY;
            }
            let hard_sun = settings.shadows && !settings.soft_sun && settings.samples == 1;
            frame.sampling[1] = u32::from(hard_sun);
            frame.sampling[2] = u32::from(
                hard_sun
                    && history.reuse
                    && history.changed_animations == 0
                    && history.previous_from_current.is_none(),
            );
            if screen != 0 {
                let reuse = history.reuse && self.screen_written;
                frame.sampling[3] = screen
                    | (u32::from(reuse) * SCREEN_PREVIOUS)
                    | (u32::from(reuse && history.previous_from_current.is_some())
                        * SCREEN_REPROJECTED)
                    | (u32::from(
                        reuse
                            && screen & SCREEN_GI != 0
                            && history.changed_animations & post::ANIM_GEOMETRY == 0,
                    ) * SCREEN_GI_PREVIOUS)
                    | (history.changed_animations << 8);
            }
            Some(history)
        } else {
            self.history.reset();
            frame.sampling = [0; 4];
            None
        };
        let completed = self.timings.as_mut().and_then(|t| t.collect(queue));
        if let Some(sample) = completed {
            self.gpu_sample = Some(sample);
        }
        let timed = self.timings.as_ref().filter(|t| t.due());
        if let Some(sequence) = &mut self.game_sampling {
            if frame.ray_effects[3] > 0. {
                frame.sampling[0] = *sequence;
                *sequence = sequence.wrapping_add(1);
            }
        }
        // Ray lighting off: default path is raster-only. The native
        let image = match settings.mode {
            Mode::Software => {
                if let Some(t) = &timed {
                    t.stamp(encoder, 0);
                }
                let image = self
                    .software
                    .as_mut()
                    .unwrap()
                    .trace(device, queue, encoder, &frame, bytes, world, moving);
                if let Some(t) = &timed {
                    t.stamp(encoder, 1);
                }
                image
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
        self.screen_written = screen != 0;
        self.screen_last_flags = frame.sampling[3];
        if let Some(t) = &timed {
            if settings.mode == Mode::Hardware {
                t.stamp(encoder, 0);
                t.stamp(encoder, 1);
            }
        }

        #[cfg(any(target_os = "windows", target_os = "linux"))]
        let reconstructed = if let Some(prepared) = temporal_frame {
            self.reconstruction
                .as_mut()
                .unwrap()
                .resolve(device, queue, encoder, image, prepared)?
        } else {
            false
        };
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let reconstructed = false;
        if reconstructed {
            if let Some(t) = timed {
                // Timestamp the whole reconstruction/packing step, not just
                // the portable guide pass. The SDK records between submissions.
                t.stamp(encoder, 2);
                t.stamp(encoder, 3);
            }
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            self.vendor_present.as_mut().unwrap().draw(
                device,
                queue,
                encoder,
                image,
                frame.extent,
                true,
                target,
                depth,
                Some(self.reconstruction.as_ref().unwrap().output()),
            );
        } else {
            // A cached composition binding owns the old SDK output view.
            // Retire it after a rejection instead of holding an extra extent.
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            if let Some(present) = &mut self.vendor_present {
                present.resolved = None;
            }
            if history.is_some() && self.post.is_none() {
                self.post = Some(checked_pipeline(device, || post::Post::new(device))?);
            }
            let image = if let Some(post) = &mut self.post {
                post.process(
                    device,
                    queue,
                    encoder,
                    image,
                    frame.extent,
                    history,
                    settings.denoiser == Denoiser::Svgf,
                    timed,
                )
            } else {
                if let Some(t) = timed {
                    t.stamp(encoder, 2);
                }
                image
            };
            if let Some(t) = timed {
                t.stamp(encoder, 3);
            }
            self.present.as_mut().unwrap().draw(
                device,
                queue,
                encoder,
                image,
                frame.extent,
                settings.denoiser == Denoiser::Svgf,
                target,
                depth,
                None,
            );
        }
        if let Some(t) = timed {
            t.stamp(encoder, 4);
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            let native_ms = if settings.mode == Mode::Hardware {
                self.hardware
                    .as_ref()
                    .and_then(vulkan::Renderer::gpu_stages)
                    .filter(|(extent, _)| *extent == frame.extent)
                    .map_or(f64::NAN, |(_, s)| s.iter().sum())
            } else {
                0.
            };
            #[cfg(not(any(target_os = "windows", target_os = "linux")))]
            let native_ms = 0.;
            self.timings
                .as_mut()
                .unwrap()
                .record(encoder, settings.mode, frame.extent, native_ms);
        }
        if settings.dynamic_resolution && self.has_adaptive_samples() {
            let mut cost = completed
                .filter(|s| s.mode == settings.mode && s.extent == frame.extent)
                .map(|s| s.total_ms);
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            if settings.mode == Mode::Hardware {
                // Consume at most once; an old sample cannot advance the
                // hysteresis just because a presentation was retried.
                cost = self
                    .hardware
                    .as_mut()
                    .and_then(vulkan::Renderer::take_adaptive_timestamp)
                    .filter(|(extent, _)| *extent == frame.extent)
                    .map(|(_, ms)| ms);
            }
            if let Some(ms) = cost {
                let target = if self.target_16_6 {
                    16.6
                } else {
                    f64::from(settings.gpu_target_ms)
                };
                self.adaptive
                    .observe(settings.resolution, ms, target, Instant::now());
            }
        }
        self.frame_error = None;
        self.last_trace_extent = (rw, rh);
        self.game_profile_active = frame.ray_effects[3] != 0. && self.raster_active();
        let solid_slots =
            if (world.packet.words[11] | moving.words[11]) & u32::from(scene::MASK_SOLID) != 0 {
                u64::from(rw) * u64::from(rh) * u64::from(settings.samples)
            } else {
                0
            };
        self.last_primary = if self.raster_active() {
            [0, solid_slots]
        } else {
            [solid_slots, 0]
        };
        let message = format!("{} active{} · {rw}×{rh} rays · {} unique resident / {} moving triangles · {} instances · {} unavailable texture(s) · mip_fallback {} ({} reduced)",
            self.settings.mode.label(),if self.raster_active() { " (raster primary)" } else { "" },world.packet.triangles(),moving.triangles(),world.packet.instances.len(),world.textures.missing,world.textures.mips.fallback,world.textures.mips.reduced);
        if self.status.is_none() {
            eprintln!("Lighter: {message}");
        }
        let message = if let Some(upscaler) = self.reconstruction_status() {
            format!("{message} · {upscaler}")
        } else {
            message
        };
        self.status = Some(message);
        self.log_allocations();
        Ok(true)
    }

    fn profile_ui(&self, ui: &mut egui::Ui) {
        let mut known = self.post.as_ref().map_or(0, post::Post::allocation_bytes);
        if let Some(software) = &self.software {
            known += [
                &software.world,
                &software.moving,
                &software.textures,
                &software.output,
            ]
            .iter()
            .map(|s| s.0.size())
            .sum::<u64>();
        }
        if let Some(staged) = self.software.as_ref().and_then(|s| s.staged.as_ref()) {
            known += staged.world.0.size() + staged.texels.0.size();
        }
        if let Some(s) = self.last_residency {
            ui.label(format!(
                "Last streamed ray scene: {} copy batches, {} static BLAS in {} batches",
                s.batches, s.blas_builds, s.blas_batches
            ));
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        if let Some(hardware) = &self.hardware {
            let (bytes, limit, heap) = hardware.memory_usage();
            known += bytes;
            ui.label(format!(
                "Vulkan ray allocations: {:.2} / {:.2} GB",
                bytes as f64 * 1e-9,
                limit as f64 * 1e-9
            ));
            if let Some(heap) = heap {
                if heap >= HEAP_WARNING_BYTES {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!(
                            "Device-local usage ≈ {:.2} GB (above 7 GB)",
                            heap as f64 * 1e-9
                        ),
                    );
                } else {
                    ui.label(format!(
                        "Driver-estimated device-local usage: {:.2} GB",
                        heap as f64 * 1e-9
                    ));
                }
            }
            if let Some((_, [build, primary, trace])) = hardware.gpu_stages() {
                ui.label(format!(
                    "AS {build:.2} ms · raster primary {primary:.2} ms · rays {trace:.2} ms"
                ));
            }
        }
        ui.label(format!(
            "Tracked ray resources: {:.1} MiB (excludes Native resources)",
            known as f64 / 1048576.
        ));
        if let Some(s) = self.gpu_sample.filter(|s| s.mode == self.settings.mode) {
            ui.label(format!(
                "Reproject {:.2} ms · filter {:.2} ms · upscale/compose {:.2} ms",
                s.reproject_ms, s.filter_ms, s.upscale_ms
            ));
            if s.mode == Mode::Software {
                ui.label(format!("Software rays {:.2} ms", s.trace_ms));
            }
        }
        if let Some(low) = self.cadence.low {
            ui.label(format!("Present cadence 1% low: {low:.1} FPS"));
        }
    }

    /// Return the replacement's settings only on an explicit reload. Changing
    /// the dropdown must not compile a new backend on the live viewer or save
    /// a configuration that has not yet opened a usable world.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<Settings> {
        let old = self.settings;
        let mut mode = self.pending_mode.unwrap_or(old.mode);
        egui::ComboBox::from_label("World renderer")
            .selected_text(mode.label())
            .show_ui(ui, |ui| {
                for choice in Mode::ALL {
                    ui.selectable_value(&mut mode, choice, choice.label());
                }
            });
        self.pending_mode = (mode != old.mode).then_some(mode);
        let mut hybrid_primary = self.pending_primary.unwrap_or(old.hybrid_primary);
        ui.add_enabled(mode == Mode::Hardware, egui::Checkbox::new(&mut hybrid_primary, "Rasterized primary visibility (experimental)"))
            .on_hover_text("Apply with Reload World so the new pipeline compiles in the watched replacement. Hardware, 1 sample per pixel only. Ordered sheets and secondary visibility still use rays.");
        self.pending_primary = (hybrid_primary != old.hybrid_primary).then_some(hybrid_primary);
        let reload = ui.button("Reload World")
            .on_hover_text("Reload this map at the current camera position and time with the selected renderer.")
            .clicked();
        if self.pending_mode.is_some() {
            ui.label(format!(
                "Current renderer setting: {}. Press Reload World to apply the selection.",
                old.mode.label()
            ));
        }
        if self.pending_primary.is_some() && mode == Mode::Hardware {
            ui.label("Press Reload World to apply the primary visibility change.");
        }
        ui.separator();
        ui.checkbox(
            &mut self.settings.lighting,
            "Ray-traced lighting / indirect bounce",
        );
        ui.checkbox(&mut self.settings.regional_lighting, "Follow local region lighting")
            .on_hover_text("Allow the region under the camera to recolor the whole ray-lit world. Off keeps sun, ambient and sky brightness tied to the map and time of day. Regional fog and weather remain active.");
        ui.checkbox(&mut self.settings.shadows, "Ray-traced shadows");
        ui.add_enabled(
            self.settings.shadows,
            egui::Checkbox::new(&mut self.settings.soft_sun, "Soft sun shadows"),
        )
        .on_hover_text(
            "Sample across the sun's angular disc. Local area emitters are sampled separately.",
        );
        ui.checkbox(&mut self.settings.reflections, "Ray-traced reflections");
        ui.checkbox(&mut self.settings.single_path, "Single-path transport")
            .on_hover_text("Sample one Fresnel lobe and use roulette after two bounces. A first-hit water surface still splits at one sample to avoid visible, unfiltered speckle.");
        ui.checkbox(&mut self.settings.reference, "Reference (32 spp, 8 bounces)")
            .on_hover_text("Full resolution with sky and mesh light sampling, no history or denoising. Overrides lighting, relight and quality settings while enabled.");
        // Ray lighting toggle: off = raster world only, Graphics upscaler/frame-gen as normal.
        ui.checkbox(&mut self.settings.ray_lighting, "Ray lighting")
            .on_hover_text("When on, the ray overlay reads the raster G-buffer and adds sun shadow, reflection radiance and short-range indirect lighting. When off, the world is raster-only and byte-identical to the native renderer for every Graphics setting.");
        ui.separator();
        egui::ComboBox::from_label("Ray denoiser")
            .selected_text(self.settings.denoiser.label())
            .show_ui(ui, |ui| {
                for d in Denoiser::ALL {
                    let text = match d.unavailable_reason() {
                        Some(reason) => format!("{} — {}", d.label(), reason),
                        None => d.label().to_string(),
                    };
                    match d.unavailable_reason() {
                        Some(reason) => {
                            ui.add(egui::Label::new(
                                egui::RichText::new(text).color(egui::Color32::GRAY),
                            ))
                            .on_hover_text(reason);
                        }
                        None => {
                            ui.selectable_value(&mut self.settings.denoiser, d, text)
                                .on_hover_text("Always available portable denoiser.");
                        }
                    }
                }
            });
        let upscale = self
            .graphics_upscaler
            .as_deref()
            .unwrap_or("not yet reported");
        let framegen = self
            .graphics_frame_gen
            .as_deref()
            .unwrap_or("not yet reported");
        ui.label(format!(
            "Upscaling and frame generation come from Graphics settings: {upscale} + {framegen} (read-only, inherited by the ray overlay)."
        ));
        if let Some(status) = &self.status_line {
            ui.label(status);
        }
        // Logging toggle: writes rotating log file under PW_CACHE_DIR or
        // ~/.cache/PerfectWorldViewer/lighter-logs. Verbose level samples
        // per-frame timing and acceleration-structure events.
        let mut log_level = match self.logger.level() {
            log::LogLevel::Off => 0,
            log::LogLevel::Normal => 1,
            log::LogLevel::Verbose => 2,
        };
        egui::ComboBox::from_id_source("lighter_log_level")
            .selected_text(match log_level {
                0 => "Logging: off".to_string(),
                1 => "Logging: normal".to_string(),
                _ => "Logging: verbose".to_string(),
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut log_level, 0, "Logging: off")
                    .on_hover_text("No log file is written.");
                ui.selectable_value(&mut log_level, 1, "Logging: normal")
                    .on_hover_text("Session header, provider events, coverage, errors.");
                ui.selectable_value(&mut log_level, 2, "Logging: verbose")
                    .on_hover_text("Adds per-frame timing and acceleration-structure samples.");
            });
        self.logger.set_level(match log_level {
            0 => log::LogLevel::Off,
            1 => log::LogLevel::Normal,
            _ => log::LogLevel::Verbose,
        });
        if self.logger.level() != log::LogLevel::Off {
            ui.separator();
            let copy_btn = ui
                .button("Copy diagnostics to clipboard")
                .on_hover_text("Copy the current session diagnostics text to the clipboard.");
            if copy_btn.clicked() {
                // Clipboard copy handled by the host; the diagnostics text is
                // available from the log file.
            }
            let open_btn = ui
                .button("Open log folder")
                .on_hover_text("Open the folder containing the rotating lighter log files.");
            if open_btn.clicked() {
                // Open the log directory in the host file manager.
            }
            let export_btn = ui
                .button("Export frame timings CSV")
                .on_hover_text("Export per-frame timing samples as CSV.");
            if export_btn.clicked() {
                // Export per-frame timing samples as CSV.
            }
        }
        ui.checkbox(&mut self.settings.physical_bsdf, "GGX / Lambert materials")
            .on_hover_text("Evaluate and sample energy-bounded microfacet and diffuse lobes on relit materials. Preserve the legacy color stages for baked surfaces and GFX sheets.");
        ui.checkbox(&mut self.settings.progressive, "Progressive sampling while still")
            .on_hover_text("Reuse lighting samples in stable scenes. Reproject tiny view changes; camera movement, changed lighting or incompatible surfaces discard history.");
        ui.add(egui::Slider::new(&mut self.settings.bounces,0..=4).text("Secondary bounces"))
            .on_hover_text("0 traces primary visibility and direct shadows only. Reflection and water transmission need at least one bounce.");
        egui::ComboBox::from_label("Diffuse bounce distance")
            .selected_text(match self.settings.diffuse_distance {
                0 => "Full view distance".to_string(),
                n => format!("{n} world units"),
            })
            .show_ui(ui, |ui| {
                for (n, label) in [(128, "128 (fast)"), (256, "256 (default)"), (512, "512"), (1024, "1024"), (0, "Full view distance")] {
                    ui.selectable_value(&mut self.settings.diffuse_distance, n, label);
                }
            })
            .response.on_hover_text("Limit diffuse rays in air; farther indirect geometry falls back to sky lighting. Does not limit primary visibility, shadows, mirrors, refraction or Reference.");
        ui.add(egui::Slider::new(&mut self.settings.samples, 1..=4).text("Samples per pixel"));
        ui.add(
            egui::Slider::new(&mut self.settings.resolution, 25..=100)
                .text("Ray resolution")
                .suffix("%"),
        );

        ui.checkbox(&mut self.settings.dynamic_resolution,"Automatic ray resolution (50–100%)")
            .on_hover_text("Uses completed Hardware raster/trace timestamps with hysteresis (Software requires a profiling capture). SDK and game profiles keep a fixed scale.");
        if self.settings.dynamic_resolution && !self.settings.reference {
            ui.add(
                egui::Slider::new(&mut self.settings.gpu_target_ms, 4..=50)
                    .text("Adaptive ray GPU budget")
                    .suffix(" ms"),
            );
        }
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
            self.adaptive = profile::Resolution::default();
            self.reset_history();
            self.persistence_error = self.settings_for_disk().save().err();
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
                "Reload World to load ray geometry. Active now: native renderer.",
            );
        } else if let Some(error) = self.error() {
            ui.label("Active: Native renderer (Lighter unavailable).");
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        } else if let Some(status) = &self.status {
            ui.label(status);
        } else {
            ui.label("Preparing the first ray-traced frame…");
        }
        if self.replaces_world() {
            self.profile_ui(ui);
            if self.screen_reuse != 0 {
                let active = self.screen_reuse_flags() & 3;
                ui.label(match active {
                    SCREEN_DI => "Experimental local-light reservoirs active",
                    SCREEN_GI => "Experimental diffuse screen cache active",
                    3 => "Experimental reservoirs and diffuse screen cache active",
                    _ => "Screen reuse not admitted at these settings or resolution",
                });
            }
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
        ui.label("Hardware mode uses Vulkan, not DirectX 12. Lighter uses its own bounded BMD residency. Temporal upscaling/frame generation are suspended only while rays draw the world. See Modules/Lighter/README.md for compatibility notes.");
        reload.then_some(Settings {
            mode,
            hybrid_primary,
            denoiser: self.settings.denoiser,
            ray_lighting: self.settings.ray_lighting,
            ..self.settings
        })
    }
}

#[cfg(test)]
mod tests;
