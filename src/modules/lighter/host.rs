//! Adapter from the viewer's actual upload and animation feeds to Lighter.
//! Kept next to the renderer because MeshData/Camera deliberately are not
//! public core interfaces. No file-format parsing or native draw replacement
//! lives in the core outside this adapter and its branch in Renderer::render.
use super::{Camera, FrameSnapshot, MeshData, Vertex as WorldVertex};
use crate::modules::lighter::{
    self,
    coverage::{Class, Counts, Inventory},
    scene::*,
    textures::{chain_bytes, fit_side, mask_bytes, Filter, Textures, Tile, COLOR_SIDE, MASK_SIDE},
    Lighter,
};
use crate::{material::Materials, world_models_gpu, Terrain};
use glam::{Mat4, Vec3};
use std::{collections::HashMap, sync::Arc};

#[path = "lightmaps.rs"]
mod lightmaps;
#[path = "scenery.rs"]
mod scenery;
#[path = "terrain_fidelity.rs"]
mod terrain_fidelity;
#[path = "vegetation.rs"]
mod vegetation;
#[path = "water_feed.rs"]
mod water_feed;
#[path = "weather.rs"]
mod weather;
pub(super) use scenery::{SceneryStream, Snapshot as ScenerySnapshot};
use vegetation::{read_tree, Tree};

/// Immutable CPU sources shared by the render feed and the stream packer.
/// Moving a packer to the worker never copies a decoded texel bank or mesh.
#[derive(Default)]
struct FeedSource {
    base: Textures,
    terrain_ids: Vec<u32>,
    material_pages: scenery::MaterialPages,
    lightmaps: Vec<[u32; 2]>,
    static_lights: Vec<Light>,
    light_cycle: Option<crate::environment::LightCycle>,
    terrain: [f32; 4],
    far: Vec<(crate::terrain_residency::Span, Mesh)>,
    trees: Vec<Option<Tree>>,
    tree_archive_missing: bool,
    placements: Vec<crate::scene::Tree>,
    waves: Vec<u32>,
    water: water_feed::Water,
    weather: weather::Weather,
    sky: lighter::sky::Sky,
    authored_mips: bool,
}

#[derive(Default)]
struct ResidentScene {
    area_table: u32,
    near_bounds: [f32; 4],
    cutoff: bool,
    forest: Vec<(usize, [f32; 3])>,
    tree_scale: f32,
    grass: vegetation::Grass,
}

struct MaskBank {
    bank: Textures,
    area_table: u32,
}

/// Ownership of this cache transfers to the existing world-streaming loader
/// after the first (monitored) frame. A stream result carries Native data and
/// this matching ray snapshot together. The draw thread never waits for it.
pub(super) struct StreamPacker {
    source: Arc<FeedSource>,
    // Remember the actual mask failure across a LOD-only update. Reusing the
    // last good table with the new geometry would silently read wrong layers.
    masks: Option<Result<MaskBank, String>>,
    accel: AccelCache,
}

pub(super) struct PreparedScene {
    packet: Packet,
    bank: Textures,
    inventory: Inventory,
    coverage: scenery::Coverage,
    resident: ResidentScene,
}

impl PreparedScene {
    pub(super) fn poll_upload(
        &self,
        ray: &mut Lighter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<bool, String> {
        ray.stage_resident(device, queue, &self.packet, &self.bank)
    }
}

pub(super) struct Feed {
    source: Arc<FeedSource>,
    resident: ResidentScene,
    packer: Option<StreamPacker>,
}

impl FeedSource {
    fn new(
        t: &mut Terrain,
        m: &MeshData,
        authored_mips: bool,
        storage: usize,
        native_bc: bool,
    ) -> Result<Self, String> {
        let mut result = Self {
            base: Textures::with_limit(storage),
            authored_mips,
            light_cycle: t.environments.light_cycle(),
            static_lights: t
                .scene_lights
                .lights
                .iter()
                .map(|l| {
                    let lamp = point_light(l.position, l.range, l.diffuse, 1.)
                        .sphere((l.range * 0.04).clamp(0.05, 1.5));
                    if l.flags & 1 != 0 {
                        lamp.night_rgb(l.diffuse)
                    } else {
                        lamp
                    }
                })
                .collect(),
            ..Self::default()
        };
        let upload = m
            .terrain_upload
            .as_ref()
            .ok_or("Lighter startup needs the terrain upload")?;
        result.terrain = [
            t.width as f32,
            t.height as f32,
            t.grid_spacing,
            upload.materials.mask_grid as f32,
        ];
        // Reserve most of the bank for masks, streamed objects and effects.
        // All terrain colour sources get the same deterministic ceiling;
        // choose a resident authored mip rather than resampling level zero.
        let terrain_bytes = (48 << 20).min(result.base.remaining_bytes() / 2);
        let terrain_limit = fit_side(COLOR_SIDE, |side| {
            upload.materials.textures.iter().fold(0usize, |sum, t| {
                sum.saturating_add(chain_bytes([t.w as u32, t.h as u32], side))
            }) <= terrain_bytes
        })
        .ok_or("Lighter terrain colours exceed their texture budget")?;
        let compact = native_bc || lighter::textures::compact_terrain_enabled();
        for texture in &upload.materials.textures {
            if compact {
                if let Some(tile) = Tile::bc_terrain(&texture.source, terrain_limit, native_bc)? {
                    let id = if native_bc {
                        result.base.insert_paged_terrain(&tile)?
                    } else {
                        result.base.insert(&tile)?
                    };
                    result.terrain_ids.push(id);
                    continue;
                }
            }
            let decoded = texture.decode()?;
            result.terrain_ids.push(
                result.base.insert(&Tile::levels(
                    decoded
                        .iter()
                        .map(|i| ([i.width, i.height], i.rgba.as_slice())),
                    terrain_limit,
                    Filter::Color,
                )?)?,
            );
        }
        if std::env::var_os("PW_TEST_NO_LIGHTMAPS").is_none() {
            result.lightmaps = lightmaps::load(t, &upload.materials, &mut result.base)?;
        }
        result.sky = match lighter::sky::Sky::load(&t.element, &mut result.base) {
            Ok(sky) => sky,
            Err(error) => {
                eprintln!("Lighter: authored sky unavailable ({error}); using the analytic sky");
                Default::default()
            }
        };
        for frame in &t.water_normals {
            if !frame.is_empty() {
                result
                    .waves
                    .push(result.base.images(frame, 128, Filter::Data)?);
            }
        }
        result.water = water_feed::Water::load(t, &mut result.base)?;
        result.weather = weather::Weather::load(t, &mut result.base)?;
        result.tree_archive_missing = !t.element.join("trees.pck").is_file();
        let tree_bytes = (32 << 20).min(result.base.remaining_bytes() / 2);
        let tree_side = fit_side(COLOR_SIDE, |side| {
            t.trees.types.iter().flatten().fold(0usize, |sum, tree| {
                sum.saturating_add(vegetation::tree_texture_bytes(tree, side, authored_mips))
            }) <= tree_bytes
        })
        .ok_or("Lighter tree materials exceed their texture budget")?;
        if tree_side < COLOR_SIDE {
            eprintln!(
                "Lighter: tree textures capped at {tree_side} pixels to fit the texture budget"
            );
        }
        for tree in &t.trees.types {
            let Some(tree) = tree else {
                result.trees.push(None);
                continue;
            };
            result.trees.push(Some(read_tree(
                tree,
                &mut result.base,
                tree_side,
                authored_mips,
            )?));
        }
        if native_bc {
            if let Some(Ok(scenery)) = &m.ray_scenery {
                result.material_pages = scenery.material_pages(&t.objects, &mut result.base)?;
            }
        }
        result.base.seal();
        // Audit small maps before borrowing the raster far grid. A narrow
        // ridge or cliff can be completely missed by stride four. Keep the
        // existing budget, including stride 16 on larger maps.
        let plan = {
            let mut reader = t.heights.reader();
            terrain_fidelity::choose(t.width, t.height, |x, y| {
                reader.cell(y * t.width + x).map(|p| p.0)
            })
            .map_err(|error| format!("Lighter far terrain height load failed: {error}"))?
        };
        match plan {
            terrain_fidelity::Layout::Refined(grid) => {
                eprintln!(
                    "Lighter: far terrain locally refined {} tiles, {} indices, {:.4}-unit maximum source-mesh error",
                    grid.refined, grid.index_count, grid.max_error,
                );
                result.far = adaptive_far_mesh(t, &grid).map_err(|error| {
                    format!("Lighter refined far terrain mesh load failed: {error}")
                })?;
            }
            terrain_fidelity::Layout::Regular(plan) => {
                match plan.max_sample_error {
                    Some(error) if error > terrain_fidelity::GRID_TOLERANCE => eprintln!(
                        "Lighter: far terrain stride {} exceeds the {:.2}-unit height tolerance (measured {error:.3}); conforming refinement would exceed its budget",
                        plan.step, terrain_fidelity::GRID_TOLERANCE,
                    ),
                    Some(error) => crate::diag!(TimingLoad, "Lighter far terrain stride {}: measured max height error {error:.3}", plan.step),
                    None => eprintln!("Lighter: far terrain uses stride {} to fit large maps; height tolerance is unverified", plan.step),
                }
                let far = if let Some(far) = upload.far.as_ref().filter(|_| plan.step == 4) {
                    far
                } else {
                    // Also prepare this for a session starting with cutoff enabled:
                    // the live cutoff toggle can bring back distant geometry.
                    &crate::terrain_gpu::world_mesh(t, plan.step)
                        .map_err(|error| format!("Lighter far terrain mesh load failed: {error}"))?
                };
                for span in &far.2 {
                    let mut patch = Mesh::default();
                    let mut remap = HashMap::new();
                    for tri in far.1[span.start as usize..(span.start + span.count) as usize]
                        .chunks_exact(3)
                    {
                        let tri = [tri[0], tri[1], tri[2]].map(|i| {
                            *remap.entry(i).or_insert_with(|| {
                                let id = patch.vertices.len() as u32;
                                let (p, n) = far.0[i as usize].position_normal();
                                patch.vertices.push(Vertex::new(p, n, [0.; 2]));
                                id
                            })
                        });
                        // The terrain material is the first surface in every packet.
                        patch.tri(tri, 0);
                    }
                    result.far.push((*span, patch));
                }
            }
        }
        result.placements = std::mem::take(&mut t.trees.placements);
        Ok(result)
    }

    fn terrain_masks(&self, material: &Materials) -> Result<MaskBank, String> {
        let mut textures = self.base.fork_prefix();
        // Plan before inflating any masks, and leave room for the streamed
        // suffix. On unusually dense maps reduce mask resolution uniformly.
        let available = textures.remaining_bytes();
        let headroom = (16 << 20).min(available / 4);
        let bytes_at = |side| {
            material
                .areas
                .iter()
                .fold(material.areas.len() * 16, |bytes, record| {
                    record.layers.iter().skip(1).fold(
                        bytes.saturating_add(record.layers.len() * 20),
                        |bytes, layer| {
                            bytes.saturating_add(mask_bytes(layer.mask_size as u32, side))
                        },
                    )
                })
        };
        let side = fit_side(MASK_SIDE, |side| bytes_at(side) <= available - headroom)
            .or_else(|| fit_side(MASK_SIDE, |side| bytes_at(side) <= available))
            .ok_or("Lighter terrain masks and material tables exceed the ray texture budget")?;
        if side < MASK_SIDE {
            crate::diag!(
                TimingLoad,
                "Lighter: terrain masks capped at {side} pixels ({} KiB with tables)",
                bytes_at(side) >> 10
            );
        }
        let table = textures.reserve(material.areas.len() * 4)?;
        for (area, record) in material.areas.iter().enumerate() {
            let offset = textures.reserve(record.layers.len() * 5)?;
            let [day, night] = self.lightmaps.get(area).copied().unwrap_or_default();
            textures.write_words(
                table + area * 4,
                &[offset as u32, record.layers.len() as u32, day, night],
            );
            for (k, l) in record.layers.iter().enumerate() {
                let color = *self
                    .terrain_ids
                    .get(l.texture)
                    .ok_or("Lighter terrain texture index out of range")?;
                let mask = if k == 0 {
                    0
                } else {
                    let bytes = l.mask.decode(l.mask_size)?;
                    textures.insert(&Tile::mask(l.mask_size as u32, side, &bytes)?)?
                };
                textures.write_words(
                    offset + k * 5,
                    &[color, mask, u32::from(l.axis), l.u.to_bits(), l.v.to_bits()],
                );
            }
        }
        textures.seal();
        Ok(MaskBank {
            bank: textures,
            area_table: table as u32,
        })
    }
}

impl Feed {
    pub(super) fn new(
        t: &mut Terrain,
        m: &MeshData,
        authored_mips: bool,
        storage: usize,
        native_bc: bool,
    ) -> Result<Self, String> {
        let source = Arc::new(FeedSource::new(t, m, authored_mips, storage, native_bc)?);
        Ok(Self {
            packer: Some(StreamPacker::new(Arc::clone(&source))),
            resident: ResidentScene::default(),
            source,
        })
    }

    pub(super) fn install_initial(
        &mut self,
        ray: &mut Lighter,
        m: &MeshData,
        device: &wgpu::Device,
    ) {
        let result = self.packer.as_mut().expect("initial ray packer").prepare(
            m,
            lighter::storage_limit(device),
            ray.settings.mode,
        );
        self.install_prepared(ray, result);
    }

    pub(super) fn take_packer(&mut self) -> Option<StreamPacker> {
        self.packer.take()
    }

    pub(super) fn install_prepared(
        &mut self,
        ray: &mut Lighter,
        scene: Result<PreparedScene, String>,
    ) {
        let packet = scene.map(|scene| {
            self.resident = scene.resident;
            ray.static_coverage = scene.inventory;
            ray.scenery_coverage = Some(scene.coverage.to_string());
            (scene.packet, scene.bank)
        });
        if packet.is_err() {
            ray.static_coverage = Inventory::default();
            ray.scenery_coverage = None;
        }
        ray.install(packet);
    }

    #[cfg(test)]
    fn snapshot(
        &mut self,
        m: &MeshData,
        storage: usize,
    ) -> Result<(Packet, Textures, Inventory, scenery::Coverage), String> {
        let scene = self
            .packer
            .as_mut()
            .unwrap()
            .prepare(m, storage, lighter::Mode::Software)?;
        self.resident = scene.resident;
        Ok((scene.packet, scene.bank, scene.inventory, scene.coverage))
    }
}

impl StreamPacker {
    fn new(source: Arc<FeedSource>) -> Self {
        Self {
            source,
            masks: None,
            accel: AccelCache::default(),
        }
    }

    fn update_masks(&mut self, materials: &Materials) {
        // Release the previous mask suffix before inflating the new one. Its
        // Arcs are still held by any scene the renderer is drawing.
        self.masks = None;
        self.masks = Some(self.source.terrain_masks(materials));
    }

    pub(super) fn prepare(
        &mut self,
        m: &MeshData,
        storage: usize,
        mode: lighter::Mode,
    ) -> Result<PreparedScene, String> {
        let started = std::time::Instant::now();
        if let Some(upload) = &m.terrain_upload {
            self.update_masks(&upload.materials);
        }
        let source = &self.source;
        let masks = self
            .masks
            .as_ref()
            .ok_or("Lighter has no terrain materials")?
            .as_ref()
            .map_err(Clone::clone)?;
        let mut bank = masks.bank.fork_prefix();
        // The far grid's stride divides the resident block size. Omit its
        // covered triangles before building either acceleration structure. A
        // rejected far hit plus t bias could otherwise skip coplanar near
        // terrain at the same distance and leave holes in flat ground.
        let near_bounds = crate::terrain_cutoff::world_bounds(
            [
                m.lod.window.x0,
                m.lod.window.y0,
                m.lod.window.x1,
                m.lod.window.y1,
            ],
            super::TILE,
            source.terrain[0] as usize,
            source.terrain[1] as usize,
            source.terrain[2],
        );
        let mut builder = Builder::for_mode(mode);
        for &light in &source.static_lights {
            builder.light(light);
        }
        let mut resident = ResidentScene {
            near_bounds,
            cutoff: m.detail.terrain_cutoff,
            area_table: masks.area_table,
            ..ResidentScene::default()
        };
        let mut inventory = Inventory::default();
        let terrain_id = builder.surface(Surface::new(0, TERRAIN, 0., 0.));
        for block in &m.spans {
            let (x, y) = block.block;
            let near = span(
                &m.vertices,
                &m.indices[block.start as usize..(block.start + block.count) as usize],
                terrain_id,
                |_| u32::MAX,
            )?;
            builder.chunk(ChunkKey::NearTerrain(x, y), &near)?;
            if near.triangles.is_empty() {
                inventory[Class::NearTerrain].empty += 1;
            } else {
                inventory[Class::NearTerrain].loaded += 1;
            }
        }
        let far = &mut inventory[Class::FarTerrain];
        if m.detail.terrain_cutoff {
            far.cutoff = source.far.len();
            crate::diag!(
                TimingLoad,
                "Lighter terrain cutoff: {} resident blocks; {} far patches intentionally omitted",
                m.spans.len(),
                source.far.len()
            );
        } else {
            let edges = near_edges(
                near_bounds,
                m.indices[..m.terrain_end as usize].iter().map(|&i| {
                    let v = &m.vertices[i as usize];
                    Vertex::new(v.pos, v.normal, v.uv)
                }),
            );
            let [x0, x1, z0, z1] = near_bounds;
            debug_assert_eq!(terrain_id, 0);
            for (block, patch) in &source.far {
                let [lo_x, _, lo_z, hi_x, _, hi_z] = block.bounds;
                let (x, y) = block.block;
                let key = ChunkKey::FarTerrain(x, y);
                if hi_x < x0 || lo_x > x1 || hi_z < z0 || lo_z > z1 {
                    builder.chunk(key, patch)?;
                    if patch.triangles.is_empty() {
                        far.empty += 1;
                    } else {
                        far.loaded += 1;
                    }
                } else if lo_x >= x0 && hi_x <= x1 && lo_z >= z0 && hi_z <= z1 {
                    far.replaced += 1;
                } else {
                    let exterior = stitch_patch(patch, &edges, near_bounds, terrain_id);
                    builder.chunk(key, &exterior)?;
                    if exterior.triangles.is_empty() {
                        far.replaced += 1;
                    } else {
                        far.loaded += 1;
                    }
                }
            }
        }
        source.water.append(m, &mut builder)?;
        source.weather.clouds(&mut builder)?;
        inventory[Class::Water] = m.water_coverage;
        let extras = &m.indices[m.terrain_end as usize..m.water_start as usize];
        if !extras.is_empty() {
            let id = builder.surface(Surface::new(0, 0, 0., 0.));
            builder.world_mesh(&span(&m.vertices, extras, id, |v| rgba(v.color))?)?;
        }
        source.vegetation(m, &mut bank, &mut inventory, &mut resident)?;
        let scenery = m
            .ray_scenery
            .as_ref()
            .ok_or("Lighter BMD stream is unavailable")?;
        let coverage = scenery.as_ref().map_err(Clone::clone)?.append(
            &mut builder,
            &mut bank,
            storage,
            &source.material_pages,
        )?;
        inventory[Class::Buildings] = coverage.counts;
        let packet = builder.finish_with_emitters(&mut self.accel, Some(&bank));
        if packet.bytes().len() > storage {
            return Err(format!(
                "Lighter resident world exceeds adapter storage limit ({} MiB)",
                storage >> 20
            ));
        }
        crate::diag!(
            TimingLoad,
            "Lighter packed {:.2} MiB geometry and {:.2} MiB texels in {:.2} ms ({})",
            packet.bytes().len() as f64 / 1048576.,
            bank.byte_len() as f64 / 1048576.,
            started.elapsed().as_secs_f64() * 1000.,
            std::thread::current().name().unwrap_or("startup")
        );
        Ok(PreparedScene {
            packet,
            bank,
            inventory,
            coverage,
            resident,
        })
    }
}

impl FeedSource {
    fn vegetation(
        &self,
        m: &MeshData,
        textures: &mut Textures,
        inventory: &mut Inventory,
        resident: &mut ResidentScene,
    ) -> Result<(), String> {
        resident.tree_scale = m.detail.tree_lod_scale
            * crate::scenery_visibility::lod_scale(
                m.detail.scenery_quality,
                crate::scenery_visibility::Class::Trees,
            );
        let trees = &mut inventory[Class::Trees];
        if super::skip_trees_test() || super::ui_test() {
            trees.disabled = self.placements.len();
        } else {
            let cutoff = m.detail.terrain_cutoff.then_some(resident.near_bounds);
            for placement in &self.placements {
                if !placement.position.iter().all(|x| x.is_finite()) {
                    trees.invalid += 1;
                    continue;
                }
                if !crate::terrain_cutoff::contains(cutoff, placement.position) {
                    trees.cutoff += 1;
                    continue;
                }
                let id = placement.tree_type as usize;
                match self.trees.get(id) {
                    Some(Some(_)) => resident.forest.push((id, placement.position)),
                    Some(None) if self.tree_archive_missing => trees.package += 1,
                    Some(None) => trees.unavailable += 1,
                    None => trees.invalid += 1,
                }
            }
        }
        resident.grass = vegetation::Grass::prepare(
            m,
            textures,
            self.authored_mips,
            &mut inventory[Class::Grass],
        )?;
        Ok(())
    }
}

impl Feed {
    pub(super) fn frame(
        &self,
        matrix: Mat4,
        camera: &Camera,
        f: &FrameSnapshot,
        w: u32,
        h: u32,
        regional_lighting: bool,
    ) -> lighter::Frame {
        let mut direction = Vec3::from_slice(&f.sun).normalize_or_zero();
        if direction.length_squared() < 0.5 {
            direction = Vec3::Y;
        }
        // Don't relight every world-space surface when the eye crosses a
        // local fog/lighting box. Use the map's default day/night palette;
        // matching the legacy camera-local tint is an explicit choice.
        let hour = camera.time_of_day;
        let sky_shade = 1.
            - 0.72
                * crate::sky_time::night_weight(hour)
                    .unwrap_or(0.)
                    .clamp(0., 1.);
        let [ambient, diffuse, sky_tint] =
            if let Some(l) = camera.regional_light.filter(|_| regional_lighting) {
                [l.ambient.map(srgb), l.diffuse.map(srgb), f.sky_tint]
            } else if let Some(cycle) = self.source.light_cycle {
                let (a, d) = cycle.colors(hour);
                [
                    a.map(srgb),
                    d.map(srgb),
                    std::array::from_fn(|i| a[i].max(d[i]) * sky_shade),
                ]
            } else {
                [[0.08 + f.daylight * 0.24; 3], [0.68; 3], [sky_shade; 3]]
            };
        let phase = crate::water_gpu::normal_phase(f.elapsed64);
        let wave_angle = crate::water_gpu::normal_offset_angle(f.elapsed64).to_bits();
        let waves = if self.source.waves.is_empty() {
            [
                0,
                0,
                0,
                if self.source.water.simple_frame(f.elapsed64) != 0 {
                    wave_angle
                } else {
                    0
                },
            ]
        } else {
            let frame = phase.floor() as usize % self.source.waves.len();
            [
                self.source.waves[frame],
                self.source.waves[(frame + 1) % self.source.waves.len()],
                phase.fract().to_bits(),
                wave_angle,
            ]
        };
        let disc = |p: [f32; 4]| {
            let (s, c) = p[1].sin_cos();
            let (zs, xc) = p[2].sin_cos();
            [xc * c, s, zs * c, p[0] * camera.celestial_brightness]
        };
        let air = camera.air_fog;
        let water = camera.underwater_fog;
        // Lighter evaluates both media along the ray. The Native camera
        // fog switch (and its nearest-window-edge cap) changes all pixels
        // at once; never use it as the air coefficients of a ray path.
        let cutoff = self.resident.cutoff.then_some(self.resident.near_bounds);
        let current_body = self.source.water.at(camera.pos, cutoff);
        let [water_table, body_count] = self.source.water.table();
        lighter::Frame {
            previous_screen_vp: [[0.; 4]; 4],
            ray_effects: [0.; 4],
            vp: matrix.to_cols_array_2d(),
            inv_vp: matrix.inverse().to_cols_array_2d(),
            eye: camera.pos.extend(camera.far).to_array(),
            sun: direction.extend(f.daylight).to_array(),
            ambient: Vec3::from_array(ambient).extend(0.).to_array(),
            sun_color: (Vec3::from_array(diffuse) * f.daylight * 1.3)
                .extend((f.sky_moon[1] / 1.047_197_6).clamp(0., 1.) * (1. - f.daylight))
                .to_array(),
            air: [
                air.start,
                air.end,
                u32::from(camera.atmospheric_fog) as f32,
                f.grass_wind[0],
            ],
            air_color: Vec3::from_array(air.color.map(srgb)).extend(0.).to_array(),
            water: [water.start, water.end, 1., 0.],
            water_color: Vec3::from_array(water.color.map(srgb))
                .extend(0.)
                .to_array(),
            terrain: self.source.terrain,
            extent: [0, 0, w, h],
            options: [
                if camera.water_reflections {
                    lighter::WATER_MIRROR
                } else {
                    0
                } | if camera.model_backface_culling {
                    lighter::CULL_BUILDINGS
                } else {
                    0
                } | if current_body != 0 {
                    lighter::UNDERWATER
                } else {
                    0
                } | if camera.cloud_detail {
                    lighter::CLOUD_DETAIL
                } else {
                    0
                } | if camera.terrain_lightmaps {
                    lighter::BAKED_LIGHTMAPS
                } else {
                    0
                } | if camera.area_grass_lightmaps {
                    lighter::GRASS_AREA_LIGHTMAPS
                } else {
                    0
                },
                0,
                0,
                self.resident.area_table,
            ],
            waves,
            sky0: self.source.sky.layers(f.sky_sets.0, f.sky_sets.2.to_bits()),
            sky1: self.source.sky.layers(f.sky_sets.1, 0),
            sky_cdf: self.source.sky.distributions(f.sky_sets.0, f.sky_sets.1),
            sky_tint: Vec3::from_array(sky_tint).extend(0.).to_array(),
            sky_uv: [
                f.sky_drift[0],
                f.sky_drift[1],
                crate::sky_gpu::SKY_DRIFT_SCALE,
                0.,
            ],
            sun_disc: disc(f.sky_sun),
            moon_disc: disc(f.sky_moon),
            sampling: [0; 4],
            material_time: [
                f.fog_range[3],
                f.grass_eye[3],
                crate::environment::night_factor(camera.time_of_day),
                0.,
            ],
            grass_history: f.grass_history,
            shore_offsets: self.source.water.shore_offsets(f.elapsed),
            resident_bounds: cutoff.unwrap_or([-1.0e9, 1.0e9, -1.0e9, 1.0e9]),
            water_effects: [
                self.source.water.simple_frame(f.elapsed64),
                current_body,
                water_table,
                body_count,
            ],
        }
    }
}

fn srgb(v: f32) -> f32 {
    if v <= 0.04045 {
        v.max(0.) / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn span(
    vertices: &[WorldVertex],
    indices: &[u32],
    material: u32,
    tint: impl Fn(&WorldVertex) -> u32,
) -> Result<Mesh, String> {
    let Some((&min, &max)) = indices.iter().min().zip(indices.iter().max()) else {
        return Ok(Mesh::default());
    };
    let verts = vertices
        .get(min as usize..=max as usize)
        .ok_or("Lighter index outside world geometry")?;
    Ok(Mesh {
        vertices: verts
            .iter()
            .map(|v| {
                let c = tint(v);
                Vertex::new(v.pos, v.normal, v.uv).tint(c, c)
            })
            .collect(),
        triangles: indices
            .chunks_exact(3)
            .map(|i| [i[0] - min, i[1] - min, i[2] - min])
            .collect(),
        surfaces: vec![material; indices.len() / 3],
        layers: vec![],
    })
}

type Perimeter = [Vec<(f32, Vertex)>; 4];

fn adaptive_far_mesh(
    t: &Terrain,
    grid: &terrain_fidelity::AdaptiveGrid,
) -> Result<Vec<(crate::terrain_residency::Span, Mesh)>, String> {
    let [cols, rows] = grid.tiles();
    let mut reader = t.heights.reader();
    let mut patches = Vec::new();
    for py in (0..rows).step_by(32) {
        for px in (0..cols).step_by(32) {
            let mut patch = Mesh::default();
            let mut indices = HashMap::new();
            let mut points = Vec::new();
            for y in py..(py + 32).min(rows) {
                for x in px..(px + 32).min(cols) {
                    grid.triangles(x, y, |tri| {
                        let tri = tri.map(|p| {
                            *indices.entry(p).or_insert_with(|| {
                                let id = points.len() as u32;
                                points.push(p);
                                id
                            })
                        });
                        patch.tri(tri, 0);
                    });
                }
            }
            let mut lo = [f32::INFINITY; 3];
            let mut hi = [f32::NEG_INFINITY; 3];
            patch.vertices.reserve(points.len());
            for [x, y] in points {
                let (h, n) = reader.cell(y * t.width + x)?;
                let p = [
                    (x as f32 - t.width as f32 * 0.5) * t.grid_spacing,
                    h,
                    (t.height as f32 * 0.5 - y as f32) * t.grid_spacing,
                ];
                for axis in 0..3 {
                    lo[axis] = lo[axis].min(p[axis]);
                    hi[axis] = hi[axis].max(p[axis]);
                }
                patch
                    .vertices
                    .push(Vertex::new(p, n.map(|v| f32::from(v) / 127.), [0.; 2]));
            }
            let span = crate::terrain_residency::Span {
                block: (px, py),
                start: 0,
                count: (patch.triangles.len() * 3) as u32,
                bounds: [lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]],
            };
            patches.push((span, patch));
        }
    }
    debug_assert_eq!(
        patches
            .iter()
            .map(|(span, _)| span.count as usize)
            .sum::<usize>(),
        grid.index_count
    );
    Ok(patches)
}

fn near_edges([x0, x1, z0, z1]: [f32; 4], vertices: impl Iterator<Item = Vertex>) -> Perimeter {
    let mut edges: Perimeter = std::array::from_fn(|_| Vec::new());
    for v in vertices {
        let [x, _, z] = v.pos;
        for (edge, (on, distance)) in
            edges
                .iter_mut()
                .zip([(x == x0, z), (x == x1, z), (z == z0, x), (z == z1, x)])
        {
            if on {
                edge.push((distance, v));
            }
        }
    }
    for edge in &mut edges {
        edge.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        edge.dedup_by(|a, b| a.0 == b.0);
    }
    edges
}

#[cfg(test)]
fn uncovered_far(far: &Mesh, near: &Mesh, bounds: [f32; 4], surface: u32) -> Mesh {
    let edges = near_edges(
        bounds,
        near.triangles
            .iter()
            .flatten()
            .map(|&i| near.vertices[i as usize]),
    );
    stitch_patch(far, &edges, bounds, surface)
}

/// If the far grid is finer than the installed native LOD, snap each
/// shared far point to the native edge segment, not just its endpoints.
fn match_near_perimeter(mut v: Vertex, edges: &Perimeter, [x0, x1, z0, z1]: [f32; 4]) -> Vertex {
    let [x, _, z] = v.pos;
    for (edge, (on, along)) in
        edges
            .iter()
            .zip([(x == x0, z), (x == x1, z), (z == z0, x), (z == z1, x)])
    {
        if !on {
            continue;
        }
        let index = edge.partition_point(|p| p.0 < along);
        let Some(&(right, b)) = edge.get(index) else {
            continue;
        };
        if right == along {
            return b;
        }
        if index > 0 {
            let (left, a) = edge[index - 1];
            let fraction = (along - left) / (right - left);
            v.pos[1] = a.pos[1] + fraction * (b.pos[1] - a.pos[1]);
            return v;
        }
    }
    v
}

/// Split only the exterior patch edges that touch the native near perimeter.
/// Vertices inserted along a coarser edge use the actual near heights.
fn stitch_patch(far: &Mesh, edges: &Perimeter, bounds: [f32; 4], surface: u32) -> Mesh {
    let [x0, x1, z0, z1] = bounds;
    // Pack only referenced exterior vertices. Far patches partly covered by
    // the near window used to copy the entire vertex list before clipping.
    let mut out = Mesh::default();
    let mut remap = vec![u32::MAX; far.vertices.len()];
    'triangle: for &tri in &far.triangles {
        if tri.iter().all(|&index| {
            let [x, _, z] = far.vertices[index as usize].pos;
            x >= x0 && x <= x1 && z >= z0 && z <= z1
        }) {
            continue;
        }
        let local = tri.map(|index| {
            let id = &mut remap[index as usize];
            if *id == u32::MAX {
                *id = out.vertices.len() as u32;
                out.vertices.push(match_near_perimeter(
                    far.vertices[index as usize],
                    edges,
                    bounds,
                ));
            }
            *id
        });
        for i in 0..3 {
            let [start, end, apex] = [local[i], local[(i + 1) % 3], local[(i + 2) % 3]];
            let [ax, _, az] = out.vertices[start as usize].pos;
            let [bx, _, bz] = out.vertices[end as usize].pos;
            let (edge, a, b) = if ax == bx && ax == x0 {
                (&edges[0], az, bz)
            } else if ax == bx && ax == x1 {
                (&edges[1], az, bz)
            } else if az == bz && az == z0 {
                (&edges[2], ax, bx)
            } else if az == bz && az == z1 {
                (&edges[3], ax, bx)
            } else {
                continue;
            };
            let lo = edge.partition_point(|p| p.0 <= a.min(b));
            let hi = edge.partition_point(|p| p.0 < a.max(b));
            if lo >= hi {
                continue;
            }
            // A far cell is aligned to the near rectangle, so a far triangle
            // outside it can have at most one edge adjacent to the near mesh.
            let mut previous = start;
            for j in lo..hi {
                let at = if a < b { j } else { (hi - 1) - (j - lo) };
                let id = out.vertices.len() as u32;
                out.vertices.push(edge[at].1);
                out.tri([previous, id, apex], surface);
                previous = id;
            }
            out.tri([previous, end, apex], surface);
            continue 'triangle;
        }
        out.tri(local, surface);
    }
    out
}

pub(super) type GrassTiles = HashMap<[usize; 2], Arc<Tile>>;

/// The atlas rectangle labels a native grass material, not its ray source.
/// Use the original rectangular image and mips even if native packed/resized
/// it. Streamer owns the small cache so packing never runs on the draw thread.
pub(super) fn grass_tiles(
    stream: &mut SceneryStream,
    grass: &crate::grass::Grass,
    rects: &[(crate::texture_atlas::Slot, &str, bool)],
) -> Result<GrassTiles, String> {
    let mut current = HashMap::new();
    let mut output = GrassTiles::new();
    for &(slot, path, _) in rects {
        let tile = if let Some(tile) = stream.grass_tiles.remove(path) {
            tile
        } else {
            let image = grass
                .textures
                .get(path)
                .ok_or("missing source grass image")?;
            let mips = grass.texture_mips.get(path).map_or(&[][..], Vec::as_slice);
            Arc::new(Tile::images(
                std::iter::once(image).chain(mips),
                COLOR_SIDE,
                Filter::cutout(84. / 255., true),
            )?)
        };
        output.insert([slot.x, slot.y], Arc::clone(&tile));
        current.insert(path.to_owned(), tile);
    }
    stream.grass_tiles = current;
    Ok(output)
}

fn atlas_uv(v: &WorldVertex, sy: f32) -> [f32; 2] {
    [
        (v.uv[0] - v.color[0]) / v.color[2].max(0.000001),
        (v.uv[1] - v.color[1]) / sy.max(0.000001),
    ]
}

fn point_light(p: [f32; 3], radius: f32, color: [f32; 4], intensity: f32) -> Light {
    Light::point(
        [p[0], p[1], p[2], radius],
        [
            srgb(color[0]) * intensity,
            srgb(color[1]) * intensity,
            srgb(color[2]) * intensity,
            1.,
        ],
    )
}

fn moving(
    feed: &Feed,
    frame: &FrameSnapshot,
    camera: &Camera,
    storage: usize,
    models: Option<&mut world_models_gpu::World>,
    modules: &mut crate::modules::Modules,
    textures: &mut Textures,
    matrix: Mat4,
    cache: &mut AccelCache,
    absences: Counts,
    settings: lighter::Settings,
    track_motion: bool,
) -> Result<(Packet, Inventory), String> {
    let mut out = lighter::model_feed::Moving::for_mode(textures, settings.mode, track_motion);
    out.coverage[Class::WorldModels] = absences;
    if let Some(models) = models {
        let (geometry, effects, dynamic, read) = models.ray_sources();
        for l in dynamic {
            out.builder.light(
                point_light(l.position, l.radius, l.color, l.color[3])
                    .disk((l.radius * 0.04).clamp(0.05, 1.5)),
            );
        }
        let mut decode =
            |path: &str| read(path).and_then(|b| crate::model_texture_gpu::decode_for_rays(&b));
        use world_models_gpu::RayActor;
        for actor in geometry {
            match actor {
                RayActor::Pose(owner, asset, pose, meshes) => out.model(
                    "world",
                    owner,
                    lighter::model_feed::Posed {
                        asset,
                        pose,
                        meshes,
                        attached: false,
                    },
                    |_| true,
                    &mut decode,
                )?,
                RayActor::Cutoff => out.coverage[Class::WorldModels].cutoff += 1,
                RayActor::Range => out.coverage[Class::WorldModels].range += 1,
                RayActor::Invalid => out.coverage[Class::WorldModels].invalid += 1,
            }
        }
        out.effects("world", effects, &mut decode)?;
    }
    modules.ray_inputs(matrix, &mut out)?;
    let mut budget = out.builder.optional_mesh_budget(storage);
    vegetation::forest(
        &feed.source.trees,
        &feed.resident.forest,
        feed.resident.tree_scale,
        frame,
        &mut out.builder,
        &mut out.coverage[Class::Trees],
        &mut budget,
    )?;
    feed.resident.grass.append(
        frame,
        &mut out.builder,
        &mut out.coverage[Class::Grass],
        &mut budget,
        settings.mode == lighter::Mode::Hardware && !settings.reference,
    )?;
    if camera.underwater_depth <= 0. {
        out.coverage.ray_rain_cards = feed.source.weather.precipitation(
            camera.pos,
            camera.yaw,
            frame,
            &mut out.builder,
            &mut budget,
        )?;
    }
    let coverage = out.coverage;
    Ok((out.finish_cached(cache)?, coverage))
}

impl super::Renderer {
    /// Snapshot counts. The same world-root queue feeds Native and ray modes.
    pub(super) fn effect_root_coverage(&self) -> (Counts, usize) {
        let mut roots = Counts::default();
        let mut warp = 0;
        if let Some(models) = &self.models {
            let status = models.ray_effect_status();
            roots.invalid = self.ray_rejected_scene_effects + models.model_event_census().1;
            if self.ray_gfx_archive_unavailable {
                roots.package = status.total();
            } else {
                roots.loaded = status.playing;
                roots.pending = status.queued;
                roots.disabled = status.inactive;
                roots.invalid += status.invalid;
                roots.unavailable = status.unavailable;
                roots.unsupported = status.unsupported;
                warp = status.warp_omitted_elements;
            }
        } else {
            roots.disabled = self.ray_scene_effects;
        }
        (roots, warp)
    }

    pub(super) fn ray_inventory(&self) -> Inventory {
        let mut inventory = self
            .lighter
            .static_coverage
            .combine(&self.lighter.moving_coverage);
        inventory[Class::EffectRoots] = self.effect_root_coverage().0;
        inventory
    }

    pub(super) fn ray_lightmap_areas_bound(&self) -> usize {
        self.lighter_feed.as_ref().map_or(0, |feed| {
            feed.source
                .lightmaps
                .iter()
                .filter(|&&[day, _]| day != 0)
                .count()
        })
    }

    /// Lighter owns primary visibility and all water/shadow/lighting rays.
    /// Post/presentation (and independent map views) still use the host.
    /// False means no current frame to present. In particular, never clear or
    /// present stale output after an error: the next redraw can render Native
    /// with its own extent, jitter, and model tick.

    /// Hybrid raster + RT overlay: traces ONLY secondary RT shadows (sun/moon),
    /// ambient occlusion, short-range GI bounce, and reflections from the
    /// rasterizer's reverse-Z depth buffer, then composites onto `scene_target`
    /// without replacing or clearing the rasterized world.
    pub(super) fn render_lighter_overlay(
        &mut self,
        scene_target: &wgpu::TextureView,
        matrix: Mat4,
        camera: &Camera,
        frame: &FrameSnapshot,
        w: u32,
        h: u32,
    ) {
        if !self.lighter.overlays_world() {
            return;
        }
        let depth = self.depth.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let resident = self.resident_bounds();
        if let Some(light) = camera.regional_light {
            self.retained_regional_light = Some(light);
        }
        if let Some(models) = &mut self.models {
            let radius = self.detail.radius.max(1) as f32 * self.model_scene.block_size.max(1.);
            models.set_view_center(camera.pos.x, camera.pos.z, radius);
            models.set_resident_bounds(resident);
            models.set_effects_enabled(camera.world_effects);
            models.set_effect_distance(camera.effect_distance);
            let direction = frame.direction.extend(frame.daylight).to_array();
            let normal = frame.uniform(
                matrix,
                direction,
                frame.clip,
                frame.fog_range,
                frame.fog_color,
                [0., 1., 0., 0.],
            );
            models.update_for_rays(
                &self.device,
                &self.queue,
                super::receiving_model_frame(
                    &normal,
                    camera,
                    camera.regional_light.or(self.retained_regional_light),
                ),
                camera.pos.to_array(),
                &self.model_lights,
                crate::environment::night_factor(camera.time_of_day),
                camera.logic_delta_ms,
            );
        }
        let [walker, populator] = self.modules.ray_skin_omissions();
        self.lighter.skin_omissions[1..].copy_from_slice(&[walker, populator]);
        let result = (|| -> Result<bool, String> {
            let feed = self
                .lighter_feed
                .as_ref()
                .ok_or("Lighter scene feed is unavailable")?;
            let lights = feed.frame(
                matrix,
                camera,
                frame,
                w,
                h,
                self.lighter.settings.regional_lighting,
            );
            let world = self
                .lighter
                .world
                .as_mut()
                .ok_or("Lighter has no resident geometry")?;
            let (moving, coverage) = moving(
                feed,
                frame,
                camera,
                lighter::storage_limit(&self.device),
                self.models.as_mut(),
                &mut self.modules,
                &mut world.textures,
                matrix,
                &mut self.lighter.moving_accel,
                self.ray_model_absences,
                self.lighter.settings,
                false,
            )?;
            let rendered = self.lighter.render_overlay(
                &self.device,
                &self.queue,
                &mut encoder,
                scene_target,
                &depth,
                self.scene_format,
                lights,
                &moving,
            )?;
            if rendered {
                self.lighter.moving_coverage = coverage;
            }
            Ok(rendered)
        })();
        match result {
            Ok(true) => {
                self.queue.submit(Some(encoder.finish()));
                self.lighter.profile_submitted();
            }
            Ok(false) => {}
            Err(error) => {
                self.lighter.fail_frame(error);
            }
        }
    }

    pub(super) fn render_lighter(
        &mut self,
        target: &wgpu::TextureView,
        matrix: Mat4,
        camera: &Camera,
        frame: &FrameSnapshot,
        w: u32,
        h: u32,
    ) -> bool {
        let depth = self.depth.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let resident = self.resident_bounds();
        if let Some(light) = camera.regional_light {
            self.retained_regional_light = Some(light);
        }
        if let Some(models) = &mut self.models {
            let radius = self.detail.radius.max(1) as f32 * self.model_scene.block_size.max(1.);
            models.set_view_center(camera.pos.x, camera.pos.z, radius);
            models.set_resident_bounds(resident);
            models.set_effects_enabled(camera.world_effects);
            models.set_effect_distance(camera.effect_distance);
            let direction = frame.direction.extend(frame.daylight).to_array();
            let normal = frame.uniform(
                matrix,
                direction,
                frame.clip,
                frame.fog_range,
                frame.fog_color,
                [0., 1., 0., 0.],
            );
            models.update_for_rays(
                &self.device,
                &self.queue,
                super::receiving_model_frame(
                    &normal,
                    camera,
                    camera.regional_light.or(self.retained_regional_light),
                ),
                camera.pos.to_array(),
                &self.model_lights,
                crate::environment::night_factor(camera.time_of_day),
                camera.logic_delta_ms,
            );
        }
        let [walker, populator] = self.modules.ray_skin_omissions();
        self.lighter.skin_omissions[1..].copy_from_slice(&[walker, populator]);
        // Underwater fog/transmission are evaluated along Lighter's actual
        // paths. Don't re-tint those pixels with the raster underwater pass.
        let use_post = crate::view_settings::post_required(
            camera.bloom,
            camera.sharpen,
            false,
            camera.client_gamma,
        );
        let post_color = use_post.then(|| self.post.scene_view());
        let scene_target = post_color.as_ref().unwrap_or(target);
        let result = (|| -> Result<bool, String> {
            // Ray lighting off: default path is raster-only. The native
            // renderer's output is already in the target texture; skip the
            // ray pass entirely. The full ray pass only runs when
            // ray_lighting is on.
            if !self.lighter.settings.ray_lighting {
                return Ok(true);
            }
            let feed = self
                .lighter_feed
                .as_ref()
                .ok_or("Lighter scene feed is unavailable")?;
            let lights = feed.frame(
                matrix,
                camera,
                frame,
                w,
                h,
                self.lighter.settings.regional_lighting,
            );
            let track_motion = self.lighter.reconstruction_requested();
            let world = self
                .lighter
                .world
                .as_mut()
                .ok_or("Lighter has no resident geometry")?;
            let (moving, coverage) = moving(
                feed,
                frame,
                camera,
                lighter::storage_limit(&self.device),
                self.models.as_mut(),
                &mut self.modules,
                &mut world.textures,
                matrix,
                &mut self.lighter.moving_accel,
                self.ray_model_absences,
                self.lighter.settings,
                track_motion,
            )?;
            let rendered = self.lighter.render(
                &self.device,
                &self.queue,
                &mut encoder,
                scene_target,
                &depth,
                self.scene_format,
                lights,
                &moving,
                track_motion.then(|| lighter::TemporalCamera {
                    view: camera.view_matrix(),
                    projection: camera.projection_matrix(w, h, camera.far),
                    lens: camera.lens(),
                    frame_ms: if camera.logic_delta_ms == 0 {
                        1000. / 60.
                    } else {
                        camera.logic_delta_ms as f32
                    },
                }),
            )?;
            if rendered {
                self.lighter.moving_coverage = coverage;
            }
            Ok(rendered)
        })();
        match result {
            Ok(true) => {}
            Ok(false) => return false, // Live preflight normally defers before acquiring a surface.
            Err(error) => {
                self.lighter.fail_frame(error);
                return false;
            }
        }
        if use_post {
            self.water.copy_depth(&mut encoder, &self.depth);
        }
        self.queue.submit(Some(encoder.finish()));
        self.lighter.profile_submitted();
        if use_post {
            self.post.render(
                &self.device,
                &self.queue,
                target,
                &self.water.sample_depth(),
                w,
                h,
                frame.elapsed,
                camera.far,
                0.,
                camera.air_fog,
                false,
                false,
                camera.bloom,
                camera.bloom_strength,
                frame.glow_parameters,
                camera.client_gamma,
                camera.gamma_setting,
            );
        }
        true
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
pub(super) mod tests;
