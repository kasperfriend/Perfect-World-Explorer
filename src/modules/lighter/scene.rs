//! Backend-neutral scene packet. Offsets in the header are *word* offsets.
//! The same indexed positions build the Vulkan BLAS and feed the software
//! intersector. Both backends shade the same triangle IDs and barycentrics.
use super::bvh::{Bounds, Bvh, Item};
use bytemuck::{Pod, Zeroable};
use glam::{Vec2, Vec3};
use std::ops::Range;

pub const TERRAIN: u32 = 1 << 0;
pub const FAR_TERRAIN: u32 = 1 << 1;
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
pub const TWO_SIDED: u32 = 1 << 12;
pub const CULL_BACK: u32 = 1 << 13;
pub const WRITE_DEPTH: u32 = 1 << 14;

const HEADER_WORDS: usize = 12;
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

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, PartialEq, Eq, Hash)]
pub struct Surface {
    pub texture: u32,
    pub flags: u32,
    cutoff: u32,
    reflectance: u32,
}

impl Surface {
    pub fn new(texture: u32, flags: u32, cutoff: f32, reflectance: f32) -> Self {
        Self {
            texture,
            flags,
            cutoff: cutoff.clamp(0., 1.).to_bits(),
            reflectance: reflectance.clamp(0., 1.).to_bits(),
        }
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

#[derive(Default, Clone)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub triangles: Vec<[u32; 3]>,
    pub surfaces: Vec<u32>,
}

impl Mesh {
    pub fn tri(&mut self, indices: [u32; 3], surface: u32) {
        self.triangles.push(indices);
        self.surfaces.push(surface);
    }
}

pub struct MeshRange {
    pub vertex: Range<u32>,
    pub triangle: Range<u32>,
    bvh: (u32, u32),
    bounds: Bounds,
}

#[derive(Clone, Copy)]
pub struct Instance {
    pub mesh: u32,
    pub transform: Transform,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Light {
    pub position_range: [f32; 4],
    pub color: [f32; 4],
}

#[derive(Default)]
pub struct Builder {
    vertices: Vec<Vertex>,
    triangles: Vec<[u32; 3]>,
    material_ids: Vec<u32>,
    materials: Vec<Surface>,
    material_map: std::collections::HashMap<Surface, u32>,
    meshes: Vec<MeshRange>,
    instances: Vec<Instance>,
    bvh: Bvh,
    pub lights: Vec<Light>,
}

impl Builder {
    pub fn surface(&mut self, surface: Surface) -> u32 {
        *self.material_map.entry(surface).or_insert_with(|| {
            let id = self.materials.len() as u32;
            self.materials.push(surface);
            id
        })
    }

    pub fn mesh(&mut self, mesh: &Mesh) -> Result<Option<u32>, String> {
        if mesh.triangles.is_empty() {
            return Ok(None);
        }
        if mesh.triangles.len() != mesh.surfaces.len()
            || mesh
                .surfaces
                .iter()
                .any(|&id| id as usize >= self.materials.len())
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
        let mut items = Vec::with_capacity(mesh.triangles.len());
        for (k, (&tri, &mat)) in mesh.triangles.iter().zip(&mesh.surfaces).enumerate() {
            let mut tb = Bounds::EMPTY;
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
                tb = tb.union(Bounds::point(p));
            }
            bounds = bounds.union(tb);
            items.push(Item {
                id: tri_start + k as u32,
                bounds: tb,
            });
            self.triangles.push(tri);
            self.material_ids.push(mat);
        }
        self.vertices.extend_from_slice(&mesh.vertices);
        let bvh = self.bvh.build(&mut items);
        let id = self.meshes.len() as u32;
        self.meshes.push(MeshRange {
            vertex: vertex_start..self.vertices.len() as u32,
            triangle: tri_start..self.triangles.len() as u32,
            bvh,
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
        {
            return Err("invalid or excessive Lighter instance transforms".into());
        }
        self.instances.push(Instance { mesh, transform });
        Ok(())
    }

    pub fn world_mesh(&mut self, mesh: &Mesh) -> Result<(), String> {
        if let Some(id) = self.mesh(mesh)? {
            self.place(id, Transform::IDENTITY)?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> Packet {
        let mut items: Vec<_> = self
            .instances
            .iter()
            .enumerate()
            .map(|(i, inst)| Item {
                id: i as u32,
                bounds: inst
                    .transform
                    .bounds(self.meshes[inst.mesh as usize].bounds),
            })
            .collect();
        let (root, end) = self.bvh.build(&mut items);
        let mut words = vec![0; HEADER_WORDS];
        fn append<T: Pod>(words: &mut Vec<u32>, field: usize, data: &[T]) {
            words[field] = words.len() as u32;
            words.extend_from_slice(bytemuck::cast_slice(data));
        }
        append(&mut words, 0, &self.vertices);
        append(&mut words, 1, &self.triangles);
        append(&mut words, 2, &self.material_ids);
        append(&mut words, 3, &self.materials);
        let gpu_instances: Vec<_> = self
            .instances
            .iter()
            .map(|inst| {
                let t = inst.transform;
                let mesh = &self.meshes[inst.mesh as usize];
                let [x, y, z] = t.position.map(f32::to_bits);
                [
                    x,
                    y,
                    z,
                    t.scale.to_bits(),
                    t.cos.to_bits(),
                    t.sin.to_bits(),
                    mesh.vertex.start,
                    mesh.triangle.start,
                    mesh.bvh.0,
                    mesh.bvh.1,
                ]
            })
            .collect();
        append(&mut words, 4, &gpu_instances);
        append(&mut words, 5, &self.bvh.nodes);
        append(&mut words, 6, &self.bvh.items);
        words[7] = root;
        words[8] = end;
        append(&mut words, 9, &self.lights);
        words[10] = self.lights.len() as u32;
        Packet {
            words,
            meshes: self.meshes,
            instances: self.instances,
        }
    }
}

pub struct Packet {
    pub words: Vec<u32>,
    pub meshes: Vec<MeshRange>,
    pub instances: Vec<Instance>,
}

impl Packet {
    pub fn bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.words)
    }

    pub fn triangles(&self) -> usize {
        self.meshes.iter().map(|m| m.triangle.len()).sum()
    }
}
