//! Adapter from the viewer's actual upload and animation feeds to Lighter.
//! Kept next to the renderer because MeshData/Camera deliberately are not
//! public core interfaces. No file-format parsing or native draw replacement
//! lives in the core outside this adapter and its branch in Renderer::render.
use super::{Camera, FrameSnapshot, MeshData, Vertex as WorldVertex};
use crate::modules::lighter::{
    self,
    scene::*,
    textures::{Filter, Textures, Tile, COLOR_SIDE, MASK_SIDE},
    Lighter,
};
use crate::{material::Materials, world_models_gpu, Terrain};
use glam::{Mat4, Vec3};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct Feed {
    // Textures that survive every streaming update. Terrain masks/tiles and
    // dynamic archive textures are appended to a fresh, bounded bank per
    // streamed scene; eviction doesn't leave stale material addresses.
    base: Textures,
    terrain_ids: Vec<u32>,
    static_lights: Vec<Light>,
    mask_bank: Option<Textures>,
    terrain: [f32; 4],
    area_table: u32,
    near_bounds: [f32; 4],
    far: Mesh,
    trees: Vec<Option<Tree>>,
    placements: Vec<crate::scene::Tree>,
    grass: HashMap<(Rect, Filter), Tile>,
    waves: Vec<u32>,
    sky: lighter::sky::Sky,
    authored_mips: bool,
}

struct Tree {
    lods: Vec<(Mesh, [Surface; 3])>,
    height: f32,
}

impl Feed {
    pub(super) fn new(t: &Terrain, m: &MeshData, authored_mips: bool) -> Result<Self, String> {
        let mut result = Self {
            placements: t.trees.placements.clone(),
            authored_mips,
            static_lights: t
                .scene_lights
                .lights
                .iter()
                .map(|l| point_light(l.position, l.range, l.diffuse, 1.))
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
        for texture in &upload.materials.textures {
            let decoded = texture.decode()?;
            let top = decoded.first().ok_or("empty terrain color DDS")?;
            result.terrain_ids.push(result.base.rgba(
                [top.width, top.height],
                &top.rgba,
                Filter::Color,
            )?);
        }
        result.sky = match lighter::sky::Sky::load(&t.element, &mut result.base) {
            Ok(sky) => sky,
            Err(error) => {
                eprintln!("Lighter: authored sky unavailable ({error}); using the analytic sky");
                Default::default()
            }
        };
        for frame in &t.water_normals {
            if let Some(image) = frame.first() {
                result.waves.push(result.base.rgba(
                    [image.width(), image.height()],
                    image.as_raw(),
                    Filter::Color,
                )?);
            }
        }
        for tree in &t.trees.types {
            let Some(tree) = tree else {
                result.trees.push(None);
                continue;
            };
            result
                .trees
                .push(Some(read_tree(tree, &mut result.base, authored_mips)?));
        }
        // The mainland's native stride-four far mesh is ~2.9M triangles.
        // Use a coarse mesh of the SAME heightfield so the whole world is
        // present without duplicating that massive buffer on every stream.
        // Keep native far geometry on small maps to avoid unnecessary LOD loss.
        let far = if let Some(far) = upload
            .far
            .as_ref()
            .filter(|(_, indices, _)| indices.len() <= 600_000)
        {
            far
        } else {
            // Also prepare this for a session starting with cutoff enabled:
            // the live cutoff toggle can bring back distant geometry.
            &crate::terrain_gpu::world_mesh(t, 16)?
        };
        result.far.vertices = far
            .0
            .iter()
            .map(|v| {
                let (pos, n) = v.position_normal();
                Vertex::new(pos, n, [0.; 2])
            })
            .collect();
        result.far.triangles = far.1.chunks_exact(3).map(|i| [i[0], i[1], i[2]]).collect();
        Ok(result)
    }

    fn terrain_masks(&mut self, material: &Materials) -> Result<(), String> {
        let mut textures = self.base.clone();
        let table = textures.reserve(material.areas.len() * 2)?;
        for (area, record) in material.areas.iter().enumerate() {
            let offset = textures.reserve(record.layers.len() * 5)?;
            textures.words[table + area * 2] = offset as u32;
            textures.words[table + area * 2 + 1] = record.layers.len() as u32;
            for (k, l) in record.layers.iter().enumerate() {
                let color = *self
                    .terrain_ids
                    .get(l.texture)
                    .ok_or("Lighter terrain texture index out of range")?;
                let mask = if k == 0 {
                    0
                } else {
                    let bytes = l.mask.decode(l.mask_size)?;
                    let side = l.mask_size as u32;
                    textures.tile([side, side], MASK_SIDE, Filter::Color, |x, y| {
                        u32::from(bytes[(y * side + x) as usize]) * 0x01010101
                    })?
                };
                textures.words[offset + k * 5..offset + k * 5 + 5].copy_from_slice(&[
                    color,
                    mask,
                    u32::from(l.axis),
                    l.u.to_bits(),
                    l.v.to_bits(),
                ]);
            }
        }
        self.area_table = table as u32;
        self.mask_bank = Some(textures);
        Ok(())
    }

    pub(super) fn snapshot(&mut self, m: &MeshData) -> Result<(Packet, Textures), String> {
        if let Some(upload) = &m.terrain_upload {
            self.terrain_masks(&upload.materials)?;
        }
        let mut bank = self
            .mask_bank
            .as_ref()
            .ok_or("Lighter has no terrain materials")?
            .clone();
        // The far grid's stride divides the resident block size. Omit its
        // covered triangles before building either acceleration structure. A
        // rejected far hit plus t bias could otherwise skip coplanar near
        // terrain at the same distance and leave holes in flat ground.
        self.near_bounds = crate::terrain_cutoff::world_bounds(
            [
                m.lod.window.x0,
                m.lod.window.y0,
                m.lod.window.x1,
                m.lod.window.y1,
            ],
            super::TILE,
            self.terrain[0] as usize,
            self.terrain[1] as usize,
            self.terrain[2],
        );
        let mut builder = Builder::default();
        for &light in &self.static_lights {
            builder.light(light);
        }
        let terrain_id = builder.surface(Surface::new(0, TERRAIN, 0., 0.));
        builder.world_mesh(&span(
            &m.vertices,
            &m.indices[..m.terrain_end as usize],
            terrain_id,
            |_| u32::MAX,
        )?)?;
        if !m.detail.terrain_cutoff {
            builder.world_mesh(&uncovered_far(&self.far, self.near_bounds, terrain_id))?;
        }
        let water_mesh = water_mesh(m, &mut builder)?;
        builder.world_mesh(&water_mesh)?;
        let extras = &m.indices[m.terrain_end as usize..m.water_start as usize];
        if !extras.is_empty() {
            let id = builder.surface(Surface::new(0, 0, 0., 0.));
            builder.world_mesh(&span(&m.vertices, extras, id, |v| rgba(v.color))?)?;
        }
        for page in &m.local_pages {
            building(&mut builder, &mut bank, page)?;
        }
        self.vegetation(m, &mut builder, &mut bank)?;
        Ok((builder.finish(), bank))
    }

    fn vegetation(
        &mut self,
        m: &MeshData,
        builder: &mut Builder,
        textures: &mut Textures,
    ) -> Result<(), String> {
        if !super::skip_trees_test() && !super::ui_test() {
            let mut ids = HashMap::<(usize, usize), Option<u32>>::new();
            let [cx, cz] = [m.tree_center.0, m.tree_center.1];
            let cutoff = m.detail.terrain_cutoff.then_some(self.near_bounds);
            for placement in &self.placements {
                if !crate::terrain_cutoff::contains(cutoff, placement.position) {
                    continue;
                }
                let type_id = placement.tree_type as usize;
                let Some(Some(tree)) = self.trees.get(type_id) else {
                    continue;
                };
                let Some(distance) = crate::tree_lod::client_distance(
                    placement.position,
                    [cx, placement.position[1], cz],
                ) else {
                    continue;
                };
                let selection = crate::tree_lod::select(
                    distance,
                    tree.height,
                    m.detail.tree_lod_scale
                        * crate::scenery_visibility::lod_scale(
                            m.detail.scenery_quality,
                            crate::scenery_visibility::Class::Trees,
                        ),
                );
                let level = selection
                    .secondary
                    .filter(|_| selection.fraction >= 0.5)
                    .unwrap_or(selection.primary);
                let id = if let Some(id) = ids.get(&(type_id, level)) {
                    *id
                } else {
                    let Some((source, materials)) = tree.lods.get(level) else {
                        continue;
                    };
                    let mut mesh = source.clone();
                    let map = materials.map(|m| builder.surface(m));
                    for a in &mut mesh.surfaces {
                        *a = map[*a as usize];
                    }
                    let id = builder.mesh(&mesh)?;
                    ids.insert((type_id, level), id);
                    id
                };
                if let Some(id) = id {
                    builder.place(id, Transform::translation(placement.position))?;
                }
            }
        }
        if m.grass_atlas_changed {
            self.grass.clear();
        }
        let mut grass_ids = HashMap::new();
        let (vertices, indices, instances, batches) = m.grass.geometry();
        for batch in batches {
            let indices = &indices[batch.indices.start as usize..batch.indices.end as usize];
            let Some((&low, &high)) = indices.iter().min().zip(indices.iter().max()) else {
                continue;
            };
            let mut mesh = Mesh::default();
            for g in &vertices[low as usize..=high as usize] {
                let v = &g.vertex;
                mesh.vertices
                    .push(Vertex::new(v.pos, v.normal, atlas_uv(v, g.scale_y)));
            }
            for tri in indices.chunks_exact(3) {
                let g = &vertices[tri[0] as usize];
                let rect = Rect::new(g.vertex.color, g.scale_y, super::ATLAS, super::ATLAS)?;
                let alpha = if self.authored_mips {
                    g.vertex.color[3]
                } else {
                    84. / 255.
                };
                let texture = atlas_texture(
                    &mut grass_ids,
                    &mut self.grass,
                    textures,
                    rect,
                    super::ATLAS,
                    &m.atlas,
                    Filter::cutout(alpha, false),
                )?;
                mesh.tri(
                    [tri[0] - low, tri[1] - low, tri[2] - low],
                    builder.surface(Surface::new(texture, CUTOUT | TWO_SIDED, alpha, 0.)),
                );
            }
            if let Some(id) = builder.mesh(&mesh)? {
                for v in &instances[batch.instances.start as usize..batch.instances.end as usize] {
                    builder.place(
                        id,
                        Transform {
                            position: [v[0], v[1], v[2]],
                            scale: v[3],
                            cos: v[4],
                            sin: -v[5],
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn frame(
        &self,
        matrix: Mat4,
        camera: &Camera,
        f: &FrameSnapshot,
        w: u32,
        h: u32,
    ) -> lighter::Frame {
        let mut direction = Vec3::from_slice(&f.sun).normalize_or_zero();
        if direction.length_squared() < 0.5 {
            direction = Vec3::Y;
        }
        let ambient = camera
            .regional_light
            .map_or([0.08 + f.daylight * 0.24; 3], |l| l.ambient.map(srgb));
        let diffuse = camera
            .regional_light
            .map_or([0.68; 3], |l| l.diffuse.map(srgb));
        let phase = crate::water_gpu::normal_phase(f.elapsed64);
        let waves = if self.waves.is_empty() {
            [0; 4]
        } else {
            let frame = phase.floor() as usize % self.waves.len();
            [
                self.waves[frame],
                self.waves[(frame + 1) % self.waves.len()],
                phase.fract().to_bits(),
                crate::water_gpu::normal_offset_angle(f.elapsed64).to_bits(),
            ]
        };
        let disc = |p: [f32; 4]| {
            let (s, c) = p[1].sin_cos();
            let (zs, xc) = p[2].sin_cos();
            [xc * c, s, zs * c, p[0] * camera.celestial_brightness]
        };
        let air = camera.air_fog;
        let water = camera.underwater_fog;
        let air_range = if camera.underwater_depth <= 0. {
            f.active_fog_range
        } else {
            [air.start, air.end, 1., 0.]
        };
        lighter::Frame {
            vp: matrix.to_cols_array_2d(),
            inv_vp: matrix.inverse().to_cols_array_2d(),
            eye: camera.pos.extend(camera.far).to_array(),
            sun: direction.extend(f.daylight).to_array(),
            ambient: Vec3::from_array(ambient).extend(0.).to_array(),
            sun_color: (Vec3::from_array(diffuse) * f.daylight * 1.3)
                .extend(0.)
                .to_array(),
            air: [
                air_range[0],
                air_range[1],
                if camera.atmospheric_fog && air_range[2] > 0.5 {
                    1.
                } else {
                    0.
                },
                f.grass_wind[0],
            ],
            air_color: Vec3::from_array(air.color.map(srgb)).extend(0.).to_array(),
            water: [water.start, water.end, 1., 0.],
            water_color: Vec3::from_array(water.color.map(srgb))
                .extend(0.)
                .to_array(),
            terrain: self.terrain,
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
                } | if camera.underwater_depth > 0. {
                    lighter::UNDERWATER
                } else {
                    0
                },
                0,
                0,
                self.area_table,
            ],
            waves,
            sky0: self.sky.layers(f.sky_sets.0, f.sky_sets.2.to_bits()),
            sky1: self.sky.layers(f.sky_sets.1, 0),
            sky_tint: Vec3::from_array(f.sky_tint).extend(0.).to_array(),
            sky_uv: [
                f.sky_drift[0],
                f.sky_drift[1],
                crate::sky_gpu::SKY_DRIFT_SCALE,
                0.,
            ],
            sun_disc: disc(f.sky_sun),
            moon_disc: disc(f.sky_moon),
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

fn uncovered_far(far: &Mesh, [x0, x1, z0, z1]: [f32; 4], surface: u32) -> Mesh {
    let triangles: Vec<_> = far
        .triangles
        .iter()
        .copied()
        .filter(|tri| {
            !tri.iter().all(|&index| {
                let [x, _, z] = far.vertices[index as usize].pos;
                x >= x0 && x <= x1 && z >= z0 && z <= z1
            })
        })
        .collect();
    if triangles.is_empty() {
        return Mesh::default();
    }
    Mesh {
        vertices: far.vertices.clone(),
        surfaces: vec![surface; triangles.len()],
        layers: vec![],
        triangles,
    }
}

fn water_mesh(m: &MeshData, builder: &mut Builder) -> Result<Mesh, String> {
    let mut water = span(&m.vertices, &m.indices[m.water_start as usize..], 0, |v| {
        rgba(v.color)
    })?;
    for batch in &m.water_batches {
        let flags = WATER | if batch.simple { SIMPLE_WATER } else { 0 };
        let id = builder.surface(Surface::new(0, flags, 0., 0.));
        water.surfaces[((batch.start - m.water_start) / 3) as usize
            ..((batch.end - m.water_start) / 3) as usize]
            .fill(id);
    }
    Ok(water)
}

fn read_tree(
    source: &crate::trees::TreeMesh,
    textures: &mut Textures,
    authored: bool,
) -> Result<Tree, String> {
    let xyz = |p: [f32; 3]| [p[0], p[2], p[1]];
    let alpha = |value: Option<f32>, default: Option<f32>| {
        if authored {
            value
                .filter(|v| *v >= 0.)
                .map_or(default.map(|r| (r, false)), |v| Some((v / 255., true)))
        } else {
            default.map(|r| (r, false))
        }
    };
    let mut height = 5f32;
    let mut tiles = HashMap::new();
    let mut lods = Vec::with_capacity(source.lods.len());
    for lod in &source.lods {
        let thresholds = [
            alpha(lod.branch_alpha_ref, None),
            alpha(lod.frond_alpha_ref, Some(0.5)),
            if authored {
                Some((84. / 255., true))
            } else {
                Some((0.5, false))
            },
        ];
        let mut materials = [Surface::new(0, 0, 0., 0.); 3];
        for (i, reference) in thresholds.into_iter().enumerate() {
            let image = if i == 0 {
                &source.bark
            } else {
                &source.composite
            };
            let mode = reference.map_or(Filter::Color, |(r, strict)| Filter::cutout(r, strict));
            let addr = match tiles.entry((i == 0, mode)) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => *e.insert(textures.rgba(
                    [image.width(), image.height()],
                    image.as_raw(),
                    mode,
                )?),
            };
            let (cutoff, flags) = reference.map_or((0., 0), |(r, strict)| {
                (
                    r,
                    CUTOUT | TWO_SIDED | if strict { STRICT_ALPHA } else { 0 },
                )
            });
            materials[i] = Surface::new(addr, flags, cutoff, 0.);
        }
        let mut mesh = Mesh::default();
        for (i, part) in [&lod.branch, &lod.frond].into_iter().enumerate() {
            let base = mesh.vertices.len() as u32;
            mesh.vertices.extend(part.vertices.iter().map(|v| {
                height = height.max(v.pos[2]);
                Vertex::new(xyz(v.pos), xyz(v.normal), v.uv)
            }));
            for tri in part.indices.chunks_exact(3) {
                mesh.tri([base + tri[0], base + tri[1], base + tri[2]], i as u32);
            }
        }
        for leaf in &lod.leaves {
            height = height.max(leaf.center[2]);
            let center = Vec3::from_array(xyz(leaf.center));
            // Real SpeedTree leaf geometry/UVs. Camera-only billboards would
            // disappear edge-on to secondary rays; put the same cards in two
            // orthogonal orientations. Fixed oriented leaves keep their own.
            let orientations: &[f32] = if leaf.rho > 1000. {
                &[0.]
            } else {
                &[0., std::f32::consts::FRAC_PI_2]
            };
            for az in orientations {
                let base = mesh.vertices.len() as u32;
                let (s, c) = (az + leaf.rho).sin_cos();
                for (j, [x, y, z]) in leaf.corners.into_iter().enumerate() {
                    let v = if leaf.rho > 1000. {
                        xyz([x, y, z])
                    } else {
                        [c * x - s * y, z, s * x + c * y]
                    };
                    mesh.vertices.push(Vertex::new(
                        (center + Vec3::from_array(v)).to_array(),
                        leaf.normal.map_or([0., 1., 0.], xyz),
                        leaf.uv[j],
                    ));
                }
                mesh.tri([base, base + 1, base + 2], 2);
                mesh.tri([base + 2, base + 1, base + 3], 2);
            }
        }
        lods.push((mesh, materials));
    }
    Ok(Tree { lods, height })
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}
impl Rect {
    fn new(color: [f32; 4], sy: f32, width: usize, height: usize) -> Result<Self, String> {
        let r = Self {
            x: (color[0] * width as f32).round() as usize,
            y: (color[1] * height as f32).round() as usize,
            w: (color[2] * width as f32).round() as usize,
            h: (sy * height as f32).round() as usize,
        };
        if r.w == 0
            || r.h == 0
            || r.x.saturating_add(r.w) > width
            || r.y.saturating_add(r.h) > height
        {
            return Err("Lighter material tile outside native atlas".into());
        }
        Ok(r)
    }
}
fn atlas_texture(
    ids: &mut HashMap<(Rect, Filter), u32>,
    cache: &mut HashMap<(Rect, Filter), Tile>,
    bank: &mut Textures,
    rect: Rect,
    width: usize,
    bytes: &[u8],
    filter: Filter,
) -> Result<u32, String> {
    let key = (rect, filter);
    if let Some(&id) = ids.get(&key) {
        return Ok(id);
    }
    let tile = match cache.entry(key) {
        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
        std::collections::hash_map::Entry::Vacant(e) => {
            if ((rect.y + rect.h - 1) * width + rect.x + rect.w) > bytes.len() / 4 {
                return Err("Lighter atlas source not present for streamed tile".into());
            }
            e.insert(Tile::new(
                [rect.w as u32, rect.h as u32],
                COLOR_SIDE,
                filter,
                |x, y| {
                    let p = ((rect.y + y as usize) * width + rect.x + x as usize) * 4;
                    u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap())
                },
            )?)
        }
    };
    let id = bank.insert(tile)?;
    ids.insert(key, id);
    Ok(id)
}
fn atlas_uv(v: &WorldVertex, sy: f32) -> [f32; 2] {
    [
        (v.uv[0] - v.color[0]) / v.color[2].max(0.000001),
        (v.uv[1] - v.color[1]) / sy.max(0.000001),
    ]
}

fn building(
    builder: &mut Builder,
    textures: &mut Textures,
    page: &crate::world_objects::Page,
) -> Result<(), String> {
    let mut mesh = Mesh::default();
    mesh.vertices = page
        .vertices
        .iter()
        .map(|v| {
            Vertex::new(v.pos, v.normal, atlas_uv(v, v.color[3]))
                .tint(argb_to_rgba(v.diffuse), argb_to_rgba(v.night_diffuse))
        })
        .collect();
    let mut tiles = HashMap::new();
    let mut ids = HashMap::new();
    let mut append = |index: &[u32], visibility: u32| -> Result<(), String> {
        for tri in index.chunks_exact(3) {
            let v = &page.vertices[tri[0] as usize];
            let is_sdr = (v.kind > 2.01 && v.kind < 2.03) || (v.kind > 2.11 && v.kind < 2.13);
            let mut flags = BAKED | visibility;
            let tex = if (1.5..2.5).contains(&v.kind) {
                let rect = Rect::new(
                    v.color,
                    v.color[3],
                    crate::world_objects::SIDE,
                    page.used_height,
                )?;
                flags |= if is_sdr { BLEND } else { CUTOUT };
                atlas_texture(
                    &mut ids,
                    &mut tiles,
                    textures,
                    rect,
                    crate::world_objects::SIDE,
                    &page.atlas,
                    if is_sdr {
                        Filter::Alpha
                    } else {
                        Filter::cutout(0.5, false)
                    },
                )?
            } else {
                0
            };
            if (v.kind > 1.05 && v.kind < 1.15) || (v.kind > 1.5 && v.kind < 2.05 && !is_sdr) {
                flags |= CULL_BACK;
            }
            let id = builder.surface(Surface::new(tex, flags, 0.5, 0.));
            mesh.tri([tri[0], tri[1], tri[2]], id);
        }
        Ok(())
    };
    if page.placements.is_empty() {
        append(&page.indices, 0)?;
    }
    for range in &page.placements {
        let flags = if range.visibility.reflect {
            0
        } else {
            NO_REFLECT
        } | if range.visibility.refract {
            0
        } else {
            NO_REFRACT
        };
        append(
            &page.indices[range.start as usize..range.end as usize],
            flags,
        )?;
    }
    builder.world_mesh(&mesh)
}

fn point_light(p: [f32; 3], radius: f32, color: [f32; 4], intensity: f32) -> Light {
    Light {
        position_range: [p[0], p[1], p[2], radius],
        color: [
            srgb(color[0]) * intensity,
            srgb(color[1]) * intensity,
            srgb(color[2]) * intensity,
            1.,
        ],
    }
}

fn moving(
    models: Option<&mut world_models_gpu::World>,
    modules: &mut crate::modules::Modules,
    textures: &mut Textures,
    matrix: Mat4,
) -> Result<Packet, String> {
    let mut out = lighter::model_feed::Moving::new(textures);
    if let Some(models) = models {
        let (geometry, effects, dynamic, read) = models.ray_sources();
        for l in dynamic {
            out.builder
                .light(point_light(l.position, l.radius, l.color, l.color[3]));
        }
        let mut decode =
            |path: &str| read(path).and_then(|b| crate::model_texture_gpu::decode_for_rays(&b));
        for (asset, poses) in geometry {
            out.model("world", asset, poses, |_| true, &mut decode)?;
        }
        out.effects("world", effects, &mut decode)?;
    }
    modules.ray_inputs(matrix, &mut out)?;
    out.finish()
}

impl super::Renderer {
    /// Lighter owns primary visibility and all water/shadow/lighting rays.
    /// Post/presentation (and independent map views) still use the host.
    pub(super) fn render_lighter(
        &mut self,
        target: &wgpu::TextureView,
        matrix: Mat4,
        camera: &Camera,
        frame: &FrameSnapshot,
        w: u32,
        h: u32,
    ) {
        let depth = self.depth.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if self.lighter.error().is_some() || self.local_scenery_skipped {
            Lighter::clear(&mut encoder, target, &depth);
            self.queue.submit(Some(encoder.finish()));
            return;
        }
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
        let result = (|| -> Result<(), String> {
            let feed = self
                .lighter_feed
                .as_ref()
                .ok_or("Lighter scene feed is unavailable")?;
            let frame = feed.frame(matrix, camera, frame, w, h);
            let world = self
                .lighter
                .world
                .as_mut()
                .ok_or("Lighter has no resident geometry")?;
            let moving = moving(
                self.models.as_mut(),
                &mut self.modules,
                &mut world.textures,
                matrix,
            )?;
            self.lighter.render(
                &self.device,
                &self.queue,
                &mut encoder,
                scene_target,
                &depth,
                self.scene_format,
                frame,
                &moving,
            )
        })();
        if let Err(error) = result {
            self.lighter.fail_frame(error);
            Lighter::clear(&mut encoder, target, &depth);
            self.queue.submit(Some(encoder.finish()));
            return;
        }
        if use_post {
            self.water.copy_depth(&mut encoder, &self.depth);
        }
        self.queue.submit(Some(encoder.finish()));
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
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
