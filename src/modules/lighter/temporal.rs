//! Ray-resolution inputs for temporal reconstruction. A position in the
//! previous camera comes from the previous *placed pose*, not the Native
//! motion attachment or a world point assumed stationary behind a moving hit.
use super::{
    binding, post, resource,
    scene::{moving_lighting_key, Fingerprint, MeshKey, MotionOwner, Packet, VERTEX_WORDS},
    Frame, Mode, Storage, PIXELS,
};
use crate::fsr::CameraLens;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3, Vec4};
use std::collections::{HashMap, HashSet};

pub(super) const INPUT: u32 = 1 << 17;
pub(super) const RR: u32 = 1 << 18;
pub(super) const TRACE: &str = include_str!("temporal_trace.wgsl");
pub(super) const PACK: &str = include_str!("temporal_inputs.wgsl");
pub(super) const TRACE_OFF: &str = r#"
fn temporal_begin() {}
fn temporal_primary(s: Surface, hit: Hit, ray: Ray, p: vec3<f32>, medium: u32) {}
fn temporal_specular_block() {}
fn temporal_specular_leg(specular: bool) {}
fn temporal_specular_hit(ray: Ray, hit: Hit) {}
fn temporal_end(at: u32) {}
"#;

const MOTION_BYTES: usize = 16 << 20;
const PLACEMENT_WORDS: usize = 16;
// Includes the draw lookup entry and headroom for table load factor. The
// previous position payload and lookup share one logical admission budget.
const DRAW_BYTES: usize = 192;
const MAX_MOTION_DRAWS: usize = 16384;

#[derive(Clone, Copy)]
pub(crate) struct Camera {
    pub view: Mat4,
    pub projection: Mat4,
    pub lens: CameraLens,
    pub frame_ms: f32,
}

#[derive(Clone, Copy)]
struct Pose {
    mesh: MeshKey,
    positions: u32,
    vertices: u32,
    rows: [f32; 12],
}

#[derive(Default)]
struct Poses {
    // Packed XYZ (no padded W). Instances sharing a mesh share its positions.
    positions: Vec<u32>,
    placements: HashMap<MotionOwner, Pose>,
}

fn unique_draws(packet: &Packet) -> impl Iterator<Item = (u32, MotionOwner)> + '_ {
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for &(_, key) in &packet.motion {
        if !seen.insert(key) {
            duplicates.insert(key);
        }
    }
    packet
        .motion
        .iter()
        .copied()
        .filter(move |(_, key)| !duplicates.contains(key))
}

impl Poses {
    fn capture(packet: &Packet, limit: usize) -> Self {
        let mut current = Self::default();
        let draws = packet.motion.len();
        if draws > MAX_MOTION_DRAWS || draws * DRAW_BYTES > limit {
            return current;
        }
        let vertex_limit = limit - draws * DRAW_BYTES;
        let mut meshes = HashMap::new();
        let vertex_start = packet.words[0] as usize;
        for (instance, owner) in unique_draws(packet) {
            let instance = &packet.instances[instance as usize];
            let mesh = &packet.meshes[instance.mesh as usize];
            let vertices = mesh.vertex.end - mesh.vertex.start;
            let base = *meshes.entry(instance.mesh).or_insert_with(|| {
                let start = current.positions.len();
                if (start + vertices as usize * 3) * 4 > vertex_limit {
                    return None;
                }
                for i in mesh.vertex.clone() {
                    let v = vertex_start + i as usize * VERTEX_WORDS as usize;
                    current.positions.extend_from_slice(&packet.words[v..v + 3]);
                }
                Some(start as u32)
            });
            if let Some(positions) = base {
                current.placements.insert(
                    owner,
                    Pose {
                        mesh: mesh.key,
                        positions,
                        vertices,
                        rows: packet.placement_rows(instance),
                    },
                );
            }
        }
        current
    }

    /// Entries are indexed by *current* packet IDs. Match semantic draw
    /// identity and topology before reusing indices from this frame's hit.
    fn remap(&self, packet: &Packet, limit: usize) -> (u32, Vec<u32>) {
        let count = packet.instances.len();
        let prefix = count * PLACEMENT_WORDS;
        if self.placements.is_empty()
            || packet.motion.len() > MAX_MOTION_DRAWS
            || (prefix + self.positions.len()) * 4 > limit
        {
            return (0, vec![0; 4]);
        }
        let mut words = vec![0; prefix];
        for (id, owner) in unique_draws(packet) {
            let mesh = &packet.meshes[packet.instances[id as usize].mesh as usize];
            if let Some(old) = self
                .placements
                .get(&owner)
                .filter(|old| old.mesh == mesh.key)
            {
                let at = id as usize * PLACEMENT_WORDS;
                words[at] = (prefix as u32) + old.positions;
                words[at + 1] = old.vertices;
                words[at + 4..at + 16].copy_from_slice(&old.rows.map(f32::to_bits));
            }
        }
        words.extend(&self.positions);
        (count as u32, words)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Continuity {
    pub revision: u64,
    pub texture_bytes: usize,
    pub mode: Mode,
    pub rr: bool,
}

#[derive(Clone, Copy)]
struct Signals {
    // Check large discontinuities, not exact per-frame day/night values.
    palette: [[f32; 4]; 6],
    optical: [f32; 4],
    sky_layers: [[u32; 3]; 2],
    wave: [u32; 4],
    shore: [f32; 4],
    scroll: f32,
    sky_offset: [f32; 2],
    moving_lights: Fingerprint,
}
impl Signals {
    fn new(frame: &Frame, moving: &Packet) -> Self {
        Self {
            palette: [
                frame.sun,
                frame.ambient,
                frame.sun_color,
                frame.air_color,
                frame.water_color,
                frame.sky_tint,
            ],
            optical: [
                frame.air[2],
                frame.material_time[3],
                frame.water_effects[1] as f32,
                frame.sky_uv[2],
            ],
            sky_layers: [
                frame.sky0[..3].try_into().unwrap(),
                frame.sky1[..3].try_into().unwrap(),
            ],
            wave: frame.waves,
            shore: frame.shore_offsets,
            scroll: frame.air[3],
            sky_offset: [frame.sky_uv[0], frame.sky_uv[1]],
            moving_lights: moving_lighting_key(moving),
        }
    }
    fn continuous(&self, old: &Self) -> bool {
        self.optical == old.optical
            && self.sky_layers == old.sky_layers
            && self.moving_lights == old.moving_lights
            && self
                .palette
                .iter()
                .flatten()
                .zip(old.palette.iter().flatten())
                .all(|(&new, &old)| (new - old).abs() <= 0.05 * new.abs().max(old.abs()).max(1.))
    }
    fn animations(&self, old: &Self) -> u32 {
        u32::from(self.wave != old.wave || self.shore != old.shore) * post::ANIM_WAVES
            | u32::from(self.scroll != old.scroll) * post::ANIM_SCROLL
            | u32::from(self.sky_offset != old.sky_offset) * post::ANIM_SKY
    }
}

struct Stamp {
    key: Continuity,
    extent: [u32; 4],
    options: [u32; 4],
    lens: CameraLens,
    unjittered: Mat4,
    jittered: Mat4,
    eye: Vec3,
    poses: Poses,
    signals: Signals,
}

#[derive(Default)]
pub(super) struct History {
    previous: Option<Stamp>,
    submitted: u32,
}

#[repr(C)]
#[derive(Pod, Zeroable, Clone, Copy)]
struct Params {
    current: [[f32; 4]; 4],
    inverse_jittered: [[f32; 4]; 4],
    previous: [[f32; 4]; 4],
    previous_jittered: [[f32; 4]; 4],
    eye: [f32; 4],
    lens: [f32; 4],
    shape: [u32; 4],   // ray dimensions, previous frame exists, motion table entries
    changed: [u32; 4], // animation-dependency bits (pixels.wgsl)
}

pub(super) struct Prepared {
    stamp: Stamp,
    params: Params,
    motion: Vec<u32>,
    pub jitter: [f32; 2],
    pub reset: bool,
    pub camera: Camera,
}

// Use one clip-space translation at ray extent. Rays sample pixel centers
// through its inverse; no second Halton/sample offset in camera_ray at 1 spp.
fn jitter_projection(projection: Mat4, jitter: [f32; 2], extent: [u32; 4]) -> Mat4 {
    Mat4::from_translation(Vec3::new(
        2. * jitter[0] / extent[0] as f32,
        -2. * jitter[1] / extent[1] as f32,
        0.,
    )) * projection
}

fn halton(mut i: u32, base: u32) -> f32 {
    let mut weight = 1.;
    let mut sum = 0.;
    while i != 0 {
        weight /= base as f32;
        sum += weight * (i % base) as f32;
        i /= base;
    }
    sum
}

impl History {
    pub(super) fn reset(&mut self) {
        self.previous = None;
    }

    // No state is changed on preparation alone. A deferred or failed draw
    // cannot become the source of the next frame's reprojection.
    pub(super) fn prepare(
        &self,
        frame: &mut Frame,
        camera: Camera,
        key: Continuity,
        moving: &Packet,
        storage_limit: usize,
    ) -> Option<Prepared> {
        let Camera {
            view,
            projection,
            lens,
            ..
        } = camera;
        let [w, h, dw, dh] = frame.extent;
        if frame.options[2] != 1
            || [w, h, dw, dh].contains(&0)
            || !(lens.near > 0.
                && lens.far > lens.near
                && lens.far.is_finite()
                && lens.fov_y_radians > 0.
                && lens.fov_y_radians < std::f32::consts::PI)
        {
            return None;
        }
        let eye = Vec4::from_array(frame.eye).truncate();
        let unjittered = projection * view;
        let inverse = unjittered.inverse();
        if !inverse.is_finite() || !eye.is_finite() {
            return None;
        }
        let signals = Signals::new(frame, moving);
        let prior = self.previous.as_ref().filter(|p| {
            p.key == key
                && p.extent == frame.extent
                && p.options == frame.options
                && p.lens == lens
                && signals.continuous(&p.signals)
                && (p.unjittered == unjittered
                    || post::camera_continuous(p.unjittered * inverse, eye - p.eye))
        });
        let phase = if key.rr {
            64
        } else {
            let scale = dw as f32 / w as f32;
            (8. * scale * scale) as u32
        };
        let sample = self.submitted % phase.clamp(8, 144) + 1;
        let jitter = [halton(sample, 2) - 0.5, halton(sample, 3) - 0.5];
        let jittered_projection = jitter_projection(projection, jitter, frame.extent);
        let jittered = jittered_projection * view;
        let inverse = jittered.inverse();
        if !inverse.is_finite() {
            return None;
        }
        let options = frame.options;
        let limit = storage_limit.min(MOTION_BYTES);
        let (entries, motion) =
            prior.map_or_else(|| (0, vec![0; 4]), |p| p.poses.remap(moving, limit));
        let matrices = |m: Mat4| m.to_cols_array_2d();
        let params = Params {
            current: matrices(unjittered),
            inverse_jittered: matrices(inverse),
            previous: matrices(prior.map_or(unjittered, |p| p.unjittered)),
            previous_jittered: matrices(prior.map_or(jittered, |p| p.jittered)),
            eye: frame.eye,
            lens: [lens.near, lens.far, 0., 0.],
            shape: [w, h, u32::from(prior.is_some()), entries],
            changed: [prior.map_or(0, |p| signals.animations(&p.signals)), 0, 0, 0],
        };
        frame.vp = matrices(jittered);
        frame.inv_vp = params.inverse_jittered;
        frame.options[0] |= INPUT | (u32::from(key.rr) * RR);
        // Rotate only transport noise. Screen DI/GI and the built-in history
        // do not own any samples in the vendor route.
        frame.sampling = [self.submitted.wrapping_add(1), 0, 0, 0];
        Some(Prepared {
            stamp: Stamp {
                key,
                extent: frame.extent,
                options,
                lens,
                unjittered,
                jittered,
                eye,
                poses: Poses::capture(moving, limit),
                signals,
            },
            params,
            motion,
            jitter,
            reset: prior.is_none(),
            camera,
        })
    }

    pub(super) fn commit(&mut self, prepared: Prepared) {
        self.previous = Some(prepared.stamp);
        self.submitted = self.submitted.wrapping_add(1);
    }
}

pub(super) struct Plane {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

impl Plane {
    pub(super) fn new(
        device: &wgpu::Device,
        label: &'static str,
        [width, height]: [u32; 2],
        format: wgpu::TextureFormat,
        extra: wgpu::TextureUsages,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | extra,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self { texture, view }
    }
}

pub(super) struct Inputs {
    pub color: Plane,
    pub velocity: Plane,
    pub depth: Plane,
    pub mask: Plane,
    // A tightly packed [depth, rejection] pair per ray. Keep irregular
    // reprojection reads out of sampled images (including in CPU Vulkan).
    history: [Storage; 2],
    pub fsr_masks: Option<[Plane; 2]>, // reactive, transparency/composition (not RR responsivity)
    pub guides: Option<[Plane; 4]>,    // diffuse, specular, normal/roughness, specular distance
    page: usize,
    params: wgpu::Buffer,
    motion: Storage,
    pack: wgpu::ComputePipeline,
    pack_guides: Option<wgpu::ComputePipeline>,
    pack_masks: Option<wgpu::ComputePipeline>,
    bindings: [Option<PackBind>; 2],
    guide_bind: Option<(wgpu::Id<wgpu::Buffer>, wgpu::BindGroup)>,
    mask_bind: Option<(wgpu::Id<wgpu::Buffer>, wgpu::BindGroup)>,
    extent: [u32; 2],
}

struct PackBind {
    image: wgpu::Id<wgpu::Buffer>,
    motion: wgpu::Id<wgpu::Buffer>,
    bind: wgpu::BindGroup,
}

pub(super) fn sampled(i: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: i,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub(super) fn texture(i: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding: i,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

impl Inputs {
    pub(super) fn new(device: &wgpu::Device, extent: [u32; 2], rr: bool) -> Self {
        use wgpu::{BufferBindingType as B, ShaderStages as S, TextureFormat as F};
        let read = B::Storage { read_only: true };
        let storage = |i, format| wgpu::BindGroupLayoutEntry {
            binding: i,
            visibility: S::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Lighter temporal ray inputs"),
            source: wgpu::ShaderSource::Wgsl([PIXELS, PACK].concat().into()),
        });
        let pipeline = |name, entries: &[wgpu::BindGroupLayoutEntry]| {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(name),
                entries,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(name),
                        bind_group_layouts: &[&layout],
                        push_constant_ranges: &[],
                    }),
                ),
                module: &shader,
                entry_point: name,
            })
        };
        let pack = pipeline(
            "pack_inputs",
            &[
                binding(0, S::COMPUTE, B::Uniform),
                binding(1, S::COMPUTE, read),
                binding(2, S::COMPUTE, read),
                binding(3, S::COMPUTE, read),
                binding(4, S::COMPUTE, B::Storage { read_only: false }),
                storage(5, F::Rgba16Float),
                storage(6, F::R32Float),
                storage(7, F::Rg32Float),
                storage(8, F::R32Float),
            ],
        );
        let pack_guides = rr.then(|| {
            pipeline(
                "pack_guides",
                &[
                    binding(0, S::COMPUTE, B::Uniform),
                    binding(1, S::COMPUTE, read),
                    storage(9, F::Rgba16Float),
                    storage(10, F::Rgba16Float),
                    storage(11, F::Rgba16Float),
                    storage(12, F::R32Float),
                ],
            )
        });
        let pack_masks = (!rr).then(|| {
            pipeline(
                "pack_fsr_masks",
                &[
                    binding(0, S::COMPUTE, B::Uniform),
                    binding(1, S::COMPUTE, read),
                    sampled(13, S::COMPUTE),
                    storage(14, F::R32Float),
                    storage(15, F::R32Float),
                ],
            )
        });
        let make = |name, format| {
            Plane::new(
                device,
                name,
                extent,
                format,
                if cfg!(test) {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            )
        };
        Self {
            color: make("Raw linear ray radiance", F::Rgba16Float),
            velocity: make("Ray motion (render pixels)", F::Rg32Float),
            depth: make("Ray reverse-Z", F::R32Float),
            mask: make("Ray history rejection", F::R32Float),
            history: std::array::from_fn(|_| {
                Storage::new(
                    device,
                    "Ray history depth and validity",
                    u64::from(extent[0]) * u64::from(extent[1]) * 8,
                    wgpu::BufferUsages::empty(),
                )
            }),
            fsr_masks: (!rr).then(|| {
                [
                    make("FSR reactive", F::R32Float),
                    make("FSR transparency/composition", F::R32Float),
                ]
            }),
            guides: rr.then(|| {
                [
                    make("RR diffuse albedo", F::Rgba16Float),
                    make("RR specular reflectivity", F::Rgba16Float),
                    make("RR world normal and roughness", F::Rgba16Float),
                    make("RR specular hit distance", F::R32Float),
                ]
            }),
            page: 0,
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Ray temporal cameras"),
                size: std::mem::size_of::<Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            motion: Storage::new(
                device,
                "Previous placed ray vertices",
                16,
                wgpu::BufferUsages::COPY_DST,
            ),
            pack,
            pack_guides,
            pack_masks,
            bindings: [None, None],
            guide_bind: None,
            mask_bind: None,
            extent,
        }
    }

    pub(super) fn page(&self) -> usize {
        self.page
    }
    pub(super) fn advance(&mut self) {
        self.page ^= 1;
    }

    pub(super) fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        image: &wgpu::Buffer,
        prepared: &Prepared,
    ) {
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&prepared.params));
        self.motion.upload(
            device,
            queue,
            "Previous placed ray vertices",
            bytemuck::cast_slice(&prepared.motion),
            0,
        );
        let page = self.page;
        let prev = page ^ 1;
        let id = image.global_id();
        let motion_id = self.motion.0.global_id();
        if self.bindings[page]
            .as_ref()
            .is_none_or(|b| b.image != id || b.motion != motion_id)
        {
            self.bindings[page] = Some(PackBind {
                image: id,
                motion: motion_id,
                bind: device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Lighter temporal inputs"),
                    layout: &self.pack.get_bind_group_layout(0),
                    entries: &[
                        resource(0, &self.params),
                        resource(1, image),
                        resource(2, &self.motion.0),
                        resource(3, &self.history[prev].0),
                        resource(4, &self.history[page].0),
                        texture(5, &self.color.view),
                        texture(6, &self.depth.view),
                        texture(7, &self.velocity.view),
                        texture(8, &self.mask.view),
                    ],
                }),
            });
        }
        let dispatch = [self.extent[0].div_ceil(8), self.extent[1].div_ceil(8)];
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pack);
            pass.set_bind_group(0, &self.bindings[page].as_ref().unwrap().bind, &[]);
            pass.dispatch_workgroups(dispatch[0], dispatch[1], 1);
        }
        if let (Some(pipe), Some(masks)) = (&self.pack_masks, &self.fsr_masks) {
            let slot = &mut self.mask_bind;
            if slot.as_ref().is_none_or(|(old, _)| *old != id) {
                *slot = Some((
                    id,
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Lighter FSR masks"),
                        layout: &pipe.get_bind_group_layout(0),
                        entries: &[
                            resource(0, &self.params),
                            resource(1, image),
                            texture(13, &self.mask.view),
                            texture(14, &masks[0].view),
                            texture(15, &masks[1].view),
                        ],
                    }),
                ));
            }
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, &slot.as_ref().unwrap().1, &[]);
            pass.dispatch_workgroups(dispatch[0], dispatch[1], 1);
        }
        if let (Some(pipe), Some(g)) = (&self.pack_guides, &self.guides) {
            if self.guide_bind.as_ref().is_none_or(|(old, _)| *old != id) {
                self.guide_bind = Some((
                    id,
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("Lighter RR guides"),
                        layout: &pipe.get_bind_group_layout(0),
                        entries: &[
                            resource(0, &self.params),
                            resource(1, image),
                            texture(9, &g[0].view),
                            texture(10, &g[1].view),
                            texture(11, &g[2].view),
                            texture(12, &g[3].view),
                        ],
                    }),
                ));
            }
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, &self.guide_bind.as_ref().unwrap().1, &[]);
            pass.dispatch_workgroups(dispatch[0], dispatch[1], 1);
        }
    }
}

#[cfg(test)]
#[path = "temporal_tests.rs"]
mod tests;
