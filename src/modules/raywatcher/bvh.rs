//! The scene the rays travel through: a compact triangle soup, a bounding
//! volume hierarchy over it, and the CPU traversal the Software path runs and
//! the Hardware path's BLAS is built from.
//!
//! The renderer already has the world as GPU meshes (terrain, far trees, grass,
//! scenery, water). Rather than invent a second world description, the module
//! harvests those meshes into [`SceneGeometry`] each time the resident set
//! changes — the same vertices, the same transforms, the same materials — and
//! builds one BVH the ray pass can walk.
//!
//! Two properties matter more than speed here:
//!
//! * **Determinism.** The build order, the split axis and the leaf packing
//!   depend only on the input, so two runs of the same frame produce the same
//!   tree and the tests can pin the traversal against a brute-force loop.
//! * **Flat output.** The tree is stored as a single node array with explicit
//!   child indices; the GPU compute path uploads it verbatim, so a tree that
//!   verifies here is the tree the shader walks.

use glam::Vec3;

/// One triangle in world space, with the material the ray pass shades with.
///
/// The vertex attributes are the ones the raster shader already has, reduced to
/// what transport needs: position, shading normal, and a packed material index
/// into [`SceneGeometry::materials`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RayTriangle {
    pub a: [f32; 4],
    /// `w` carries the triangle's material index as a float; the shader reads it
    /// back with `as u32`, so a 24-bit float mantissa is far more materials than
    /// the world has.
    pub b: [f32; 4],
    pub c: [f32; 4],
    pub normal: [f32; 4],
}

impl RayTriangle {
    pub fn new(a: Vec3, b: Vec3, c: Vec3, normal: Vec3, material: u32) -> Self {
        Self {
            a: [a.x, a.y, a.z, 0.],
            b: [b.x, b.y, b.z, material as f32],
            c: [c.x, c.y, c.z, 0.],
            normal: [normal.x, normal.y, normal.z, 0.],
        }
    }

    pub fn material(&self) -> u32 {
        self.b[3] as u32
    }

    /// Twice the area, which is also the length of the un-normalised cross
    /// product: the build uses it to pick a dominant axis.
    pub fn area2(&self) -> f32 {
        let a = Vec3::from_slice(&self.a);
        let b = Vec3::new(self.b[0], self.b[1], self.b[2]);
        let c = Vec3::new(self.c[0], self.c[1], self.c[2]);
        (b - a).cross(c - a).length()
    }

    pub fn centroid(&self) -> Vec3 {
        Vec3::from_slice(&self.a)
            + Vec3::new(self.b[0], self.b[1], self.b[2])
            + Vec3::new(self.c[0], self.c[1], self.c[2])
    }
    pub fn centre(&self) -> Vec3 {
        self.centroid() / 3.
    }

    pub fn barycentric(&self, point: Vec3) -> [f32; 3] {
        let a = Vec3::from_slice(&self.a);
        let b = Vec3::new(self.b[0], self.b[1], self.b[2]);
        let c = Vec3::new(self.c[0], self.c[1], self.c[2]);
        let v0 = b - a;
        let v1 = c - a;
        let v2 = point - a;
        let d00 = v0.dot(v0);
        let d01 = v0.dot(v1);
        let d11 = v1.dot(v1);
        let d20 = v2.dot(v0);
        let d21 = v2.dot(v1);
        let denom = d00 * d11 - d01 * d01;
        if !denom.is_finite() || denom.abs() < 1e-20 {
            return [1., 0., 0.];
        }
        let v = (d11 * d20 - d01 * d21) / denom;
        let w = (d00 * d21 - d01 * d20) / denom;
        [1. - v - w, v, w]
    }
}

/// A shading material, mirroring the raster shader's per-vertex lighting inputs
/// closely enough that the ray-traced world and the raster world agree where
/// they meet (the sky, the page sprites, the UI overlay).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RayMaterial {
    /// Linear albedo, the vertex colour with its per-kind tint already applied.
    pub albedo: [f32; 4],
    /// `x` roughness, `y` metalness, `z` emission, `w` opacity.
    pub surface: [f32; 4],
}

impl Default for RayMaterial {
    fn default() -> Self {
        // A neutral dielectric: the fallback that keeps an unmapped mesh visibly
        // shaded instead of returning black.
        Self {
            albedo: [0.6, 0.6, 0.6, 1.],
            surface: [0.8, 0., 0., 1.],
        }
    }
}

/// A node in the flat BVH. The layout is the classic two-array form: a leaf
/// stores a first-triangle index and a count, an interior node stores the index
/// of its right child and the left child is always the next node.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BvhNode {
    pub min: [f32; 4],
    /// `w` is the right-child index for an interior node and the first triangle
    /// for a leaf.
    pub max: [f32; 4],
}

impl BvhNode {
    pub const LEAF_FLAG: f32 = -1.;
    /// A node is a leaf when `min.w >= 0` and its triangle count is `min.w`.
    pub fn leaf_count(&self) -> Option<u32> {
        (self.min[3] >= 0.).then_some(self.min[3] as u32)
    }
    pub fn right_child(&self) -> u32 {
        self.max[3] as u32
    }
    pub fn is_leaf(&self) -> bool {
        self.min[3] >= 0.
    }
    pub fn bounds(&self) -> Bounds {
        Bounds {
            min: Vec3::new(self.min[0], self.min[1], self.min[2]),
            max: Vec3::new(self.max[0], self.max[1], self.max[2]),
        }
    }
    /// Slab test against this node's box; see [`Bounds::hit`].
    pub fn hit(&self, origin: Vec3, dir: Vec3, t_min: f32, t_max: f32) -> Option<f32> {
        self.bounds().hit(origin, dir, t_min, t_max)
    }
}

/// Bounds of a scene, kept as a pair of `Vec3` so the uploader can copy them
/// into a uniform without re-deriving anything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl Bounds {
    pub const EMPTY: Bounds = Bounds {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    pub fn extend(&mut self, point: Vec3) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }

    pub fn union(&mut self, other: &Bounds) {
        self.extend(other.min);
        self.extend(other.max);
    }

    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Self {
        let mut b = Self::EMPTY;
        for p in points {
            b.extend(p);
        }
        b
    }

    /// Slab test. Returns the entry distance along `dir` when the ray hits the
    /// box in front of its origin. Degenerate directions are handled by the
    /// division-free epsilon form, which is what makes this safe for the
    /// traverser's axis-aligned shadow rays.
    pub fn hit(&self, origin: Vec3, dir: Vec3, t_min: f32, t_max: f32) -> Option<f32> {
        let mut t0 = t_min;
        let mut t1 = t_max;
        for axis in 0..3 {
            let o = origin[axis];
            let d = dir[axis];
            let lo = self.min[axis];
            let hi = self.max[axis];
            if d.abs() < 1e-12 {
                if o < lo || o > hi {
                    return None;
                }
                continue;
            }
            let inv = 1. / d;
            let mut near = (lo - o) * inv;
            let mut far = (hi - o) * inv;
            if near > far {
                std::mem::swap(&mut near, &mut far);
            }
            t0 = t0.max(near);
            t1 = t1.min(far);
            if t0 > t1 {
                return None;
            }
        }
        Some(t0)
    }

    pub fn centre(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn half_extent(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }
}

/// A hit, in the form the shading pass consumes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// Distance along the ray, in world units.
    pub t: f32,
    pub triangle: u32,
    /// Perspective-correct barycentrics `(a, b, c)` of the hit point.
    pub bary: [f32; 3],
    /// Geometric normal of the triangle, oriented against the ray.
    pub normal: Vec3,
}

impl Hit {
    /// The hit point, from the triangle and the barycentrics the traversal
    /// already computed. Callers need it because a hit carries no position of
    /// its own: storing one per triangle test would be a wasted write in the
    /// shader.
    pub fn point(&self, triangles: &[RayTriangle]) -> Vec3 {
        let Some(tri) = triangles.get(self.triangle as usize) else {
            return Vec3::ZERO;
        };
        let a = Vec3::from_slice(&tri.a);
        let b = Vec3::new(tri.b[0], tri.b[1], tri.b[2]);
        let c = Vec3::new(tri.c[0], tri.c[1], tri.c[2]);
        a * self.bary[0] + b * self.bary[1] + c * self.bary[2]
    }
}

/// Maximum leaf size before a node is split further. Four matches the
/// traverser's unrolled leaf loop.
pub const MAX_LEAF: usize = 4;
/// Maximum tree depth. A pathological input (all centroids coincident) stops
/// splitting here and keeps a large leaf rather than recursing forever.
pub const MAX_DEPTH: u32 = 40;
/// Traversal stack depth. Both walks push at most one node per level (the
/// any-hit walk two), so twice the depth cap is the hard bound.
pub const STACK_DEPTH: usize = (MAX_DEPTH as usize) * 2;

/// The world, in ray-tracing form.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneGeometry {
    pub triangles: Vec<RayTriangle>,
    pub materials: Vec<RayMaterial>,
    pub nodes: Vec<BvhNode>,
    /// Triangle indices, reordered so every leaf's triangles are contiguous.
    pub order: Vec<u32>,
    pub bounds: Bounds,
    /// Depth actually reached by the build; reported by the module's stats line.
    pub depth: u32,
    /// Highest number of triangles in a leaf.
    pub widest_leaf: u32,
}

impl Default for Bounds {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl SceneGeometry {
    /// Build a BVH over `triangles`.
    ///
    /// The split is the surface-area heuristic's cheap cousin: each axis is
    /// sorted by centroid and cut at the median, then the axis whose two halves
    /// overlap least is chosen. For a heightfield world that is enough — the
    /// terrain's cells are already spatially coherent — and it keeps the build
    /// fast enough to rerun when the resident set changes without a hitch.
    pub fn build(triangles: Vec<RayTriangle>, materials: Vec<RayMaterial>) -> Self {
        let mut geometry = Self {
            triangles,
            materials,
            nodes: Vec::new(),
            order: Vec::new(),
            bounds: Bounds::EMPTY,
            depth: 0,
            widest_leaf: 0,
        };
        if geometry.triangles.is_empty() {
            return geometry;
        }
        for tri in &geometry.triangles {
            geometry.bounds.extend(Vec3::from_slice(&tri.a));
            geometry
                .bounds
                .extend(Vec3::new(tri.b[0], tri.b[1], tri.b[2]));
            geometry
                .bounds
                .extend(Vec3::new(tri.c[0], tri.c[1], tri.c[2]));
        }
        let centroids: Vec<Vec3> = geometry.triangles.iter().map(|t| t.centre()).collect();
        let mut index: Vec<u32> = (0..geometry.triangles.len() as u32).collect();
        geometry.order = index.clone();
        // Reserve a power-of-two node array like a real builder would; the
        // vector still grows if the tree is deeper than the guess.
        geometry
            .nodes
            .reserve(geometry.triangles.len().saturating_mul(2).clamp(1, 1 << 22));
        let mut state = BuildState {
            tris: &geometry.triangles,
            centroids: &centroids,
            nodes: Vec::new(),
            order: Vec::new(),
            depth: 0,
            widest: 0,
        };
        state.build_node(&mut index, 0);
        geometry.nodes = state.nodes;
        geometry.order = state.order;
        geometry.depth = state.depth;
        geometry.widest_leaf = state.widest;
        geometry
    }

    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// Triangles in the tree.
    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    /// The triangle soup, for a caller that wants to upload or inspect it.
    pub fn triangles(&self) -> &[RayTriangle] {
        &self.triangles
    }

    /// The material table the triangles index into.
    pub fn materials(&self) -> &[RayMaterial] {
        &self.materials
    }

    /// The flat node array, root first.
    pub fn nodes(&self) -> &[BvhNode] {
        &self.nodes
    }

    /// The leaf-ordered triangle index list.
    pub fn order(&self) -> &[u32] {
        &self.order
    }

    /// Traverse one ray. This is the reference implementation the compute
    /// shader mirrors signature for signature.
    ///
    /// The traversal is the standard explicit-stack form: a node is tested
    /// against the ray when it is *popped*, so the stack only ever holds nodes
    /// whose box the ray already hit. Children are pushed near-first so the
    /// near subtree is popped first, which tightens `best_t` before the far box
    /// is tested and prunes most of it.
    pub fn closest_hit(&self, origin: Vec3, dir: Vec3, t_min: f32, t_max: f32) -> Option<Hit> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut best: Option<Hit> = None;
        let mut best_t = t_max;
        let mut stack = [0u32; STACK_DEPTH];
        let mut sp = 1usize;
        stack[0] = 0;
        while sp > 0 {
            sp -= 1;
            let node_index = stack[sp];
            let node = &self.nodes[node_index as usize];
            if node.hit(origin, dir, t_min, best_t).is_none() {
                continue;
            }
            if let Some(count) = node.leaf_count() {
                let first = node.max[3] as u32;
                for i in 0..count {
                    let tri_index = self.order[(first + i) as usize];
                    let tri = &self.triangles[tri_index as usize];
                    if let Some((t, bary)) = intersect_triangle(tri, origin, dir, t_min, best_t) {
                        best_t = t;
                        let geo = triangle_normal(tri).normalize_or_zero();
                        let normal = if geo.dot(dir) > 0. { -geo } else { geo };
                        best = Some(Hit {
                            t,
                            triangle: tri_index,
                            bary,
                            normal,
                        });
                    }
                }
            } else {
                let left = node_index + 1;
                let right = node.max[3] as u32;
                let lh = self.nodes[left as usize].hit(origin, dir, t_min, best_t);
                let rh = self.nodes[right as usize].hit(origin, dir, t_min, best_t);
                // Push the far child first so the near one is popped next. A box
                // the ray misses is never pushed at all.
                let (near, far) = match (lh, rh) {
                    (None, None) => continue,
                    (Some(_), None) => (left, None),
                    (None, Some(_)) => (right, None),
                    (Some(lt), Some(rt)) => {
                        if lt <= rt {
                            (left, Some(right))
                        } else {
                            (right, Some(left))
                        }
                    }
                };
                if let Some(far) = far {
                    if sp < STACK_DEPTH {
                        stack[sp] = far;
                        sp += 1;
                    }
                }
                if sp < STACK_DEPTH {
                    stack[sp] = near;
                    sp += 1;
                }
            }
        }
        best
    }

    /// Any-hit traversal for shadow rays: returns as soon as one triangle is
    /// between `origin` and `t_max`.
    pub fn occluded(&self, origin: Vec3, dir: Vec3, t_min: f32, t_max: f32) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let mut stack = [0u32; STACK_DEPTH];
        let mut sp = 1usize;
        stack[0] = 0;
        while sp > 0 {
            sp -= 1;
            let node_index = stack[sp];
            let node = &self.nodes[node_index as usize];
            if node.hit(origin, dir, t_min, t_max).is_none() {
                continue;
            }
            if let Some(count) = node.leaf_count() {
                let first = node.max[3] as u32;
                for i in 0..count {
                    let tri_index = self.order[(first + i) as usize];
                    if intersect_triangle(
                        &self.triangles[tri_index as usize],
                        origin,
                        dir,
                        t_min,
                        t_max,
                    )
                    .is_some()
                    {
                        return true;
                    }
                }
            } else {
                if sp + 2 > STACK_DEPTH {
                    // Overflow is impossible for a tree this shallow; returning
                    // "occluded" would be a wrong shadow rather than a slow one,
                    // so fall back to testing what still fits.
                    continue;
                }
                stack[sp] = node_index + 1;
                stack[sp + 1] = node.max[3] as u32;
                sp += 2;
            }
        }
        false
    }
}

struct BuildState<'a> {
    tris: &'a [RayTriangle],
    centroids: &'a [Vec3],
    nodes: Vec<BvhNode>,
    order: Vec<u32>,
    depth: u32,
    widest: u32,
}

impl<'a> BuildState<'a> {
    /// Build the subtree over `partition` and return the node index of its root.
    ///
    /// `partition` is `&mut [u32]` of triangle indices this subtree owns; the
    /// node is appended first and then its children are appended in order, so
    /// "left child = node + 1" holds.
    fn build_node(&mut self, partition: &mut [u32], depth: u32) -> u32 {
        let node_index = self.nodes.len() as u32;
        let mut bounds = Bounds::EMPTY;
        for &i in partition.iter() {
            let tri = &self.tris[i as usize];
            bounds.extend(Vec3::from_slice(&tri.a));
            bounds.extend(Vec3::new(tri.b[0], tri.b[1], tri.b[2]));
            bounds.extend(Vec3::new(tri.c[0], tri.c[1], tri.c[2]));
        }
        self.depth = self.depth.max(depth);
        // Placeholder; the leaf branch fills it in and the interior branch
        // rewrites it once the right child's index is known.
        self.nodes.push(BvhNode {
            min: [
                bounds.min.x,
                bounds.min.y,
                bounds.min.z,
                partition.len() as f32,
            ],
            max: [bounds.max.x, bounds.max.y, bounds.max.z, 0.],
        });
        if partition.len() <= MAX_LEAF || depth >= MAX_DEPTH {
            let first = self.order.len() as u32;
            self.order.extend_from_slice(partition);
            self.widest = self.widest.max(partition.len() as u32);
            self.nodes[node_index as usize] = BvhNode {
                min: [
                    bounds.min.x,
                    bounds.min.y,
                    bounds.min.z,
                    partition.len() as f32,
                ],
                max: [bounds.max.x, bounds.max.y, bounds.max.z, first as f32],
            };
            return node_index;
        }
        // Split at the median of the widest centroid axis. Ties resolve to X,
        // then Y, then Z, so the tree is reproducible.
        let mut axis = 0usize;
        let mut best = f32::NEG_INFINITY;
        for a in 0..3 {
            let spread = bounds.half_extent()[a];
            if spread > best {
                best = spread;
                axis = a;
            }
        }
        let mid = partition.len() / 2;
        partition.select_nth_unstable_by(mid, |&l, &r| {
            let lc = self.centroids[l as usize][axis];
            let rc = self.centroids[r as usize][axis];
            lc.total_cmp(&rc).then_with(|| l.cmp(&r))
        });
        let (left, right) = partition.split_at_mut(mid);
        self.build_node(left, depth + 1);
        let right_child = self.build_node(right, depth + 1);
        self.nodes[node_index as usize] = BvhNode {
            min: [
                bounds.min.x,
                bounds.min.y,
                bounds.min.z,
                -1., // interior
            ],
            max: [bounds.max.x, bounds.max.y, bounds.max.z, right_child as f32],
        };
        node_index
    }
}

/// Möller–Trumbore, returning the distance and the barycentrics.
pub fn intersect_triangle(
    tri: &RayTriangle,
    origin: Vec3,
    dir: Vec3,
    t_min: f32,
    t_max: f32,
) -> Option<(f32, [f32; 3])> {
    let a = Vec3::from_slice(&tri.a);
    let b = Vec3::new(tri.b[0], tri.b[1], tri.b[2]);
    let c = Vec3::new(tri.c[0], tri.c[1], tri.c[2]);
    let e1 = b - a;
    let e2 = c - a;
    let p = dir.cross(e2);
    let det = e1.dot(p);
    // Two-sided by construction: the world's pages and grass are single-sided
    // quads and must still occlude from behind.
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = 1. / det;
    let tv = origin - a;
    let u = tv.dot(p) * inv;
    if !(-1e-6..=1.000_001).contains(&u) {
        return None;
    }
    let q = tv.cross(e1);
    let v = dir.dot(q) * inv;
    if v < -1e-6 || u + v > 1.000_001 {
        return None;
    }
    let t = e2.dot(q) * inv;
    if t < t_min || t > t_max {
        return None;
    }
    Some((t, [1. - u - v, u, v]))
}

pub fn triangle_normal(tri: &RayTriangle) -> Vec3 {
    let a = Vec3::from_slice(&tri.a);
    let b = Vec3::new(tri.b[0], tri.b[1], tri.b[2]);
    let c = Vec3::new(tri.c[0], tri.c[1], tri.c[2]);
    (b - a).cross(c - a)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic scatter of triangles plus one big quad: an easy way to
    /// compare the tree against brute force without a real world.
    fn scattered(count: u32) -> Vec<RayTriangle> {
        let mut tris = Vec::new();
        let mut seed = 0x9e3779b9u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2. - 1.
        };
        for i in 0..count {
            let centre = Vec3::new(next() * 40., next() * 8., next() * 40.);
            let scale = 0.2 + (i % 7) as f32 * 0.05;
            tris.push(RayTriangle::new(
                centre + Vec3::new(0., scale, 0.),
                centre + Vec3::new(scale, 0., scale * 0.5),
                centre + Vec3::new(-scale, 0., scale),
                Vec3::Y,
                i % 5,
            ));
        }
        // A ground quad, so a downward shadow ray always has something to hit.
        tris.push(RayTriangle::new(
            Vec3::new(-100., -1., -100.),
            Vec3::new(100., -1., -100.),
            Vec3::new(100., -1., 100.),
            Vec3::Y,
            0,
        ));
        tris.push(RayTriangle::new(
            Vec3::new(-100., -1., -100.),
            Vec3::new(100., -1., 100.),
            Vec3::new(-100., -1., 100.),
            Vec3::Y,
            0,
        ));
        tris
    }

    fn brute_force(
        tris: &[RayTriangle],
        origin: Vec3,
        dir: Vec3,
        t_min: f32,
        t_max: f32,
    ) -> Option<(f32, u32)> {
        let mut best: Option<(f32, u32)> = None;
        for (i, tri) in tris.iter().enumerate() {
            if let Some((t, _)) = intersect_triangle(tri, origin, dir, t_min, t_max) {
                if best.map_or(true, |(bt, _)| t < bt) {
                    best = Some((t, i as u32));
                }
            }
        }
        best
    }

    #[test]
    fn the_tree_agrees_with_brute_force_on_a_thousand_rays() {
        let tris = scattered(400);
        let scene = SceneGeometry::build(tris.clone(), vec![]);
        assert_eq!(scene.triangles.len(), tris.len());
        assert!(scene.nodes.len() >= 2);
        let mut seed = 12345u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2. - 1.
        };
        let mut hits = 0;
        for _ in 0..1000 {
            let origin = Vec3::new(next() * 45., next() * 10. + 4., next() * 45.);
            let dir = Vec3::new(next(), next(), next()).normalize();
            let traced = scene.closest_hit(origin, dir, 0., 1e4);
            let reference = brute_force(&tris, origin, dir, 0., 1e4);
            match (traced, reference) {
                (None, None) => {}
                (Some(h), Some((t, i))) => {
                    hits += 1;
                    assert_eq!(h.triangle, i, "nearest triangle");
                    assert!((h.t - t).abs() < 1e-3, "distance {} vs {}", h.t, t);
                }
                (a, b) => panic!("tree and brute force disagree: {a:?} vs {b:?}"),
            }
        }
        assert!(hits > 100, "the test rays must actually hit ({hits})");
    }

    #[test]
    fn occlusion_matches_the_closest_hit_query() {
        let tris = scattered(200);
        let scene = SceneGeometry::build(tris.clone(), vec![]);
        let mut seed = 777u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2. - 1.
        };
        for _ in 0..300 {
            let origin = Vec3::new(next() * 30., next() * 3. + 2., next() * 30.);
            let dir = Vec3::new(next() * 0.3, -1., next() * 0.3).normalize();
            let occluded = scene.occluded(origin, dir, 1e-3, 1e4);
            assert_eq!(
                occluded,
                scene.closest_hit(origin, dir, 1e-3, 1e4).is_some(),
                "shadow ray {origin:?} {dir:?}"
            );
        }
    }

    #[test]
    fn a_shadow_ray_through_a_gap_is_unoccluded() {
        // Two separate panels with a gap; a ray through the gap must miss both,
        // which is the case a broken traversal (one that tests the wrong leaf)
        // gets wrong.
        let panel = |x: f32| {
            vec![
                RayTriangle::new(
                    Vec3::new(x, 0., 0.),
                    Vec3::new(x, 4., 0.),
                    Vec3::new(x, 4., 4.),
                    Vec3::X,
                    0,
                ),
                RayTriangle::new(
                    Vec3::new(x, 0., 0.),
                    Vec3::new(x, 4., 4.),
                    Vec3::new(x, 0., 4.),
                    Vec3::X,
                    0,
                ),
            ]
        };
        let mut tris = panel(-1.);
        tris.extend(panel(10.));
        let scene = SceneGeometry::build(tris, vec![]);
        // Along the gap between the two panels: both panels face X, so a ray
        // travelling in Z through x = 5 cannot reach either.
        assert!(!scene.occluded(Vec3::new(5., 2., 2.), Vec3::Z, 1e-3, 100.));
        assert!(scene
            .closest_hit(Vec3::new(5., 2., 2.), Vec3::Z, 1e-3, 100.)
            .is_none());
        // Straight into a panel.
        assert!(scene.occluded(Vec3::new(-5., 2., 2.), Vec3::X, 1e-3, 100.));
        // A ray that stops short of the panel is not occluded, which is what
        // makes the light radius's soft shadows usable.
        assert!(!scene.occluded(Vec3::new(-5., 2., 2.), Vec3::X, 1e-3, 3.5));
    }

    #[test]
    fn the_tree_reports_its_own_shape_and_the_numbers_are_sane() {
        let scene = SceneGeometry::build(scattered(64), vec![]);
        assert!(
            scene.depth >= 2,
            "a 66-triangle scene needs multiple levels"
        );
        assert!(scene.depth <= MAX_DEPTH);
        assert!(scene.widest_leaf as usize <= MAX_LEAF);
        assert!(scene.widest_leaf >= 1);
        // Every triangle appears exactly once in the leaf order, which is what
        // the GPU upload relies on.
        let mut seen = scene.order.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), scene.triangles.len());
        assert!(scene.nodes.len() > 1);
        // Every interior node points at a real right child and the left child is
        // the next node, per the layout contract.
        for (i, node) in scene.nodes.iter().enumerate() {
            if !node.is_leaf() {
                assert!((node.right_child() as usize) < scene.nodes.len());
                assert_ne!(node.right_child() as usize, i);
                assert!((i + 1) < scene.nodes.len());
            }
        }
    }

    #[test]
    fn gpu_struct_layouts_match_wgsl_and_hlsl_stride() {
        assert_eq!(std::mem::size_of::<RayTriangle>(), 64);
        assert_eq!(std::mem::size_of::<BvhNode>(), 32);
        assert_eq!(std::mem::size_of::<RayMaterial>(), 32);
    }

    #[test]
    fn an_empty_scene_traces_nothing_instead_of_panicking() {
        let scene = SceneGeometry::build(vec![], vec![]);
        assert!(scene.is_empty());
        assert!(scene.nodes.is_empty());
        assert!(!scene.occluded(Vec3::ZERO, Vec3::Y, 0., 10.));
        assert!(scene.closest_hit(Vec3::ZERO, Vec3::Y, 0., 10.).is_none());
        assert!(scene.bounds.is_empty());
    }

    #[test]
    fn coincident_triangles_stop_splitting_at_the_depth_cap() {
        // Every centroid identical: a median split makes no progress, so the
        // depth cap has to end the recursion or the build never returns.
        let tris: Vec<_> = (0..64)
            .map(|_| {
                RayTriangle::new(
                    Vec3::new(0., 0., 0.),
                    Vec3::new(1., 0., 0.),
                    Vec3::new(0., 1., 0.),
                    Vec3::Z,
                    0,
                )
            })
            .collect();
        let scene = SceneGeometry::build(tris, vec![]);
        assert_eq!(scene.order.len(), 64);
        assert!(scene.depth >= 1);
        // The hit is still found.
        let hit = scene
            .closest_hit(Vec3::new(0.25, 0.25, 5.), -Vec3::Z, 0., 100.)
            .expect("hit");
        assert!((hit.t - 5.).abs() < 1e-3);
    }

    #[test]
    fn barycentrics_reconstruct_the_hit_point() {
        let tri = RayTriangle::new(
            Vec3::new(0., 0., 0.),
            Vec3::new(2., 0., 0.),
            Vec3::new(0., 2., 0.),
            Vec3::Z,
            3,
        );
        let (t, bary) =
            intersect_triangle(&tri, Vec3::new(0.25, 0.25, 4.), -Vec3::Z, 0., 100.).expect("hit");
        assert!((t - 4.).abs() < 1e-5);
        let a = Vec3::ZERO;
        let b = Vec3::new(2., 0., 0.);
        let c = Vec3::new(0., 2., 0.);
        let point = a * bary[0] + b * bary[1] + c * bary[2];
        assert!((point - Vec3::new(0.25, 0.25, 0.)).length() < 1e-4);
        assert_eq!(tri.material(), 3);
        // And the helper agrees with the intersection's answer.
        let direct = tri.barycentric(Vec3::new(0.25, 0.25, 0.));
        for i in 0..3 {
            assert!((direct[i] - bary[i]).abs() < 1e-4);
        }
    }

    #[test]
    fn the_normal_is_flipped_towards_the_ray_so_shading_never_sees_a_backface() {
        let tri = RayTriangle::new(
            Vec3::new(0., 0., 0.),
            Vec3::new(1., 0., 0.),
            Vec3::new(0., 0., 1.),
            Vec3::Y,
            0,
        );
        let scene = SceneGeometry::build(vec![tri], vec![]);
        // Downward ray hits the top face.
        let hit = scene
            .closest_hit(Vec3::new(0.2, 5., 0.2), -Vec3::Y, 0., 100.)
            .expect("top hit");
        assert!(hit.normal.y > 0.9, "normal {:?}", hit.normal);
        // Upward ray from below hits the same triangle and must see it flipped.
        let hit = scene
            .closest_hit(Vec3::new(0.2, -5., 0.2), Vec3::Y, 0., 100.)
            .expect("bottom hit");
        assert!(hit.normal.y < -0.9, "normal {:?}", hit.normal);
    }

    #[test]
    fn bounds_reject_rays_that_miss_and_accept_ones_that_hit() {
        let b = Bounds {
            min: Vec3::new(-1., -1., -1.),
            max: Vec3::new(1., 1., 1.),
        };
        assert!(b.hit(Vec3::new(0., 0., 5.), -Vec3::Z, 0., 100.).is_some());
        assert!(b.hit(Vec3::new(5., 5., 5.), -Vec3::Z, 0., 100.).is_none());
        // A ray parallel to a slab outside it misses; parallel inside it hits.
        assert!(b.hit(Vec3::new(0., 5., 5.), -Vec3::Z, 0., 100.).is_none());
        assert!(b.hit(Vec3::new(0., 0., 5.), -Vec3::Z, 0., 100.).is_some());
        // And the returned distance is the entry point, not the exit.
        let t = b
            .hit(Vec3::new(0., 0., 5.), -Vec3::Z, 0., 100.)
            .expect("hit");
        assert!((t - 4.).abs() < 1e-5, "t {t}");
    }

    #[test]
    fn material_indices_survive_the_tree_reordering() {
        // Every triangle carries its index as its material, so a traversal that
        // returns a triangle also tells us which slot it came from.
        let tris: Vec<_> = (0..40)
            .map(|i| {
                let x = i as f32;
                RayTriangle::new(
                    Vec3::new(x, 0., 0.),
                    Vec3::new(x + 0.9, 0., 0.),
                    Vec3::new(x, 0., 1.),
                    Vec3::Y,
                    i,
                )
            })
            .collect();
        let scene = SceneGeometry::build(tris, vec![]);
        for i in 0..40u32 {
            let hit = scene
                .closest_hit(Vec3::new(i as f32 + 0.2, 5., 0.2), -Vec3::Y, 0., 100.)
                .expect("hit");
            assert_eq!(scene.triangles[hit.triangle as usize].material(), i);
        }
    }
}
