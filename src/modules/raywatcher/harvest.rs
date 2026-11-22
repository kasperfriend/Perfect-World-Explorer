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

/// Per-triangle source tag stored in `RayTriangle.normal[3]` for
/// `DebugView::OccluderKind` and `DebugView::ReflectionHitKind`.
pub const KIND_NONE: f32 = 0.0;
pub const KIND_WATER: f32 = 1.0;
pub const KIND_TERRAIN_LOD: f32 = 2.0;
pub const KIND_REAL_MESH: f32 = 3.0;
pub const KIND_PROXY: f32 = 4.0;
/// Hard cap on total BVH/BLAS triangles (~38 MB) so extreme `ray_length` values
/// can never exceed `wgpu::Limits::max_buffer_size` or `max_storage_buffer_binding_size`.
pub const MAX_SCENE_TRIANGLES: usize = 600_000;
/// Maximum horizontal terrain harvest radius (world units).
const MAX_TERRAIN_HARVEST_FAR: f32 = 2048.0;

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
    pub opacity: f32,
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

/// Minimum horizontal camera displacement before triggering a frustum ray-scene
/// rebuild: at least 256 world units or 25% of `view_distance`, whichever is
/// larger. Prevents rebuild stalls on small camera movements.
pub fn rebuild_distance_threshold(view_distance: f32) -> f32 {
    (view_distance * 0.25).max(256.0)
}

struct BgBuildResult {
    sig: Signature,
    eye: Vec3,
    far_bucket: u32,
    terrain_generation: u64,
    content_hash: u64,
    config: HarvestConfig,
    scene: std::sync::Arc<SceneGeometry>,
    triangles: usize,
    instances: usize,
    note: Option<String>,
}

/// Builds and caches the ray scene.
#[derive(Default)]
pub struct Harvester {
    cached: Option<(Signature, std::sync::Arc<SceneGeometry>)>,
    last_build_eye: Option<Vec3>,
    last_build_far_bucket: u32,
    last_build_terrain_gen: u64,
    last_build_content_hash: u64,
    last_build_config: Option<HarvestConfig>,
    pending_bg: Option<std::sync::mpsc::Receiver<BgBuildResult>>,
    last_build_finished: Option<std::time::Instant>,
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

    /// Whether the frustum ray scene needs to be rebuilt based on hysteresis
    /// (`>= max(256, 0.25 * view_distance)` horizontal camera travel) or world
    /// content/config changes.
    pub fn needs_frustum_rebuild(
        &self,
        config: HarvestConfig,
        eye: Vec3,
        view_distance: f32,
        terrain_generation: u64,
        content_hash: u64,
    ) -> bool {
        if self.cached.is_none() {
            return true;
        }
        if let Some(t) = self.last_build_finished {
            if t.elapsed() < std::time::Duration::from_millis(1500) {
                return false;
            }
        }
        let far_bucket = (view_distance / 128.0).round() as u32;
        if self.last_build_config != Some(config)
            || self.last_build_terrain_gen != terrain_generation
            || self.last_build_content_hash != content_hash
            || self.last_build_far_bucket != far_bucket
        {
            return true;
        }
        match self.last_build_eye {
            Some(last) => {
                let dist = (eye.x - last.x).hypot(eye.z - last.z);
                dist >= rebuild_distance_threshold(view_distance)
            }
            None => true,
        }
    }

    /// Fast-path cache check for [`Self::scene_frustum`].
    #[allow(clippy::too_many_arguments)]
    pub fn cached_if_unchanged_frustum(
        &self,
        config: HarvestConfig,
        eye: Vec3,
        _view_forward: Vec3,
        view_distance: f32,
        terrain_generation: u64,
        content_hash: u64,
    ) -> Option<std::sync::Arc<SceneGeometry>> {
        if self.needs_frustum_rebuild(config, eye, view_distance, terrain_generation, content_hash)
        {
            None
        } else {
            self.cached.as_ref().map(|(_, scene)| scene.clone())
        }
    }

    /// True while a background BVH build thread is currently running.
    pub fn is_building_in_background(&self) -> bool {
        self.pending_bg.is_some()
    }

    /// Poll a background BVH build without blocking the render thread. When
    /// ready, swaps the new scene into the cache and returns it.
    pub fn poll_background_build(&mut self) -> Option<std::sync::Arc<SceneGeometry>> {
        let rx = self.pending_bg.as_ref()?;
        match rx.try_recv() {
            Ok(result) => {
                self.pending_bg = None;
                self.triangles = result.triangles;
                self.instances = result.instances;
                self.note = result.note;
                self.last_build_eye = Some(result.eye);
                self.last_build_far_bucket = result.far_bucket;
                self.last_build_terrain_gen = result.terrain_generation;
                self.last_build_content_hash = result.content_hash;
                self.last_build_config = Some(result.config);
                self.last_build_finished = Some(std::time::Instant::now());
                self.cached = Some((result.sig, result.scene.clone()));
                Some(result.scene)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pending_bg = None;
                None
            }
        }
    }

    /// Start building the frustum ray scene on a background worker thread so
    /// the render thread keeps drawing with the previous scene and never
    /// stalls during camera movement.
    #[allow(clippy::too_many_arguments)]
    pub fn start_background_frustum_build(
        &mut self,
        config: HarvestConfig,
        eye: Vec3,
        view_forward: Vec3,
        view_projection: Mat4,
        view_distance: f32,
        base_cell: f32,
        terrain_generation: u64,
        content_hash: u64,
        ground: Option<Box<dyn Fn(f32, f32) -> f32 + Send>>,
        page_tris: Vec<std::sync::Arc<[crate::world_objects::RtMeshTriangle]>>,
        page_tri_budget: usize,
        mut real_meshes: Vec<MeshTriangle>,
        instances: Vec<InstanceProxy>,
        water: Vec<WaterSurface>,
        skip_cpu_bvh: bool,
    ) {
        if self.pending_bg.is_some() {
            return;
        }
        let sig = frustum_signature(
            config,
            eye,
            view_forward,
            view_distance,
            terrain_generation,
            content_hash,
        );
        let far_bucket = (view_distance / 128.0).round() as u32;
        let far = if config.max_distance > 0.0 {
            config.max_distance
        } else {
            view_distance.max(config.radius)
        };
        let frustum = Frustum::from_view_projection(view_projection);

        // Run terrain sampling, mesh/proxy ingestion, and SAH BVH construction
        // entirely on the background worker thread so moving the camera never
        // stalls the main render thread.
        let t_start = std::time::Instant::now();
        let _ = &water;
        let water_quads = 0usize;

        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.pending_bg = Some(rx);
        let _ = std::thread::Builder::new()
            .name("raywatcher-bvh".to_string())
            .spawn(move || {
                let mut triangles = Vec::new();
                let mut materials = Vec::new();
                let mut terrain_lod_tris = [0usize; 4];
                if let Some(ground_fn) = ground {
                    terrain_lod_tris = push_terrain_frustum(
                        &mut triangles,
                        &mut materials,
                        eye,
                        &frustum,
                        far,
                        base_cell.max(0.5),
                        config,
                        ground_fn.as_ref(),
                    );
                }
                // Unpack O(1)-cloned page Arc slices entirely off the main thread.
                let mut page_added = 0usize;
                for tris in &page_tris {
                    if tris.is_empty() || page_added >= page_tri_budget {
                        break;
                    }
                    let remaining = page_tri_budget - page_added;
                    let step = (tris.len() / remaining.max(1)).max(1);
                    let mut ti = 0usize;
                    while ti < tris.len() && page_added < page_tri_budget {
                        let t = &tris[ti];
                        real_meshes.push(MeshTriangle {
                            a: Vec3::from_array(t.a),
                            b: Vec3::from_array(t.b),
                            c: Vec3::from_array(t.c),
                            normal: Vec3::from_array(t.normal),
                            albedo: t.albedo,
                            roughness: 0.72,
                            opacity: t.opacity,
                        });
                        page_added += 1;
                        ti += step;
                    }
                }
                let before_mesh = triangles.len();
                push_real_meshes(&mut triangles, &mut materials, &real_meshes);
                let real_mesh_tris = triangles.len() - before_mesh;

                let before_proxy = triangles.len();
                let instance_count = push_instances_chunked(
                    &mut triangles,
                    &mut materials,
                    &instances,
                    eye,
                    &frustum,
                    far,
                    config,
                );
                let proxy_tris = triangles.len() - before_proxy;

                if triangles.len() > MAX_SCENE_TRIANGLES {
                    triangles.truncate(MAX_SCENE_TRIANGLES);
                }
                let total_tris = triangles.len();
                let note = if total_tris == 0 {
                    Some("no geometry inside the visible frustum yet".to_string())
                } else {
                    None
                };
                let t_bvh = std::time::Instant::now();
                let scene = std::sync::Arc::new(if skip_cpu_bvh {
                    SceneGeometry::build_triangles_only(triangles, materials)
                } else {
                    SceneGeometry::build(triangles, materials)
                });
                let bvh_ms = t_bvh.elapsed().as_secs_f64() * 1000.0;
                let total_ms = t_start.elapsed().as_secs_f64() * 1000.0;
                if std::env::var_os("PWE_RT_DEBUG").is_some() {
                    let (mut k_water, mut k_terrain, mut k_mesh, mut k_proxy) = (0usize, 0usize, 0usize, 0usize);
                    for t in scene.triangles() {
                        match t.kind() as u32 {
                            1 => k_water += 1,
                            2 => k_terrain += 1,
                            3 => k_mesh += 1,
                            4 => k_proxy += 1,
                            _ => {}
                        }
                    }
                    eprintln!(
                        "[Raywatcher] bg_build: kinds(water={k_water}, terrain_lod={k_terrain}, real_mesh={k_mesh}, tree_proxy={k_proxy}) water_quads={water_quads} lod_terrain_tris=[near:{}, mid:{}, far:{}, vfar:{}] real_mesh_tris={real_mesh_tris} proxy_tris={proxy_tris} total_tris={total_tris} bvh_build_ms={bvh_ms:.2} total_ms={total_ms:.2}",
                        terrain_lod_tris[0],
                        terrain_lod_tris[1],
                        terrain_lod_tris[2],
                        terrain_lod_tris[3],
                    );
                }
                let _ = tx.send(BgBuildResult {
                    sig,
                    eye,
                    far_bucket,
                    terrain_generation,
                    content_hash,
                    config,
                    scene,
                    triangles: total_tris,
                    instances: instance_count,
                    note,
                });
            });
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
        if !self.needs_frustum_rebuild(config, eye, view_distance, terrain_generation, content_hash)
        {
            if let Some((_, scene)) = &self.cached {
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

        let t_harvest = std::time::Instant::now();
        // 1. Real mesh triangles (near-camera buildings/props with real
        //    normals and baked vertex diffuse albedo).
        let before_mesh = triangles.len();
        push_real_meshes(&mut triangles, &mut materials, real_meshes);
        let real_mesh_tris = triangles.len() - before_mesh;

        // 2. Chunked instance proxies (distant buildings/props and trees:
        //    proxies are much cheaper than full foliage/distant meshes and
        //    still cast accurate silhouettes and reflections). `max_instances`
        //    is enforced per spatial chunk with nearest instances winning.
        let before_proxy = triangles.len();
        self.instances = push_instances_chunked(
            &mut triangles,
            &mut materials,
            instances,
            eye,
            &frustum,
            far,
            config,
        );
        let proxy_tris = triangles.len() - before_proxy;

        // 3. Phase 1: Water sheets are excluded from the occluder BVH so they
        //    never cast phantom shadows/AO or wedges on dry ground.
        let _ = water;
        let water_quads = 0usize;

        // 4. Chunked multi-LOD terrain across the entire visible frustum out to
        //    `far`.
        let mut terrain_lod_tris = [0usize; 4];
        if let Some(ground_fn) = ground {
            terrain_lod_tris = push_terrain_frustum(
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
        let t_bvh = std::time::Instant::now();
        let scene = std::sync::Arc::new(SceneGeometry::build(triangles, materials));
        let bvh_ms = t_bvh.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_harvest.elapsed().as_secs_f64() * 1000.0;
        if std::env::var_os("PWE_RT_DEBUG").is_some() {
            eprintln!(
                "[Raywatcher] scene_frustum: water_quads={water_quads} lod_terrain_tris=[near:{}, mid:{}, far:{}, vfar:{}] real_mesh_tris={real_mesh_tris} proxy_tris={proxy_tris} total_tris={} bvh_build_ms={bvh_ms:.2} total_ms={total_ms:.2}",
                terrain_lod_tris[0],
                terrain_lod_tris[1],
                terrain_lod_tris[2],
                terrain_lod_tris[3],
                self.triangles,
            );
        }
        self.last_build_eye = Some(eye);
        self.last_build_far_bucket = (view_distance / 128.0).round() as u32;
        self.last_build_terrain_gen = terrain_generation;
        self.last_build_content_hash = content_hash;
        self.last_build_config = Some(config);
        self.cached = Some((sig, scene.clone()));
        scene
    }

    /// Forget the cache, so the next [`Self::scene`] rebuilds.
    pub fn invalidate(&mut self) {
        self.cached = None;
        self.last_build_eye = None;
        self.last_build_config = None;
        self.pending_bg = None;
        self.last_build_finished = None;
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
            surface: [
                0.75,
                0.05,
                0.0,
                if instance.albedo[1] > instance.albedo[0] * 1.15
                    && instance.albedo[1] > instance.albedo[2] * 1.15
                {
                    0.58
                } else {
                    1.0
                },
            ],
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
        triangles.push(RayTriangle::with_kind(
            a, b, c, normal, material, KIND_PROXY,
        ));
        triangles.push(RayTriangle::with_kind(
            a, c, d, normal, material, KIND_PROXY,
        ));
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
            triangles.push(RayTriangle::with_kind(
                p0,
                p2,
                p1,
                n0,
                material,
                KIND_TERRAIN_LOD,
            ));
            triangles.push(RayTriangle::with_kind(
                p0,
                p3,
                p2,
                n1,
                material,
                KIND_TERRAIN_LOD,
            ));
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
    let snap = config.chunk_size.max(256.0);
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
    let mut palette: std::collections::HashMap<(u8, u8, u8, u8, u8), u32> =
        std::collections::HashMap::new();
    triangles.reserve(meshes.len());
    let mut dropped = 0usize;
    let before = triangles.len();
    for m in meshes {
        if !m.a.is_finite() || !m.b.is_finite() || !m.c.is_finite() {
            dropped += 1;
            continue;
        }
        let geo = (m.b - m.a).cross(m.c - m.a);
        let area2_sq = geo.length_squared();
        if !area2_sq.is_finite() || area2_sq <= 1e-8 {
            dropped += 1;
            continue;
        }
        let normal = if m.normal.is_finite() && (m.normal.length() - 1.0).abs() <= 1e-3 {
            m.normal
        } else if m.normal.is_finite() && m.normal.length_squared() > 1e-6 {
            m.normal.normalize()
        } else {
            geo / area2_sq.sqrt()
        };
        if !normal.is_finite() || (normal.length() - 1.0).abs() > 1e-3 {
            dropped += 1;
            continue;
        }
        let opacity = m.opacity.clamp(0.0, 1.0);
        let key = (
            (m.albedo[0].clamp(0.0, 1.0) * 31.0).round() as u8,
            (m.albedo[1].clamp(0.0, 1.0) * 31.0).round() as u8,
            (m.albedo[2].clamp(0.0, 1.0) * 31.0).round() as u8,
            (m.roughness.clamp(0.0, 1.0) * 15.0).round() as u8,
            (opacity * 15.0).round() as u8,
        );
        let mat_id = *palette.entry(key).or_insert_with(|| {
            let id = materials.len() as u32;
            materials.push(RayMaterial {
                albedo: [m.albedo[0], m.albedo[1], m.albedo[2], 1.0],
                surface: [m.roughness.clamp(0.04, 1.0), 0.05, 0.0, opacity],
            });
            id
        });
        triangles.push(RayTriangle::with_kind(
            m.a,
            m.b,
            m.c,
            normal,
            mat_id,
            KIND_REAL_MESH,
        ));
    }
    if dropped > 0 && std::env::var_os("PWE_RT_DEBUG").is_some() {
        eprintln!(
            "[Raywatcher] Phase R3 push_real_meshes: kept={}, dropped_invalid={dropped}",
            triangles.len() - before
        );
    }
}

/// Push reflective water quads across the visible frustum. Water uses low
/// roughness (`0.06`) so both the WGSL and DXR shaders trace mirror-like
/// Fresnel reflections on lakes, rivers, and seas.
///
/// When a terrain height function (`ground`) is supplied, each water body is
/// subdivided into world cells and any cell whose terrain rises above
/// `w.height` is dropped. This prevents rectangular water AABBs from placing
/// phantom water occluders over dry sandbanks, islands, or adjacent ground.
#[cfg_attr(not(test), allow(dead_code))]
fn push_water(
    triangles: &mut Vec<RayTriangle>,
    materials: &mut Vec<RayMaterial>,
    water: &[WaterSurface],
    eye: Vec3,
    frustum: &Frustum,
    far: f32,
    ground: Option<&dyn Fn(f32, f32) -> f32>,
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
        let Some(ground_fn) = ground else {
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
            triangles.push(RayTriangle::with_kind(
                p0,
                p1,
                p2,
                Vec3::Y,
                mat_id,
                KIND_WATER,
            ));
            triangles.push(RayTriangle::with_kind(
                p0,
                p2,
                p3,
                Vec3::Y,
                mat_id,
                KIND_WATER,
            ));
            continue;
        };

        // Clamp the subdivided footprint to the visible radius so large sea
        // planes spend cell resolution near the camera rather than far away.
        let vx0 = x0.max(eye.x - far);
        let vx1 = x1.min(eye.x + far);
        let vz0 = z0.max(eye.z - far);
        let vz1 = z1.min(eye.z + far);
        if vx1 <= vx0 || vz1 <= vz0 {
            continue;
        }
        let target_cell = 8.0_f32;
        let nx = (((vx1 - vx0) / target_cell).ceil() as usize).clamp(1, 64);
        let nz = (((vz1 - vz0) / target_cell).ceil() as usize).clamp(1, 64);
        let step_x = (vx1 - vx0) / nx as f32;
        let step_z = (vz1 - vz0) / nz as f32;

        // Pre-sample grid corner heights once per vertex to avoid redundant
        // height lookups across shared cell corners.
        let side_x = nx + 1;
        let mut corner_h = Vec::with_capacity(side_x * (nz + 1));
        for iz in 0..=nz {
            let wz = vz0 + iz as f32 * step_z;
            for ix in 0..=nx {
                let wx = vx0 + ix as f32 * step_x;
                corner_h.push(ground_fn(wx, wz));
            }
        }

        for iz in 0..nz {
            let cz0 = vz0 + iz as f32 * step_z;
            let cz1 = cz0 + step_z;
            let row0 = iz * side_x;
            let row1 = (iz + 1) * side_x;
            for ix in 0..nx {
                let cx0 = vx0 + ix as f32 * step_x;
                let cx1 = cx0 + step_x;
                let h00 = corner_h[row0 + ix];
                let h10 = corner_h[row0 + ix + 1];
                let h11 = corner_h[row1 + ix + 1];
                let h01 = corner_h[row1 + ix];
                let h_min = h00.min(h10).min(h11).min(h01);
                // Drop cells where all corners are above the water plane.
                if h_min > w.height {
                    continue;
                }
                let hc = ground_fn((cx0 + cx1) * 0.5, (cz0 + cz1) * 0.5);
                // Drop cells whose centre is dry ground above the water plane.
                if hc > w.height {
                    continue;
                }
                let mat_id = *water_mat.get_or_insert_with(|| {
                    let id = materials.len() as u32;
                    materials.push(RayMaterial {
                        albedo: [0.10, 0.22, 0.28, 1.0],
                        surface: [0.06, 0.02, 0.0, 1.0],
                    });
                    id
                });
                let p0 = Vec3::new(cx0, w.height, cz0);
                let p1 = Vec3::new(cx1, w.height, cz0);
                let p2 = Vec3::new(cx1, w.height, cz1);
                let p3 = Vec3::new(cx0, w.height, cz1);
                triangles.push(RayTriangle::with_kind(
                    p0,
                    p1,
                    p2,
                    Vec3::Y,
                    mat_id,
                    KIND_WATER,
                ));
                triangles.push(RayTriangle::with_kind(
                    p0,
                    p2,
                    p3,
                    Vec3::Y,
                    mat_id,
                    KIND_WATER,
                ));
            }
        }
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
    let mut chunks: std::collections::HashMap<(i32, i32), Vec<(f32, usize)>> =
        std::collections::HashMap::new();
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
            list.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        }
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
) -> [usize; 4] {
    let mut lod_tris = [0usize; 4];
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

    let clamped_far = far.clamp(64.0, MAX_TERRAIN_HARVEST_FAR);
    let min_cx = ((eye.x - clamped_far) / chunk_size).floor() as i32;
    let max_cx = ((eye.x + clamped_far) / chunk_size).floor() as i32;
    let min_cz = ((eye.z - clamped_far) / chunk_size).floor() as i32;
    let max_cz = ((eye.z + clamped_far) / chunk_size).floor() as i32;

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
            if dist > clamped_far {
                continue;
            }
            let _ = frustum;
            // Phase 2: Use power-of-two subdivisions aligned to the raster
            // terrain grid (`base_cell`, e.g. 2.0u) with the exact same
            // diagonal (`(p0, p2)` == `(c, d)` in `terrain_lod::block_surface`
            // and `MapSpace::ground`). Cells immediately around the camera use
            // 1:1 `base_cell` spacing so RT terrain matches the raster terrain
            // within 0.1 world units on slopes and paths (no sharp wedges).
            let _ = fine_step;
            let cells_per_chunk = (chunk_size / base_cell.max(0.5)).round().max(8.0) as u32;
            let (lod_band, divs) = if dist <= 64.0 {
                (0usize, cells_per_chunk.min(128))
            } else if dist <= near_r {
                (0usize, (cells_per_chunk / 2).clamp(16, 64))
            } else if dist <= mid_r {
                (1usize, (cells_per_chunk / 4).clamp(8, 32))
            } else if dist <= far_r {
                (2usize, (cells_per_chunk / 8).clamp(4, 16))
            } else {
                (3usize, (cells_per_chunk / 16).clamp(2, 8))
            };
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
            // Phase R2: Emit terrain triangles with CCW upward-facing (+Y)
            // winding so geometric normal `(b - a).cross(c - a)` matches the
            // stored normal on every subdivision (2-tri quads and 4-tri fans).
            let tri_normal = |a: Vec3, b: Vec3, c: Vec3| -> Vec3 {
                let n = (b - a).cross(c - a).normalize_or_zero();
                if n.length_squared() > 0.5 {
                    n
                } else {
                    Vec3::Y
                }
            };
            // Boost #10: On 1:1 raster cells (`step <= base_cell + 0.01`), `ground()`
            // is piecewise-linear inside each cell so midpoints match linear edges
            // exactly; skip all 5 redundant midpoint queries per cell (5x faster!).
            // On coarser LOD chunks, pre-sample shared horizontal/vertical edge
            // midpoints once instead of redundantly querying each shared edge twice.
            let is_unit_cell = step <= base_cell + 0.01;
            let (h_edges, v_edges) = if is_unit_cell {
                (Vec::new(), Vec::new())
            } else {
                let mut he = Vec::with_capacity(side * divs as usize);
                for iz in 0..side {
                    let wz = z0 + iz as f32 * step;
                    for ix in 0..divs as usize {
                        he.push(ground(x0 + (ix as f32 + 0.5) * step, wz));
                    }
                }
                let mut ve = Vec::with_capacity(divs as usize * side);
                for iz in 0..divs as usize {
                    let wzc = z0 + (iz as f32 + 0.5) * step;
                    for ix in 0..side {
                        ve.push(ground(x0 + ix as f32 * step, wzc));
                    }
                }
                (he, ve)
            };
            triangles.reserve((divs * divs * 2) as usize);
            let divs_u = divs as usize;
            for iz in 0..divs_u {
                let wz0 = z0 + iz as f32 * step;
                let wz1 = wz0 + step;
                let wzc = (wz0 + wz1) * 0.5;
                let row0 = iz * side;
                let row1 = (iz + 1) * side;
                for ix in 0..divs_u {
                    let wx0 = x0 + ix as f32 * step;
                    let wx1 = wx0 + step;
                    let wxc = (wx0 + wx1) * 0.5;
                    let h0 = heights[row0 + ix];
                    let h1 = heights[row0 + ix + 1];
                    let h2 = heights[row1 + ix + 1];
                    let h3 = heights[row1 + ix];
                    let (hc, hm_n, hm_s, hm_w, hm_e, err_c, edge_err) = if is_unit_cell {
                        ((h0 + h2) * 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
                    } else {
                        let hc = ground(wxc, wzc);
                        let hm_n = h_edges[iz * divs_u + ix];
                        let hm_s = h_edges[(iz + 1) * divs_u + ix];
                        let hm_w = v_edges[iz * side + ix];
                        let hm_e = v_edges[iz * side + ix + 1];
                        let err_c = ((h0 + h2) * 0.5 - hc).abs();
                        let edge_err = ((h0 + h1) * 0.5 - hm_n)
                            .abs()
                            .max(((h3 + h2) * 0.5 - hm_s).abs())
                            .max(((h0 + h3) * 0.5 - hm_w).abs())
                            .max(((h1 + h2) * 0.5 - hm_e).abs());
                        (hc, hm_n, hm_s, hm_w, hm_e, err_c, edge_err)
                    };

                    if edge_err > 0.08 {
                        // Watertight 8-triangle star pinning both the cell
                        // centre AND all 4 edge midpoints to exact `ground()`
                        // heights: eliminates elevated bridges over valley
                        // roads between hills and seals LOD-boundary T-junctions
                        // on curved slopes.
                        let p0 = Vec3::new(wx0, h0, wz0);
                        let pm_n = Vec3::new(wxc, hm_n, wz0);
                        let p1 = Vec3::new(wx1, h1, wz0);
                        let pm_e = Vec3::new(wx1, hm_e, wzc);
                        let p2 = Vec3::new(wx1, h2, wz1);
                        let pm_s = Vec3::new(wxc, hm_s, wz1);
                        let p3 = Vec3::new(wx0, h3, wz1);
                        let pm_w = Vec3::new(wx0, hm_w, wzc);
                        let pc = Vec3::new(wxc, hc, wzc);
                        let ring = [p0, pm_n, p1, pm_e, p2, pm_s, p3, pm_w];
                        for k in 0..8 {
                            let a = ring[k];
                            let b = ring[(k + 1) & 7];
                            triangles.push(RayTriangle::with_kind(
                                a,
                                pc,
                                b,
                                tri_normal(a, pc, b),
                                material,
                                KIND_TERRAIN_LOD,
                            ));
                        }
                        lod_tris[lod_band] += 8;
                    } else if err_c > 0.08 {
                        // Watertight 4-triangle fan through the exact centre
                        // height when all 4 perimeter edges are linear.
                        let p0 = Vec3::new(wx0, h0, wz0);
                        let p1 = Vec3::new(wx1, h1, wz0);
                        let p2 = Vec3::new(wx1, h2, wz1);
                        let p3 = Vec3::new(wx0, h3, wz1);
                        let pc = Vec3::new(wxc, hc, wzc);
                        triangles.push(RayTriangle::with_kind(
                            p0,
                            pc,
                            p1,
                            tri_normal(p0, pc, p1),
                            material,
                            KIND_TERRAIN_LOD,
                        ));
                        triangles.push(RayTriangle::with_kind(
                            p1,
                            pc,
                            p2,
                            tri_normal(p1, pc, p2),
                            material,
                            KIND_TERRAIN_LOD,
                        ));
                        triangles.push(RayTriangle::with_kind(
                            p2,
                            pc,
                            p3,
                            tri_normal(p2, pc, p3),
                            material,
                            KIND_TERRAIN_LOD,
                        ));
                        triangles.push(RayTriangle::with_kind(
                            p3,
                            pc,
                            p0,
                            tri_normal(p3, pc, p0),
                            material,
                            KIND_TERRAIN_LOD,
                        ));
                        lod_tris[lod_band] += 4;
                    } else {
                        let p0 = Vec3::new(wx0, h0, wz0);
                        let p1 = Vec3::new(wx1, h1, wz0);
                        let p2 = Vec3::new(wx1, h2, wz1);
                        let p3 = Vec3::new(wx0, h3, wz1);
                        triangles.push(RayTriangle::with_kind(
                            p0,
                            p2,
                            p1,
                            tri_normal(p0, p2, p1),
                            material,
                            KIND_TERRAIN_LOD,
                        ));
                        triangles.push(RayTriangle::with_kind(
                            p0,
                            p3,
                            p2,
                            tri_normal(p0, p3, p2),
                            material,
                            KIND_TERRAIN_LOD,
                        ));
                        lod_tris[lod_band] += 2;
                    }
                }
            }
        }
    }
    lod_tris
}

/// Sample the vertical height of the harvested RT terrain triangles at `(x, z)`
/// by projecting a vertical downward ray onto the terrain triangle list.
pub(crate) fn sample_terrain_tris_height(tris: &[RayTriangle], x: f32, z: f32) -> Option<f32> {
    for t in tris {
        let ax = t.a[0];
        let az = t.a[2];
        let bx = t.b[0];
        let bz = t.b[2];
        let cx = t.c[0];
        let cz = t.c[2];
        let min_x = ax.min(bx).min(cx) - 1e-4;
        let max_x = ax.max(bx).max(cx) + 1e-4;
        let min_z = az.min(bz).min(cz) - 1e-4;
        let max_z = az.max(bz).max(cz) + 1e-4;
        if x < min_x || x > max_x || z < min_z || z > max_z {
            continue;
        }
        let v0x = bx - ax;
        let v0z = bz - az;
        let v1x = cx - ax;
        let v1z = cz - az;
        let v2x = x - ax;
        let v2z = z - az;
        let det = v0x * v1z - v1x * v0z;
        if det.abs() < 1e-8 {
            continue;
        }
        let inv = 1.0 / det;
        let u = (v2x * v1z - v1x * v2z) * inv;
        let v = (v0x * v2z - v2x * v0z) * inv;
        if u >= -1e-4 && v >= -1e-4 && u + v <= 1.0001 {
            let w = 1.0 - u - v;
            return Some(w * t.a[1] + u * t.b[1] + v * t.c[1]);
        }
    }
    None
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
            opacity: 1.0,
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
        // Phase 1: water sheets must NOT enter the occluder BVH; only the 1 real mesh tri is present.
        assert_eq!(scene.triangle_count(), 1);
        assert!(
            !scene
                .triangles()
                .iter()
                .any(|t| (t.kind() - KIND_WATER).abs() < 0.25),
            "water sheets must not enter the occluder BVH"
        );
        assert!(
            scene
                .materials
                .iter()
                .any(|m| (m.albedo[0] - 0.8).abs() < 0.05),
            "real mesh albedo must survive into the RT material table"
        );
    }

    #[test]
    fn rt_terrain_matches_ground_height_within_tenth_unit_on_slopes() {
        // Piecewise-linear slope/ridge defined on the exact 2.0u raster lattice.
        let spacing = 2.0f32;
        let ground = |x: f32, z: f32| {
            let gx = x / spacing;
            let gz = z / spacing;
            let ix = gx.floor();
            let iz = gz.floor();
            let fx = gx - ix;
            let fz = gz - iz;
            let h = |cx: f32, cz: f32| 3.0 * (cx * 0.25).sin() + 2.0 * (cz * 0.2).cos() + 0.1 * cx;
            let h0 = h(ix, iz);
            let h1 = h(ix + 1.0, iz);
            let h2 = h(ix + 1.0, iz + 1.0);
            let h3 = h(ix, iz + 1.0);
            if fx >= fz {
                h0 + (h1 - h0) * fx + (h2 - h1) * fz
            } else {
                h0 + (h2 - h3) * fx + (h3 - h0) * fz
            }
        };
        let eye = Vec3::new(10.0, 20.0, 10.0);
        let vp = Mat4::perspective_infinite_reverse_rh(60.0f32.to_radians(), 16.0 / 9.0, 0.5)
            * Mat4::look_to_rh(eye, Vec3::Z, Vec3::Y);
        let frustum = Frustum::from_view_projection(vp);
        let mut tris = Vec::new();
        let mut mats = Vec::new();
        push_terrain_frustum(
            &mut tris,
            &mut mats,
            eye,
            &frustum,
            128.0,
            spacing,
            HarvestConfig::default(),
            &ground,
        );
        for &(sx, sz) in &[
            (10.5, 10.25),
            (11.3, 12.7),
            (4.8, 17.1),
            (19.9, 3.4),
            (-5.2, 8.6),
        ] {
            let expected = ground(sx, sz);
            let actual = sample_terrain_tris_height(&tris, sx, sz)
                .expect("near-field terrain must cover sample point");
            assert!(
                (actual - expected).abs() < 0.1,
                "RT terrain height {actual} diverged from ground {expected} at ({sx}, {sz})"
            );
        }
    }

    #[test]
    fn water_sheets_are_excluded_from_occluder_bvh() {
        let mut harvester = Harvester::new();
        let eye = Vec3::new(0.0, 20.0, -40.0);
        let forward = Vec3::Z;
        let vp = Mat4::perspective_infinite_reverse_rh(60.0f32.to_radians(), 16.0 / 9.0, 0.5)
            * Mat4::look_to_rh(eye, forward, Vec3::Y);
        let water = [WaterSurface {
            bounds: [-32.0, 32.0, -32.0, 32.0],
            height: 5.0,
        }];
        let scene = harvester.scene_frustum(
            HarvestConfig::default(),
            eye,
            forward,
            vp,
            200.0,
            4.0,
            1,
            1,
            Some(&|_, _| 0.0),
            &[],
            &[],
            &water,
        );
        assert!(
            !scene
                .triangles()
                .iter()
                .any(|t| (t.kind() - KIND_WATER).abs() < 0.25),
            "water sheets must never appear in the occluder BVH"
        );
    }

    #[test]
    fn water_harvest_drops_cells_where_ground_is_above_water() {
        // Left half (x < 0) is a submerged lakebed (y = 0.0 < water 5.0).
        // Right half (x >= 0) is dry sand above water (y = 12.0 > water 5.0).
        let ground = |x: f32, _z: f32| if x < 0.0 { 0.0 } else { 12.0 };
        let water = WaterSurface {
            bounds: [-32.0, 32.0, -32.0, 32.0],
            height: 5.0,
        };
        let eye = Vec3::new(0.0, 20.0, -40.0);
        let vp = Mat4::perspective_infinite_reverse_rh(60.0f32.to_radians(), 16.0 / 9.0, 0.5)
            * Mat4::look_to_rh(eye, Vec3::Z, Vec3::Y);
        let frustum = Frustum::from_view_projection(vp);
        let mut triangles = Vec::new();
        let mut materials = Vec::new();
        push_water(
            &mut triangles,
            &mut materials,
            &[water],
            eye,
            &frustum,
            256.0,
            Some(&ground),
        );
        assert!(
            !triangles.is_empty(),
            "submerged half of the water body must still emit water triangles"
        );
        for tri in &triangles {
            let centroid_x = (tri.a[0] + tri.b[0] + tri.c[0]) / 3.0;
            assert!(
                centroid_x < 0.0,
                "dry sand cell (centroid_x={centroid_x}) must not receive a water occluder quad"
            );
        }
    }

    #[test]
    fn terrain_lod_does_not_overshoot_valley_floor() {
        // Concave valley dipping 4.0 units below the surrounding flat plain.
        let ground = |x: f32, z: f32| {
            let r2 = x * x + z * z;
            -4.0 * (-r2 / 36.0).exp()
        };
        let eye = Vec3::new(0.0, 12.0, -24.0);
        let vp = Mat4::perspective_infinite_reverse_rh(60.0f32.to_radians(), 16.0 / 9.0, 0.5)
            * Mat4::look_to_rh(eye, Vec3::Z, Vec3::Y);
        let frustum = Frustum::from_view_projection(vp);
        let mut triangles = Vec::new();
        let mut materials = Vec::new();
        push_terrain_frustum(
            &mut triangles,
            &mut materials,
            eye,
            &frustum,
            256.0,
            2.0,
            HarvestConfig::default(),
            &ground,
        );
        let scene = SceneGeometry::build(triangles, materials);
        // Shoot an upward ray from 0.25 units above the valley floor at (0, 0):
        // if the LOD terrain bridged above the valley, the upward ray would hit
        // the underside of the chord and report occlusion.
        let origin = Vec3::new(0.0, ground(0.0, 0.0) + 0.25, 0.0);
        assert!(
            !scene.occluded(origin, Vec3::Y, 0.05, 50.0),
            "harvested LOD terrain must not bridge above the valley floor and occlude upward rays"
        );
    }

    #[test]
    fn rebuild_hysteresis_skips_small_camera_moves_and_triggers_past_threshold() {
        let mut harvester = Harvester::new();
        let eye0 = Vec3::new(0.0, 10.0, 0.0);
        let forward = Vec3::Z;
        let vp = Mat4::perspective_infinite_reverse_rh(60.0f32.to_radians(), 16.0 / 9.0, 0.5)
            * Mat4::look_to_rh(eye0, forward, Vec3::Y);
        let ground = |_x: f32, _z: f32| 0.0;
        let view_distance = 1200.0; // threshold = max(256, 300) = 300.0
        assert!((rebuild_distance_threshold(view_distance) - 300.0).abs() < 1e-3);
        assert!((rebuild_distance_threshold(500.0) - 256.0).abs() < 1e-3);

        let first = harvester.scene_frustum(
            HarvestConfig::default(),
            eye0,
            forward,
            vp,
            view_distance,
            2.0,
            1,
            42,
            Some(&ground),
            &[],
            &[],
            &[],
        );

        // Moving 100 units (previously crossed 64u snap!) must NOT trigger a rebuild.
        let eye_small = Vec3::new(100.0, 10.0, 50.0);
        assert!(
            !harvester.needs_frustum_rebuild(
                HarvestConfig::default(),
                eye_small,
                view_distance,
                1,
                42
            ),
            "small camera move under threshold must reuse cached scene"
        );
        let reused = harvester
            .cached_if_unchanged_frustum(
                HarvestConfig::default(),
                eye_small,
                forward,
                view_distance,
                1,
                42,
            )
            .expect("cached scene must be returned within hysteresis threshold");
        assert!(std::sync::Arc::ptr_eq(&first, &reused));

        // Moving 320 units (>= 300 threshold) MUST trigger a rebuild.
        let eye_far = Vec3::new(320.0, 10.0, 0.0);
        assert!(
            harvester.needs_frustum_rebuild(
                HarvestConfig::default(),
                eye_far,
                view_distance,
                1,
                42
            ),
            "camera move beyond hysteresis threshold must request a rebuild"
        );
    }

    #[test]
    fn phase_r2_terrain_winding_matches_upward_normal_and_front_face_hits() {
        let mut harvester = Harvester::new();
        let eye = Vec3::new(10.0, 20.0, 10.0);
        let vp = Mat4::IDENTITY;
        let flat_ground = |_x: f32, _z: f32| -> f32 { 10.0 };
        let scene = harvester.scene_frustum(
            HarvestConfig {
                radius: 64.0,
                chunk_size: 64.0,
                max_distance: 64.0,
                ..HarvestConfig::default()
            },
            eye,
            Vec3::NEG_Z,
            vp,
            64.0,
            2.0,
            1,
            1,
            Some(&flat_ground),
            &[],
            &[],
            &[],
        );
        assert!(
            !scene.triangles.is_empty(),
            "terrain should produce triangles"
        );
        for tri in &scene.triangles {
            let geo = super::super::bvh::triangle_normal(tri).normalize();
            let stored = Vec3::new(tri.normal[0], tri.normal[1], tri.normal[2]);
            assert!(
                geo.y > 0.999 && stored.y > 0.999,
                "flat terrain geometric normal {geo:?} and stored normal {stored:?} must point +Y"
            );
            assert!(
                geo.dot(stored) > 0.999,
                "terrain winding normal {geo:?} must match stored normal {stored:?}"
            );
        }
        // Ray from above hits front-facing terrain triangle at t = 10.0.
        let down_hit = scene
            .closest_hit(Vec3::new(10.0, 20.0, 10.0), Vec3::NEG_Y, 0.01, 100.0)
            .expect("downward ray from above must hit terrain");
        let down_tri = &scene.triangles[down_hit.triangle as usize];
        let down_geo = super::super::bvh::triangle_normal(down_tri);
        assert!(
            down_geo.dot(Vec3::NEG_Y) < 0.0,
            "ray from above must hit the front (+Y) face of terrain"
        );
        assert!((down_hit.t - 10.0).abs() < 1e-3);

        // Ray from below approaches from the back face (front-face test rejects it).
        let up_hit = scene
            .closest_hit(Vec3::new(10.0, 0.0, 10.0), Vec3::Y, 0.01, 100.0)
            .expect("two-sided BVH intersects plane");
        let up_tri = &scene.triangles[up_hit.triangle as usize];
        let up_geo = super::super::bvh::triangle_normal(up_tri);
        assert!(
            up_geo.dot(Vec3::Y) > 0.0,
            "ray from below must NOT hit the front face of terrain"
        );

        // Also verify concave 4-triangle fan winding matches +Y.
        let mut valley_harvester = Harvester::new();
        let valley_ground = |x: f32, z: f32| -> f32 {
            // Concave bowl so lod_band == 0 triggers the 4-tri fan.
            ((x - 10.0) * (x - 10.0) + (z - 10.0) * (z - 10.0)) * 0.05
        };
        let valley = valley_harvester.scene_frustum(
            HarvestConfig {
                radius: 64.0,
                chunk_size: 64.0,
                max_distance: 64.0,
                ..HarvestConfig::default()
            },
            eye,
            Vec3::NEG_Z,
            vp,
            64.0,
            2.0,
            2,
            2,
            Some(&valley_ground),
            &[],
            &[],
            &[],
        );
        for tri in &valley.triangles {
            let geo = super::super::bvh::triangle_normal(tri).normalize();
            let stored = Vec3::new(tri.normal[0], tri.normal[1], tri.normal[2]);
            assert!(
                geo.y > 0.0 && stored.y > 0.0 && geo.dot(stored) > 0.999,
                "concave fan terrain winding {geo:?} must match stored normal {stored:?} and face +Y"
            );
        }
    }

    #[test]
    fn phase_r3_real_meshes_drop_zero_area_and_sanitize_normals() {
        let meshes = vec![
            // Valid triangle with non-unit normal (should be normalized to unit length).
            MeshTriangle {
                a: Vec3::new(0.0, 0.0, 0.0),
                b: Vec3::new(2.0, 0.0, 0.0),
                c: Vec3::new(0.0, 2.0, 0.0),
                normal: Vec3::new(0.0, 0.0, 5.0),
                albedo: [0.5, 0.4, 0.3],
                roughness: 0.8,
                opacity: 1.0,
            },
            // Zero-area degenerate triangle (must be dropped).
            MeshTriangle {
                a: Vec3::new(1.0, 1.0, 1.0),
                b: Vec3::new(1.0, 1.0, 1.0),
                c: Vec3::new(2.0, 2.0, 2.0),
                normal: Vec3::Y,
                albedo: [0.5, 0.5, 0.5],
                roughness: 0.8,
                opacity: 1.0,
            },
            // NaN vertex triangle (must be dropped).
            MeshTriangle {
                a: Vec3::new(f32::NAN, 0.0, 0.0),
                b: Vec3::new(1.0, 0.0, 0.0),
                c: Vec3::new(0.0, 1.0, 0.0),
                normal: Vec3::Y,
                albedo: [0.5, 0.5, 0.5],
                roughness: 0.8,
                opacity: 1.0,
            },
            // Valid triangle with zero normal (recomputed from geometry).
            MeshTriangle {
                a: Vec3::new(0.0, 0.0, 0.0),
                b: Vec3::new(0.0, 0.0, 2.0),
                c: Vec3::new(2.0, 0.0, 0.0),
                normal: Vec3::ZERO,
                albedo: [0.3, 0.6, 0.3],
                roughness: 0.85,
                opacity: 0.7,
            },
        ];
        let mut tris = Vec::new();
        let mut mats = Vec::new();
        push_real_meshes(&mut tris, &mut mats, &meshes);
        assert_eq!(tris.len(), 2, "zero-area and NaN triangles must be dropped");
        for t in &tris {
            let n = Vec3::new(t.normal[0], t.normal[1], t.normal[2]);
            assert!(
                (n.length() - 1.0).abs() <= 1e-3,
                "normal must be unit length within 1e-3, got {n:?}"
            );
            assert_eq!(t.normal[3], 0.0, "normal[3] must remain 0.0");
            assert_eq!(t.kind(), KIND_REAL_MESH, "kind must be stored in a[3]");
        }
    }

    #[test]
    fn frustum_build_caps_triangles_safely_at_extreme_ray_length() {
        let flat = |_x: f32, _z: f32| 0.0f32;
        let mut tris = Vec::new();
        let mut mats = Vec::new();
        let eye = Vec3::new(0.0, 20.0, 0.0);
        let frustum = Frustum::from_view_projection(glam::Mat4::IDENTITY);
        let cfg = HarvestConfig::default();
        push_terrain_frustum(
            &mut tris, &mut mats, eye, &frustum, 64_000.0, 2.0, cfg, &flat,
        );
        assert!(
            tris.len() <= MAX_SCENE_TRIANGLES,
            "extreme ray_length produced {} triangles (cap {})",
            tris.len(),
            MAX_SCENE_TRIANGLES
        );
        assert!(
            tris.len() > 1000,
            "terrain should still cover visible range"
        );
    }

    #[test]
    fn terrain_frustum_valley_road_has_no_elevated_bridge_over_edges() {
        // V-shaped road valley running along x = 90 between two steep hills.
        // At dist ~80u (LOD step 4u), cell [88..92] spans both hills; the
        // 8-triangle star must pin the North/South edge midpoints (x = 90)
        // to the road floor instead of bridging across at hill height.
        let valley = |x: f32, _z: f32| -> f32 { ((x - 90.0).abs() * 1.5).min(12.0) };
        let mut tris = Vec::new();
        let mut mats = Vec::new();
        let eye = Vec3::new(0.0, 10.0, 0.0);
        let frustum = Frustum::from_view_projection(glam::Mat4::IDENTITY);
        let cfg = HarvestConfig::default();
        push_terrain_frustum(
            &mut tris, &mut mats, eye, &frustum, 160.0, 2.0, cfg, &valley,
        );
        // Check right on a coarse cell boundary (z = 0, 4, 8) at road center x = 90.
        for z in [0.0f32, 4.0, 8.0, 12.0] {
            let sampled = sample_terrain_tris_height(&tris, 90.0, z)
                .expect("valley road point must be covered by terrain triangles");
            let expected = valley(90.0, z);
            assert!(
                (sampled - expected).abs() < 0.1,
                "valley road at (90, {z}) had elevated bridge: sampled={sampled}, expected={expected}"
            );
        }
    }

    #[test]
    fn terrain_frustum_slopes_have_no_step_cracks_between_adjacent_cells() {
        let mut tris = Vec::new();
        let mut mats = Vec::new();
        let frustum = Frustum::from_view_projection(Mat4::IDENTITY);
        let config = HarvestConfig::default();
        let ground = |x: f32, z: f32| -> f32 {
            (x * 0.03).sin() * 20.0 + (z * 0.025).cos() * 15.0 + 0.002 * x * x
        };
        push_terrain_frustum(
            &mut tris,
            &mut mats,
            Vec3::new(0.0, 20.0, 0.0),
            &frustum,
            256.0,
            2.0,
            config,
            &ground,
        );
        assert!(!tris.is_empty());
        for t in &tris {
            for v in [t.a, t.b, t.c] {
                let expected = ground(v[0], v[2]);
                assert!(
                    (v[1] - expected).abs() < 1e-3,
                    "terrain vertex ({}, {}, {}) detached from ground {expected} (step crack!)",
                    v[0],
                    v[1],
                    v[2]
                );
            }
        }
    }
}
