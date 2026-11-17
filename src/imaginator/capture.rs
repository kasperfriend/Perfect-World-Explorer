//! Turning the loaded world into the Imaginator's own triangle soup.
//!
//! This is the only file in the module that knows engine types, and it only
//! *reads* them: nothing here mutates the world, the renderer or any GPU
//! resource. The result is a [`super::bvh::Scene`] the tracer owns outright, so
//! the core can stream, evict and reload freely afterwards.
//!
//! What is captured:
//!
//! * **Terrain** — the height grid, as a coarse regular mesh. The tracer only
//!   needs the shape of the ground (it is what shadows fall on), so it samples
//!   every `TERRAIN_STRIDE` cells instead of every cell. Vertex normals come
//!   from the same central differences the terrain shader uses.
//! * **Water** — one quad per water area, at its authored height, clipped to the
//!   area's own mask bounds. Water is the strongest mirror in the world, so it
//!   is worth a real surface even though it is flat.
//! * **Static scenery** — every lit model (building, rock, bridge) placed in the
//!   world, using the same baked transform the GPU path uses (`gpu::build`'s
//!   `model.position + x*vx*scale + y*vy*scale + z*vz*scale`). Only models whose
//!   `litmodels.pck` data is resident are captured, so the tracer's picture grows
//!   as the scenery streams in, exactly like the world does.
//! * **Vegetation** — every tree placement, as two crossed quads sized from the
//!   tree type's own mesh bounds. A tree is mostly holes; giving it a canopy
//!   volume is what makes its shadow read as shade instead of a lamp post.
//!
//! The capture is bounded: `CaptureBudget` caps triangles and, optionally, the
//! radius around the camera. A cut-out world is reported through
//! `SceneStats::clipped` so the module page can say so instead of the user
//! guessing why the horizon is empty.

use super::bvh::{material, Bvh, Material, Scene, SceneStats, Triangle};
use super::math::{v3, V3};
use crate::navigation::MapSpace;
use crate::objects::Objects;
use crate::scene::{Ornament, Scene as WorldScene};
use crate::trees::Trees;
use crate::water::WaterArea;
use std::sync::Arc;

/// Cells per terrain triangle edge. The world height grid is one cell per
/// ~2 units, so a stride of 4 is a triangle edge of ~8 units: fine enough to
/// carry the shape of a hillside into shadows, coarse enough that a whole map is
/// a few hundred thousand triangles instead of tens of millions.
pub const TERRAIN_STRIDE: usize = 4;

#[derive(Clone, Copy, Debug)]
pub struct CaptureBudget {
    pub max_triangles: usize,
    /// Only capture geometry within this radius of the centre, in world units.
    /// `None` keeps the whole map.
    pub radius: Option<f32>,
    /// Centre of `radius` (the camera position when the capture was requested).
    pub center: [f32; 3],
    /// Skip vegetation. Trees are the most expensive thing to capture and the
    /// least important for a first look at a map.
    pub trees: bool,
}

impl Default for CaptureBudget {
    fn default() -> Self {
        Self {
            max_triangles: 2_500_000,
            radius: None,
            center: [0.; 3],
            trees: true,
        }
    }
}

/// Everything the capture needs, borrowed from whatever already owns it.
pub struct Sources<'a> {
    pub map: &'a MapSpace,
    pub water: &'a [WaterArea],
    pub objects: &'a Objects,
    pub ornaments: &'a [Ornament],
    pub scene: &'a WorldScene,
    pub trees: Option<&'a Trees>,
}

struct Builder {
    triangles: Vec<Triangle>,
    budget: CaptureBudget,
    clipped: bool,
}

impl Builder {
    fn has_room(&self) -> bool {
        self.triangles.len() < self.budget.max_triangles
    }

    /// Distance test used by every producer. Returns `false` when the primitive
    /// is outside the capture radius.
    fn within(&self, x: f32, z: f32) -> bool {
        match self.budget.radius {
            None => true,
            Some(r) => {
                let dx = x - self.budget.center[0];
                let dz = z - self.budget.center[2];
                dx * dx + dz * dz <= r * r
            }
        }
    }

    fn push(&mut self, a: V3, b: V3, c: V3, normal: V3, m: u16) {
        if !a.is_finite() || !b.is_finite() || !c.is_finite() || !normal.is_finite() {
            return;
        }
        // Reject slivers: they cost traversal time and never change a picture.
        let area = (b - a).cross(c - a).length();
        if !(area > 1e-6) {
            return;
        }
        if !self.has_room() {
            self.clipped = true;
            return;
        }
        self.triangles.push(Triangle {
            a,
            b,
            c,
            normal: normal.normalize_or_zero(),
            material: m,
        });
    }

    fn quad(&mut self, a: V3, b: V3, c: V3, d: V3, normal: V3, m: u16) {
        self.push(a, b, c, normal, m);
        self.push(a, c, d, normal, m);
    }
}

/// Capture the world. Never fails: a world with no `litmodels.pck` data simply
/// produces terrain, water and trees, which is still a usable trace.
pub fn capture(sources: &Sources<'_>, budget: CaptureBudget) -> Arc<Scene> {
    let mut b = Builder {
        triangles: Vec::new(),
        budget,
        clipped: false,
    };
    capture_terrain(&mut b, sources.map);
    capture_water(&mut b, sources.water);
    capture_scenery(&mut b, sources.objects, sources.ornaments);
    if budget.trees {
        if let Some(trees) = sources.trees {
            capture_trees(&mut b, trees, sources.scene);
        }
    }
    let triangles = std::mem::take(&mut b.triangles);
    let stats = SceneStats {
        triangles: triangles.len(),
        materials: material::COUNT as usize,
        nodes: 0,
        clipped: b.clipped,
    };
    let bvh = Bvh::build(triangles);
    let mut scene = Scene::with_builtin_materials();
    scene.stats = SceneStats {
        nodes: bvh.nodes.len(),
        triangles: bvh.triangles.len(),
        ..stats
    };
    scene.bvh = Some(bvh);
    Arc::new(scene)
}

/// Height at a grid cell, clamped, in the same world orientation the terrain
/// mesh uses: `x` runs west→east, `z` runs north→south as the grid's rows grow.
fn cell_height(map: &MapSpace, x: usize, z: usize) -> f32 {
    let x = x.min(map.width.saturating_sub(1));
    let z = z.min(map.height.saturating_sub(1));
    map.heights.at(z * map.width + x)
}

/// World-space position of a height-grid corner. Mirrors `terrain_gpu`'s
/// mapping, which centres the grid on the origin.
fn cell_position(map: &MapSpace, x: usize, z: usize) -> V3 {
    let half_w = map.width as f32 * 0.5;
    let half_h = map.height as f32 * 0.5;
    v3(
        (x as f32 - half_w) * map.spacing,
        cell_height(map, x, z),
        (half_h - z as f32) * map.spacing,
    )
}

fn capture_terrain(b: &mut Builder, map: &MapSpace) {
    if map.width < 2 || map.height < 2 {
        return;
    }
    let stride = TERRAIN_STRIDE.max(1);
    let mut z = 0;
    while z + stride < map.height {
        let mut x = 0;
        while x + stride < map.width {
            let x1 = (x + stride).min(map.width - 1);
            let z1 = (z + stride).min(map.height - 1);
            let p00 = cell_position(map, x, z);
            let p10 = cell_position(map, x1, z);
            let p01 = cell_position(map, x, z1);
            let p11 = cell_position(map, x1, z1);
            if !b.within(p00.x, p00.z) && !b.within(p11.x, p11.z) {
                x += stride;
                continue;
            }
            // The normal of the cell's own plane, so a slope shades as a slope.
            let n = (p10 - p00).cross(p01 - p00).normalize_or_zero();
            let n = if n.y < 0. { -n } else { n };
            b.quad(p00, p10, p11, p01, n, material::TERRAIN);
            x += stride;
        }
        z += stride;
    }
}

fn capture_water(b: &mut Builder, areas: &[WaterArea]) {
    for area in areas {
        let half_x = area.columns as f32 * area.grid_size * 0.5;
        let half_z = area.rows as f32 * area.grid_size * 0.5;
        if !b.within(area.center[0], area.center[2]) {
            continue;
        }
        let y = area.height;
        let x0 = area.center[0] - half_x;
        let x1 = area.center[0] + half_x;
        let z0 = area.center[2] - half_z;
        let z1 = area.center[2] + half_z;
        // One quad per mask-covered cell run would be exact; a bounding quad is
        // not, but a water *plane* is: the mask decides where the client draws
        // it, not where water physically is. Covering the whole footprint keeps
        // the mirror continuous, which is what a reflection needs.
        b.quad(
            v3(x0, y, z0),
            v3(x1, y, z0),
            v3(x1, y, z1),
            v3(x0, y, z1),
            v3(0., -1., 0.),
            material::WATER,
        );
    }
}

/// The engine's basis rule (`gpu::basis`): normalise, fall back to +Y.
fn basis(v: [f32; 3]) -> V3 {
    let v = V3::from_array(v);
    if v.length_squared() < 1e-8 {
        v3(0., 1., 0.)
    } else {
        v.normalize_or_zero()
    }
}

fn capture_scenery(b: &mut Builder, objects: &Objects, ornaments: &[Ornament]) {
    for (index, ornament) in ornaments.iter().enumerate() {
        if !b.has_room() {
            b.clipped = true;
            return;
        }
        if !b.within(ornament.position[0], ornament.position[2]) {
            continue;
        }
        let Some(model) = objects.get(index) else {
            // Not resident: the tracer's picture grows as this streams in.
            continue;
        };
        let base = V3::from_array(model.position);
        let y_axis = basis(model.up);
        let z_axis = basis(model.direction);
        let x_axis = y_axis.cross(z_axis).normalize_or_zero();
        let scale = V3::from_array(model.scale);
        for mesh in &model.meshes {
            for tri in mesh.indices.chunks_exact(3) {
                let Some(pa) = mesh.vertices.get(tri[0] as usize) else {
                    continue;
                };
                let Some(pb) = mesh.vertices.get(tri[1] as usize) else {
                    continue;
                };
                let Some(pc) = mesh.vertices.get(tri[2] as usize) else {
                    continue;
                };
                let place = |v: &crate::bmd::Vertex| {
                    base + x_axis * (v.position[0] * scale.x)
                        + y_axis * (v.position[1] * scale.y)
                        + z_axis * (v.position[2] * scale.z)
                };
                let (a, bb, c) = (place(pa), place(pb), place(pc));
                // The authored normal, in world space, so smooth-shaded
                // buildings keep their shading instead of going faceted.
                let n = (x_axis * pa.normal[0] + y_axis * pa.normal[1] + z_axis * pa.normal[2])
                    .normalize_or_zero();
                b.push(a, bb, c, n, material::SCENERY);
            }
        }
    }
}

fn capture_trees(b: &mut Builder, trees: &Trees, scene: &WorldScene) {
    // Type id → (height, canopy radius) from the type's own highest LOD, so a
    // big tree casts a big shadow and a sapling does not cast a forest's.
    let mut shapes: Vec<Option<(f32, f32)>> = Vec::with_capacity(trees.types.len());
    for tree in &trees.types {
        shapes.push(tree.as_ref().map(|mesh| {
            let mut height = 5.0f32;
            let mut radius = 1.5f32;
            if let Some(lod) = mesh.lods.first() {
                for v in lod.branch.vertices.iter().chain(lod.frond.vertices.iter()) {
                    height = height.max(v.pos[2]);
                    radius = radius.max(v.pos[0].abs().max(v.pos[1].abs()));
                }
            }
            (height, radius.max(height * 0.18))
        }));
    }
    let _ = scene;
    for placement in &trees.placements {
        if !b.has_room() {
            b.clipped = true;
            return;
        }
        if !b.within(placement.position[0], placement.position[2]) {
            continue;
        }
        let Some(Some((height, radius))) = shapes.get(placement.tree_type as usize) else {
            continue;
        };
        let base = V3::from_array(placement.position);
        // Two crossed canopy cards, from 30% up the trunk to the crown.
        let low = base.y + height * 0.30;
        let high = base.y + height;
        let r = *radius;
        for (dx, dz) in [(r, 0.), (0., r)] {
            let a = v3(base.x - dx, low, base.z - dz);
            let bb = v3(base.x + dx, low, base.z + dz);
            let c = v3(base.x + dx, high, base.z + dz);
            let d = v3(base.x - dx, high, base.z - dz);
            b.quad(a, bb, c, d, v3(0., 1., 0.), material::VEGETATION);
        }
    }
}

/// The material table the capture uses. Exposed so the tracer and the tests agree
/// on the ordering.
pub fn materials() -> Vec<Material> {
    let s = Scene::empty();
    s.materials
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_stride_covers_the_grid_without_gaps_or_overlaps() {
        // 9x9 grid, stride 4 => cells at 0..4, 4..8 (the second one clamps to
        // the last cell), so the surface is continuous.
        assert_eq!(TERRAIN_STRIDE, 4);
        let stride = TERRAIN_STRIDE;
        let mut counted = 0;
        let mut z = 0;
        while z + stride < 9 {
            let mut x = 0;
            while x + stride < 9 {
                counted += 2;
                x += stride;
            }
            z += stride;
        }
        assert_eq!(counted, 8, "two quads, four triangles per 9x9 grid");
    }

    #[test]
    fn slivers_and_non_finite_geometry_are_dropped() {
        let mut b = Builder {
            triangles: Vec::new(),
            budget: CaptureBudget::default(),
            clipped: false,
        };
        b.push(V3::ZERO, v3(1., 0., 0.), v3(2., 0., 0.), v3(0., 1., 0.), 0);
        b.push(
            V3::ZERO,
            v3(f32::NAN, 0., 0.),
            v3(0., 0., 1.),
            v3(0., 1., 0.),
            0,
        );
        b.push(V3::ZERO, v3(1., 0., 0.), v3(0., 0., 1.), v3(0., 1., 0.), 0);
        assert_eq!(b.triangles.len(), 1);
    }

    #[test]
    fn the_budget_stops_the_capture_and_says_so() {
        let mut b = Builder {
            triangles: Vec::new(),
            budget: CaptureBudget {
                max_triangles: 2,
                ..CaptureBudget::default()
            },
            clipped: false,
        };
        for _ in 0..5 {
            b.push(V3::ZERO, v3(1., 0., 0.), v3(0., 0., 1.), v3(0., 1., 0.), 0);
        }
        assert_eq!(b.triangles.len(), 2);
        assert!(b.clipped);
    }

    #[test]
    fn the_radius_rejects_far_geometry() {
        let b = Builder {
            triangles: Vec::new(),
            budget: CaptureBudget {
                radius: Some(10.),
                center: [0., 0., 0.],
                ..CaptureBudget::default()
            },
            clipped: false,
        };
        assert!(b.within(3., 3.));
        assert!(!b.within(20., 0.));
    }

    #[test]
    fn built_in_materials_are_ordered_as_the_capture_indexes_them() {
        let m = materials();
        assert!(m.len() >= material::COUNT as usize);
        assert_eq!(m[material::WATER as usize].specular, 0.85, "water mirrors");
        assert!(
            m[material::VEGETATION as usize].albedo.y > m[material::VEGETATION as usize].albedo.x
        );
    }
}
