//! Harvesting the loaded world into the ray scene.
//!
//! The renderer already has the world as GPU meshes and an instance list. This
//! file turns those into [`SceneGeometry`] without asking the raster path for
//! anything it does not already have, and without copying the world: the caller
//! hands in triangle soups and transforms, and this file packs them.
//!
//! Two sources matter for a scene this size:
//!
//! * **Terrain** — the height grid the renderer already streams. It is by far
//!   the largest surface and the one shadows and ambient occlusion are about,
//!   so it is harvested at a coarser stride than the render mesh would suggest
//!   and only inside a bounded window around the camera.
//! * **Instances** — every placed model with a box proxy and a material taken
//!   from its placement. A model's own triangles would be a million-triangle
//!   build for a town, which is not what a real-time tracer of this budget can
//!   afford; the proxy is what the rays can actually resolve anyway, and it
//!   keeps the build linear in *placed objects* rather than in geometry.
//!
//! The result is deliberately cached: [`Harvester`] remembers the inputs it was
//! built from and returns the previous scene when nothing that matters changed,
//! so a still camera does not rebuild a BVH every frame.

use glam::{Mat4, Vec3};

use super::bvh::{RayMaterial, RayTriangle, SceneGeometry};

/// How the harvest is bounded. Every field is a knob the module's UI could
/// expose later; the defaults are what a 1080p frame at 60 Hz can afford.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HarvestConfig {
    /// Half-extent in world units of the square the terrain is harvested over,
    /// around the camera.
    pub radius: f32,
    /// Terrain grid cells are `cell` world units wide; the harvest samples every
    /// `stride`-th grid line, so the ray mesh has `cell * stride` spacing.
    pub stride: u32,
    /// Instance proxies smaller than this are skipped: a plant or a lamp post
    /// contributes no visible occlusion.
    pub min_instance_size: f32,
    /// Hard cap on instance proxies. The nearest ones win, so a dense town
    /// degrades gracefully instead of growing without bound.
    pub max_instances: usize,
}

impl Default for HarvestConfig {
    fn default() -> Self {
        Self {
            radius: 128.0,
            stride: 2,
            min_instance_size: 0.75,
            max_instances: 4096,
        }
    }
}

/// One placed model the harvester needs to know about: where its box sits and
/// what it is made of. The renderer fills this from its solved transforms.
#[derive(Clone, Copy, Debug)]
pub struct InstanceProxy {
    /// World-space centre of the model's bounding box.
    pub centre: Vec3,
    /// World-space half-extents of the model's bounding box.
    pub half: Vec3,
    /// Linear albedo; the harvester darkens it by how rough the model reads.
    pub albedo: [f32; 3],
    /// A yaw rotation the box is spun by, if the placement has one. Boxes stay
    /// axis-aligned in the BVH either way — this only changes which faces are
    /// presented, which is what a reflection needs to not be a flat wall.
    pub yaw: f32,
}

/// The inputs a scene is built from, kept so an unchanged frame is a cache hit.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Signature {
    config: HarvestConfig,
    /// Camera position rounded to the harvest cell: sub-cell motion cannot move
    /// a terrain vertex, so it must not invalidate the build.
    anchor: (i32, i32),
    /// A hash of the instance list. Positions move, so the hash has to see them.
    instances: u64,
    /// The height grid's generation counter, bumped by whoever replaces it.
    terrain: u64,
}

/// Terrain heights sampled from the caller's own ground function.
///
/// The renderer's terrain is a streamed block tree, which is not something this
/// module should reach into; instead the renderer samples its own
/// `MapSpace::ground` into this flat grid once per rebuild. The rows run along
/// `+z` from `origin`, and `heights[z * width + x]` is the height at
/// `origin + (x * cell, 0, z * cell)`.
pub fn sample_heights(
    width: u32,
    height: u32,
    origin: Vec3,
    cell: f32,
    ground: impl Fn(f32, f32) -> f32,
) -> Vec<f32> {
    let mut heights = vec![0f32; (width as usize).saturating_mul(height as usize)];
    for z in 0..height {
        let wz = origin.z + z as f32 * cell;
        for x in 0..width {
            let wx = origin.x + x as f32 * cell;
            heights[(z * width + x) as usize] = ground(wx, wz);
        }
    }
    heights
}

/// Terrain heights, as the harvester needs them: a dense grid plus its origin.
///
/// The renderer's own terrain resource is a streamed block tree; rather than
/// reach into it, the caller samples it into this flat grid once per rebuild.
pub struct HeightField<'a> {
    /// Heights in row-major order, `width * height` entries, world units.
    pub heights: &'a [f32],
    pub width: u32,
    pub height: u32,
    /// World position of grid cell `(0, 0)`.
    pub origin: Vec3,
    /// World units between grid cells.
    pub cell: f32,
    /// A counter that changes whenever `heights` is replaced, so the harvester
    /// can tell a reloaded block from a still one.
    pub generation: u64,
}

impl HeightField<'_> {
    fn sample(&self, x: f32, z: f32) -> Option<f32> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let u = (x - self.origin.x) / self.cell;
        let v = (z - self.origin.z) / self.cell;
        if u < 0. || v < 0. || u >= (self.width - 1) as f32 || v >= (self.height - 1) as f32 {
            return None;
        }
        let ix = u.floor() as usize;
        let iz = v.floor() as usize;
        let fx = u - ix as f32;
        let fz = v - iz as f32;
        let at = |ix: usize, iz: usize| self.heights[iz * self.width as usize + ix];
        let h00 = at(ix, iz);
        let h10 = at(ix + 1, iz);
        let h01 = at(ix, iz + 1);
        let h11 = at(ix + 1, iz + 1);
        Some((h00 * (1. - fx) + h10 * fx) * (1. - fz) + (h01 * (1. - fx) + h11 * fx) * fz)
    }
}

/// Builds and caches the ray scene.
#[derive(Default)]
pub struct Harvester {
    cached: Option<(Signature, std::sync::Arc<SceneGeometry>)>,
    /// Why the last build was refused or clipped, for the tab.
    pub note: Option<String>,
    /// Triangles in the last build.
    pub triangles: usize,
    /// Instances that made it into the last build.
    pub instances: usize,
}

impl Harvester {
    pub fn new() -> Self {
        Self::default()
    }

    /// The scene for this frame, rebuilding only when an input moved.
    ///
    /// `instances` must be in the order the caller wants ties broken by
    /// distance; it does not matter, because the harvester sorts by distance
    /// itself, but a stable input makes the cache hash stable too.
    /// Fast-path cache check that lets callers skip sampling the terrain height
    /// field when neither the camera anchor cell, the terrain generation, nor
    /// the instance list changed.
    pub fn cached_if_unchanged(
        &self,
        config: HarvestConfig,
        eye: Vec3,
        terrain_generation: u64,
        instances: &[InstanceProxy],
    ) -> Option<std::sync::Arc<SceneGeometry>> {
        let anchor = (
            (eye.x / (config.radius.max(1.) / 64.)).round() as i32,
            (eye.z / (config.radius.max(1.) / 64.)).round() as i32,
        );
        let signature = Signature {
            config,
            anchor,
            instances: hash_instances(instances, eye, config),
            terrain: terrain_generation,
        };
        let (cached, scene) = self.cached.as_ref()?;
        if *cached == signature {
            Some(scene.clone())
        } else {
            None
        }
    }

    pub fn scene(
        &mut self,
        config: HarvestConfig,
        eye: Vec3,
        terrain: Option<&HeightField<'_>>,
        instances: &[InstanceProxy],
    ) -> std::sync::Arc<SceneGeometry> {
        let anchor = (
            (eye.x / (config.radius.max(1.) / 64.)).round() as i32,
            (eye.z / (config.radius.max(1.) / 64.)).round() as i32,
        );
        let signature = Signature {
            config,
            anchor,
            instances: hash_instances(instances, eye, config),
            terrain: terrain.map(|t| t.generation).unwrap_or(0),
        };
        if let Some((cached, scene)) = &self.cached {
            if *cached == signature {
                return scene.clone();
            }
        }
        let mut triangles = Vec::new();
        let mut materials = Vec::new();
        self.instances = push_instances(&mut triangles, &mut materials, instances, eye, config);
        if let Some(field) = terrain {
            push_terrain(&mut triangles, &mut materials, field, eye, config);
        }
        self.note = if triangles.is_empty() {
            Some("no geometry inside the harvest radius yet".to_string())
        } else if self.instances == 0 {
            Some("terrain only: no placed model is close enough to occlude".to_string())
        } else {
            None
        };
        self.triangles = triangles.len();
        let scene = std::sync::Arc::new(SceneGeometry::build(triangles, materials));
        self.cached = Some((signature, scene.clone()));
        scene
    }

    /// Forget the cache, so the next [`Self::scene`] rebuilds.
    pub fn invalidate(&mut self) {
        self.cached = None;
    }

    pub fn cached(&self) -> Option<&std::sync::Arc<SceneGeometry>> {
        self.cached.as_ref().map(|(_, scene)| scene)
    }
}

fn hash_instances(instances: &[InstanceProxy], eye: Vec3, config: HarvestConfig) -> u64 {
    // FNV-1a over the quantised proxy state, mixed with the eye so that a camera
    // move which pulls different instances into range is a miss.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bits: u64| {
        hash ^= bits;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    };
    mix((eye.x / config.radius.max(1.) * 64.).round() as i64 as u64);
    mix((eye.z / config.radius.max(1.) * 64.).round() as i64 as u64);
    mix(instances.len() as u64);
    for instance in instances {
        for value in [
            instance.centre.x,
            instance.centre.y,
            instance.centre.z,
            instance.half.x,
            instance.half.y,
            instance.half.z,
            instance.albedo[0],
            instance.albedo[1],
            instance.albedo[2],
            instance.yaw,
        ] {
            mix((value * 64.).round() as i64 as u64);
        }
    }
    hash
}

/// Push the twelve triangles of a box per instance, nearest first, up to the
/// budget. Boxes are built in world space already; no transform is stored.
fn push_instances(
    triangles: &mut Vec<RayTriangle>,
    materials: &mut Vec<RayMaterial>,
    instances: &[InstanceProxy],
    eye: Vec3,
    config: HarvestConfig,
) -> usize {
    let mut order: Vec<(f32, usize)> = instances
        .iter()
        .enumerate()
        .filter(|(_, instance)| {
            instance.half.min_element() >= config.min_instance_size * 0.5
                && instance.half.max_element() > 0.
        })
        .map(|(index, instance)| {
            (
                instance.centre.distance_squared(eye) + instance.half.length_squared(),
                index,
            )
        })
        .collect();
    if order.len() > config.max_instances && config.max_instances > 0 {
        order.select_nth_unstable_by(config.max_instances, |a, b| a.0.total_cmp(&b.0));
        order.truncate(config.max_instances);
    }
    order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut pushed = 0;
    for (_, index) in order {
        let instance = instances[index];
        let material = materials.len() as u32;
        materials.push(RayMaterial {
            albedo: [
                instance.albedo[0],
                instance.albedo[1],
                instance.albedo[2],
                1.,
            ],
            // Buildings read as rough dielectric; the slight sheen keeps a
            // grazing reflection from being a flat wash.
            surface: [0.75, 0.05, 0., 1.],
        });
        push_box(triangles, instance, material);
        pushed += 1;
    }
    pushed
}

/// The twelve triangles of a yawed box.
fn push_box(triangles: &mut Vec<RayTriangle>, instance: InstanceProxy, material: u32) {
    let (sin, cos) = instance.yaw.sin_cos();
    let rotate = |local: Vec3| {
        Vec3::new(
            instance.centre.x + local.x * cos - local.z * sin,
            instance.centre.y + local.y,
            instance.centre.z + local.x * sin + local.z * cos,
        )
    };
    let h = instance.half.abs().max(Vec3::splat(0.01));
    // Corner order: -x -y -z, +x -y -z, +x -y +z, -x -y +z, and the same four
    // one half-extent up.
    let bottom = [
        Vec3::new(-h.x, -h.y, -h.z),
        Vec3::new(h.x, -h.y, -h.z),
        Vec3::new(h.x, -h.y, h.z),
        Vec3::new(-h.x, -h.y, h.z),
    ];
    let top = [
        Vec3::new(-h.x, h.y, -h.z),
        Vec3::new(h.x, h.y, -h.z),
        Vec3::new(h.x, h.y, h.z),
        Vec3::new(-h.x, h.y, h.z),
    ];
    let b: [Vec3; 4] = bottom.map(rotate);
    let t: [Vec3; 4] = top.map(rotate);
    let mut quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3, normal: Vec3| {
        let n = (b - a).cross(c - a).normalize_or_zero();
        let normal = if n.length_squared() > 0.5 { n } else { normal };
        triangles.push(RayTriangle::new(a, b, c, normal, material));
        triangles.push(RayTriangle::new(a, c, d, normal, material));
    };
    let up = Vec3::Y;
    let down = Vec3::NEG_Y;
    // Top and bottom.
    quad(t[0], t[1], t[2], t[3], up);
    quad(b[3], b[2], b[1], b[0], down);
    // The four walls, each with a normal pointing away from the box centre.
    for i in 0..4 {
        let j = (i + 1) % 4;
        let outward = ((b[i] - instance.centre) + (b[j] - instance.centre))
            .normalize_or_zero()
            .reject_from(up)
            .normalize_or_zero();
        quad(b[i], b[j], t[j], t[i], outward);
    }
}

/// Terrain: one quad per `stride * stride` cell block inside the radius.
fn push_terrain(
    triangles: &mut Vec<RayTriangle>,
    materials: &mut Vec<RayMaterial>,
    field: &HeightField<'_>,
    eye: Vec3,
    config: HarvestConfig,
) {
    if field.width < 2
        || field.height < 2
        || field.heights.len() < (field.width * field.height) as usize
    {
        return;
    }
    let material = materials.len() as u32;
    materials.push(RayMaterial {
        albedo: [0.32, 0.36, 0.28, 1.],
        // Ground is as rough as a surface gets; a tiny sheen only catches the
        // sun at grazing angles, where a wet field should.
        surface: [0.92, 0., 0., 1.],
    });
    let stride = config.stride.max(1);
    let spacing = field.cell * stride as f32;
    if spacing <= 0. {
        return;
    }
    // Snap the grid to the stride so neighbouring frames share vertices: without
    // this the whole mesh slides under the camera and the BVH cache never hits.
    let min_x = eye.x - config.radius;
    let max_x = eye.x + config.radius;
    let min_z = eye.z - config.radius;
    let max_z = eye.z + config.radius;
    // The first lattice node at or before `min`, clamped into the grid. A
    // window larger than the grid is the normal case near the edge of a loaded
    // block, so this must clamp rather than give up: the harvest simply covers
    // the part of the window the heights actually describe.
    let first = |min: f32, origin: f32, size: u32, cell: f32| -> u32 {
        let index = ((min - origin) / cell.max(1e-3)).floor();
        if index <= 0. {
            0
        } else if index >= size as f32 {
            // Entirely past the grid: report a start the loop will reject
            // immediately, by pointing at the last usable node.
            size.saturating_sub(1)
        } else {
            index as u32
        }
    };
    let mut x = first(min_x, field.origin.x, field.width - 1, field.cell);
    let mut z = first(min_z, field.origin.z, field.height - 1, field.cell);
    // Align to the stride lattice in grid space, so a camera that moves by less
    // than one harvested cell reuses every vertex and the cache still hits.
    x -= x % stride;
    z -= z % stride;
    let width_usize = field.width as usize;
    let step = field.cell * stride as f32;
    let est_x = ((field.width.saturating_sub(x)) / stride) as usize;
    let est_z = ((field.height.saturating_sub(z)) / stride) as usize;
    triangles.reserve(est_x.saturating_mul(est_z).saturating_mul(2));
    let mut wz = z;
    while wz + stride < field.height {
        let world_z = field.origin.z + wz as f32 * field.cell;
        if world_z > max_z {
            break;
        }
        let row0 = (wz as usize) * width_usize;
        let row1 = ((wz + stride) as usize) * width_usize;
        let mut wx = x;
        while wx + stride < field.width {
            let world_x = field.origin.x + wx as f32 * field.cell;
            if world_x > max_x {
                break;
            }
            let ix0 = wx as usize;
            let ix1 = (wx + stride) as usize;
            let h0 = field.heights[row0 + ix0];
            let h1 = field.heights[row0 + ix1];
            let h2 = field.heights[row1 + ix1];
            let h3 = field.heights[row1 + ix0];
            let p0 = Vec3::new(world_x, h0, world_z);
            let p1 = Vec3::new(world_x + step, h1, world_z);
            let p2 = Vec3::new(world_x + step, h2, world_z + step);
            let p3 = Vec3::new(world_x, h3, world_z + step);
            // Compute upward-facing (+Y) normals per triangle.
            let n0 = (p2 - p0).cross(p1 - p0).normalize_or_zero();
            let n0 = if n0.length_squared() > 0.5 {
                n0
            } else {
                Vec3::Y
            };
            let n1 = (p3 - p0).cross(p2 - p0).normalize_or_zero();
            let n1 = if n1.length_squared() > 0.5 {
                n1
            } else {
                Vec3::Y
            };
            triangles.push(RayTriangle::new(p0, p1, p2, n0, material));
            triangles.push(RayTriangle::new(p0, p2, p3, n1, material));
            wx += stride;
        }
        wz += stride;
    }
}

/// Convenience for callers that already hold a world matrix: the instance's box
/// corners in world space, reduced to a centre and half-extents.
pub fn proxy_from_box(centre: Vec3, half: Vec3, matrix: &Mat4, albedo: [f32; 3]) -> InstanceProxy {
    let centre = matrix.transform_point3(centre);
    let scale = matrix.to_scale_rotation_translation().0.abs();
    InstanceProxy {
        centre,
        half: Vec3::new(half.x * scale.x, half.y * scale.y, half.z * scale.z),
        albedo,
        yaw: 0.,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_field(heights: &[f32], w: u32, h: u32) -> HeightField<'_> {
        HeightField {
            heights,
            width: w,
            height: h,
            origin: Vec3::new(-64., 0., -64.),
            cell: 2.,
            generation: 1,
        }
    }

    #[test]
    fn a_flat_field_produces_upward_facing_terrain_that_occludes_from_above() {
        let heights = vec![0.0; 65 * 65];
        let field = flat_field(&heights, 65, 65);
        let mut harvester = Harvester::new();
        let scene = harvester.scene(
            HarvestConfig {
                stride: 4,
                ..HarvestConfig::default()
            },
            Vec3::new(0., 10., 0.),
            Some(&field),
            &[],
        );
        assert!(scene.triangle_count() > 0, "terrain harvested");
        // Straight down from above the middle must hit the ground.
        let hit = scene.closest_hit(Vec3::new(0., 10., 0.), Vec3::NEG_Y, 0., 100.);
        let hit = hit.expect("ground under the camera");
        assert!(hit.t > 9. && hit.t < 11., "{hit:?}");
        // From below it occludes too: traversal is two-sided, which is what
        // lets a wall of single-sided quads still stop a shadow ray.
        assert!(scene
            .closest_hit(Vec3::new(0., -10., 0.), Vec3::Y, 0., 100.)
            .is_some());
        // And the ground faces the sky, which is what the shading normal is for.
        assert!(hit.normal.y > 0.9, "{hit:?}");
    }

    #[test]
    fn a_box_instance_occludes_its_own_volume_and_stops_a_shadow_ray() {
        let mut harvester = Harvester::new();
        let instance = InstanceProxy {
            centre: Vec3::new(0., 5., 0.),
            half: Vec3::splat(2.),
            albedo: [0.8, 0.2, 0.2],
            yaw: 0.3,
        };
        let scene = harvester.scene(
            HarvestConfig::default(),
            Vec3::new(0., 0., 40.),
            None,
            &[instance],
        );
        assert_eq!(harvester.instances, 1);
        assert_eq!(scene.triangle_count(), 12);
        // A ray from the side must hit the box.
        let hit = scene
            .closest_hit(Vec3::new(0., 5., 20.), Vec3::NEG_Z, 0., 100.)
            .expect("box in the way");
        assert!((hit.t - 18.).abs() < 1., "{hit:?}");
        // And a ray under it must miss.
        assert!(scene
            .closest_hit(Vec3::new(0., -10., 20.), Vec3::NEG_Z, 0., 100.)
            .is_none());
    }

    #[test]
    fn an_unchanged_frame_reuses_the_cached_scene() {
        let mut harvester = Harvester::new();
        let config = HarvestConfig::default();
        let eye = Vec3::new(0., 2., 0.);
        let instance = InstanceProxy {
            centre: Vec3::ZERO,
            half: Vec3::splat(1.),
            albedo: [1.; 3],
            yaw: 0.,
        };
        let first = harvester.scene(config, eye, None, &[instance]);
        let second = harvester.scene(config, eye, None, &[instance]);
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        // A different eye at the far end of the world is a miss.
        let third = harvester.scene(config, Vec3::new(9000., 2., 0.), None, &[instance]);
        assert!(!std::sync::Arc::ptr_eq(&first, &third));
    }

    #[test]
    fn instances_beyond_the_budget_are_dropped_nearest_last() {
        let mut harvester = Harvester::new();
        let instances: Vec<InstanceProxy> = (0..40)
            .map(|i| InstanceProxy {
                centre: Vec3::new(0., 0., -(i as f32) * 10.),
                half: Vec3::splat(1.),
                albedo: [1.; 3],
                yaw: 0.,
            })
            .collect();
        let config = HarvestConfig {
            max_instances: 4,
            ..HarvestConfig::default()
        };
        harvester.scene(config, Vec3::ZERO, None, &instances);
        assert_eq!(harvester.instances, 4);
        assert_eq!(harvester.triangles, 48);
    }

    #[test]
    fn an_empty_world_says_so_instead_of_building_an_empty_tree() {
        let mut harvester = Harvester::new();
        let scene = harvester.scene(HarvestConfig::default(), Vec3::ZERO, None, &[]);
        assert_eq!(scene.triangle_count(), 0);
        assert!(harvester.note.as_deref().unwrap().contains("no geometry"));
    }

    #[test]
    fn the_height_field_interpolates_rather_than_stepping() {
        let heights = vec![0., 0., 10., 10.];
        let field = HeightField {
            heights: &heights,
            width: 2,
            height: 2,
            origin: Vec3::ZERO,
            cell: 1.,
            generation: 0,
        };
        // The grid rows are z, so the ramp is along z: row 0 is height 0, row 1
        // is height 10, and the sample is the bilinear blend of the two.
        assert_eq!(field.sample(0.5, 0.), Some(0.));
        assert_eq!(field.sample(0., 0.5), Some(5.));
        assert_eq!(field.sample(0., 0.25), Some(2.5));
        assert_eq!(field.sample(0.5, 0.75), Some(7.5));
        // Outside the grid there is no answer, so the harvest skips the cell
        // instead of inventing a height of zero.
        assert_eq!(field.sample(-0.5, 0.), None);
        assert_eq!(field.sample(1.5, 0.), None);
    }
}
