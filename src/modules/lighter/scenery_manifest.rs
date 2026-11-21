//! The loaded map's BMD placement index. An ornament has one stable owner
//! chunk, even if decoded geometry crosses several chunk boundaries.
use crate::{modules::lighter::scene::Bounds, scene::Ornament, scene_lights::SceneLight};
use glam::{Vec2, Vec3, Vec3Swizzles};
use std::collections::{BTreeMap, HashSet};

pub(super) const CELL_SIZE: f32 = 128.;
const PLACEMENT_PAD: f32 = CELL_SIZE * 0.5;
const SECONDARY_REACH: f32 = 768.;
const LIGHT_HINTS: usize = 32;

pub(super) type ChunkId = (i32, i32);
pub(super) fn owner([x, _, z]: [f32; 3]) -> ChunkId {
    (
        (x / CELL_SIZE).floor() as i32,
        (z / CELL_SIZE).floor() as i32,
    )
}

struct Entry {
    owner: ChunkId,
    position: [f32; 3],
    bounds: Bounds,
    decoded: bool,
}

struct Chunk {
    entries: Vec<usize>,
    bounds: Bounds,
}

#[derive(Default)]
pub(super) struct Manifest {
    entries: Vec<Option<Entry>>,
    chunks: BTreeMap<ChunkId, Chunk>,
    estimated: usize,
    invalid: usize,
    warned_expansion: bool,
}

impl Manifest {
    pub fn new(ornaments: &[Ornament]) -> Self {
        let mut manifest = Self::default();
        manifest.entries.reserve(ornaments.len());
        for (id, ornament) in ornaments.iter().enumerate() {
            let position = ornament.position;
            let p = Vec3::from_array(position);
            if !p.is_finite() {
                manifest.entries.push(None);
                manifest.invalid += 1;
                continue;
            }
            let owner = owner(position);
            let bounds = Bounds {
                min: p - Vec3::splat(PLACEMENT_PAD),
                max: p + Vec3::splat(PLACEMENT_PAD),
            };
            let chunk = manifest.chunks.entry(owner).or_insert_with(|| Chunk {
                entries: Vec::new(),
                bounds: Bounds::EMPTY,
            });
            chunk.entries.push(id);
            chunk.bounds = chunk.bounds.union(bounds);
            manifest.entries.push(Some(Entry {
                owner,
                position,
                bounds,
                decoded: false,
            }));
            manifest.estimated += 1;
        }
        if manifest.estimated > 0 {
            eprintln!(
                "Lighter: BMD manifest uses conservative ±{PLACEMENT_PAD} placement bounds for {} assets in {} chunks until decoded; very large or offset models may extend past the estimate",
                manifest.estimated,
                manifest.chunks.len(),
            );
        }
        manifest
    }

    pub fn learned_bounds(&mut self, id: usize, bounds: Bounds) {
        let Some(entry) = &mut self.entries[id] else {
            return;
        };
        if !entry.decoded {
            if !self.warned_expansion
                && (bounds.min.cmplt(entry.bounds.min).any()
                    || bounds.max.cmpgt(entry.bounds.max).any())
            {
                self.warned_expansion = true;
                eprintln!("Lighter: decoded BMD bounds exceeded placement estimate; manifest expanded without changing chunk ownership");
            }
            self.estimated -= 1;
            entry.decoded = true;
        }
        entry.bounds = bounds;
        let chunk = self.chunks.get_mut(&entry.owner).unwrap();
        // Do not drop the parent's former envelope: enlarging it is cheap and
        // never incorrectly culls a child. Exact per-asset bounds drive ranking.
        chunk.bounds = chunk.bounds.union(bounds);
    }

    pub fn chunks(&self) -> usize {
        self.chunks.len()
    }
    pub fn estimated(&self) -> usize {
        self.estimated
    }

    pub fn invalid_anchors(&self) -> usize {
        self.invalid
    }

    pub fn valid_anchor(&self, id: usize) -> bool {
        self.entries.get(id).is_some_and(Option::is_some)
    }

    pub fn cutoff_count(&self, cutoff: Option<[f32; 4]>) -> usize {
        if cutoff.is_none() {
            return 0;
        }
        self.entries
            .iter()
            .flatten()
            .filter(|entry| !crate::terrain_cutoff::contains(cutoff, entry.position))
            .count()
    }

    pub fn reachable(&self, id: usize, query: &Query) -> bool {
        self.entries[id]
            .as_ref()
            .and_then(|e| query.placement(e))
            .is_some()
    }

    pub fn select(&self, required: &HashSet<usize>, query: &Query) -> Vec<usize> {
        let mut ranked = Vec::new();
        for chunk in self.chunks.values() {
            if query.rank(chunk.bounds).is_none() {
                continue;
            }
            for &id in &chunk.entries {
                if required.contains(&id) {
                    continue;
                }
                if let Some(rank) = query.placement(self.entries[id].as_ref().unwrap()) {
                    ranked.push((rank, id));
                }
            }
        }
        ranked.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        ranked.into_iter().map(|(_, id)| id).collect()
    }
}

#[derive(Clone, Copy)]
struct Rect {
    min: Vec2,
    max: Vec2,
}

impl Rect {
    fn point(point: Vec2) -> Self {
        Self {
            min: point,
            max: point,
        }
    }
    fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
    fn distance_squared(self, other: Self) -> f32 {
        (self.min - other.max)
            .max(other.min - self.max)
            .max(Vec2::ZERO)
            .length_squared()
    }
}

struct LightLink {
    source: Rect,
    corridor: Rect,
    range_sq: f32,
}

pub(super) struct Query {
    camera: Rect,
    receiver: Rect,
    cutoff: Option<[f32; 4]>,
    lights: Vec<LightLink>,
}

impl Query {
    pub fn new(
        center: [f32; 2],
        local_reach: f32,
        cutoff: Option<[f32; 4]>,
        lights: &[SceneLight],
    ) -> Self {
        let center = Vec2::from_array(center);
        let extent = Vec2::splat(local_reach);
        let receiver = Rect {
            min: center - extent,
            max: center + extent,
        };
        let mut links: Vec<_> = lights
            .iter()
            .enumerate()
            .filter_map(|(i, l)| {
                if l.range <= 0. || !l.range.is_finite() {
                    return None;
                }
                let source = Rect::point(Vec3::from_array(l.position).xz());
                let range_sq = l.range * l.range;
                let distance = receiver.distance_squared(source);
                (distance <= range_sq).then(|| {
                    (
                        distance,
                        i,
                        LightLink {
                            source,
                            corridor: receiver.union(source),
                            range_sq,
                        },
                    )
                })
            })
            .collect();
        links.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        Self {
            camera: Rect::point(center),
            receiver,
            cutoff,
            lights: links
                .into_iter()
                .take(LIGHT_HINTS)
                .map(|(_, _, l)| l)
                .collect(),
        }
    }

    fn placement(&self, entry: &Entry) -> Option<f32> {
        // Match the native terrain-cutoff policy for placement anchors. This
        // is an intentional exclusion, not an archive failure.
        if !crate::terrain_cutoff::contains(self.cutoff, entry.position) {
            return None;
        }
        self.rank(entry.bounds)
    }

    fn rank(&self, bounds: Bounds) -> Option<f32> {
        let asset = Rect {
            min: bounds.min.xz(),
            max: bounds.max.xz(),
        };
        let secondary_sq = self.receiver.distance_squared(asset);
        // Broad, direction-independent shadow corridors. A horizontal
        // overlap only raises priority; it doesn't assert actual visibility.
        let link = self.lights.iter().any(|l| {
            l.corridor.distance_squared(asset) == 0.
                && l.source.distance_squared(asset) <= l.range_sq
        });
        if secondary_sq > SECONDARY_REACH * SECONDARY_REACH && !link {
            return None;
        }
        let distance = self.camera.distance_squared(asset).sqrt();
        let secondary = secondary_sq.sqrt();
        Some((distance * 0.6 + secondary * 0.4) * if link { 0.5 } else { 1. })
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
    fn cutoff_and_required_placements_are_not_readded_by_the_extension() {
        let manifest = Manifest::new(&[
            ornament(-160., 0.),
            ornament(160., 0.),
            ornament(0., 1.),
            ornament(0., 1000.),
            ornament(100., -10.),
            ornament(f32::NAN, 0.),
        ]);
        let required = HashSet::from([2]);
        let select =
            |center, cutoff| manifest.select(&required, &Query::new(center, 64., cutoff, &[]));
        assert_eq!(select([0.; 2], None), [4, 0, 1]);
        assert_eq!(select([0.; 2], Some([-100., 200., -20., 20.])), [4, 1]);
        assert!(select([-1600., 0.], None).is_empty());
    }

    #[test]
    fn mesh_bounds_persist_across_teleports_and_owner_cells_do_not_duplicate_geometry() {
        let mut manifest =
            Manifest::new(&[ornament(127., 0.), ornament(129., 0.), ornament(1400., 0.)]);
        let query = |x| Query::new([x, 0.], 64., None, &[]);
        assert_eq!(manifest.select(&HashSet::new(), &query(1400.)), [2]);
        assert!(!manifest.reachable(2, &query(0.)));
        // The decoded asset extends into two other owner cells. It still has
        // one packet owner, and can now be found during the return teleport.
        manifest.learned_bounds(
            2,
            Bounds {
                min: Vec3::new(128., 0., -4.),
                max: Vec3::new(1405., 12., 4.),
            },
        );
        for x in [127.5, 128.5, 0.] {
            let ids = manifest.select(&HashSet::new(), &query(x));
            assert_eq!(ids.len(), 3);
            assert_eq!(
                ids.into_iter().collect::<HashSet<_>>(),
                HashSet::from([0, 1, 2])
            );
        }
        assert_eq!(manifest.select(&HashSet::new(), &query(1400.)), [2]);
        assert_eq!(
            manifest.entries[2].as_ref().unwrap().owner,
            owner([1400., 0., 0.])
        );
        assert_eq!(manifest.chunks(), 3);
        assert_eq!(manifest.estimated(), 2);
    }

    #[test]
    fn a_light_corridor_can_outrank_nearer_secondary_casters() {
        let manifest =
            Manifest::new(&[ornament(0., -400.), ornament(500., 0.), ornament(1150., 0.)]);
        let select = |lights: &[SceneLight]| {
            manifest.select(&HashSet::new(), &Query::new([0.; 2], 80., None, lights))
        };
        assert_eq!(select(&[]), [0, 1]);
        let light = SceneLight {
            position: [1200., 25., 0.],
            range: 1220.,
            stored_attenuation: [1., 0., 0.],
            diffuse: [1.; 4],
            ambient: [0.; 4],
            bounds_center: [1200., 25., 0.],
            bounds_half_extent: [1220.; 3],
            bounds_min: [-20., -1195., -1220.],
            bounds_max: [2420., 1245., 1220.],
            flags: 0,
        };
        assert_eq!(select(&[light]), [1, 0, 2]);
    }
}
