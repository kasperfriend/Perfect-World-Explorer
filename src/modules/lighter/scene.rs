//! Backend-neutral scene packet. Offsets in the header are *word* offsets.
//! The same indexed positions build the Vulkan BLAS and feed the software
//! intersector. Both backends shade the same triangle IDs and barycentrics.
pub(crate) use super::bvh::Bounds;
use super::bvh::Bvh;
use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Mat4, Vec2, Vec3};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

pub const TERRAIN: u32 = 1 << 0;
pub const BAKED: u32 = 1 << 2;
pub const CUTOUT: u32 = 1 << 3;
pub const BLEND: u32 = 1 << 4;
pub const EMISSIVE: u32 = 1 << 5;
pub const ADDITIVE: u32 = 1 << 6;
pub const WATER: u32 = 1 << 7;
pub const CLAMP_UV: u32 = 1 << 8;
pub const NO_REFLECT: u32 = 1 << 9;
pub const NO_REFRACT: u32 = 1 << 10;
pub const SIMPLE_WATER: u32 = 1 << 11;
pub const CULL_BACK: u32 = 1 << 13;
pub const WRITE_DEPTH: u32 = 1 << 14;
pub const IGNORE_TEXTURE_ALPHA: u32 = 1 << 15;
pub const STRICT_ALPHA: u32 = 1 << 16;
/// The local first-person head is still visible to shadow and bounce rays.
pub const HIDE_PRIMARY: u32 = 1 << 17;
pub const SINGLE_SIDED: u32 = 1 << 18;
pub const GRASS_MATERIAL: u32 = 1 << 19;
pub const UV_TRANSFORM: u32 = 1 << 20;
pub const BAKED_COLOR_ONLY: u32 = 1 << 21;
pub const FOLIAGE: u32 = 1 << 22;
pub const PRIMARY_ONLY: u32 = 1 << 23;
pub const TREE_MATERIAL: u32 = 1 << 24;
pub const SHORE: u32 = 1 << 25;
pub const OVERLAY: u32 = 1 << 26;
pub const CLOUD: u32 = 1 << 27;
pub const PRECIPITATION: u32 = 1 << 28;
pub const ORDERED_GFX: u32 = 1 << 29;
/// Explicit optical material, never inferred from a legacy blend mode.
pub const GLASS: u32 = 1 << 30;
pub const GRASS_WIND_SLOTS: usize = 80;

// These bits also occupy the upper eight bits of a Vulkan TLAS instance.
// A mixed mesh can participate in several queries; its individual triangles
// are then tested by the shared coverage predicate.
pub const MASK_SOLID: u8 = 1;
pub const MASK_BLEND: u8 = 2;
pub const MASK_SHADOW: u8 = 4;
pub const MASK_GFX: u8 = 8;
pub const INSTANCE_WORDS: u32 = 14;
const HEADER_WORDS: usize = 16;
pub const VERTEX_WORDS: u32 = 8;
const MAX_ITEMS: usize = 8_000_000;
pub const DYNAMIC_BIT: u32 = 1 << 23;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    normal: u32,
    pub uv: [f32; 2],
    day: u32,
    night: u32,
}

impl Vertex {
    pub fn new(pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> Self {
        let n = Vec3::from_array(normal).normalize_or_zero().to_array();
        let normal = u32::from_le_bytes([
            ((n[0] * 0.5 + 0.5) * 255.).round() as u8,
            ((n[1] * 0.5 + 0.5) * 255.).round() as u8,
            ((n[2] * 0.5 + 0.5) * 255.).round() as u8,
            255,
        ]);
        Self {
            pos,
            normal,
            uv,
            day: u32::MAX,
            night: u32::MAX,
        }
    }

    /// Grass has no vertex tint. Keep the native range and per-vertex
    /// bend weight in the otherwise unused color lanes.
    pub(crate) fn grass(self, sight: u32, weight: u32) -> Self {
        self.tint(sight, weight)
    }

    pub(crate) fn grass_weight(self) -> f32 {
        f32::from_bits(self.night)
    }

    pub fn tint(mut self, day: u32, night: u32) -> Self {
        self.day = day;
        self.night = night;
        self
    }
}

pub fn rgba(v: [f32; 4]) -> u32 {
    u32::from_le_bytes(v.map(|x| (x.clamp(0., 1.) * 255.).round() as u8))
}

pub fn argb_to_rgba(v: u32) -> u32 {
    let [b, g, r, a] = v.to_le_bytes();
    u32::from_le_bytes([r, g, b, a])
}

/// Optional legacy specular metadata. This does not change any native vertex,
/// material constants or serialized scenery pages.
#[derive(Clone, Copy, Default, Debug)]
pub struct Gloss {
    reflectance: f32,
    power: f32,
    tint: u32,
}

impl Gloss {
    pub fn authored(specular: [f32; 4], power: f32) -> Self {
        let rgb = [specular[0], specular[1], specular[2]].map(|v| v.max(0.));
        let peak = rgb.into_iter().fold(0., f32::max);
        let reflectance = peak * 0.25;
        let [r, g, b] = rgb.map(|v| v / peak.max(1e-7));
        Self {
            reflectance: reflectance.min(0.35),
            tint: rgba([r, g, b, 1.]),
            power: if power.is_finite() {
                power.clamp(0., 128.)
            } else {
                0.
            },
        }
    }

    pub fn reflective(self) -> bool {
        self.reflectance > 0.
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, PartialEq, Eq, Hash)]
pub struct Surface {
    pub texture: u32,
    pub flags: u32,
    cutoff: u32,
    reflectance: u32,
    power: u32,
    specular_tint: u32,
    pub normal_map: u32,
    emission: [u32; 3],
    ambient: u32,
    uv_scale: [u32; 2],
    uv_scroll: [u32; 2],
    face_stage: [u32; 4], // mode, feature and skin colors, highlight tile
    face_gloss: u32,
    diffuse: u32,
    blend: u32,      // D3D source and destination factors (0: default ray alpha)
    glass: [u32; 2], // linear thin transmission tint, index of refraction
}
pub const SURFACE_WORDS: usize = std::mem::size_of::<Surface>() / 4;
const _: () = assert!(SURFACE_WORDS == 24);

impl Surface {
    pub fn new(texture: u32, flags: u32, cutoff: f32, reflectance: f32) -> Self {
        Self {
            texture,
            flags,
            cutoff: cutoff.clamp(0., 1.).to_bits(),
            reflectance: reflectance.clamp(0., 1.).to_bits(),
            power: (-1.0f32).to_bits(), // ideal mirror when no authored lobe
            specular_tint: u32::MAX,
            normal_map: 0,
            emission: [0; 3],
            ambient: u32::MAX,
            uv_scale: [1.0f32.to_bits(); 2],
            uv_scroll: [0; 2],
            face_stage: [0; 4],
            face_gloss: 0,
            diffuse: u32::MAX,
            blend: 0,
            glass: [0; 2],
        }
    }

    pub fn with_gloss(mut self, gloss: Gloss) -> Self {
        if gloss.reflective() {
            self.reflectance = gloss.reflectance.to_bits();
            self.power = gloss.power.to_bits();
            self.specular_tint = gloss.tint;
        }
        self
    }

    pub fn with_lighting(
        mut self,
        diffuse: [f32; 4],
        ambient: [f32; 4],
        emission: [f32; 4],
    ) -> Self {
        self.diffuse = rgba(diffuse);
        self.ambient = rgba(ambient);
        self.emission = [emission[0], emission[1], emission[2]].map(|v| v.max(0.).to_bits());
        self
    }

    pub fn uv_transform(mut self, scale: [f32; 2], velocity: [f32; 2]) -> Self {
        if scale != [1.; 2] || velocity != [0.; 2] {
            self.flags |= UV_TRANSFORM;
            self.uv_scale = scale.map(f32::to_bits);
            self.uv_scroll = velocity.map(f32::to_bits);
        }
        self
    }

    pub fn face_stage(
        mut self,
        mode: u32,
        tone: u32,
        skin: u32,
        highlight: u32,
        gloss: u32,
    ) -> Self {
        self.face_stage = [mode, argb_to_rgba(tone), argb_to_rgba(skin), highlight];
        self.face_gloss = gloss;
        self
    }

    pub(crate) fn with_blend(mut self, blend: u32) -> Self {
        self.blend = blend;
        self
    }

    pub fn with_glass(mut self, transmittance: [f32; 3], ior: f32) -> Self {
        self.flags = (self.flags | GLASS) & !(BLEND | ORDERED_GFX | ADDITIVE);
        self.glass = [
            rgba([transmittance[0], transmittance[1], transmittance[2], 1.]),
            ior.clamp(1.001, 3.).to_bits(),
        ];
        self
    }

    fn ray_mask(self) -> u8 {
        let blend = self.flags & BLEND != 0;
        let mut mask = if !blend {
            MASK_SOLID
        } else if self.flags & ORDERED_GFX != 0 {
            MASK_GFX
        } else {
            MASK_BLEND
        };
        if self.flags & PRIMARY_ONLY == 0
            && (self.flags & (WRITE_DEPTH | CLOUD) != 0 || (!blend && self.flags & WATER == 0))
        {
            mask |= MASK_SHADOW;
        }
        mask
    }
}

/// Uniform scale and a Y rotation, as used by the client's grass instances.
/// The shader transforms the direction without renormalizing it in object
/// space; this preserves ray parameter t in world units for scaled instances.
#[derive(Clone, Copy, Debug)]
pub struct Transform {
    pub position: [f32; 3],
    pub scale: f32,
    pub cos: f32,
    pub sin: f32,
}

impl Transform {
    pub const IDENTITY: Self = Self::translation([0.; 3]);

    pub const fn translation(position: [f32; 3]) -> Self {
        Self {
            position,
            scale: 1.,
            cos: 1.,
            sin: 0.,
        }
    }

    pub fn rows(self) -> [f32; 12] {
        let Self {
            position: [x, y, z],
            scale: s,
            cos: c,
            sin: r,
        } = self;
        [s * c, 0., -s * r, x, 0., s, 0., y, s * r, 0., s * c, z]
    }

    fn bounds(self, b: Bounds) -> Bounds {
        let [x0, y0, z0] = b.min.to_array();
        let [x1, y1, z1] = b.max.to_array();
        let rows = self.rows();
        let mut result = Bounds::EMPTY;
        for x in [x0, x1] {
            for z in [z0, z1] {
                for y in [y0, y1] {
                    result = result.union(Bounds::point(Vec3::new(
                        rows[0] * x + rows[2] * z + rows[3],
                        rows[5] * y + rows[7],
                        rows[8] * x + rows[10] * z + rows[11],
                    )));
                }
            }
        }
        result
    }
}

/// Optional general affine placement for a bone/hook relative child. Most
/// instances (important for grass) keep the old six-word scale/yaw transform.
#[derive(Clone, Copy)]
pub(super) struct Attachment {
    position: Vec3,
    forward: Mat3,
    inverse: Mat3,
}

impl Attachment {
    pub(super) fn new(matrix: Mat4) -> Option<Self> {
        let forward = Mat3::from_mat4(matrix);
        if !matrix.is_finite()
            || [
                matrix.x_axis.w,
                matrix.y_axis.w,
                matrix.z_axis.w,
                matrix.w_axis.w,
            ] != [0., 0., 0., 1.]
            || forward.determinant().abs() < 1e-12
        {
            return None;
        }
        let inverse = forward.inverse();
        inverse.is_finite().then_some(Self {
            position: matrix.w_axis.truncate(),
            forward,
            inverse,
        })
    }

    pub(super) fn local_point(self, p: [f32; 3]) -> [f32; 3] {
        (self.inverse * (Vec3::from_array(p) - self.position)).to_array()
    }

    pub(super) fn local_vector(self, n: [f32; 3]) -> [f32; 3] {
        (self.inverse * Vec3::from_array(n)).to_array()
    }

    fn bounds(self, b: Bounds) -> Bounds {
        let center = self.forward * ((b.min + b.max) * 0.5) + self.position;
        let half = (b.max - b.min) * 0.5;
        let radius = self.forward.x_axis.abs() * half.x
            + self.forward.y_axis.abs() * half.y
            + self.forward.z_axis.abs() * half.z;
        // These arithmetic bounds should conservatively enclose all raster
        // endpoints, including rounding for large translations.
        let pad = (center.abs() + radius).max(Vec3::ONE) * (4. * f32::EPSILON);
        Bounds {
            min: center - radius - pad,
            max: center + radius + pad,
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn rows(self) -> [f32; 12] {
        let a = self.forward.transpose();
        std::array::from_fn(|i| match (i / 4, i % 4) {
            (row, 3) => self.position[row],
            (row, col) => a.col(row)[col],
        })
    }
}

#[derive(Default, Clone)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub triangles: Vec<[u32; 3]>,
    pub surfaces: Vec<u32>,
    // Zero denotes an ungrouped triangle; a nonzero ID identifies the two
    // halves of one sheet or a coplanar triangulated draw on the same plane.
    pub layers: Vec<u32>,
}

impl Mesh {
    pub fn tri(&mut self, indices: [u32; 3], surface: u32) {
        if !self.layers.is_empty() {
            self.layers.push(0);
        }
        self.triangles.push(indices);
        self.surfaces.push(surface);
    }

    pub fn tri_layer(&mut self, indices: [u32; 3], surface: u32, layer: u32) {
        self.layers.resize(self.triangles.len(), 0);
        self.tri(indices, surface);
        self.layers.resize(self.triangles.len(), 0);
        self.layers[self.triangles.len() - 1] = layer;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChunkKey {
    NearTerrain(usize, usize),
    FarTerrain(usize, usize),
    Scenery(i32, i32),
    Water(u32),
    ShoreWaves,
    Clouds,
}

pub type Fingerprint = [u8; 32];

/// Unlike packet offsets, these identities survive repacking as a streaming
/// window changes. An animated owner keeps its address as long as the index
/// topology is identical; vertex changes alone do not change that identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MeshKey {
    Static(Option<ChunkKey>, Fingerprint),
    Posed(&'static str, u64, Fingerprint),
    RigidPart(&'static str, u64, usize, usize, Fingerprint),
}

pub struct MeshRange {
    pub vertex: Range<u32>,
    pub triangle: Range<u32>,
    pub mask: u8,
    pub opaque: bool,
    pub key: MeshKey,
    pub shape: Fingerprint,
    bvh: (u32, u32),
    bounds: Bounds,
}

#[derive(Clone, Copy)]
pub struct Instance {
    pub mesh: u32,
    transform: Transform,
    attachment: Option<u32>,
    aux: [u32; 3], // grass area x/z and wind slot, or tree LOD fade and side
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Light {
    pub position_range: [f32; 4],
    color: [f32; 4], // linear unless the native night flag scales encoded RGB
    shape: [u32; 4], // kind (0 point, 1 sphere, 2 disk), radius, flags, reserved
}
const LIGHT_WORDS: usize = std::mem::size_of::<Light>() / 4;
const _: () = assert!(LIGHT_WORDS == 12);

impl Light {
    pub fn point(position_range: [f32; 4], color: [f32; 4]) -> Self {
        Self {
            position_range,
            color,
            shape: [0; 4],
        }
    }

    pub fn sphere(mut self, radius: f32) -> Self {
        self.set_shape(1, radius);
        self
    }
    pub fn disk(mut self, radius: f32) -> Self {
        self.set_shape(2, radius);
        self
    }
    fn set_shape(&mut self, kind: u32, radius: f32) {
        if radius.is_finite() && radius > 0. && self.position_range[3] > 0. {
            self.shape[0] = kind;
            self.shape[1] = radius.min(self.position_range[3] * 0.5).to_bits();
        }
    }
    pub fn night_rgb(mut self, rgb_encoded: [f32; 4]) -> Self {
        self.color = rgb_encoded;
        self.shape[2] |= 1;
        self
    }
}

// Affine inverse of a draped cell's XZ footprint and its bounded vertical
// projection range. The four corner UVs/colors use native triangulation.
#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Projector {
    xz: [f32; 2],
    row_u: [f32; 2],
    row_v: [f32; 2],
    height: [f32; 2],
    uv: [[f32; 2]; 4],
    tint: [u32; 4],
    texture: u32,
    blend: u32, // factors in low bytes, CLAMP_UV bit for tiling
}
const _: () = assert!(std::mem::size_of::<Projector>() == 22 * 4);

#[derive(Default)]
pub struct Builder {
    vertices: Vec<Vertex>,
    triangles: Vec<[u32; 3]>,
    material_layers: Vec<[u32; 2]>,
    materials: Vec<Surface>,
    material_map: std::collections::HashMap<Surface, u32>,
    meshes: Vec<MeshRange>,
    instances: Vec<Instance>,
    attachments: Vec<Attachment>,
    lights: Vec<Light>,
    projectors: Vec<Projector>,
}

struct CachedMesh {
    shape: Fingerprint,
    tree: Bvh,
}

struct CachedTree<K> {
    membership: Vec<K>,
    bounds: Vec<Bounds>,
    tree: Bvh,
}
impl<K> Default for CachedTree<K> {
    fn default() -> Self {
        Self {
            membership: Vec::new(),
            bounds: Vec::new(),
            tree: Bvh::default(),
        }
    }
}
impl<K: PartialEq> CachedTree<K> {
    fn update(&mut self, membership: Vec<K>, bounds: Vec<Bounds>) -> [u64; 2] {
        if self.membership != membership {
            self.tree = Bvh::from_bounds(&bounds);
            self.membership = membership;
            self.bounds = bounds;
            [1, 0]
        } else if self.bounds != bounds {
            self.tree.refit(&bounds);
            self.bounds = bounds;
            [0, 1]
        } else {
            [0, 0]
        }
    }
}

#[derive(Default)]
pub(crate) struct AccelCache {
    meshes: HashMap<MeshKey, CachedMesh>,
    instances: CachedTree<MeshKey>,
    lights: CachedTree<usize>,
    /// Bottom/top-level topology builds and refits, respectively.
    pub(crate) builds: [u64; 2],
    pub(crate) refits: [u64; 2],
}

impl AccelCache {
    fn pack(&mut self, builder: &mut Builder) -> (Bvh, (u32, u32), (u32, u32)) {
        let mut bvh = Bvh::default();
        let mut live = HashSet::with_capacity(builder.meshes.len());
        for mesh in &mut builder.meshes {
            live.insert(mesh.key);
            let vertices = &builder.vertices[mesh.vertex.start as usize..mesh.vertex.end as usize];
            let triangles =
                &builder.triangles[mesh.triangle.start as usize..mesh.triangle.end as usize];
            let bounds = || {
                triangles
                    .iter()
                    .map(|tri| {
                        tri.iter().fold(Bounds::EMPTY, |b, &v| {
                            b.union(Bounds::point(Vec3::from_array(vertices[v as usize].pos)))
                        })
                    })
                    .collect::<Vec<_>>()
            };
            let entry = match self.meshes.entry(mesh.key) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    let entry = e.into_mut();
                    if entry.shape != mesh.shape {
                        entry.tree.refit(&bounds());
                        entry.shape = mesh.shape;
                        self.refits[0] += 1;
                    }
                    entry
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    self.builds[0] += 1;
                    e.insert(CachedMesh {
                        shape: mesh.shape,
                        tree: Bvh::from_bounds(&bounds()),
                    })
                }
            };
            mesh.bvh = bvh.append(&entry.tree, |id| id + mesh.triangle.start);
        }
        self.meshes.retain(|key, _| live.contains(key));
        // A packet reorder changes the payload IDs, not the tree membership.
        let mut order: Vec<_> = (0..builder.instances.len()).collect();
        order.sort_by_key(|&id| builder.meshes[builder.instances[id].mesh as usize].key);
        let (keys, bounds) = order
            .iter()
            .map(|&id| {
                let i = &builder.instances[id];
                let mesh = &builder.meshes[i.mesh as usize];
                let bounds = i.attachment.map_or_else(
                    || i.transform.bounds(mesh.bounds),
                    |a| builder.attachments[a as usize].bounds(mesh.bounds),
                );
                (mesh.key, bounds)
            })
            .unzip();
        let [builds, refits] = self.instances.update(keys, bounds);
        self.builds[1] += builds;
        self.refits[1] += refits;
        let scene = bvh.append(&self.instances.tree, |id| order[id as usize] as u32);
        // A pulsing intensity doesn't move the light volumes or their nodes.
        self.lights.update(
            (0..builder.lights.len()).collect(),
            builder
                .lights
                .iter()
                .map(|l| {
                    let [x, y, z, r] = l.position_range;
                    let center = Vec3::new(x, y, z);
                    let extent = Vec3::splat(r);
                    Bounds {
                        min: center - extent,
                        max: center + extent,
                    }
                })
                .collect(),
        );
        let light = bvh.append(&self.lights.tree, |id| id);
        (bvh, scene, light)
    }
}

/// Budget for one optional world-space mesh appended after required geometry.
/// Surfaces are charged before interning; nodes are charged at most two per
/// item, so this bounds rather than guesses the final storage binding size.
#[derive(Clone, Copy)]
pub(crate) struct MeshBudget {
    pub vertices: usize,
    pub triangles: usize,
    pub bytes: usize,
}

impl MeshBudget {
    pub fn debit(&mut self, mesh: &Mesh, surfaces: usize) -> bool {
        let v = mesh.vertices.len();
        let t = mesh.triangles.len();
        let bytes = v * std::mem::size_of::<Vertex>()
            + t * (6 + 2 * super::bvh::NODE_WORDS as usize) * 4
            + surfaces * std::mem::size_of::<Surface>()
            // At most one new top-level entry per optional item.
            + (INSTANCE_WORDS + 2 * super::bvh::NODE_WORDS + 1) as usize * 4;
        if v > self.vertices || t > self.triangles || bytes > self.bytes {
            return false;
        }
        self.vertices -= v;
        self.triangles -= t;
        self.bytes -= bytes;
        true
    }
}

impl Builder {
    /// Reserve space for the static TLAS and light BVH, which are only appended
    /// by `finish`. An optional extension must not overflow a binding when
    /// the ordinary resident scene fits on its own.
    pub fn optional_mesh_budget(&self, storage_bytes: usize) -> MeshBudget {
        let instances = self.instances.len();
        let nodes: usize = self
            .meshes
            .iter()
            .map(|m| Bvh::node_count(m.triangle.len()))
            .sum();
        let bvh_words = nodes * super::bvh::NODE_WORDS as usize
            + self.triangles.len()
            + (instances + self.lights.len()) * (2 * super::bvh::NODE_WORDS as usize + 1);
        let fixed = (HEADER_WORDS
            + self.vertices.len() * VERTEX_WORDS as usize
            + self.triangles.len() * 5
            + self.materials.len() * SURFACE_WORDS
            + instances * INSTANCE_WORDS as usize
            + self.lights.len() * LIGHT_WORDS
            + self.projectors.len() * 22
            + bvh_words)
            * 4;
        MeshBudget {
            vertices: MAX_ITEMS.saturating_sub(self.vertices.len()),
            triangles: MAX_ITEMS.saturating_sub(self.triangles.len()),
            bytes: storage_bytes.saturating_sub(fixed),
        }
    }

    // Admission errors are counted rather than replacing a drape with a
    // floating card. A bounded list keeps per-hit cost predictable.
    pub(crate) fn project_decal(
        &mut self,
        p: [[f32; 3]; 4],
        uv: [[f32; 2]; 4],
        tint: [u32; 4],
        texture: u32,
        blend: u32,
    ) -> Result<(), bool> {
        if self.projectors.len() >= 128 {
            return Err(true);
        }
        if !p
            .iter()
            .flatten()
            .chain(uv.iter().flatten())
            .all(|v| v.is_finite())
        {
            return Err(false);
        }
        let xz = [p[0][0], p[0][2]];
        let a = [p[1][0] - xz[0], p[1][2] - xz[1]];
        let b = [p[2][0] - xz[0], p[2][2] - xz[1]];
        let d = a[0] * b[1] - a[1] * b[0];
        if d.abs() < 1e-7 {
            return Err(false);
        }
        let ymin = p.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        let ymax = p.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
        self.projectors.push(Projector {
            xz,
            row_u: [b[1] / d, -b[0] / d],
            row_v: [-a[1] / d, a[0] / d],
            height: [ymin - 0.75, ymax + 0.25],
            uv,
            tint,
            texture,
            blend,
        });
        Ok(())
    }

    pub fn light(&mut self, light: Light) {
        let [x, y, z, r] = light.position_range;
        if r > 0.
            && r <= 1.0e7
            && Vec3::new(x, y, z).abs().max_element() <= 1.0e7
            && light
                .position_range
                .iter()
                .chain(&light.color)
                .all(|v| v.is_finite())
            && light.color[..3].iter().any(|&v| v > 0.)
        {
            self.lights.push(light);
        }
    }

    pub fn surface(&mut self, surface: Surface) -> u32 {
        *self.material_map.entry(surface).or_insert_with(|| {
            let id = self.materials.len() as u32;
            self.materials.push(surface);
            id
        })
    }

    pub fn mesh(&mut self, mesh: &Mesh) -> Result<Option<u32>, String> {
        self.mesh_keyed(mesh, Some, |_, shape| MeshKey::Static(None, shape))
    }

    pub(crate) fn mesh_mapped(
        &mut self,
        mesh: &Mesh,
        materials: &[u32],
    ) -> Result<Option<u32>, String> {
        self.mesh_keyed(
            mesh,
            |id| materials.get(id as usize).copied(),
            |_, shape| MeshKey::Static(None, shape),
        )
    }

    fn mesh_keyed(
        &mut self,
        mesh: &Mesh,
        material_id: impl Fn(u32) -> Option<u32>,
        key: impl FnOnce(Fingerprint, Fingerprint) -> MeshKey,
    ) -> Result<Option<u32>, String> {
        if mesh.triangles.is_empty() {
            return Ok(None);
        }
        if mesh.triangles.len() != mesh.surfaces.len()
            || (!mesh.layers.is_empty() && mesh.layers.len() != mesh.triangles.len())
            || mesh
                .surfaces
                .iter()
                .any(|&id| material_id(id).is_none_or(|id| id as usize >= self.materials.len()))
        {
            return Err("invalid Lighter material assignments".into());
        }
        if mesh.vertices.len() + self.vertices.len() > MAX_ITEMS
            || mesh.triangles.len() + self.triangles.len() > MAX_ITEMS
        {
            return Err(
                "Lighter scene exceeds its mesh budget; reduce the terrain window/scenery quality"
                    .into(),
            );
        }
        let vertex_start = self.vertices.len() as u32;
        let tri_start = self.triangles.len() as u32;
        let mut bounds = Bounds::EMPTY;
        let mut mask = 0;
        let first_mask = self.materials[material_id(mesh.surfaces[0]).unwrap() as usize].ray_mask();
        let mut opaque = true;
        let mut topology = Sha256::new();
        topology.update((mesh.vertices.len() as u32).to_le_bytes());
        topology.update(bytemuck::cast_slice::<_, u8>(&mesh.triangles));
        let topology: Fingerprint = topology.finalize().into();
        let mut shape = Sha256::new();
        shape.update(topology);
        for v in &mesh.vertices {
            shape.update(bytemuck::bytes_of(&v.pos));
        }
        let shape: Fingerprint = shape.finalize().into();
        let key = key(topology, shape);
        for (&tri, &mat) in mesh.triangles.iter().zip(&mesh.surfaces) {
            for i in tri {
                let p = Vec3::from_array(
                    mesh.vertices
                        .get(i as usize)
                        .ok_or("invalid ray mesh index")?
                        .pos,
                );
                if !p.is_finite() || p.abs().max_element() > 1.0e7 {
                    return Err("invalid ray mesh position".into());
                }
                bounds = bounds.union(Bounds::point(p));
            }
            let material = self.materials[material_id(mat).unwrap() as usize];
            let bits = material.ray_mask();
            mask |= bits;
            opaque &= bits == first_mask
                && material.flags
                    & (CUTOUT
                        | BLEND
                        | NO_REFLECT
                        | NO_REFRACT
                        | CULL_BACK
                        | WRITE_DEPTH
                        | HIDE_PRIMARY
                        | SINGLE_SIDED
                        | FOLIAGE
                        | PRIMARY_ONLY
                        | TREE_MATERIAL
                        | OVERLAY
                        | CLOUD)
                    == 0;
        }
        // Don't leave partial ranges behind if validation rejected this draw.
        self.triangles.extend_from_slice(&mesh.triangles);
        self.material_layers
            .extend(mesh.surfaces.iter().enumerate().map(|(i, &material)| {
                // Reserve zero for the ungrouped case. Tagged draws sort by their
                // encounter-order tag, untagged triangles by their own order.
                let layer = mesh.layers.get(i).copied().unwrap_or(0);
                [
                    material_id(material).unwrap(),
                    if layer == 0 {
                        (i as u32 + 1) << 1
                    } else {
                        layer << 1 | 1
                    },
                ]
            }));
        self.vertices.extend_from_slice(&mesh.vertices);
        let id = self.meshes.len() as u32;
        self.meshes.push(MeshRange {
            vertex: vertex_start..self.vertices.len() as u32,
            triangle: tri_start..self.triangles.len() as u32,
            mask,
            opaque,
            key,
            shape,
            bvh: (0, 0),
            bounds,
        });
        Ok(Some(id))
    }

    pub fn place(&mut self, mesh: u32, transform: Transform) -> Result<(), String> {
        if mesh as usize >= self.meshes.len()
            || self.instances.len() >= DYNAMIC_BIT as usize
            || !Vec3::from_array(transform.position).is_finite()
            || !transform.scale.is_finite()
            || transform.scale <= 0.
            || !Vec2::new(transform.cos, transform.sin).is_finite()
            || (Vec2::new(transform.cos, transform.sin).length_squared() - 1.).abs() > 0.001
        {
            return Err("invalid or excessive Lighter instance transforms".into());
        }
        self.instances.push(Instance {
            mesh,
            transform,
            attachment: None,
            aux: [0, 0, GRASS_WIND_SLOTS as u32],
        });
        Ok(())
    }

    pub fn place_grass(
        &mut self,
        mesh: u32,
        transform: Transform,
        area: [f32; 2],
    ) -> Result<(), String> {
        self.place_grass_wind(mesh, transform, area, GRASS_WIND_SLOTS as u32)
    }

    pub(crate) fn place_grass_wind(
        &mut self,
        mesh: u32,
        transform: Transform,
        area: [f32; 2],
        slot: u32,
    ) -> Result<(), String> {
        self.place(mesh, transform)?;
        self.instances.last_mut().unwrap().aux = [area[0].to_bits(), area[1].to_bits(), slot];
        Ok(())
    }

    pub(crate) fn place_tree(
        &mut self,
        mesh: u32,
        position: [f32; 3],
        fraction: f32,
        side: u32,
    ) -> Result<(), String> {
        self.place(mesh, Transform::translation(position))?;
        self.instances.last_mut().unwrap().aux[0..2].copy_from_slice(&[fraction.to_bits(), side]);
        Ok(())
    }

    pub fn world_mesh(&mut self, mesh: &Mesh) -> Result<(), String> {
        if let Some(id) = self.mesh(mesh)? {
            self.place(id, Transform::IDENTITY)?;
        }
        Ok(())
    }

    pub fn chunk(&mut self, key: ChunkKey, mesh: &Mesh) -> Result<(), String> {
        if let Some(id) =
            self.mesh_keyed(mesh, Some, |_, shape| MeshKey::Static(Some(key), shape))?
        {
            self.place(id, Transform::IDENTITY)?;
        }
        Ok(())
    }

    pub(crate) fn chunk_mapped(
        &mut self,
        key: ChunkKey,
        mesh: &Mesh,
        materials: &[u32],
    ) -> Result<(), String> {
        if let Some(id) = self.mesh_keyed(
            mesh,
            |i| materials.get(i as usize).copied(),
            |_, shape| MeshKey::Static(Some(key), shape),
        )? {
            self.place(id, Transform::IDENTITY)?;
        }
        Ok(())
    }

    /// A shared deforming mesh (e.g. all foliage of one SpeedTree type/LOD).
    /// Retain index topology and update positions independently of instances.
    pub(crate) fn posed_template(
        &mut self,
        namespace: &'static str,
        owner: u64,
        mesh: &Mesh,
        materials: &[u32],
    ) -> Result<Option<u32>, String> {
        self.mesh_keyed(
            mesh,
            |id| materials.get(id as usize).copied(),
            |topology, _| MeshKey::Posed(namespace, owner, topology),
        )
    }

    pub fn posed_world_mesh(
        &mut self,
        namespace: &'static str,
        id: u64,
        mesh: &Mesh,
    ) -> Result<(), String> {
        self.posed_mesh(namespace, id, mesh, None)
    }

    pub(super) fn posed_mesh(
        &mut self,
        namespace: &'static str,
        owner: u64,
        mesh: &Mesh,
        placement: Option<Attachment>,
    ) -> Result<(), String> {
        if let Some(id) = self.mesh_keyed(mesh, Some, |topology, _| {
            MeshKey::Posed(namespace, owner, topology)
        })? {
            self.place_model(id, placement)?;
        }
        Ok(())
    }

    pub(super) fn rigid_part(
        &mut self,
        namespace: &'static str,
        owner: u64,
        part: (usize, usize),
        mesh: &Mesh,
        placement: Attachment,
    ) -> Result<(), String> {
        if let Some(id) = self.mesh_keyed(mesh, Some, |_, shape| {
            MeshKey::RigidPart(namespace, owner, part.0, part.1, shape)
        })? {
            self.place_model(id, Some(placement))?;
        }
        Ok(())
    }

    fn place_model(&mut self, mesh: u32, placement: Option<Attachment>) -> Result<(), String> {
        self.place(
            mesh,
            placement.map_or(Transform::IDENTITY, |a| {
                Transform::translation(a.position.to_array())
            }),
        )?;
        if let Some(a) = placement {
            self.instances.last_mut().unwrap().attachment = Some(self.attachments.len() as u32);
            self.attachments.push(a);
        }
        Ok(())
    }

    pub fn finish(self) -> Packet {
        self.finish_cached(&mut AccelCache::default())
    }

    pub(crate) fn finish_cached(mut self, cache: &mut AccelCache) -> Packet {
        let (bvh, (root, end), (light_root, light_end)) = cache.pack(&mut self);
        let mut words = vec![0; HEADER_WORDS];
        fn append<T: Pod>(words: &mut Vec<u32>, field: usize, data: &[T]) {
            words[field] = words.len() as u32;
            words.extend_from_slice(bytemuck::cast_slice(data));
        }
        append(&mut words, 0, &self.vertices);
        append(&mut words, 1, &self.triangles);
        append(&mut words, 2, &self.material_layers);
        append(&mut words, 3, &self.materials);
        // Affine basis data is indirect and only present for attached parts.
        // A zero scale (invalid for ordinary instances) selects it in WGSL.
        let attachment_start = (words.len()
            + self.instances.len() * INSTANCE_WORDS as usize
            + bvh.nodes.len() * super::bvh::NODE_WORDS as usize
            + bvh.items.len()
            + self.lights.len() * LIGHT_WORDS) as u32;
        let gpu_instances: Vec<_> = self
            .instances
            .iter()
            .map(|inst| {
                let t = inst.transform;
                let mesh = &self.meshes[inst.mesh as usize];
                let [x, y, z] = t.position.map(f32::to_bits);
                let words: [u32; INSTANCE_WORDS as usize] = [
                    x,
                    y,
                    z,
                    if inst.attachment.is_some() {
                        0
                    } else {
                        t.scale.to_bits()
                    },
                    inst.attachment
                        .map_or(t.cos.to_bits(), |i| attachment_start + i * 18),
                    t.sin.to_bits(),
                    mesh.vertex.start,
                    mesh.triangle.start,
                    mesh.bvh.0,
                    mesh.bvh.1,
                    mesh.mask.into(),
                    inst.aux[0],
                    inst.aux[1],
                    inst.aux[2],
                ];
                words
            })
            .collect();
        append(&mut words, 4, &gpu_instances);
        let flag_bits = self.instances.iter().fold(0, |bits, inst| {
            bits | u32::from(self.meshes[inst.mesh as usize].mask)
        });
        append(&mut words, 5, &bvh.nodes);
        append(&mut words, 6, &bvh.items);
        words[7] = root;
        words[8] = end;
        append(&mut words, 9, &self.lights);
        words[10] = self.lights.len() as u32;
        words[11] = flag_bits;
        words[12] = light_root;
        words[13] = light_end;
        debug_assert_eq!(words.len(), attachment_start as usize);
        for a in &self.attachments {
            words.extend(a.forward.to_cols_array().map(f32::to_bits));
            words.extend(a.inverse.to_cols_array().map(f32::to_bits));
        }
        append(&mut words, 14, &self.projectors);
        words[15] = self.projectors.len() as u32;
        Packet {
            words,
            meshes: self.meshes,
            instances: self.instances,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            attachments: self.attachments,
            triangle_count: self.triangles.len(),
        }
    }
}

pub struct Packet {
    pub words: Vec<u32>,
    pub meshes: Vec<MeshRange>,
    pub instances: Vec<Instance>,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    attachments: Vec<Attachment>,
    triangle_count: usize,
}

impl Packet {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    pub(super) fn placement_rows(&self, instance: &Instance) -> [f32; 12] {
        instance.attachment.map_or_else(
            || instance.transform.rows(),
            |a| self.attachments[a as usize].rows(),
        )
    }

    pub fn bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.words)
    }

    pub fn triangles(&self) -> usize {
        self.triangle_count
    }
}
