//! BMD scene residency for Lighter. This uses the original archive resolver and
//! the BMD's baked world transform, but neither a raster atlas nor its draw
//! queue. The native region is retained; a bounded, direction-independent
//! extension can supply geometry to any kind of ray.
use crate::{
    bmd_baked::{self, Cull, Stage},
    modules::lighter::{
        coverage::Counts,
        scene::{self, Builder, Gloss, Mesh, MeshBudget, Surface, Vertex},
        textures::{Filter, Textures, Tile, COLOR_SIDE, MAX_TEXTURE_BYTES},
    },
    objects::{Objects, SdrUvAnim},
    scene::Ornament,
    scene_lights::SceneLight,
    scenery_visibility::Visibility,
};
use glam::Vec3;
use std::{
    collections::{hash_map::Entry, BTreeMap, HashMap, HashSet},
    sync::{Arc, Weak},
};

#[path = "scenery_manifest.rs"]
mod manifest;
use manifest::{ChunkId, Manifest, Query};

const EXTRA_PLACEMENTS: usize = 512;
const EXTRA_GEOMETRY_BYTES: usize = 16 << 20;
const EXTRA_TEXTURE_BYTES: usize = 16 << 20;
const DECODE_BATCH: usize = 16;
type TileKey = (u64, Filter);

#[derive(Default)]
pub(in crate::gpu) struct SceneryStream {
    catalog: Option<Arc<[Ornament]>>,
    manifest: Manifest,
    assets: HashMap<usize, Result<Arc<Asset>, Omission>>,
    tiles: HashMap<TileKey, Weak<Tile>>,
    pub(super) grass_tiles: HashMap<String, Arc<Tile>>,
}

pub(in crate::gpu) struct Snapshot {
    local: Vec<Arc<Asset>>,
    extra: Vec<Arc<Asset>>,
    counts: Counts,
    catalog_chunks: usize,
    estimated_bounds: usize,
}

#[derive(Default, Clone, Copy, Debug)]
pub(in crate::gpu) struct Coverage {
    pub local: usize,
    pub extra: usize,
    /// Final counts, after applying both the loader and the GPU packet budgets.
    pub counts: Counts,
    pub catalog_chunks: usize,
    pub resident_chunks: usize,
    pub estimated_bounds: usize,
}

impl std::fmt::Display for Coverage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BMD ray coverage: {} local + {} extended in {}/{} chunks; {} out of range, {} cutoff, {} budget-limited, {} failed, {} missing texture references, {} estimated bounds",
            self.local, self.extra, self.resident_chunks, self.catalog_chunks, self.counts.range, self.counts.cutoff, self.counts.budget, self.counts.failed(), self.counts.texture_fallback, self.estimated_bounds
        )
    }
}

#[derive(Clone, Copy)]
enum Omission {
    Package,
    Unavailable,
    Invalid,
    Empty,
}

impl Omission {
    fn count(self, counts: &mut Counts) {
        match self {
            Self::Package => counts.package += 1,
            Self::Unavailable => counts.unavailable += 1,
            Self::Invalid => counts.invalid += 1,
            Self::Empty => counts.empty += 1,
        }
    }
}

struct Asset {
    id: usize,
    block: ChunkId,
    bounds: scene::Bounds,
    mesh: Mesh,
    surfaces: Vec<(Surface, Option<usize>, Option<usize>)>,
    tiles: Vec<(TileKey, Arc<Tile>)>,
    missing: usize,
}

impl Asset {
    fn read(
        objects: &Objects,
        id: usize,
        model: &crate::bmd::Model,
        tiles: &mut HashMap<TileKey, Weak<Tile>>,
    ) -> Result<Option<Self>, String> {
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
        // Group by stable 128 m world cells, not by camera or local/extra status.
        let mut asset = Self {
            id,
            block: manifest::owner(objects.ornaments[id].position),
            bounds: scene::Bounds::EMPTY,
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
            let anim = SdrUvAnim::from_texture_path(&part.texture);
            let mut albedo = None;
            let mut normal = None;
            if state.stage == Stage::Modulate2x {
                let blend = anim.is_some_and(|a| a.tc_scroll[1] < 0.);
                flags |= if blend { scene::BLEND } else { scene::CUTOUT };
                if !blend && state.cull == Cull::BackFace {
                    flags |= scene::CULL_BACK;
                }
                let filter = if blend {
                    Filter::Alpha
                } else {
                    Filter::cutout(0.5, false)
                };
                if let Some((hash, tex)) = objects.texture_with_key(&part.texture) {
                    albedo = Some(asset.add_tile(hash, tex, filter, &mut local_tiles, tiles)?);
                } else {
                    asset.missing += 1;
                }
                if let Some((hash, tex)) = objects.ray_normal_with_key(&part.texture) {
                    normal =
                        Some(asset.add_tile(hash, tex, Filter::Data, &mut local_tiles, tiles)?);
                }
            } else {
                flags |= scene::BAKED_COLOR_ONLY;
                if state.cull == Cull::BackFace {
                    flags |= scene::CULL_BACK;
                }
            }
            let mut surface = Surface::new(0, flags, 0.5, 0.);
            if let Some(m) = &part.material {
                surface = surface
                    .with_gloss(Gloss::authored(m.specular, m.power))
                    .with_lighting(m.diffuse, m.ambient, m.emissive);
            }
            if let Some(a) = anim {
                surface = surface.uv_transform(a.tc_scale, a.tc_scroll);
            }
            let mat = asset.surfaces.len() as u32;
            asset.surfaces.push((surface, albedo, normal));
            let base = asset.mesh.vertices.len() as u32;
            for v in &part.vertices {
                let p = position
                    + x * (v.position[0] * model.scale[0])
                    + y * (v.position[1] * model.scale[1])
                    + z * (v.position[2] * model.scale[2]);
                if !p.is_finite() || p.abs().max_element() > 1.0e7 {
                    return Err("invalid baked world position".into());
                }
                asset.bounds = asset.bounds.union(scene::Bounds::point(p));
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

    fn add_tile(
        &mut self,
        hash: u64,
        tex: &crate::objects::Texture,
        filter: Filter,
        local: &mut HashMap<(u64, Filter), usize>,
        tiles: &mut HashMap<(u64, Filter), Weak<Tile>>,
    ) -> Result<usize, String> {
        let key = (hash, filter);
        if let Some(&i) = local.get(&key) {
            return Ok(i);
        }
        let tile = if let Some(tile) = tiles.get(&key).and_then(Weak::upgrade) {
            tile
        } else {
            let chain = tex
                .ray_dds
                .as_deref()
                .and_then(|raw| crate::model_texture_gpu::decode_for_rays(raw).ok());
            let tile = Arc::new(match chain {
                Some((images, _)) => Tile::images(&images, COLOR_SIDE, filter)?,
                None => Tile::levels(
                    [([tex.width as u32, tex.height as u32], tex.rgba.as_slice())],
                    COLOR_SIDE,
                    filter,
                )?,
            });
            tiles.insert(key, Arc::downgrade(&tile));
            tile
        };
        let index = self.tiles.len();
        self.tiles.push((key, tile));
        local.insert(key, index);
        Ok(index)
    }

    fn materials(
        &self,
        builder: &mut Builder,
        bank: &mut Textures,
        addresses: &mut HashMap<TileKey, u32>,
    ) -> Result<Vec<u32>, String> {
        let mut material_ids = Vec::with_capacity(self.surfaces.len());
        for &(mut surface, albedo, normal) in &self.surfaces {
            for (id, dest) in [
                (albedo, &mut surface.texture),
                (normal, &mut surface.normal_map),
            ] {
                if let Some(index) = id {
                    let (key, tile) = &self.tiles[index];
                    *dest = match addresses.entry(*key) {
                        Entry::Occupied(e) => *e.get(),
                        Entry::Vacant(e) => *e.insert(bank.insert(tile)?),
                    };
                }
            }
            material_ids.push(builder.surface(surface));
        }
        Ok(material_ids)
    }

    fn pack(&self, mesh: &mut Mesh, material_ids: &[u32]) {
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend_from_slice(&self.mesh.vertices);
        for (&i, &surface) in self.mesh.triangles.iter().zip(&self.mesh.surfaces) {
            mesh.tri(i.map(|i| base + i), material_ids[surface as usize]);
        }
    }
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
    fn read(&mut self, objects: &Objects, id: usize) -> Result<Arc<Asset>, Omission> {
        if let Some(asset) = self.assets.remove(&id) {
            return asset;
        }
        let model = objects.get(id).ok_or_else(|| {
            if objects.lit_archive_available() {
                Omission::Unavailable
            } else {
                Omission::Package
            }
        })?;
        match Asset::read(objects, id, model, &mut self.tiles) {
            Ok(Some(asset)) => {
                self.manifest.learned_bounds(id, asset.bounds);
                Ok(Arc::new(asset))
            }
            Ok(None) => Err(Omission::Empty),
            Err(error) => {
                eprintln!(
                    "Lighter: BMD {:?} unavailable ({error})",
                    objects.ornaments[id].path
                );
                Err(Omission::Invalid)
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
        local_reach: f32,
        cutoff: Option<[f32; 4]>,
        lights: &[SceneLight],
    ) -> Result<Snapshot, String> {
        objects.retain_ray_dds();
        if !self
            .catalog
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &objects.ornaments))
        {
            self.assets.clear();
            self.tiles.clear();
            self.manifest = Manifest::new(&objects.ornaments);
            self.catalog = Some(Arc::clone(&objects.ornaments));
        }
        let mut ids = HashSet::with_capacity(local_ids.len());
        let mut next = HashMap::new();
        let mut local = Vec::new();
        let mut counts = Counts {
            cutoff: self.manifest.cutoff_count(cutoff),
            invalid: self.manifest.invalid_anchors(),
            ..Counts::default()
        };
        for &id in local_ids {
            if self.manifest.valid_anchor(id)
                && crate::terrain_cutoff::contains(cutoff, objects.ornaments[id].position)
                && ids.insert(id)
            {
                let asset = self.read(objects, id);
                match &asset {
                    Ok(asset) => local.push(asset.clone()),
                    Err(reason) => reason.count(&mut counts),
                }
                next.insert(id, asset);
            }
        }
        let query = Query::new(center, local_reach, cutoff, lights);
        let mut order = self.manifest.select(&ids, &query);
        counts.range = objects.ornaments.len()
            - ids.len()
            - order.len()
            - counts.cutoff
            - self.manifest.invalid_anchors();
        counts.budget = order.len().saturating_sub(EXTRA_PLACEMENTS);
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
                    // A first decode can invalidate a placement estimate. Do
                    // not publish geometry ranked out of the exact reach.
                    Ok(_) if !self.manifest.reachable(id, &query) => counts.range += 1,
                    Ok(asset) if budget.admit(&asset) => {
                        extra.push(asset.clone());
                        next.insert(id, Ok(asset));
                    }
                    Ok(_) => counts.budget += 1,
                    Err(reason) => {
                        reason.count(&mut counts);
                        next.insert(id, Err(reason));
                    }
                }
            }
        }
        self.assets = next;
        self.tiles.retain(|_, t| t.strong_count() > 0);
        Ok(Snapshot {
            local,
            extra,
            counts,
            catalog_chunks: self.manifest.chunks(),
            estimated_bounds: self.manifest.estimated(),
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
        let mut chunks = BTreeMap::<_, Vec<(&Asset, Vec<u32>)>>::new();
        let mut coverage = Coverage {
            local: self.local.len(),
            counts: self.counts,
            catalog_chunks: self.catalog_chunks,
            estimated_bounds: self.estimated_bounds,
            ..Coverage::default()
        };
        let mut geometry = builder.optional_mesh_budget(storage);
        for asset in &self.local {
            if !geometry.debit(&asset.mesh, asset.surfaces.len()) {
                // Required geometry still goes through, but leave no allowance
                // for optional assets if it has exhausted the budget.
                geometry.bytes = 0;
            }
            let materials = asset.materials(builder, bank, &mut addresses)?;
            chunks
                .entry(asset.block)
                .or_default()
                .push((asset, materials));
            coverage.counts.texture_fallback += asset.missing;
        }
        let mut budget = Budget {
            geometry,
            // Keep at least half the remaining bank for posed models/effects.
            texture_bytes: storage
                .min(MAX_TEXTURE_BYTES)
                .saturating_sub(bank.byte_len())
                / 2,
            tiles: addresses.keys().copied().collect(),
        };
        for asset in &self.extra {
            if budget.admit(asset) {
                let materials = asset.materials(builder, bank, &mut addresses)?;
                chunks
                    .entry(asset.block)
                    .or_default()
                    .push((asset, materials));
                coverage.extra += 1;
                coverage.counts.texture_fallback += asset.missing;
            } else {
                coverage.counts.budget += 1;
            }
        }
        coverage.resident_chunks = chunks.len();
        for ((x, z), mut assets) in chunks {
            // Reprioritizing or promoting a resident from extra to local
            // doesn't change the order/indices inside an unchanged chunk.
            assets.sort_unstable_by_key(|(a, _)| a.id);
            let mut mesh = Mesh::default();
            for (asset, materials) in assets {
                asset.pack(&mut mesh, &materials);
            }
            builder.chunk(scene::ChunkKey::Scenery(x, z), &mesh)?;
        }
        coverage.counts.loaded = coverage.local + coverage.extra;
        Ok(coverage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_rejects_whole_assets_without_charging_and_shares_tiles() {
        let tile = Arc::new(Tile::new([1, 1], 1, Filter::Color, |_, _| u32::MAX).unwrap());
        let asset = |hash| {
            let mut mesh = Mesh::default();
            mesh.vertices = vec![Vertex::new([0.; 3], [0., 1., 0.], [0.; 2]); 3];
            mesh.tri([0, 1, 2], 0);
            Asset {
                id: 0,
                block: (0, 0),
                bounds: scene::Bounds::point(Vec3::ZERO),
                mesh,
                surfaces: vec![(Surface::new(0, 0, 0., 0.), Some(0), None)],
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
