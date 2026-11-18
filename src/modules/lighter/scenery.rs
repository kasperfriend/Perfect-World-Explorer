//! BMD scene residency for Lighter. This uses the original archive resolver and
//! the BMD's baked world transform, but neither a raster atlas nor its draw
//! queue. The native region is retained; a bounded, direction-independent
//! extension can supply geometry to any kind of ray.
use crate::{
    bmd_baked::{self, Cull, Stage},
    modules::lighter::{
        scene::{self, Builder, Gloss, Mesh, MeshBudget, Surface, Vertex},
        textures::{Filter, Textures, Tile, COLOR_SIDE, MAX_TEXTURE_BYTES},
    },
    objects::{Objects, SdrUvAnim},
    scene::Ornament,
    scenery_visibility::Visibility,
};
use glam::Vec3;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Weak},
};

const REACH: f32 = 768.;
const EXTRA_PLACEMENTS: usize = 512;
const EXTRA_GEOMETRY_BYTES: usize = 16 << 20;
const EXTRA_TEXTURE_BYTES: usize = 16 << 20;
const DECODE_BATCH: usize = 16;
type TileKey = (u64, Filter);

#[derive(Default)]
pub(in crate::gpu) struct SceneryStream {
    catalog: Option<Arc<[Ornament]>>,
    assets: HashMap<usize, Option<Arc<Asset>>>,
    tiles: HashMap<TileKey, Weak<Tile>>,
}

pub(in crate::gpu) struct Snapshot {
    local: Vec<Arc<Asset>>,
    extra: Vec<Arc<Asset>>,
    outside: usize,
    deferred: usize,
    unavailable: usize,
}

#[derive(Default, Clone, Copy, Debug)]
pub(in crate::gpu) struct Coverage {
    pub local: usize,
    pub extra: usize,
    pub outside: usize,
    pub deferred: usize,
    pub unavailable: usize,
    pub missing_textures: usize,
}

impl std::fmt::Display for Coverage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BMD ray coverage: {} local + {} extended; {} out of range/cutoff, {} budget-limited, {} unavailable, {} missing texture references",
            self.local, self.extra, self.outside, self.deferred, self.unavailable, self.missing_textures
        )
    }
}

struct Asset {
    mesh: Mesh,
    surfaces: Vec<(Surface, Option<usize>)>,
    tiles: Vec<(TileKey, Arc<Tile>)>,
    missing: usize,
}

impl Asset {
    fn read(
        objects: &Objects,
        id: usize,
        tiles: &mut HashMap<TileKey, Weak<Tile>>,
    ) -> Result<Option<Self>, String> {
        let Some(model) = objects.get(id) else {
            return Ok(None);
        };
        let visibility = Visibility::from_extra(objects.ornaments[id].extra);
        let visibility = if visibility.reflect {
            0
        } else {
            scene::NO_REFLECT
        } | if visibility.refract {
            0
        } else {
            scene::NO_REFRACT
        };
        // EC-BSD positions select residency. Geometry in these BMDs is already
        // world baked (including the transform in the file header).
        let z = super::super::basis(model.direction);
        let y = super::super::basis(model.up);
        let x = y.cross(z).normalize_or_zero();
        let position = Vec3::from_array(model.position);
        let mut asset = Self {
            mesh: Mesh::default(),
            surfaces: Vec::new(),
            tiles: Vec::new(),
            missing: 0,
        };
        let mut local_tiles = HashMap::new();
        for part in &model.meshes {
            if part.indices.is_empty() {
                continue;
            }
            let state = bmd_baked::classify(
                part.version,
                !part.texture.trim().is_empty(),
                part.material.as_ref().is_some_and(|m| m.two_sided),
            )?;
            let mut flags = scene::BAKED | visibility;
            let tile = if state.stage == Stage::Modulate2x {
                let scroll = SdrUvAnim::from_texture_path(&part.texture)
                    .is_some_and(|a| a.tc_scroll[1] < 0.);
                flags |= if scroll { scene::BLEND } else { scene::CUTOUT };
                if !scroll && state.cull == Cull::BackFace {
                    flags |= scene::CULL_BACK;
                }
                let filter = if scroll {
                    Filter::Alpha
                } else {
                    Filter::cutout(0.5, false)
                };
                match objects
                    .texture_key(&part.texture)
                    .zip(objects.texture(&part.texture))
                {
                    Some((hash, tex)) => {
                        let key = (hash, filter);
                        let index = if let Some(&index) = local_tiles.get(&key) {
                            index
                        } else {
                            let tile = if let Some(tile) = tiles.get(&key).and_then(Weak::upgrade) {
                                tile
                            } else {
                                let tile = Arc::new(Tile::new(
                                    [tex.width as u32, tex.height as u32],
                                    COLOR_SIDE,
                                    filter,
                                    |x, y| {
                                        let p = (y as usize * tex.width + x as usize) * 4;
                                        u32::from_le_bytes(tex.rgba[p..p + 4].try_into().unwrap())
                                    },
                                )?);
                                tiles.insert(key, Arc::downgrade(&tile));
                                tile
                            };
                            let index = asset.tiles.len();
                            asset.tiles.push((key, tile));
                            local_tiles.insert(key, index);
                            index
                        };
                        Some(index)
                    }
                    None => {
                        // Preserve the existing white fallback, but count it.
                        asset.missing += 1;
                        None
                    }
                }
            } else {
                if state.cull == Cull::BackFace {
                    flags |= scene::CULL_BACK;
                }
                None
            };
            let gloss = part
                .material
                .as_ref()
                .map_or_else(Gloss::default, |m| Gloss::authored(m.specular, m.power));
            let mat = asset.surfaces.len() as u32;
            asset
                .surfaces
                .push((Surface::new(0, flags, 0.5, 0.).with_gloss(gloss), tile));
            let base = asset.mesh.vertices.len() as u32;
            for v in &part.vertices {
                let p = position
                    + x * (v.position[0] * model.scale[0])
                    + y * (v.position[1] * model.scale[1])
                    + z * (v.position[2] * model.scale[2]);
                if !p.is_finite() || p.abs().max_element() > 1.0e7 {
                    return Err("invalid baked world position".into());
                }
                let n = (x * v.normal[0] + y * v.normal[1] + z * v.normal[2]).normalize_or_zero();
                asset
                    .mesh
                    .vertices
                    .push(Vertex::new(p.to_array(), n.to_array(), v.uv).tint(
                        scene::argb_to_rgba(v.day_diffuse),
                        scene::argb_to_rgba(v.night_diffuse),
                    ));
            }
            for i in part.indices.chunks_exact(3) {
                asset.mesh.tri(
                    [
                        base + u32::from(i[0]),
                        base + u32::from(i[1]),
                        base + u32::from(i[2]),
                    ],
                    mat,
                );
            }
        }
        Ok((!asset.mesh.triangles.is_empty()).then_some(asset))
    }

    fn pack(
        &self,
        mesh: &mut Mesh,
        builder: &mut Builder,
        bank: &mut Textures,
        addresses: &mut HashMap<TileKey, u32>,
    ) -> Result<(), String> {
        let mut material_ids = Vec::with_capacity(self.surfaces.len());
        for &(mut surface, tile) in &self.surfaces {
            if let Some(i) = tile {
                let (key, tile) = &self.tiles[i];
                surface.texture = match addresses.entry(*key) {
                    std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                    std::collections::hash_map::Entry::Vacant(e) => *e.insert(bank.insert(tile)?),
                };
            }
            material_ids.push(builder.surface(surface));
        }
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend_from_slice(&self.mesh.vertices);
        for (&i, &surface) in self.mesh.triangles.iter().zip(&self.mesh.surfaces) {
            mesh.tri(i.map(|i| base + i), material_ids[surface as usize]);
        }
        Ok(())
    }
}

// Sort by horizontal distance to the *installed* stream block, never by view
// direction. A stable ID breaks ties. Required native placements are never
// replaced by or duplicated in the optional extension.
fn select(
    ornaments: &[Ornament],
    local: &HashSet<usize>,
    center: [f32; 2],
    cutoff: Option<[f32; 4]>,
) -> Vec<usize> {
    let mut order: Vec<_> = ornaments
        .iter()
        .enumerate()
        .filter_map(|(id, p)| {
            let distance =
                (p.position[0] - center[0]).powi(2) + (p.position[2] - center[1]).powi(2);
            (distance <= REACH * REACH
                && !local.contains(&id)
                && crate::terrain_cutoff::contains(cutoff, p.position))
            .then_some((distance, id))
        })
        .collect();
    order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    order.into_iter().map(|(_, id)| id).collect()
}

struct Budget {
    geometry: MeshBudget,
    texture_bytes: usize,
    tiles: HashSet<TileKey>,
}

impl Budget {
    fn admit(&mut self, asset: &Asset) -> bool {
        let bytes: usize = asset
            .tiles
            .iter()
            .filter(|(key, _)| !self.tiles.contains(key))
            .map(|(_, tile)| tile.bytes())
            .sum();
        if bytes > self.texture_bytes || !self.geometry.debit(&asset.mesh, asset.surfaces.len()) {
            return false;
        }
        self.texture_bytes -= bytes;
        self.tiles.extend(asset.tiles.iter().map(|(key, _)| *key));
        true
    }
}

impl SceneryStream {
    fn read(&mut self, objects: &Objects, id: usize) -> Option<Arc<Asset>> {
        if let Some(asset) = self.assets.remove(&id) {
            return asset;
        }
        match Asset::read(objects, id, &mut self.tiles) {
            Ok(asset) => asset.map(Arc::new),
            Err(error) => {
                eprintln!(
                    "Lighter: BMD {:?} unavailable ({error})",
                    objects.ornaments[id].path
                );
                None
            }
        }
    }

    /// Runs on the world loader. Unchanged models and shared texture tiles
    /// survive block crossings. The latest snapshot owns its data until it
    /// reaches the renderer; the stream holds no unbounded history of assets.
    pub(in crate::gpu) fn prepare(
        &mut self,
        objects: &mut Objects,
        local_ids: &[usize],
        center: [f32; 2],
        cutoff: Option<[f32; 4]>,
    ) -> Result<Snapshot, String> {
        if !self
            .catalog
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &objects.ornaments))
        {
            self.assets.clear();
            self.tiles.clear();
            self.catalog = Some(objects.ornaments.clone());
        }
        let mut ids = HashSet::with_capacity(local_ids.len());
        let mut next = HashMap::new();
        let mut local = Vec::new();
        let mut unavailable = 0;
        for &id in local_ids {
            if ids.insert(id) {
                let asset = self.read(objects, id);
                if let Some(asset) = &asset {
                    local.push(asset.clone());
                } else {
                    unavailable += 1;
                }
                next.insert(id, asset);
            }
        }
        let mut order = select(&objects.ornaments, &ids, center, cutoff);
        let outside = objects.ornaments.len() - ids.len() - order.len();
        let mut deferred = order.len().saturating_sub(EXTRA_PLACEMENTS);
        order.truncate(EXTRA_PLACEMENTS);
        let mut budget = Budget {
            geometry: MeshBudget {
                vertices: usize::MAX,
                triangles: usize::MAX,
                bytes: EXTRA_GEOMETRY_BYTES,
            },
            texture_bytes: EXTRA_TEXTURE_BYTES,
            tiles: local
                .iter()
                .flat_map(|a| a.tiles.iter().map(|(key, _)| *key))
                .collect(),
        };
        let mut extra = Vec::new();
        for batch in order.chunks(DECODE_BATCH) {
            let needed: Vec<_> = batch
                .iter()
                .copied()
                .filter(|id| !self.assets.contains_key(id))
                .collect();
            if !needed.is_empty() {
                objects.with_supplemental_ids(&needed, |objects| {
                    for id in needed.iter().copied() {
                        let asset = self.read(objects, id);
                        self.assets.insert(id, asset);
                    }
                })?;
            }
            for &id in batch {
                let asset = self.assets.remove(&id).expect("prepared BMD batch");
                match asset {
                    Some(asset) if budget.admit(&asset) => {
                        extra.push(asset.clone());
                        next.insert(id, Some(asset));
                    }
                    Some(_) => deferred += 1,
                    None => {
                        unavailable += 1;
                        next.insert(id, None);
                    }
                }
            }
        }
        self.assets = next;
        self.tiles.retain(|_, t| t.strong_count() > 0);
        Ok(Snapshot {
            local,
            extra,
            outside,
            deferred,
            unavailable,
        })
    }
}

impl Snapshot {
    pub(in crate::gpu) fn append(
        &self,
        builder: &mut Builder,
        bank: &mut Textures,
        storage: usize,
    ) -> Result<Coverage, String> {
        let mut addresses = HashMap::new();
        let mut local = Mesh::default();
        let mut coverage = Coverage {
            local: self.local.len(),
            outside: self.outside,
            deferred: self.deferred,
            unavailable: self.unavailable,
            ..Coverage::default()
        };
        for asset in &self.local {
            asset.pack(&mut local, builder, bank, &mut addresses)?;
            coverage.missing_textures += asset.missing;
        }
        builder.world_mesh(&local)?;
        drop(local);
        let mut budget = Budget {
            geometry: builder.optional_mesh_budget(storage),
            // Keep at least half the remaining bank for posed models/effects.
            texture_bytes: storage
                .min(MAX_TEXTURE_BYTES)
                .saturating_sub(bank.bytes().len())
                / 2,
            tiles: addresses.keys().copied().collect(),
        };
        let mut extra = Mesh::default();
        for asset in &self.extra {
            if budget.admit(asset) {
                asset.pack(&mut extra, builder, bank, &mut addresses)?;
                coverage.extra += 1;
                coverage.missing_textures += asset.missing;
            } else {
                coverage.deferred += 1;
            }
        }
        builder.world_mesh(&extra)?;
        Ok(coverage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ornament(x: f32, z: f32) -> Ornament {
        Ornament {
            export_id: 0,
            offset: 0,
            position: [x, 0., z],
            direction: [0., 0., 1.],
            up: [0., 1., 0.],
            extra: 0,
            path: String::new(),
            map_id: "world".into(),
        }
    }

    #[test]
    fn selects_both_sides_in_order_without_duplicates_or_leaking_past_cutoff() {
        let all = [
            ornament(-160., 0.),
            ornament(160., 0.),
            ornament(0., 1.), // already local
            ornament(0., REACH + 1.),
            ornament(100., -10.),
            ornament(f32::NAN, 0.),
        ];
        let local = HashSet::from([2]);
        assert_eq!(select(&all, &local, [0.; 2], None), [4, 0, 1]);
        assert_eq!(
            select(&all, &local, [0.; 2], Some([-100., 200., -20., 20.])),
            [4, 1]
        );
        assert!(select(&all, &local, [REACH * 2., 0.], None).is_empty());
    }

    #[test]
    fn budget_rejects_whole_assets_without_charging_and_shares_tiles() {
        let tile = Arc::new(Tile::new([1, 1], 1, Filter::Color, |_, _| u32::MAX).unwrap());
        let asset = |hash| {
            let mut mesh = Mesh::default();
            mesh.vertices = vec![Vertex::new([0.; 3], [0., 1., 0.], [0.; 2]); 3];
            mesh.tri([0, 1, 2], 0);
            Asset {
                mesh,
                surfaces: vec![(Surface::new(0, 0, 0., 0.), Some(0))],
                tiles: vec![((hash, Filter::Color), tile.clone())],
                missing: 0,
            }
        };
        let mut budget = Budget {
            geometry: MeshBudget {
                vertices: 6,
                triangles: 2,
                bytes: 1024,
            },
            texture_bytes: tile.bytes(),
            tiles: HashSet::new(),
        };
        assert!(budget.admit(&asset(42)));
        assert!(!budget.admit(&asset(43)));
        assert!(budget.admit(&asset(42))); // no second texture charge
        assert!(!budget.admit(&asset(42))); // triangle/vertex budget spent
        assert_eq!(budget.texture_bytes, 0);
        assert_eq!(budget.tiles.len(), 1);
    }
}
