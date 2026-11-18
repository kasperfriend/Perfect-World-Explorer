//! The Imaginator's own scene: triangles, a material table and a BVH.
//!
//! Everything here is plain data with no engine types, so the whole file can be
//! compiled and tested with a bare `rustc --test`. The engine-side builder that
//! fills it in lives in `super::capture`.

use super::math::{v3, Aabb, Ray, V3};
use std::sync::Arc;

/// One surface class. The tracer needs only what decides how light and shadow
/// behave: a base colour, how much of the light is diffuse, and how a shadow ray
/// leaves the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub albedo: V3,
    /// Reflected share for the reflection pass (0 = no mirror term).
    pub specular: f32,
}

impl Material {
    pub const fn new(albedo: V3, specular: f32) -> Self {
        Self { albedo, specular }
    }
}

/// One triangle, already flattened into world space by `capture`.
#[derive(Clone, Copy, Debug)]
pub struct Triangle {
    pub a: V3,
    pub b: V3,
    pub c: V3,
    /// Unit geometric normal (or the authored vertex normal for terrain-like
    /// smooth shading, which the builder decides).
    pub normal: V3,
    pub material: u16,
}

impl Triangle {
    pub fn centroid(&self) -> V3 {
        (self.a + self.b + self.c) / 3.
    }

    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        b.grow(self.a);
        b.grow(self.b);
        b.grow(self.c);
        b
    }

    /// Möller-Trumbore. Returns the distance and the barycentrics `(u, v)`,
    /// accepting hits from both sides because world geometry is frequently
    /// single-sided and thin.
    pub fn intersect(&self, ray: &Ray, tmin: f32, tmax: f32) -> Option<(f32, [f32; 2])> {
        let e1 = self.b - self.a;
        let e2 = self.c - self.a;
        let p = ray.direction.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = 1. / det;
        let tvec = ray.origin - self.a;
        let u = tvec.dot(p) * inv;
        if !(-1e-6..=1.000001).contains(&u) {
            return None;
        }
        let q = tvec.cross(e1);
        let v = ray.direction.dot(q) * inv;
        if v < -1e-6 || u + v > 1.000001 {
            return None;
        }
        let t = e2.dot(q) * inv;
        if t < tmin || t > tmax {
            return None;
        }
        Some((t, [u, v]))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub bounds: Aabb,
    /// `0` for a leaf, otherwise the index of the second child; the first is
    /// always `self + 1`, which is how a compact layout is normally stored.
    pub left_first: u32,
    pub count: u32,
}

/// Binned SAH BVH over a triangle soup.
///
/// The build is breadth-first over a work list so `left_first` can be patched in
/// place, exactly like a two-children-plus-offset layout. Twelve bins per axis
/// keeps the cost of a rebuild acceptable while the scene is streamed in.
pub struct Bvh {
    pub nodes: Vec<Node>,
    /// Triangle indices, permuted so every leaf owns a contiguous range.
    pub order: Vec<u32>,
    pub triangles: Vec<Triangle>,
}

/// Depth bound for the traversal stack.
///
/// The builder always makes progress (binned or median split) and stops at
/// [`LEAF_SIZE`], so the depth is bounded by `log2(triangles)`; for the 2.5 M
/// triangle capture this is 20. 96 leaves an enormous margin while staying at
/// 384 bytes of stack per ray, which the per-frame stacks already have.
const MAX_STACK: usize = 96;

const BINS: usize = 12;
const LEAF_SIZE: usize = 4;

impl Bvh {
    pub fn build(triangles: Vec<Triangle>) -> Self {
        let count = triangles.len();
        let mut order: Vec<u32> = (0..count as u32).collect();
        if count == 0 {
            return Self {
                nodes: vec![Node {
                    bounds: Aabb::EMPTY,
                    left_first: 0,
                    count: 0,
                }],
                order,
                triangles,
            };
        }
        // Precompute centroids and bounds once; the builder reads them many
        // times and a soup of a few million triangles makes recomputation show.
        let centroids: Vec<V3> = triangles.iter().map(|t| t.centroid()).collect();
        let bounds: Vec<Aabb> = triangles.iter().map(|t| t.bounds()).collect();
        let mut nodes: Vec<Node> = vec![Node {
            bounds: Aabb::EMPTY,
            left_first: 0,
            count: 0,
        }];
        for b in &bounds {
            nodes[0].bounds.union(b);
        }
        // (node index, range) work items.
        let mut stack = vec![(0usize, 0usize, count)];
        while let Some((node_index, start, end)) = stack.pop() {
            let n = end - start;
            if n <= LEAF_SIZE {
                nodes[node_index].left_first = start as u32;
                nodes[node_index].count = n as u32;
                continue;
            }
            let mut centroid_bounds = Aabb::EMPTY;
            for i in start..end {
                centroid_bounds.grow(centroids[order[i] as usize]);
            }
            let extent = centroid_bounds.extent();
            let axis = extent.largest_axis();
            let mut split = None;
            if extent.component(axis) > 1e-9 {
                split = Self::partition_binned(
                    &mut order,
                    &centroids,
                    &bounds,
                    start,
                    end,
                    centroid_bounds.min.component(axis),
                    extent.component(axis),
                    axis,
                );
            }
            // Degenerate centroid spread (a stack of coincident geometry, or a
            // cluster that is all one triangle repeated): fall back to a sorted
            // median split, which always makes progress.
            let mid = match split {
                Some(m) => m,
                None => {
                    let slice = &mut order[start..end];
                    slice.sort_by(|a, b| {
                        let pa = centroids[*a as usize].component(axis);
                        let pb = centroids[*b as usize].component(axis);
                        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    start + n / 2
                }
            };
            let leaf_cost = n as f32 * Self::node_area(&nodes[node_index].bounds);
            let left_bounds = Self::range_bounds(&bounds, &order, start, mid);
            let right_bounds = Self::range_bounds(&bounds, &order, mid, end);
            let split_cost = left_bounds.surface_area() * (mid - start) as f32
                + right_bounds.surface_area() * (end - mid) as f32;
            if n > LEAF_SIZE && split_cost >= leaf_cost * 4. && n <= LEAF_SIZE * 2 {
                // The split does not pay for itself in a tiny node; keep a leaf
                // instead of paying two extra node traversals.
                nodes[node_index].left_first = start as u32;
                nodes[node_index].count = n as u32;
                continue;
            }
            let left = nodes.len();
            nodes.push(Node {
                bounds: left_bounds,
                left_first: 0,
                count: 0,
            });
            let right = nodes.len();
            nodes.push(Node {
                bounds: right_bounds,
                left_first: 0,
                count: 0,
            });
            nodes[node_index].left_first = left as u32;
            nodes[node_index].count = 0;
            stack.push((right, mid, end));
            stack.push((left, start, mid));
        }
        Self {
            nodes,
            order,
            triangles,
        }
    }

    fn node_area(bounds: &Aabb) -> f32 {
        bounds.surface_area().max(1e-6)
    }

    fn range_bounds(bounds: &[Aabb], order: &[u32], start: usize, end: usize) -> Aabb {
        let mut b = Aabb::EMPTY;
        for i in start..end {
            b.union(&bounds[order[i] as usize]);
        }
        b
    }

    /// One pass of binning over one axis. Returns the split position in
    /// `order`, or `None` when every bin ended up on one side.
    #[allow(clippy::too_many_arguments)]
    fn partition_binned(
        order: &mut [u32],
        centroids: &[V3],
        bounds: &[Aabb],
        start: usize,
        end: usize,
        min: f32,
        extent: f32,
        axis: usize,
    ) -> Option<usize> {
        let scale = BINS as f32 / extent;
        let mut bin_bounds = [Aabb::EMPTY; BINS];
        let mut bin_counts = [0usize; BINS];
        for i in start..end {
            let t = centroids[order[i] as usize].component(axis);
            let bin = (((t - min) * scale) as usize).min(BINS - 1);
            bin_counts[bin] += 1;
            bin_bounds[bin].union(&bounds[order[i] as usize]);
        }
        // Sweep both ways to get the cost of each of the BINS-1 planes.
        let mut left_area = [0.0f32; BINS - 1];
        let mut left_count = [0usize; BINS - 1];
        let mut acc = Aabb::EMPTY;
        let mut n = 0;
        for i in 0..BINS - 1 {
            acc.union(&bin_bounds[i]);
            n += bin_counts[i];
            left_area[i] = acc.surface_area();
            left_count[i] = n;
        }
        let mut right_area = [0.0f32; BINS - 1];
        let mut right_count = [0usize; BINS - 1];
        acc = Aabb::EMPTY;
        n = 0;
        for i in (1..BINS).rev() {
            acc.union(&bin_bounds[i]);
            n += bin_counts[i];
            right_area[i - 1] = acc.surface_area();
            right_count[i - 1] = n;
        }
        let mut best = f32::INFINITY;
        let mut best_plane = None;
        for i in 0..BINS - 1 {
            if left_count[i] == 0 || right_count[i] == 0 {
                continue;
            }
            let cost = left_area[i] * left_count[i] as f32 + right_area[i] * right_count[i] as f32;
            if cost < best {
                best = cost;
                best_plane = Some(i);
            }
        }
        let plane = best_plane?;
        // Stable in-place partition around the chosen plane.
        let mut i = start;
        let mut j = end;
        while i < j {
            let t = centroids[order[i] as usize].component(axis);
            let bin = (((t - min) * scale) as usize).min(BINS - 1);
            if bin <= plane {
                i += 1;
            } else {
                j -= 1;
                order.swap(i, j);
            }
        }
        if i == start || i == end {
            None
        } else {
            Some(i)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// Closest hit along `ray` inside `(tmin, tmax]`.
    ///
    /// The traversal stack lives on the caller's own stack rather than in a
    /// `Vec`. It used to be `Vec::with_capacity(64)` per ray: a full frame is
    /// millions of rays, so that was millions of `malloc`/`free` pairs, and
    /// profiling a 590x343 frame put the tracer at ~2 Mray/s. A fixed array sized
    /// for the deepest tree the builder can produce removes the allocation
    /// entirely; [`MAX_STACK`] is a hard bound because the builder splits until a
    /// leaf holds at most [`LEAF_SIZE`] triangles and the tree is balanced, so the
    /// depth is `log2(triangles)`, far under the cap for any world this loads.
    pub fn closest_hit(&self, ray: &Ray, tmin: f32, tmax: f32) -> Option<Hit> {
        if self.triangles.is_empty() {
            return None;
        }
        let mut best_t = tmax;
        let mut best: Option<Hit> = None;
        let mut stack = [0u32; MAX_STACK];
        let mut top = 0usize;
        stack[top] = 0;
        top += 1;
        while top > 0 {
            top -= 1;
            let index = stack[top];
            let node = &self.nodes[index as usize];
            if node.bounds.intersect(ray, best_t).is_none() {
                continue;
            }
            if node.count > 0 {
                for k in 0..node.count as usize {
                    let slot = node.left_first as usize + k;
                    let tri = &self.triangles[self.order[slot] as usize];
                    if let Some((t, bary)) = tri.intersect(ray, tmin, best_t) {
                        best_t = t;
                        best = Some(Hit {
                            t,
                            normal: tri.normal,
                            bary,
                            material: tri.material,
                            triangle: self.order[slot],
                        });
                    }
                }
            } else {
                // Children are stored adjacently and the tree is built by
                // splitting the further side first, so the *first* child is the
                // nearer one in the builder's own ordering only by accident. Push
                // the further child first so the nearer one is popped next: the
                // nearer box is the one that can tighten `best_t` before the
                // further subtree is visited, which lets that subtree be skipped.
                let left = node.left_first;
                let right = left + 1;
                let left_t = self.nodes[left as usize].bounds.intersect(ray, best_t);
                let right_t = self.nodes[right as usize].bounds.intersect(ray, best_t);
                match (left_t, right_t) {
                    (Some(l), Some(r)) => {
                        let (near, far) = if l <= r { (left, right) } else { (right, left) };
                        stack[top] = far;
                        top += 1;
                        stack[top] = near;
                        top += 1;
                    }
                    (Some(_), None) => {
                        stack[top] = left;
                        top += 1;
                    }
                    (None, Some(_)) => {
                        stack[top] = right;
                        top += 1;
                    }
                    (None, None) => {}
                }
            }
        }
        best
    }

    /// Any hit at all inside `(tmin, tmax)`, for shadow and ambient-occlusion
    /// rays. Stops at the first blocking triangle.
    pub fn any_hit(&self, ray: &Ray, tmin: f32, tmax: f32) -> bool {
        if self.triangles.is_empty() {
            return false;
        }
        // Shadow and occlusion rays are the majority of a frame's launches, and
        // they stop at the first blocker, so this stack matters even more than
        // the closest-hit one. See `closest_hit` for why it is an array.
        let mut stack = [0u32; MAX_STACK];
        let mut top = 0usize;
        stack[top] = 0;
        top += 1;
        while top > 0 {
            top -= 1;
            let index = stack[top];
            let node = &self.nodes[index as usize];
            if node.bounds.intersect(ray, tmax).is_none() {
                continue;
            }
            if node.count > 0 {
                for k in 0..node.count as usize {
                    let slot = node.left_first as usize + k;
                    let tri = &self.triangles[self.order[slot] as usize];
                    if tri.intersect(ray, tmin, tmax).is_some() {
                        return true;
                    }
                }
            } else {
                stack[top] = node.left_first;
                top += 1;
                stack[top] = node.left_first + 1;
                top += 1;
            }
        }
        false
    }

    pub fn bounds(&self) -> Aabb {
        self.nodes.first().map(|n| n.bounds).unwrap_or(Aabb::EMPTY)
    }

    /// Approximate resident bytes, for the module's status line.
    pub fn memory_bytes(&self) -> usize {
        self.nodes.len() * std::mem::size_of::<Node>()
            + self.order.len() * std::mem::size_of::<u32>()
            + self.triangles.len() * std::mem::size_of::<Triangle>()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub t: f32,
    pub normal: V3,
    pub bary: [f32; 2],
    pub material: u16,
    pub triangle: u32,
}

/// Immutable, reference-counted scene shared between the renderer and the
/// tracer thread. Replacement is cheap: the tracer keeps whatever snapshot it
/// was handed and the renderer swaps in a new one when geometry changes.
pub type SharedScene = Arc<Scene>;

#[derive(Default)]
pub struct Scene {
    pub bvh: Option<Bvh>,
    pub materials: Vec<Material>,
    /// Anything the tracer needs to describe itself in the status line.
    pub stats: SceneStats,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneStats {
    pub triangles: usize,
    /// Per-producer breakdown of what actually made it into the soup.
    ///
    /// The load-time log used to print `t.objects.ornaments.len()` and call it
    /// "resident ornaments" - the number of placements in the map, not the
    /// number that were loaded and captured. It read as though the whole world
    /// had been traced when `capture_scenery` had in fact skipped every model
    /// that was not resident yet. These counts are the honest ones.
    pub terrain_triangles: usize,
    pub scenery_triangles: usize,
    pub tree_triangles: usize,
    pub water_triangles: usize,
    /// Ornaments a model was actually placed for, not placements in the map.
    pub ornaments_captured: usize,
    pub materials: usize,
    pub nodes: usize,
    /// True when the capture clipped geometry (block window or distance), which
    /// the status line reports so a thin picture is explained rather than
    /// guessed at.
    pub clipped: bool,
}

/// Index of each built-in material in `Scene::empty`.
pub mod material {
    pub const DEFAULT: u16 = 0;
    pub const TERRAIN: u16 = 1;
    pub const WATER: u16 = 2;
    pub const SCENERY: u16 = 3;
    pub const VEGETATION: u16 = 4;
    pub const MODEL: u16 = 5;
    pub const COUNT: u16 = 6;
}

impl Scene {
    /// A scene carrying only the built-in materials, for a capture that has not
    /// found any geometry yet.
    pub fn with_builtin_materials() -> Self {
        Self::empty()
    }
    pub fn empty() -> Self {
        Self {
            bvh: None,
            materials: vec![
                Material::new(V3::splat(0.5), 0.),
                // Terrain: rough, slightly reflective when wet-looking.
                Material::new(V3::splat(0.22), 0.03),
                // Water: dark, strongly mirror-like.
                Material::new(v3(0.02, 0.05, 0.06), 0.85),
                // Static scenery (buildings, rocks): mid albedo, low gloss.
                Material::new(V3::splat(0.38), 0.05),
                // Vegetation: dark, matte, mostly translucent-looking.
                Material::new(v3(0.09, 0.14, 0.05), 0.01),
                // World models (NPCs, props).
                Material::new(V3::splat(0.34), 0.04),
            ],
            stats: SceneStats::default(),
        }
    }

    pub fn triangle_count(&self) -> usize {
        self.bvh.as_ref().map(|b| b.triangles.len()).unwrap_or(0)
    }

    pub fn memory_bytes(&self) -> usize {
        self.bvh.as_ref().map(|b| b.memory_bytes()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two quads at y = 0 and y = 5, the second only over x in [1, 2].
    fn scene() -> Bvh {
        let mut tris = Vec::new();
        for i in 0..10 {
            let x = i as f32;
            tris.push(Triangle {
                a: super::super::math::v3(x, 0., 0.),
                b: super::super::math::v3(x + 1., 0., 0.),
                c: super::super::math::v3(x, 0., 1.),
                normal: super::super::math::v3(0., 1., 0.),
                material: 0,
            });
            tris.push(Triangle {
                a: super::super::math::v3(x, 0., 0.),
                b: super::super::math::v3(x, 0., 1.),
                c: super::super::math::v3(x + 1., 0., 1.),
                normal: super::super::math::v3(0., 1., 0.),
                material: 0,
            });
        }
        tris.push(Triangle {
            a: super::super::math::v3(1., 5., 0.),
            b: super::super::math::v3(2., 5., 0.),
            c: super::super::math::v3(1., 5., 1.),
            normal: super::super::math::v3(0., 1., 0.),
            material: 1,
        });
        Bvh::build(tris)
    }

    #[test]
    fn closest_hit_finds_the_nearest_surface() {
        let bvh = scene();
        // From above x = 1.5 there are two surfaces; the roof wins.
        let ray = Ray::new(
            super::super::math::v3(1.5, 10., 0.25),
            super::super::math::v3(0., -1., 0.),
        );
        let hit = bvh.closest_hit(&ray, 0., 1e9).expect("a hit");
        assert!((hit.t - 5.).abs() < 1e-4, "{}", hit.t);
        assert_eq!(hit.material, 1);
        // From x = 5 only the ground exists.
        let ray = Ray::new(
            super::super::math::v3(5.5, 10., 0.25),
            super::super::math::v3(0., -1., 0.),
        );
        let hit = bvh.closest_hit(&ray, 0., 1e9).expect("a hit");
        assert!((hit.t - 10.).abs() < 1e-4, "{}", hit.t);
        assert_eq!(hit.material, 0);
    }

    #[test]
    fn any_hit_reports_occlusion_only_inside_the_interval() {
        let bvh = scene();
        let down = Ray::new(
            super::super::math::v3(1.5, 10., 0.25),
            super::super::math::v3(0., -1., 0.),
        );
        assert!(bvh.any_hit(&down, 0., 100.));
        // The roof occludes the ground; asking only past the roof misses both.
        assert!(!bvh.any_hit(&down, 0., 4.9));
        assert!(!bvh.any_hit(&down, 6., 9.9));
        // A ray to the side hits nothing.
        let side = Ray::new(
            super::super::math::v3(1.5, 10., 0.25),
            super::super::math::v3(1., 0., 0.),
        );
        assert!(!bvh.any_hit(&side, 0., 100.));
    }

    #[test]
    fn empty_scene_is_safe() {
        let bvh = Bvh::build(Vec::new());
        assert!(bvh.is_empty());
        let ray = Ray::new(V3::ZERO, super::super::math::v3(0., -1., 0.));
        assert!(bvh.closest_hit(&ray, 0., 100.).is_none());
        assert!(!bvh.any_hit(&ray, 0., 100.));
    }

    #[test]
    fn every_triangle_is_reachable_from_the_root() {
        let bvh = scene();
        // A ray straight down through each column must find *something*; this
        // catches a build that drops a range (the classic blocked-BVH bug).
        for i in 0..10 {
            let ray = Ray::new(
                super::super::math::v3(i as f32 + 0.5, 10., 0.5),
                super::super::math::v3(0., -1., 0.),
            );
            assert!(bvh.closest_hit(&ray, 0., 1e9).is_some(), "column {i}");
        }
        assert_eq!(bvh.order.len(), bvh.triangles.len());
        let mut seen = vec![false; bvh.triangles.len()];
        for &t in &bvh.order {
            seen[t as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }
}
