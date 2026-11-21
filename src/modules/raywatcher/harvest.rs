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
    /// Hard cap on instance proxies **per spatial chunk**. The nearest ones
    /// within each chunk win, so a dense town degrades gracefully instead of
    /// growing without bound while distant chunks still keep their own budget.
    pub max_instances: usize,
    /// Side length of one spatial harvest chunk in world units. Terrain LOD
    /// and per-chunk instance budgets are evaluated per chunk across the full
    /// visible frustum.
    pub chunk_size: f32,
    /// Optional explicit view-distance override in world units (`0.0` uses the
    /// camera's far plane / view distance).
    pub max_distance: f32,
}

impl Default for HarvestConfig {
    fn default() -> Self {
        Self {
            radius: 128.0,
            stride: 2,
            min_instance_size: 0.75,
            max_instances: 4096,
            chunk_size: 256.0,
            max_distance: 0.0,
        }
    }
}

/// Camera view frustum (six inward-pointing half-space planes) extracted from a
/// reversed-Z `view_projection` matrix (`z_ndc` in `[0, 1]`).
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    planes: [[f32; 4]; 6],
}

impl Frustum {
    pub fn from_view_projection(vp: Mat4) -> Self {
        let m = vp.to_cols_array();
        let r0 = [m[0], m[4], m[8], m[12]];
        let r1 = [m[1], m[5], m[9], m[13]];
        let r2 = [m[2], m[6], m[10], m[14]];
        let r3 = [m[3], m[7], m[11], m[15]];
        Self {
            planes: [
                [r3[0] + r0[0], r3[1] + r0[1], r3[2] + r0[2], r3[3] + r0[3]],
                [r3[0] - r0[0], r3[1] - r0[1], r3[2] - r0[2], r3[3] - r0[3]],
                [r3[0] + r1[0], r3[1] + r1[1], r3[2] + r1[2], r3[3] + r1[3]],
                [r3[0] - r1[0], r3[1] - r1[1], r3[2] - r1[2], r3[3] - r1[3]],
                r2,
                [r3[0] - r2[0], r3[1] - r2[1], r3[2] - r2[2], r3[3] - r2[3]],
            ],
        }
    }

    /// Conservative AABB-vs-frustum test: returns `true` if any corner could be
    /// inside all six frustum half-spaces.
    pub fn intersects_aabb(&self, min: Vec3, max: Vec3) -> bool {
        for p in &self.planes {
            let px = if p[0] >= 0.0 { max.x } else { min.x };
            let py = if p[1] >= 0.0 { max.y } else { min.y };
            let pz = if p[2] >= 0.0 { max.z } else { min.z };
            if p[0] * px + p[1] * py + p[2] * pz + p[3] < 0.0 {
                return false;
            }
        }
        true
    }
}

/// Real world-space mesh triangle harvested from the raster pass's resident
/// pages (used near the camera so shadows and reflections match actual building
/// and prop silhouettes rather than coarse bounding boxes).
#[derive(Clone, Copy, Debug)]
pub struct MeshTriangle {
    pub a: Vec3,
    pub b: Vec3,
    pub c: Vec3,
    pub normal: Vec3,
    pub albedo: [f32; 3],
    pub roughness: f32,
}

/// Axis-aligned water surface plane harvested from the renderer's water
/// batches. Given a low roughness (`0.06`) so water receives crisp ray-traced
/// reflections across the entire visible frustum.
#[derive(Clone, Copy, Debug)]
pub struct WaterSurface {
    /// `[min_x, max_x, min_z, max_z]` in world units.
    pub bounds: [f32; 4],
    pub height: f32,
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
    /// Quantised view direction & view distance for frustum-wide builds (0 for
    /// legacy radius-only calls).
    frustum_key: u64,
    /// A hash of the instance / real-mesh / water inputs.
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
            frustum_key: 0,
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
            frustum_key: 0,
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

    /// Fast-path cache check for [`Self::scene_frustum`].
    #[allow(clippy::too_many_arguments)]
    pub fn cached_if_unchanged_frustum(
        &self,
        config: HarvestConfig,
        eye: Vec3,
        view_forward: Vec3,
        view_distance: f32,
        terrain_generation: u64,
        content_hash: u64,
    ) -> Option<std::sync::Arc<SceneGeometry>> {
        let sig = frustum_signature(
            config,
            eye,
            view_forward,
            view_distance,
            terrain_generation,
            content_hash,
        );
        let (cached, scene) = self.cached.as_ref()?;
        if *cached == sig {
            Some(scene.clone())
        } else {
            None
        }
    }

    /// Build a frustum-wide, chunked multi-LOD ray scene covering the full
    /// visible frustum out to `view_distance` (the camera's far plane).
    ///
    /// * **Terrain** is partitioned into world-aligned chunks of `chunk_size`
    ///   and frustum-culled (keeping a near shadow-caster ring around the eye).
    ///   Near chunks use fine `stride`, mid chunks use `stride * 2`, and far
    ///   chunks use coarser LOD strides out to `view_distance`.
    /// * **Real meshes** (`real_meshes`) are inserted directly for near-camera
    ///   scenery/buildings so shadows and reflections match actual silhouettes
    ///   and baked material albedo.
    /// * **Proxy instances** (`instances`) are grouped by spatial chunk and
    ///   capped at `config.max_instances` **per chunk** (nearest winning).
    ///   Proxies are used for distant scenery and trees where full per-leaf
    ///   triangle soups would exceed the real-time BVH/BLAS budget.
    /// * **Water surfaces** (`water`) are emitted as low-roughness (`0.06`)
    ///   reflective quads across the visible frustum.
    #[allow(clippy::too_many_arguments)]
    pub fn scene_frustum(
        &mut self,
        config: HarvestConfig,
        eye: Vec3,
        view_forward: Vec3,
        view_projection: Mat4,
        view_distance: f32,
        base_cell: f32,
        terrain_generation: u64,
        content_hash: u64,
        ground: Option<&dyn Fn(f32, f32) -> f32>,
        real_meshes: &[MeshTriangle],
        instances: &[InstanceProxy],
        water: &[WaterSurface],
    ) -> std::sync::Arc<SceneGeometry> {
        let sig = frustum_signature(
            config,
            eye,
            view_forward,
            view_distance,
            terrain_generation,
            content_hash,
        );
        if let Some((cached, scene)) = &self.cached {
            if *cached == sig {
                return scene.clone();
            }
        }
        let far = if config.max_distance > 0.0 {
            config.max_distance
        } else {
            view_distance.max(config.radius)
        };
        let frustum = Frustum::from_view_projection(view_projection);
        let mut triangles = Vec::new();
        let mut materials = Vec::new();

        // 1. Real mesh triangles (near-camera buildings/props with real
        //    normals and baked vertex diffuse albedo).
        push_real_meshes(&mut triangles, &mut materials, real_meshes);

        // 2. Chunked instance proxies (distant buildings/props and trees:
        //    proxies are much cheaper than full foliage/distant meshes and
        //    still cast accurate silhouettes and reflections). `max_instances`
        //    is enforced per spatial chunk with nearest instances winning.
        self.instances = push_instances_chunked(
            &mut triangles,
            &mut materials,
            instances,
            eye,
            &frustum,
            far,
            config,
        );

        // 3. Reflective water surfaces across the visible frustum.
        push_water(&mut triangles, &mut materials, water, eye, &frustum, far);

        // 4. Chunked multi-LOD terrain across the entire visible frustum out to
        //    `far`.
        if let Some(ground_fn) = ground {
            push_terrain_frustum(
                &mut triangles,
                &mut materials,
                eye,
                &frustum,
                far,
                base_cell.max(0.5),
                config,
                ground_fn,
            );
        }

        self.note = if triangles.is_empty() {
            Some("no geometry inside the visible frustum yet".to_string())
        } else {
            None
        };
        self.triangles = triangles.len();
        let scene = std::sync::Arc::new(SceneGeometry::build(triangles, materials));
        self.cached = Some((sig, scene.clone()));
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

fn frustum_signature(
    config: HarvestConfig,
    eye: Vec3,
    _view_forward: Vec3,
    view_distance: f32,
    terrain_generation: u64,
    content_hash: u64,
) -> Signature {
    // Snap to 64-unit steps and omit camera yaw/pitch: the harvested BVH/BLAS
    // covers 360 degrees around the camera out to `view_distance`, so rotating
    // the camera never triggers a CPU BVH or DXR BLAS rebuild stall, and
    // buildings behind the camera still cast shadows into the view.
    let snap = (config.chunk_size.max(64.0) / 4.0).max(32.0);
    let anchor = ((eye.x / snap).round() as i32, (eye.z / snap).round() as i32);
    let far_bucket = ((view_distance / 128.0).round() as u32) as u64;
    let frustum_key = (far_bucket & 0xffff) | (1u64 << 63);
    Signature {
        config,
        anchor,
        frustum_key,
        instances: content_hash,
        terrain: terrain_generation,
    }
}

/// Push real mesh triangles from resident scenery pages. Materials are bucketed
/// by quantised albedo so the material table stays compact while preserving
/// real surface colours for reflections and GI bounces.
fn push_real_meshes(
    triangles: &mut Vec<RayTriangle>,
    materials: &mut Vec<RayMaterial>,
    meshes: &[MeshTriangle],
) {
    if meshes.is_empty() {
        return;
    }
    let mut palette: std::collections::HashMap<(u8, u8, u8, u8), u32> =
        std::collections::HashMap::new();
    triangles.reserve(meshes.len());
    for m in meshes {
        let key = (
            (m.albedo[0].clamp(0.0, 1.0) * 31.0).round() as u8,
            (m.albedo[1].clamp(0.0, 1.0) * 31.0).round() as u8,
            (m.albedo[2].clamp(0.0, 1.0) * 31.0).round() as u8,
            (m.roughness.clamp(0.0, 1.0) * 15.0).round() as u8,
        );
        let mat_id = *palette.entry(key).or_insert_with(|| {
            let id = materials.len() as u32;
            materials.push(RayMaterial {
                albedo: [m.albedo[0], m.albedo[1], m.albedo[2], 1.0],
                surface: [m.roughness.clamp(0.04, 1.0), 0.05, 0.0, 1.0],
            });
            id
        });
        triangles.push(RayTriangle::new(m.a, m.b, m.c, m.normal, mat_id));
    }
}

/// Push reflective water quads across the visible frustum. Water uses low
/// roughness (`0.06`) so both the WGSL and DXR shaders trace mirror-like
/// Fresnel reflections on lakes, rivers, and seas.
fn push_water(
    triangles: &mut Vec<RayTriangle>,
    materials: &mut Vec<RayMaterial>,
    water: &[WaterSurface],
    eye: Vec3,
    frustum: &Frustum,
    far: f32,
) {
    if water.is_empty() {
        return;
    }
    let mut water_mat: Option<u32> = None;
    let far_sq = far * far;
    for w in water {
        let [x0, x1, z0, z1] = w.bounds;
        if x1 <= x0 || z1 <= z0 {
            continue;
        }
        let min = Vec3::new(x0, w.height - 1.0, z0);
        let max = Vec3::new(x1, w.height + 1.0, z1);
        let cx = (x0 + x1) * 0.5;
        let cz = (z0 + z1) * 0.5;
        let dx = (eye.x - cx).abs().saturating_sub_f32((x1 - x0) * 0.5);
        let dz = (eye.z - cz).abs().saturating_sub_f32((z1 - z0) * 0.5);
        if dx * dx + dz * dz > far_sq {
            continue;
        }
        let _ = (frustum, min, max);
        let mat_id = *water_mat.get_or_insert_with(|| {
            let id = materials.len() as u32;
            materials.push(RayMaterial {
                albedo: [0.10, 0.22, 0.28, 1.0],
                surface: [0.06, 0.02, 0.0, 1.0],
            });
            id
        });
        let p0 = Vec3::new(x0, w.height, z0);
        let p1 = Vec3::new(x1, w.height, z0);
        let p2 = Vec3::new(x1, w.height, z1);
        let p3 = Vec3::new(x0, w.height, z1);
        triangles.push(RayTriangle::new(p0, p1, p2, Vec3::Y, mat_id));
        triangles.push(RayTriangle::new(p0, p2, p3, Vec3::Y, mat_id));
    }
}

trait SatSubF32 {
    fn saturating_sub_f32(self, rhs: f32) -> f32;
}
impl SatSubF32 for f32 {
    #[inline]
    fn saturating_sub_f32(self, rhs: f32) -> f32 {
        (self - rhs).max(0.0)
    }
}

/// Group instance proxies into spatial chunks, frustum-cull chunks (keeping a
/// near shadow-caster ring around `eye`), and enforce `config.max_instances`
/// **per chunk** with the nearest instances winning.
fn push_instances_chunked(
    triangles: &mut Vec<RayTriangle>,
    materials: &mut Vec<RayMaterial>,
    instances: &[InstanceProxy],
    eye: Vec3,
    frustum: &Frustum,
    far: f32,
    config: HarvestConfig,
) -> usize {
    if instances.is_empty() || config.max_instances == 0 {
        return 0;
    }
    let chunk = config.chunk_size.max(32.0);
    let far_sq = far * far;
    let near_ring_sq = config.radius.max(32.0).powi(2);
    let mut chunks: std::collections::BTreeMap<(i32, i32), Vec<(f32, usize)>> =
        std::collections::BTreeMap::new();
    for (index, inst) in instances.iter().enumerate() {
        if inst.half.min_element() < config.min_instance_size * 0.5
            || inst.half.max_element() <= 0.0
        {
            continue;
        }
        let dist_sq = inst.centre.distance_squared(eye);
        if dist_sq > far_sq {
            continue;
        }
        let _ = (near_ring_sq, frustum);
        let cx = (inst.centre.x / chunk).floor() as i32;
        let cz = (inst.centre.z / chunk).floor() as i32;
        chunks
            .entry((cx, cz))
            .or_default()
            .push((dist_sq + inst.half.length_squared(), index));
    }
    let mut pushed = 0usize;
    let mut palette: std::collections::HashMap<(u8, u8, u8), u32> =
        std::collections::HashMap::new();
    for (_, mut list) in chunks {
        if list.len() > config.max_instances {
            list.select_nth_unstable_by(config.max_instances, |a, b| a.0.total_cmp(&b.0));
            list.truncate(config.max_instances);
        }
        list.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        triangles.reserve(list.len() * 12);
        for (_, index) in list {
            let instance = instances[index];
            let key = (
                (instance.albedo[0].clamp(0.0, 1.0) * 31.0).round() as u8,
                (instance.albedo[1].clamp(0.0, 1.0) * 31.0).round() as u8,
                (instance.albedo[2].clamp(0.0, 1.0) * 31.0).round() as u8,
            );
            let material = *palette.entry(key).or_insert_with(|| {
                let id = materials.len() as u32;
                let is_foliage = instance.albedo[1] > instance.albedo[0] * 1.2
                    && instance.albedo[1] > instance.albedo[2] * 1.2;
                let opacity = if is_foliage { 0.62 } else { 1.0 };
                materials.push(RayMaterial {
                    albedo: [
                        instance.albedo[0],
                        instance.albedo[1],
                        instance.albedo[2],
                        1.0,
                    ],
                    surface: [0.75, 0.05, 0.0, opacity],
                });
                id
            });
            push_box(triangles, instance, material);
            pushed += 1;
        }
    }
    pushed
}

/// Chunked multi-LOD terrain harvesting covering the entire visible frustum
/// from the eye out to `far`.
///
/// Near chunks use `base_cell * stride`, mid-distance chunks double the step,
/// and far chunks use coarse LOD steps so the BVH reaches the far frustum edge
/// without exploding triangle counts.
#[allow(clippy::too_many_arguments)]
fn push_terrain_frustum(
    triangles: &mut Vec<RayTriangle>,
    materials: &mut Vec<RayMaterial>,
    eye: Vec3,
    frustum: &Frustum,
    far: f32,
    base_cell: f32,
    config: HarvestConfig,
    ground: &dyn Fn(f32, f32) -> f32,
) {
    let chunk_size = config.chunk_size.max(64.0);
    let near_r = config.radius.max(64.0);
    let mid_r = near_r * 3.0;
    let far_r = near_r * 6.0;
    let fine_step = (base_cell * config.stride.max(1) as f32).max(2.0);

    let material = materials.len() as u32;
    materials.push(RayMaterial {
        albedo: [0.32, 0.36, 0.28, 1.0],
        surface: [0.92, 0.0, 0.0, 1.0],
    });

    let min_cx = ((eye.x - far) / chunk_size).floor() as i32;
    let max_cx = ((eye.x + far) / chunk_size).floor() as i32;
    let min_cz = ((eye.z - far) / chunk_size).floor() as i32;
    let max_cz = ((eye.z + far) / chunk_size).floor() as i32;

    for cz in min_cz..=max_cz {
        let z0 = cz as f32 * chunk_size;
        let z1 = z0 + chunk_size;
        for cx in min_cx..=max_cx {
            let x0 = cx as f32 * chunk_size;
            let x1 = x0 + chunk_size;
            // Shortest horizontal distance from eye to chunk AABB.
            let dx = if eye.x < x0 {
                x0 - eye.x
            } else if eye.x > x1 {
                eye.x - x1
            } else {
                0.0
            };
            let dz = if eye.z < z0 {
                z0 - eye.z
            } else if eye.z > z1 {
                eye.z - z1
            } else {
                0.0
            };
            let dist = (dx * dx + dz * dz).sqrt();
            if dist > far {
                continue;
            }
            let _ = frustum;
            // Pick compact 360-degree LOD divisions per chunk based on distance
            // from eye so the full view distance is covered in every direction
            // without camera-rotation rebuild stalls.
            let target_step = if dist <= near_r {
                fine_step * 2.0
            } else if dist <= mid_r {
                fine_step * 4.0
            } else if dist <= far_r {
                fine_step * 8.0
            } else {
                fine_step * 16.0
            };
            let divs = ((chunk_size / target_step).round() as u32).clamp(2, 20);
            let step = chunk_size / divs as f32;
            let side = (divs + 1) as usize;
            let mut heights = Vec::with_capacity(side * side);
            for iz in 0..side {
                let wz = z0 + iz as f32 * step;
                for ix in 0..side {
                    let wx = x0 + ix as f32 * step;
                    heights.push(ground(wx, wz));
                }
            }
            triangles.reserve((divs * divs * 2) as usize);
            for iz in 0..divs as usize {
                let wz0 = z0 + iz as f32 * step;
                let wz1 = wz0 + step;
                let row0 = iz * side;
                let row1 = (iz + 1) * side;
                for ix in 0..divs as usize {
                    let wx0 = x0 + ix as f32 * step;
                    let wx1 = wx0 + step;
                    let p0 = Vec3::new(wx0, heights[row0 + ix], wz0);
                    let p1 = Vec3::new(wx1, heights[row0 + ix + 1], wz0);
                    let p2 = Vec3::new(wx1, heights[row1 + ix + 1], wz1);
                    let p3 = Vec3::new(wx0, heights[row1 + ix], wz1);
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
                }
            }
        }
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

    #[test]
    fn frustum_harvest_covers_far_frustum_edge() {
        let mut harvester = Harvester::new();
        let eye = Vec3::new(0.0, 25.0, 0.0);
        let forward = Vec3::new(0.0, -0.05, 1.0).normalize();
        let view = Mat4::look_to_rh(eye, forward, Vec3::Y);
        let proj = Mat4::perspective_infinite_reverse_rh(60.0f32.to_radians(), 16.0 / 9.0, 0.5);
        let vp = proj * view;
        let view_distance = 1200.0;
        let ground = |_x: f32, _z: f32| 0.0f32;

        let scene = harvester.scene_frustum(
            HarvestConfig::default(),
            eye,
            forward,
            vp,
            view_distance,
            2.0,
            1,
            0,
            Some(&ground),
            &[],
            &[],
            &[],
        );
        assert!(scene.triangle_count() > 0);
        // Bounds must extend well past the legacy 128-unit tile and reach near
        // the far frustum edge along +Z.
        assert!(
            scene.bounds.max.z >= view_distance * 0.9,
            "expected far frustum coverage >= {}, got max.z = {}",
            view_distance * 0.9,
            scene.bounds.max.z
        );
        // A ray aimed down onto terrain near the far frustum edge must hit.
        let far_hit = scene.closest_hit(Vec3::new(0.0, 40.0, 1050.0), Vec3::NEG_Y, 0.01, 200.0);
        assert!(
            far_hit.is_some(),
            "terrain near far frustum edge must be traced"
        );
    }

    #[test]
    fn chunked_instance_cap_applies_per_chunk_with_nearest_winning() {
        let mut harvester = Harvester::new();
        let eye = Vec3::new(0.0, 10.0, 0.0);
        let forward = Vec3::Z;
        let view = Mat4::look_to_rh(eye, forward, Vec3::Y);
        let proj = Mat4::perspective_infinite_reverse_rh(60.0f32.to_radians(), 16.0 / 9.0, 0.5);
        let vp = proj * view;

        // Place 6 instances in chunk (0, 0) and 6 instances in chunk (0, 2)
        // with max_instances = 3 per chunk. Both chunks must keep 3 (total 6),
        // and the nearest 3 in each chunk must win.
        let mut instances = Vec::new();
        for i in 0..6 {
            instances.push(InstanceProxy {
                centre: Vec3::new(0.0, 5.0, 20.0 + i as f32 * 10.0),
                half: Vec3::splat(2.0),
                albedo: [0.5, 0.5, 0.5],
                yaw: 0.0,
            });
            instances.push(InstanceProxy {
                centre: Vec3::new(0.0, 5.0, 540.0 + i as f32 * 10.0),
                half: Vec3::splat(2.0),
                albedo: [0.5, 0.5, 0.5],
                yaw: 0.0,
            });
        }
        let cfg = HarvestConfig {
            max_instances: 3,
            chunk_size: 256.0,
            ..HarvestConfig::default()
        };
        let scene = harvester.scene_frustum(
            cfg,
            eye,
            forward,
            vp,
            1000.0,
            2.0,
            1,
            123,
            None,
            &[],
            &instances,
            &[],
        );
        assert_eq!(
            harvester.instances, 6,
            "3 instances per chunk across 2 chunks"
        );
        // Nearest instance in chunk (0, 2) at z = 540 must be present; farthest at z = 590 dropped.
        assert!(scene
            .closest_hit(Vec3::new(0.0, 5.0, 535.0), Vec3::Z, 0.01, 10.0)
            .is_some());
        assert!(scene
            .closest_hit(Vec3::new(0.0, 20.0, 590.0), Vec3::NEG_Y, 0.01, 30.0)
            .is_none());
    }

    #[test]
    fn rotating_camera_reuses_cached_bvh_without_rebuilding() {
        let mut harvester = Harvester::new();
        let config = HarvestConfig::default();
        let eye = Vec3::new(10.0, 20.0, 10.0);
        let vp_north = Mat4::perspective_rh(60f32.to_radians(), 16.0 / 9.0, 1000.0, 0.5)
            * Mat4::look_to_rh(eye, Vec3::NEG_Z, Vec3::Y);
        let ground = |_: f32, _: f32| 0.0f32;
        let first = harvester.scene_frustum(
            config,
            eye,
            Vec3::NEG_Z,
            vp_north,
            1000.0,
            2.0,
            1,
            42,
            Some(&ground),
            &[],
            &[],
            &[],
        );
        // Rotating 180 degrees at the same position must be a cache hit so
        // looking around never triggers a CPU BVH or DXR BLAS rebuild stall.
        let cached = harvester
            .cached_if_unchanged_frustum(config, eye, Vec3::Z, 1000.0, 1, 42)
            .expect("rotating the camera must hit the 360-degree BVH cache");
        assert!(std::sync::Arc::ptr_eq(&first, &cached));
    }

    #[test]
    fn real_meshes_and_water_surfaces_are_harvested_with_expected_roughness() {
        let mut harvester = Harvester::new();
        let eye = Vec3::new(0.0, 10.0, 0.0);
        let forward = Vec3::Z;
        let view = Mat4::look_to_rh(eye, forward, Vec3::Y);
        let proj = Mat4::perspective_infinite_reverse_rh(60.0f32.to_radians(), 16.0 / 9.0, 0.5);
        let vp = proj * view;

        let mesh = MeshTriangle {
            a: Vec3::new(-4.0, 0.0, 30.0),
            b: Vec3::new(4.0, 0.0, 30.0),
            c: Vec3::new(0.0, 8.0, 30.0),
            normal: Vec3::NEG_Z,
            albedo: [0.8, 0.2, 0.1],
            roughness: 0.72,
        };
        let water = WaterSurface {
            bounds: [-50.0, 50.0, 40.0, 400.0],
            height: 2.0,
        };
        let scene = harvester.scene_frustum(
            HarvestConfig::default(),
            eye,
            forward,
            vp,
            1000.0,
            2.0,
            1,
            7,
            None,
            &[mesh],
            &[],
            &[water],
        );
        assert_eq!(scene.triangle_count(), 3);
        assert!(
            scene.materials.iter().any(|m| m.surface[0] <= 0.1),
            "water must emit a low-roughness reflective material"
        );
        assert!(
            scene
                .materials
                .iter()
                .any(|m| (m.albedo[0] - 0.8).abs() < 0.05),
            "real mesh albedo must survive into the RT material table"
        );
    }
}
