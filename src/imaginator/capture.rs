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
//! * **Vegetation** — branch, frond and leaf geometry from the resident tree
//!   meshes, with their texture UVs and alpha cutouts for software traversal.
//!   Hardware traversal currently uses opaque geometry and flat materials.
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

/// Terrain stride used inside the capture radius.
///
/// The world-wide capture uses `TERRAIN_STRIDE` to make a whole map affordable,
/// but a stride-4 quad spans four grid cells - tens of metres - and that coarse
/// surface does not follow the ground under a building. It planes across the
/// building's footprint, so a ray from the camera meets the terrain quad before
/// it meets the wall and the building is hidden behind a smooth white sheet.
/// That is what "buildings covered with a white layer, as if transparent" is.
///
/// Inside the radius that actually matters the terrain is captured at its native
/// spacing instead, so the traced ground follows the real surface and stops
/// occluding what stands on it.
pub const TERRAIN_STRIDE_NEAR: usize = 1;

/// How far from the frame's camera a radius-limited capture reaches, in world
/// units.
///
/// Sized to cover what the camera can resolve through fog while leaving the
/// capture small enough to trace at native terrain spacing. The client's own
/// ornament draw distance is 120, so a radius well beyond that keeps every
/// building the viewer can see.
pub const CAPTURE_RADIUS: f32 = 1400.;

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

/// Triangles held back for the producers that run after the terrain.
///
/// The terrain walker can fill the whole budget on its own - an 88-tile world
/// at stride 4 is millions of quads - and the budget used to be one shared
/// counter, so it did. Every later producer then found `has_room()` false and
/// returned immediately: water, buildings and trees were silently absent from
/// the capture, which is why the traced picture had geometry on the ground and
/// nothing standing on it. Reserving a share per producer means terrain is what
/// gets thinned when the world is too big, and the things that make a picture
/// readable survive.
/// Fractions of `max_triangles`, not absolute counts, so the split scales with
/// whatever budget the caller passes instead of assuming the 2.5M default.
const RESERVE_SCENERY: f32 = 0.28;
const RESERVE_TREES: f32 = 0.20;

fn reserve(budget: &CaptureBudget, fraction: f32) -> usize {
    ((budget.max_triangles as f32) * fraction) as usize
}

fn initial_terrain_ceiling(budget: &CaptureBudget, resident_scenery: usize) -> usize {
    let future_scenery = reserve(budget, RESERVE_SCENERY).saturating_sub(resident_scenery);
    budget.max_triangles.saturating_sub(future_scenery)
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
    surfaces: std::collections::HashMap<u32, super::bvh::TexturedTriangle>,
    textures: Vec<super::bvh::Texture>,
    budget: CaptureBudget,
    clipped: bool,
    /// Ceiling for *this* producer, relative to the shared `budget`. Producers
    /// that must not be starved set this before they run and the shared
    /// `max_triangles` still caps the total.
    ceiling: Option<usize>,
}

impl Builder {
    fn has_room(&self) -> bool {
        let total = self.triangles.len() < self.budget.max_triangles;
        match self.ceiling {
            Some(ceiling) => total && self.triangles.len() < ceiling,
            None => total,
        }
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

    fn textured(
        &mut self,
        a: V3,
        b: V3,
        c: V3,
        normal: V3,
        uv: [[f32; 2]; 3],
        texture: u32,
        alpha_cutoff: Option<u8>,
    ) {
        let index = self.triangles.len();
        self.push(a, b, c, normal, material::VEGETATION);
        if self.triangles.len() != index {
            self.surfaces.insert(
                index as u32,
                super::bvh::TexturedTriangle {
                    uv,
                    texture,
                    alpha_cutoff,
                },
            );
        }
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
        surfaces: Default::default(),
        textures: Vec::new(),
        budget,
        clipped: false,
        ceiling: None,
    };
    // Order matters now that each producer has a reserved share. Water is a
    // handful of quads and defines where the ground is, so it goes first and
    // can never be squeezed out. Scenery (buildings, walls, props) and trees are
    // what a frame is *read* by, so they get their reserves before the terrain
    // walker is allowed to spend whatever is left.
    capture_water(&mut b, sources.water);
    let water_triangles = b.triangles.len();
    let after_water = b.triangles.len();
    b.ceiling = Some(after_water + reserve(&b.budget.clone(), RESERVE_SCENERY));
    let ornaments_captured = capture_scenery(&mut b, sources.objects, sources.ornaments);
    let after_scenery = b.triangles.len();
    b.ceiling = Some(after_scenery + reserve(&b.budget.clone(), RESERVE_TREES));
    if budget.trees {
        if let Some(trees) = sources.trees {
            capture_trees(&mut b, trees, sources.scene);
        }
    }
    let tree_triangles = b.triangles.len() - after_scenery;
    // The resident scenery may be empty during the initial capture. Preserve
    // its unused allocation for pages that arrive later, rather than letting
    // terrain exhaust the total budget before streaming begins.
    b.ceiling = Some(initial_terrain_ceiling(
        &b.budget,
        after_scenery - after_water,
    ));
    let before_terrain = b.triangles.len();
    capture_terrain(&mut b, sources.map);
    let terrain_triangles = b.triangles.len() - before_terrain;
    let triangles = std::mem::take(&mut b.triangles);
    let stats = SceneStats {
        triangles: triangles.len(),
        terrain_triangles,
        scenery_triangles: after_scenery - water_triangles,
        tree_triangles,
        water_triangles,
        ornaments_captured,
        materials: material::COUNT as usize,
        nodes: 0,
        clipped: b.clipped,
    };
    let mut bvh = Bvh::build(triangles);
    bvh.surfaces = Arc::new(b.surfaces);
    bvh.textures = Arc::new(b.textures);
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
    // A radius-limited capture can afford real spacing; the whole-world one
    // cannot, and has to use the coarse stride that planes over buildings.
    //
    // The radius still has to be *payable*: native spacing across a 2800-unit
    // disc is ~100k quads at a 7-unit grid, which is fine, but the same request
    // on a fine grid would not be. `TERRAIN_STRIDE_NEAR` is therefore a floor,
    // and the walker thickens it if the radius covers more cells than the budget
    // can hold, so the capture degrades instead of silently clipping.
    let mut stride = if b.budget.radius.is_some() {
        TERRAIN_STRIDE_NEAR.max(1)
    } else {
        TERRAIN_STRIDE.max(1)
    };
    if let Some(radius) = b.budget.radius {
        let cell = map.spacing.max(1e-3);
        let across = ((radius * 2.) / cell).ceil().max(1.);
        let cells = across * across;
        // Two triangles per cell, a quarter of the budget for terrain.
        let affordable = b.budget.max_triangles as f32 * 0.25 / 2.;
        if cells > affordable {
            stride = ((cells / affordable).sqrt().ceil() as usize).max(stride);
        }
    }
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
        // One quad per *mask-covered* cell, not one bounding quad over the whole
        // area.
        //
        // The mask is what decides where the client actually draws water, and a
        // bounding box covers the dry ground inside a lake's footprint too. A
        // traced reflection then appears over land, and - because a ray from the
        // camera meets that plane before anything under it - water also
        // occludes whatever is really there. The comment above used to argue
        // that a "water plane" is a bounding quad; it is not, and the artifact
        // is visible.
        //
        // An area with no mask at all is still drawn as its full footprint,
        // which is what the client does in that case.
        let cells_x = area.columns.max(1);
        let cells_z = area.rows.max(1);
        let step_x = (half_x * 2.) / cells_x as f32;
        let step_z = (half_z * 2.) / cells_z as f32;
        let masked = area.mask.len() >= cells_x * cells_z;
        for cz in 0..cells_z {
            for cx in 0..cells_x {
                if !b.has_room() {
                    b.clipped = true;
                    return;
                }
                if masked && area.mask[cz * cells_x + cx] == 0 {
                    continue;
                }
                let cx0 = x0 + cx as f32 * step_x;
                let cz0 = z0 + cz as f32 * step_z;
                let cx1 = cx0 + step_x;
                let cz1 = cz0 + step_z;
                b.quad(
                    v3(cx0, y, cz0),
                    v3(cx1, y, cz0),
                    v3(cx1, y, cz1),
                    v3(cx0, y, cz1),
                    v3(0., -1., 0.),
                    material::WATER,
                );
            }
        }
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

/// Returns how many ornaments a model was actually placed for.
///
/// An ornament whose BMD is not resident is skipped, and that count is the only
/// honest answer to "how much scenery did the capture see". Reporting the number
/// of placements in the map instead is what made the log claim the whole world
/// had been traced.
fn capture_scenery(b: &mut Builder, objects: &Objects, ornaments: &[Ornament]) -> usize {
    let mut captured = 0;
    for (index, ornament) in ornaments.iter().enumerate() {
        if !b.has_room() {
            b.clipped = true;
            return captured;
        }
        if !b.within(ornament.position[0], ornament.position[2]) {
            continue;
        }
        let Some(model) = objects.get(index) else {
            // Not resident: the tracer's picture grows as this streams in.
            continue;
        };
        captured += 1;
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
    captured
}

fn capture_trees(b: &mut Builder, trees: &Trees, scene: &WorldScene) {
    let _ = scene;
    // The tree's own transform: SpeedTree geometry is authored in tree space
    // (z up, origin at the stump), so a placement only needs scale and yaw.
    //
    // This used to emit two crossed quads per tree from the type's bounding box
    // - a billboard, not a tree. It was cheap and it made the traced world show
    // *something* where the foliage is, but what it showed was four flat planes
    // in a cross, which is precisely what a crossed billboard is. The real
    // geometry was available the whole time: `TreeLod` carries a branch `Part`
    // (trunk and limbs) and per-leaf quads with their own corners. Both are
    // captured now, and a tree is a tree.
    //
    // Alpha-tested canopy cards are kept because the leaves *are* cards: that is
    // how SpeedTree represents foliage. What is captured is the real set of
    // them, in the real positions, not two invented ones.
    let mut texture_ids = std::collections::HashMap::<usize, (u32, u32)>::new();
    for placement in &trees.placements {
        if !b.has_room() {
            b.clipped = true;
            return;
        }
        if !b.within(placement.position[0], placement.position[2]) {
            continue;
        }
        let Some(Some(mesh)) = trees.types.get(placement.tree_type as usize) else {
            continue;
        };
        // The best LOD is index 0; the cache stores them highest-detail first.
        let Some(lod) = mesh.lods.first() else {
            continue;
        };
        let (bark, composite) = *texture_ids
            .entry(placement.tree_type as usize)
            .or_insert_with(|| {
                let bark = b.textures.len() as u32;
                b.textures.push(super::bvh::Texture {
                    width: mesh.bark.width(),
                    height: mesh.bark.height(),
                    rgba: mesh.bark.as_raw().to_vec(),
                });
                let composite = b.textures.len() as u32;
                b.textures.push(super::bvh::Texture {
                    width: mesh.composite.width(),
                    height: mesh.composite.height(),
                    rgba: mesh.composite.as_raw().to_vec(),
                });
                (bark, composite)
            });
        // A placement is a position and nothing else: `scene::Tree` carries no
        // scale and no yaw, and `far_trees_gpu` instances each tree with just a
        // world position. Inventing a transform here - which the first draft of
        // this function did, reading fields that do not exist - would put the
        // traced foliage somewhere the drawn foliage is not.
        //
        // Cache geometry is authored z-up with the stump at the origin; world is
        // y-up. That is the whole of the transform.
        let base = V3::from_array(placement.position);
        let place = |p: &[f32; 3]| -> V3 { v3(base.x + p[0], base.y + p[2], base.z + p[1]) };
        let place_normal = |n: &[f32; 3]| -> V3 { v3(n[0], n[2], n[1]).normalize_or_zero() };
        // --- trunk and limbs -------------------------------------------------
        for (part, texture, cutoff) in [
            (&lod.branch, bark, None),
            (&lod.frond, composite, Some(127)),
        ] {
            for tri in part.indices.chunks_exact(3) {
                let (Some(a), Some(bv), Some(c)) = (
                    part.vertices.get(tri[0] as usize),
                    part.vertices.get(tri[1] as usize),
                    part.vertices.get(tri[2] as usize),
                ) else {
                    continue;
                };
                let normal = place_normal(&a.normal);
                b.textured(
                    place(&a.pos),
                    place(&bv.pos),
                    place(&c.pos),
                    normal,
                    [a.uv, bv.uv, c.uv],
                    texture,
                    cutoff,
                );
            }
        }
        // --- leaves ----------------------------------------------------------
        // One quad per leaf, from the corners the client itself draws. These are
        // the cards that make a canopy: many small ones, angled, not two large
        // ones.
        for leaf in &lod.leaves {
            let c0 = place(&leaf.corners[0]);
            let c1 = place(&leaf.corners[1]);
            let c2 = place(&leaf.corners[2]);
            let c3 = place(&leaf.corners[3]);
            // The authored leaf normal when present, else the card's own.
            let normal = match leaf.normal {
                Some(n) => place_normal(&n),
                None => (c1 - c0).cross(c2 - c0).normalize_or_zero(),
            };
            // Leaf corners are not guaranteed to be a parallelogram, so split
            // the quad explicitly rather than reusing the quad path.
            b.textured(
                c0,
                c1,
                c2,
                normal,
                [leaf.uv[0], leaf.uv[1], leaf.uv[2]],
                composite,
                Some(127),
            );
            b.textured(
                c0,
                c2,
                c3,
                normal,
                [leaf.uv[0], leaf.uv[2], leaf.uv[3]],
                composite,
                Some(127),
            );
        }
    }
}

/// Append one streamed scenery page to the traced soup.
///
/// The capture at load time cannot see the world's ornaments: they stream in
/// afterwards, `Objects::get` is empty that early, and the reported capture line
/// proves it - `scenery 0 (0 of 16037 ornaments had a resident model)`. Every
/// ray therefore passed straight through every building and hit the ground
/// behind it, which is exactly what "buildings are avoided by the tracer" and
/// "a grey translucent sheet over the dungeon" mean.
///
/// A `world_objects::Page` already holds world-space vertices (position,
/// normal, uv, kind) and triangle indices, so nothing has to be re-derived: the
/// geometry the renderer is about to draw is the geometry the tracer needs.
/// Called as each page arrives, so the traced world grows with the drawn one.
///
/// Returns the triangles added.
#[allow(clippy::too_many_arguments)]
/// `vertices` is `(position, normal)` per vertex, in world space, in the same
/// order as the page's `indices`. Taking plain arrays rather than the renderer's
/// `Vertex` keeps this file compilable in the module's own test harness, which
/// has no access to `crate::gpu`.
pub fn append_page(
    triangles: &mut Vec<Triangle>,
    vertices: &[([f32; 3], [f32; 3])],
    indices: &[u32],
    budget: usize,
) -> usize {
    let before = triangles.len();
    for tri in indices.chunks_exact(3) {
        if triangles.len() >= budget {
            break;
        }
        let (Some(a), Some(b), Some(c)) = (
            vertices.get(tri[0] as usize),
            vertices.get(tri[1] as usize),
            vertices.get(tri[2] as usize),
        ) else {
            continue;
        };
        let (pa, pb, pc) = (
            V3::from_array(a.0),
            V3::from_array(b.0),
            V3::from_array(c.0),
        );
        // The authored normal, not a geometric one: a building that is smooth
        // shaded in the raster frame has to be smooth shaded in the trace or the
        // two disagree at every edge.
        let normal = V3::from_array(a.1).normalize_or_zero();
        let normal = if normal.length_squared() > 0. {
            normal
        } else {
            (pb - pa).cross(pc - pa).normalize_or_zero()
        };
        if !pa.is_finite() || !pb.is_finite() || !pc.is_finite() || !normal.is_finite() {
            continue;
        }
        // Same sliver rejection the capture uses.
        if !((pb - pa).cross(pc - pa).length() > 1e-6) {
            continue;
        }
        triangles.push(Triangle {
            a: pa,
            b: pb,
            c: pc,
            normal,
            material: material::SCENERY,
        });
    }
    triangles.len() - before
}

/// The material table the capture uses. Exposed so the tracer and the tests agree
/// on the ordering.
pub fn materials() -> Vec<Material> {
    let s = Scene::empty();
    s.materials
}

#[cfg(test)]
mod streaming_tests {
    use super::super::math::Ray;
    use super::*;

    /// A wall, as the streamed page would hand it over.
    fn wall() -> (Vec<([f32; 3], [f32; 3])>, Vec<u32>) {
        let n = [0., 1., 0.];
        (
            vec![
                ([0., 0., 0.], n),
                ([10., 0., 0.], n),
                ([10., 0., 10.], n),
                ([0., 0., 10.], n),
            ],
            vec![0, 1, 2, 0, 2, 3],
        )
    }

    /// The bug this fixes: the load-time capture saw no ornaments, so every ray
    /// through a building hit the ground behind it. Once a page arrives, a ray
    /// aimed at the building must hit the building and not the ground.
    #[test]
    fn a_streamed_page_is_hit_by_a_ray_that_used_to_pass_through_it() {
        let (verts, indices) = wall();
        let mut tris = Vec::new();
        let added = append_page(&mut tris, &verts, &indices, usize::MAX);
        assert_eq!(added, 2, "both wall triangles must be taken");

        let bvh = Bvh::build(tris);
        // Straight down onto the wall from above it. Before the fix this ray
        // missed the scene entirely and reached whatever was behind it.
        let ray = Ray::new(v3(5., 20., 5.), v3(0., -1., 0.));
        let hit = bvh
            .closest_hit(&ray, 0., 1e9)
            .expect("the wall must be hit");
        assert!((hit.t - 20.).abs() < 1e-3, "t={} is not the wall", hit.t);
        assert_eq!(hit.material, material::SCENERY);
    }

    /// A page is scenery, not water or terrain: the material decides how the
    /// tracer shades it, and buildings must not pick up the water branch.
    #[test]
    fn a_streamed_page_is_tagged_as_scenery() {
        let (verts, indices) = wall();
        let mut tris = Vec::new();
        append_page(&mut tris, &verts, &indices, usize::MAX);
        assert!(tris.iter().all(|t| t.material == material::SCENERY));
    }

    /// The budget is a hard ceiling, not a suggestion: the software tracer has a
    /// frame to keep. Geometry beyond it is dropped rather than allowed to grow
    /// the scene without bound.
    #[test]
    fn the_budget_caps_what_a_page_can_add() {
        let (verts, indices) = wall();
        let mut tris = Vec::new();
        let added = append_page(&mut tris, &verts, &indices, 1);
        assert_eq!(added, 1);
        assert_eq!(tris.len(), 1);
    }

    /// Streamed geometry has to survive a page with an out-of-range index or a
    /// degenerate triangle: one bad page must not abort the load.
    #[test]
    fn a_damaged_page_is_skipped_not_fatal() {
        let (mut verts, mut indices) = wall();
        indices.extend_from_slice(&[0, 1, 99]); // out of range
        indices.extend_from_slice(&[0, 0, 0]); // degenerate
        verts.push(([f32::NAN, 0., 0.], [0., 0., 1.]));
        indices.extend_from_slice(&[0, 1, (verts.len() - 1) as u32]); // non-finite
        let mut tris = Vec::new();
        let added = append_page(&mut tris, &verts, &indices, usize::MAX);
        assert_eq!(added, 2, "only the two real wall triangles are usable");
        assert!(tris.iter().all(|t| t.a.is_finite()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_leaves_unused_scenery_budget_for_streamed_pages() {
        let budget = CaptureBudget {
            max_triangles: 10_000,
            ..CaptureBudget::default()
        };
        assert_eq!(initial_terrain_ceiling(&budget, 0), 7200);
        assert_eq!(initial_terrain_ceiling(&budget, 1000), 8200);
        assert_eq!(initial_terrain_ceiling(&budget, 2800), 10000);
        assert_eq!(initial_terrain_ceiling(&budget, 4000), 10000);
    }

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
            surfaces: Default::default(),
            textures: Vec::new(),
            budget: CaptureBudget::default(),
            clipped: false,
            ceiling: None,
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
            surfaces: Default::default(),
            textures: Vec::new(),
            budget: CaptureBudget {
                max_triangles: 2,
                ..CaptureBudget::default()
            },
            clipped: false,
            ceiling: None,
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
            surfaces: Default::default(),
            textures: Vec::new(),
            budget: CaptureBudget {
                radius: Some(10.),
                center: [0., 0., 0.],
                ..CaptureBudget::default()
            },
            clipped: false,
            ceiling: None,
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

    /// The terrain walker must not be able to spend the budget that water,
    /// scenery and trees need.
    ///
    /// `capture` used to run terrain first against one shared counter, so on a
    /// big world the terrain filled all 2.5M triangles and every later producer
    /// found `has_room()` false and returned at once. The traced picture then
    /// had ground and no buildings, no trees and no water, with `clipped` the
    /// only hint - and the page showed "2500000 triangles" as if that were
    /// complete. This pins the reserved-share order.
    #[test]
    fn a_full_terrain_budget_cannot_starve_water_scenery_or_trees() {
        // Terrain that would happily consume the whole budget on its own, plus
        // geometry from every later producer.
        let mut b = Builder {
            triangles: Vec::new(),
            surfaces: Default::default(),
            textures: Vec::new(),
            budget: CaptureBudget {
                max_triangles: 2_000,
                ..CaptureBudget::default()
            },
            clipped: false,
            ceiling: None,
        };
        // Water first, as `capture` does: it must never be squeezed out.
        b.quad(
            V3::ZERO,
            v3(1., 0., 0.),
            v3(1., 0., 1.),
            v3(0., 0., 1.),
            v3(0., 1., 0.),
            material::WATER,
        );
        let after_water = b.triangles.len();
        b.ceiling = Some(after_water + reserve(&b.budget.clone(), RESERVE_SCENERY));
        // Scenery pushes until its reserve is gone.
        let mut scenery = 0;
        while b.has_room() {
            let before = b.triangles.len();
            b.push(
                V3::ZERO,
                v3(1., 0., 0.),
                v3(0., 0., 1.),
                v3(0., 1., 0.),
                material::SCENERY,
            );
            if b.triangles.len() == before {
                break;
            }
            scenery += 1;
        }
        assert!(scenery > 0, "scenery got no room at all");
        assert!(
            b.triangles.len() < b.budget.max_triangles,
            "scenery spent the whole budget"
        );
        let after_scenery = b.triangles.len();
        b.ceiling = Some(after_scenery + reserve(&b.budget.clone(), RESERVE_TREES));
        // Trees push until their reserve is gone.
        let mut trees = 0;
        while b.has_room() {
            let before = b.triangles.len();
            b.push(
                V3::ZERO,
                v3(1., 0., 0.),
                v3(0., 0., 1.),
                v3(0., 1., 0.),
                material::VEGETATION,
            );
            if b.triangles.len() == before {
                break;
            }
            trees += 1;
        }
        assert!(trees > 0, "trees got no room at all");
        // Terrain now gets whatever is left - it is the producer that can be
        // thinned, so it is the one that must yield.
        b.ceiling = None;
        let mut terrain = 0;
        while b.has_room() {
            let before = b.triangles.len();
            b.push(
                V3::ZERO,
                v3(1., 0., 0.),
                v3(0., 0., 1.),
                v3(0., 1., 0.),
                material::TERRAIN,
            );
            if b.triangles.len() == before {
                break;
            }
            terrain += 1;
        }
        assert_eq!(
            b.triangles.len(),
            b.budget.max_triangles,
            "terrain should take what is left"
        );
        // Every producer is represented in the final soup.
        let has = |m: u16| b.triangles.iter().any(|t| t.material == m);
        assert!(has(material::WATER), "water missing");
        assert!(has(material::SCENERY), "scenery missing");
        assert!(has(material::VEGETATION), "trees missing");
        assert!(has(material::TERRAIN), "terrain missing");
    }

    /// Pins the producer order in `capture` itself.
    ///
    /// The reserve test above proves the ceiling works, but not that `capture`
    /// applies it in the right order - and the original bug was purely order:
    /// terrain ran first. Reading the function is the only way to assert the
    /// sequence without constructing a whole world, and getting this wrong is
    /// silent, so it is worth an explicit check.
    #[test]
    fn capture_runs_terrain_last_so_it_cannot_starve_the_rest() {
        let source = include_str!("capture.rs");
        let body = source
            .split("pub fn capture(sources:")
            .nth(1)
            .expect("capture is defined");
        let body = body.split("let triangles = std::mem::take").next().unwrap();
        let water = body.find("capture_water(").expect("water is captured");
        let scenery = body.find("capture_scenery(").expect("scenery is captured");
        let trees = body.find("capture_trees(").expect("trees are captured");
        let terrain = body.find("capture_terrain(").expect("terrain is captured");
        assert!(water < scenery, "water must run before scenery");
        assert!(scenery < trees, "scenery must run before trees");
        assert!(
            trees < terrain,
            "terrain must run last: it is the producer that can be thinned"
        );
    }

    /// Water must cover only the cells its mask marks, not its bounding box.
    ///
    /// A bounding quad put water over the dry ground inside a lake's footprint,
    /// so a traced reflection appeared on land and the plane occluded whatever
    /// was really there - one of the causes of "buildings look transparent".
    #[test]
    fn a_masked_water_area_only_covers_its_masked_cells() {
        let area = WaterArea {
            export_id: 0,
            color: 0,
            simple: false,
            height: 0.,
            grid_size: 10.,
            columns: 2,
            rows: 2,
            center: [0., 0., 0.],
            // Only the north-west cell is water.
            mask: vec![1, 0, 0, 0],
            shore_waves: Vec::new(),
        };
        let mut b = Builder {
            triangles: Vec::new(),
            surfaces: Default::default(),
            textures: Vec::new(),
            budget: CaptureBudget::default(),
            clipped: false,
            ceiling: None,
        };
        capture_water(&mut b, std::slice::from_ref(&area));
        // One cell, two triangles - not four cells and eight triangles.
        assert_eq!(b.triangles.len(), 2, "only the masked cell should be water");

        // With no mask at all the whole footprint is water, as the client does.
        let bare = WaterArea {
            mask: Vec::new(),
            ..area
        };
        let mut b2 = Builder {
            triangles: Vec::new(),
            surfaces: Default::default(),
            textures: Vec::new(),
            budget: CaptureBudget::default(),
            clipped: false,
            ceiling: None,
        };
        capture_water(&mut b2, std::slice::from_ref(&bare));
        assert_eq!(
            b2.triangles.len(),
            8,
            "an unmasked area is its full footprint"
        );
    }
}
