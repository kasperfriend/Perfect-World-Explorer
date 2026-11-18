//! Native building and SpeedTree collision for Walker.
//!
//! `CELBrushBuilding` stores `CCDBrush` convex hulls after BMD geometry.
//! `CELTree::InitCDObjects` stores the same brushes in version-nine TBH files,
//! after a serialized convex-hull payload. The sweep below follows
//! `CHBasedCD::ClipBrush`: expand each plane by the player's AABB, track the
//! latest entering and earliest leaving fractions, and retain the entering
//! plane for sliding. Terrain is deliberately not represented here; Walker's
//! permanent terrain/support pass remains independent of this object option.

use super::physics::Ground;
use crate::{
    bmd::CollisionBrush,
    pck, scene,
    texture_resolution::{litmodel_basename_index, litmodel_entry_key, litmodel_key, normalize},
};
use glam::{Vec2, Vec3};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender},
};

const CELL_SIZE: f32 = 60.0;
const DIST_EPSILON: f32 = 0.03125;
const CHDATA_SKIP_COLLISION: u32 = 0x00000004;
const MAX_CELLS_PER_QUERY: usize = 4096;
const MAX_SLIDE_ITERATIONS: usize = 4;
/// Bound the streaming work one frame may absorb. The loader uses a bounded
/// channel, but inserting a batch of 32 hulls into the cell grids is main-thread
/// work; spreading it keeps a large world load from producing one long frame.
const MAX_BATCHES_PER_POLL: usize = 4;
/// Half-width of the box the name line-of-sight test sweeps along the view ray.
const SIGHT_PROBE: f32 = 0.02;

type Cell = (i32, i32);

fn cell(value: f32) -> i32 {
    (value / CELL_SIZE).floor() as i32
}

fn cell_count(min: Vec3, max: Vec3) -> Option<(i32, i32, i32, i32, usize)> {
    let x0 = cell(min.x);
    let x1 = cell(max.x);
    let z0 = cell(min.z);
    let z1 = cell(max.z);
    let width = i64::from(x1) - i64::from(x0) + 1;
    let depth = i64::from(z1) - i64::from(z0) + 1;
    if width <= 0 || depth <= 0 {
        return None;
    }
    let count = width.checked_mul(depth)? as usize;
    Some((x0, x1, z0, z1, count))
}

fn bounds(center: Vec3, extents: Vec3) -> (Vec3, Vec3) {
    (center - extents, center + extents)
}

fn overlaps(a_center: Vec3, a_extents: Vec3, b_center: Vec3, b_extents: Vec3) -> bool {
    (a_center.x - b_center.x).abs() <= a_extents.x + b_extents.x
        && (a_center.y - b_center.y).abs() <= a_extents.y + b_extents.y
        && (a_center.z - b_center.z).abs() <= a_extents.z + b_extents.z
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trace {
    pub fraction: f32,
    pub normal: Vec3,
    pub start_solid: bool,
    pub all_solid: bool,
}

/// Exact plane sweep used by native `CCDBrush::Trace` for a non-point AABB.
pub fn trace_brush(
    brush: &CollisionBrush,
    start: Vec3,
    delta: Vec3,
    extents: Vec3,
) -> Option<Trace> {
    let end = start + delta;
    let mut enter = -1.0f32;
    let mut leave = 1.0f32;
    let mut clip = None;
    let mut start_out = false;
    let mut get_out = false;

    for side in &brush.sides {
        let normal = Vec3::from_array(side.normal);
        let offset = Vec3::new(
            if normal.x < 0.0 {
                extents.x
            } else {
                -extents.x
            },
            if normal.y < 0.0 {
                extents.y
            } else {
                -extents.y
            },
            if normal.z < 0.0 {
                extents.z
            } else {
                -extents.z
            },
        );
        let distance = side.distance - offset.dot(normal);
        let d1 = start.dot(normal) - distance;
        let d2 = end.dot(normal) - distance;
        get_out |= d2 > 0.0;
        start_out |= d1 > 0.0;

        if d1 > 0.0 && d2 >= d1 {
            return None;
        }
        if d1 <= 0.0 && d2 <= 0.0 {
            continue;
        }
        if d1 > d2 {
            let fraction = ((d1 - DIST_EPSILON) / (d1 - d2)).max(0.0);
            if fraction > enter {
                enter = fraction;
                clip = Some(normal);
            }
        } else {
            let fraction = ((d1 + DIST_EPSILON) / (d1 - d2)).min(1.0);
            leave = leave.min(fraction);
        }
    }

    if !start_out {
        return Some(Trace {
            fraction: 0.0,
            normal: Vec3::ZERO,
            start_solid: true,
            all_solid: !get_out,
        });
    }
    if enter < leave && enter > -1.0 && enter < 1.0 {
        return Some(Trace {
            fraction: enter.max(0.0),
            normal: clip.unwrap_or(Vec3::ZERO),
            start_solid: false,
            all_solid: false,
        });
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedMove {
    pub feet: Vec3,
    pub blocked: bool,
    pub normal: Vec3,
}

#[derive(Debug)]
struct TreeInstance {
    tree_type: usize,
    position: Vec3,
    center: Vec3,
    extents: Vec3,
}

/// The upward-facing top of one world model, kept for the models that ship no
/// collision hull (props, stalls, wagons, low platforms). A placement that the
/// hull set cannot support still stands on the model it was authored onto
/// instead of on the terrain under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PropDeck {
    /// XZ centre of the model's upward-facing surface.
    pub center: [f32; 2],
    /// XZ half-extents of that surface.
    pub extents: [f32; 2],
    /// Height of the surface.
    pub top: f32,
}

#[derive(Debug, Default)]
struct CollisionWorld {
    buildings: Vec<CollisionBrush>,
    building_grid: HashMap<Cell, Vec<usize>>,
    large_buildings: Vec<usize>,
    decks: Vec<PropDeck>,
    deck_grid: HashMap<Cell, Vec<usize>>,
    tree_types: Vec<Option<Vec<CollisionBrush>>>,
    trees: Vec<TreeInstance>,
    tree_grid: HashMap<Cell, Vec<usize>>,
    large_trees: Vec<usize>,
}

impl CollisionWorld {
    fn add_buildings(&mut self, brushes: Vec<CollisionBrush>) {
        for brush in brushes {
            let index = self.buildings.len();
            let center = Vec3::from_array(brush.center);
            let extents = Vec3::from_array(brush.extents);
            let (min, max) = bounds(center, extents);
            self.buildings.push(brush);
            if let Some((x0, x1, z0, z1, count)) = cell_count(min, max) {
                if count <= MAX_CELLS_PER_QUERY {
                    for x in x0..=x1 {
                        for z in z0..=z1 {
                            self.building_grid.entry((x, z)).or_default().push(index);
                        }
                    }
                } else {
                    self.large_buildings.push(index);
                }
            }
        }
    }

    fn add_decks(&mut self, decks: Vec<PropDeck>) {
        for deck in decks {
            let index = self.decks.len();
            let min = Vec3::new(
                deck.center[0] - deck.extents[0],
                deck.top,
                deck.center[1] - deck.extents[1],
            );
            let max = Vec3::new(
                deck.center[0] + deck.extents[0],
                deck.top,
                deck.center[1] + deck.extents[1],
            );
            self.decks.push(deck);
            if let Some((x0, x1, z0, z1, count)) = cell_count(min, max) {
                if count <= MAX_CELLS_PER_QUERY {
                    for x in x0..=x1 {
                        for z in z0..=z1 {
                            self.deck_grid.entry((x, z)).or_default().push(index);
                        }
                    }
                }
            }
        }
    }

    /// Lowest model deck under `x`/`z` whose top lies inside `min_y..=max_y`.
    fn deck_top(&self, x: f32, z: f32, min_y: f32, max_y: f32) -> Option<f32> {
        let ids = self.deck_grid.get(&(cell(x), cell(z)))?;
        let mut best: Option<f32> = None;
        for &index in ids {
            let deck = &self.decks[index];
            if (x - deck.center[0]).abs() > deck.extents[0]
                || (z - deck.center[1]).abs() > deck.extents[1]
            {
                continue;
            }
            if deck.top < min_y || deck.top > max_y {
                continue;
            }
            best = Some(best.map_or(deck.top, |best: f32| best.min(deck.top)));
        }
        best
    }

    fn add_tree_type(&mut self, tree_type: usize, brushes: Vec<CollisionBrush>) {
        if self.tree_types.len() <= tree_type {
            self.tree_types.resize_with(tree_type + 1, || None);
        }
        self.tree_types[tree_type] = Some(brushes);
    }

    fn add_tree(&mut self, tree_type: usize, position: Vec3) {
        let Some(brushes) = self
            .tree_types
            .get(tree_type)
            .and_then(Option::as_ref)
            .filter(|brushes| !brushes.is_empty())
        else {
            return;
        };
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for brush in brushes {
            let center = Vec3::from_array(brush.center);
            let extents = Vec3::from_array(brush.extents);
            min = min.min(center - extents);
            max = max.max(center + extents);
        }
        let center = (min + max) * 0.5 + position;
        let extents = (max - min) * 0.5;
        let index = self.trees.len();
        let (world_min, world_max) = bounds(center, extents);
        self.trees.push(TreeInstance {
            tree_type,
            position,
            center,
            extents,
        });
        if let Some((x0, x1, z0, z1, count)) = cell_count(world_min, world_max) {
            if count <= MAX_CELLS_PER_QUERY {
                for x in x0..=x1 {
                    for z in z0..=z1 {
                        self.tree_grid.entry((x, z)).or_default().push(index);
                    }
                }
            } else {
                self.large_trees.push(index);
            }
        }
    }

    /// Candidate hull indices for a sweep and the query AABB used to reject
    /// distant brushes. A query that would span more than
    /// [`MAX_CELLS_PER_QUERY`] cells falls back to the whole world, matching the
    /// original per-sweep behaviour without rebuilding the id lists twice.
    fn candidates(
        &self,
        start: Vec3,
        delta: Vec3,
        extents: Vec3,
        include_trees: bool,
    ) -> (Vec<usize>, Vec<usize>, Vec3, Vec3) {
        let end = start + delta;
        let min = start.min(end) - extents;
        let max = start.max(end) + extents;
        let query_center = (min + max) * 0.5;
        let query_extents = (max - min) * 0.5;
        let mut building_ids = self.large_buildings.clone();
        let mut tree_ids = if include_trees {
            self.large_trees.clone()
        } else {
            Vec::new()
        };
        let mut cover_all = true;
        if let Some((x0, x1, z0, z1, count)) = cell_count(min, max) {
            if count <= MAX_CELLS_PER_QUERY {
                cover_all = false;
                for x in x0..=x1 {
                    for z in z0..=z1 {
                        if let Some(ids) = self.building_grid.get(&(x, z)) {
                            building_ids.extend_from_slice(ids);
                        }
                        if include_trees {
                            if let Some(ids) = self.tree_grid.get(&(x, z)) {
                                tree_ids.extend_from_slice(ids);
                            }
                        }
                    }
                }
            }
        }
        if cover_all {
            building_ids.extend(0..self.buildings.len());
            if include_trees {
                tree_ids.extend(0..self.trees.len());
            }
        }
        building_ids.sort_unstable();
        building_ids.dedup();
        if include_trees {
            tree_ids.sort_unstable();
            tree_ids.dedup();
        }
        (building_ids, tree_ids, query_center, query_extents)
    }

    fn trace(&self, start: Vec3, delta: Vec3, extents: Vec3) -> Option<Trace> {
        let (building_ids, tree_ids, query_center, query_extents) =
            self.candidates(start, delta, extents, true);
        let mut best: Option<Trace> = None;
        let mut retain = |candidate: Trace| {
            if best.is_none_or(|old| candidate.fraction < old.fraction) {
                best = Some(candidate);
            }
        };
        for index in building_ids {
            let brush = &self.buildings[index];
            if brush.flags & CHDATA_SKIP_COLLISION != 0 {
                continue;
            }
            let center = Vec3::from_array(brush.center);
            let brush_extents = Vec3::from_array(brush.extents);
            if !overlaps(query_center, query_extents, center, brush_extents) {
                continue;
            }
            if let Some(hit) = trace_brush(brush, start, delta, extents) {
                retain(hit);
            }
        }
        for index in tree_ids {
            let tree = &self.trees[index];
            if !overlaps(query_center, query_extents, tree.center, tree.extents) {
                continue;
            }
            let Some(brushes) = self.tree_types[tree.tree_type].as_ref() else {
                continue;
            };
            // Native CELTree subtracts the instance position before tracing
            // its shared brushes. Tree hull flags are not filtered there.
            let local_start = start - tree.position;
            for brush in brushes {
                if let Some(hit) = trace_brush(brush, local_start, delta, extents) {
                    retain(hit);
                }
            }
        }
        best
    }

    /// The lowest walkable *building* surface at or above `feet`, searching
    /// upward. [`Self::support`] answers "what is under this point", which picks
    /// a roof or a ceiling when an actor is authored onto a bridge deck or an
    /// interior floor above the terrain sample. The client's own NPC placements
    /// carry no height at all (`task_npc.data` stores 0 for "the server's
    /// height"), so the placement whoever authored them stands on is the first
    /// floor plate above the bare terrain in that column.
    ///
    /// Tree hulls are deliberately excluded: a quest NPC does not stand on a
    /// canopy, and their hulls are the only other brush source here.
    fn floor_above(&self, feet: Vec3, extents: Vec3, max_up: f32) -> Option<Ground> {
        if !max_up.is_finite() || max_up < 0.0 {
            return None;
        }
        // Probe upward in short steps and accept the first plane a downward
        // sweep reaches, so a bridge deck wins over the river bed below it and
        // an interior floor wins over the terrain poking through it.
        const STEP: f32 = 0.25;
        const CONTACT_LIFT: f32 = 0.01;
        let delta = Vec3::NEG_Y * (STEP + CONTACT_LIFT * 2.0);
        let mut lift = 0.0;
        while lift <= max_up {
            let start = feet + Vec3::Y * (lift + extents.y + CONTACT_LIFT);
            if let Some(hit) = self.trace_buildings(start, delta, extents) {
                if !hit.start_solid && hit.normal.y >= 0.5 {
                    let center = start + delta * hit.fraction;
                    return Some(Ground {
                        height: center.y - extents.y,
                        normal: hit.normal,
                    });
                }
            }
            lift += STEP;
        }
        // No hull at all: many props (stalls, wagons, low platforms) ship only
        // render geometry, so their authored top surface is the support.
        self.deck_top(feet.x, feet.z, feet.y, feet.y + max_up)
            .map(|top| Ground {
                height: top,
                normal: Vec3::Y,
            })
    }

    /// [`Self::trace`] restricted to BMD building brushes.
    fn trace_buildings(&self, start: Vec3, delta: Vec3, extents: Vec3) -> Option<Trace> {
        let (building_ids, _, query_center, query_extents) =
            self.candidates(start, delta, extents, false);
        let mut best: Option<Trace> = None;
        for index in building_ids {
            let brush = &self.buildings[index];
            if brush.flags & CHDATA_SKIP_COLLISION != 0 {
                continue;
            }
            if !overlaps(
                query_center,
                query_extents,
                Vec3::from_array(brush.center),
                Vec3::from_array(brush.extents),
            ) {
                continue;
            }
            if let Some(hit) = trace_brush(brush, start, delta, extents) {
                if best.is_none_or(|old| hit.fraction < old.fraction) {
                    best = Some(hit);
                }
            }
        }
        best
    }

    /// Down trace that only accepts an upward-facing building hull. Tree hulls
    /// are scenery, never a floor, and a brush that already contains the player
    /// AABB reports `start_solid` (fraction 0); letting either win made the
    /// whole support query fail and dropped the body off the real floor.
    fn trace_support(&self, start: Vec3, delta: Vec3, extents: Vec3) -> Option<Trace> {
        let (building_ids, _, query_center, query_extents) =
            self.candidates(start, delta, extents, false);
        let mut best: Option<Trace> = None;
        for index in building_ids {
            let brush = &self.buildings[index];
            if brush.flags & CHDATA_SKIP_COLLISION != 0 {
                continue;
            }
            if !overlaps(
                query_center,
                query_extents,
                Vec3::from_array(brush.center),
                Vec3::from_array(brush.extents),
            ) {
                continue;
            }
            let Some(hit) = trace_brush(brush, start, delta, extents) else {
                continue;
            };
            if hit.start_solid || hit.normal.y < 0.5 {
                continue;
            }
            if best.is_none_or(|old| hit.fraction < old.fraction) {
                best = Some(hit);
            }
        }
        best
    }

    fn support(&self, feet: Vec3, extents: Vec3, max_down: f32) -> Option<Ground> {
        if !max_down.is_finite() || max_down < 0.0 {
            return None;
        }
        // RetrieveSupportPlane traces the native player AABB downward. Start
        // slightly above the supplied feet position so an exact contact from
        // our fixed-step integrator is not misclassified as `start_solid`.
        // The lift is included in the trace distance, not in the result.
        const CONTACT_LIFT: f32 = 0.01;
        let start = feet + Vec3::Y * (extents.y + CONTACT_LIFT);
        let delta = Vec3::NEG_Y * (max_down + CONTACT_LIFT);
        let hit = self.trace_support(start, delta, extents)?;
        let center = start + delta * hit.fraction;
        Some(Ground {
            // A down trace must never raise the body. When a sweep begins
            // inside DIST_EPSILON of the plane its fraction is zero and the raw
            // height is CONTACT_LIFT above the feet; that used to climb a
            // centimetre per frame and jitter the landing animation.
            height: (center.y - extents.y).min(feet.y),
            normal: hit.normal,
        })
    }

    fn resolve(&self, old_feet: Vec3, desired_feet: Vec3, extents: Vec3) -> ResolvedMove {
        if old_feet == desired_feet {
            // A stationary body cannot sweep into anything. Skipping the hull
            // query also avoids a spurious `start_solid` hit on the very plane
            // the fixed step just settled the feet onto.
            return ResolvedMove {
                feet: old_feet,
                blocked: false,
                normal: Vec3::ZERO,
            };
        }
        let mut center = old_feet + Vec3::Y * extents.y;
        let mut remaining = desired_feet - old_feet;
        let mut blocked = false;
        let mut last_normal = Vec3::ZERO;
        for _ in 0..MAX_SLIDE_ITERATIONS {
            if remaining.length_squared() < 1e-12 {
                break;
            }
            let Some(hit) = self.trace(center, remaining, extents) else {
                center += remaining;
                break;
            };
            blocked = true;
            if hit.start_solid {
                // Match the native air-move escape behavior: a sweep that
                // begins inside a hull must not permanently trap a player who
                // spawned or loaded inside scenery. Subsequent sweeps from
                // outside are stopped normally.
                center += remaining;
                break;
            }
            center += remaining * hit.fraction;
            last_normal = hit.normal;
            remaining *= 1.0 - hit.fraction;
            let into_plane = remaining.dot(hit.normal);
            if into_plane < 0.0 {
                remaining -= hit.normal * into_plane;
            }
        }
        ResolvedMove {
            feet: center - Vec3::Y * extents.y,
            blocked,
            normal: last_normal,
        }
    }
}

/// The Populator resolves authored placement heights against the same building
/// hulls Walker walks on, so a quest NPC authored onto a bridge deck or an
/// interior floor stands there instead of on the terrain under the geometry.
impl crate::modules::populator::FloorSource for Collision {
    fn lowest_floor(&self, feet: Vec3, extents: Vec3, max_up: f32) -> Option<f32> {
        self.floor_above(feet, extents, max_up)
            .map(|ground| ground.height)
    }

    fn floor_stamp(&self) -> u64 {
        // Every streamed batch grows both the hull and the model-deck sets; the
        // completion flag covers the final batch and a loader that produced
        // nothing.
        (self.buildings() as u64 + self.decks() as u64) * 2 + u64::from(self.building_done)
    }

    fn sight_blocked(&self, from: Vec3, to: Vec3) -> bool {
        Collision::sight_blocked(self, from, to)
    }

    fn solid_at(&self, feet: Vec3, extents: Vec3) -> bool {
        Collision::solid_at(self, feet, extents)
    }
}

/// The fixed-step physics integrator asks for the same hull tops that the
/// swept wall pass uses. Terrain remains the permanent floor in `physics`.
impl super::physics::ObjectSupport for Collision {
    fn object_support(&self, feet: Vec3, extents: Vec3, max_down: f32) -> Option<Ground> {
        Collision::support(self, feet, extents, max_down)
    }
}

#[derive(Default, Debug)]
struct TreeReport {
    loaded_types: usize,
    missing: usize,
    failed: usize,
    errors: Vec<String>,
}

fn read_u32(data: &[u8], p: &mut usize) -> Result<u32, String> {
    let end = p.checked_add(4).ok_or("TBH offset overflow")?;
    let value = u32::from_le_bytes(
        data.get(*p..end)
            .ok_or("truncated TBH")?
            .try_into()
            .unwrap(),
    );
    *p = end;
    Ok(value)
}

fn skip_hull_payload(data: &[u8], p: &mut usize) -> Result<(), String> {
    const V02: &[u8] = b"V0.2======Convex_Hull_Data_Start=========";
    const V01: &[u8] = b"V0.1======Convex_Hull_Data_Start=========";
    const V00: &[u8] = b"==========Convex_Hull_Data_Start=========";
    let end = p.checked_add(V02.len()).ok_or("TBH header overflow")?;
    let header = data.get(*p..end).ok_or("truncated TBH hull header")?;
    *p = end;
    if header == V02 || header == V01 {
        let _flags = read_u32(data, p)?;
    } else if header != V00 {
        return Err("unknown TBH convex-hull header".into());
    }
    let size = read_u32(data, p)? as usize;
    if size > 64 * 1024 * 1024 {
        return Err(format!("TBH hull payload is too large: {size}"));
    }
    *p = p.checked_add(size).ok_or("TBH payload overflow")?;
    if *p > data.len() {
        return Err("truncated TBH hull payload".into());
    }
    Ok(())
}

fn parse_tbh(data: &[u8]) -> Result<Vec<CollisionBrush>, String> {
    // AFileImage consumes the container marker before CELTree::InitCDObjects
    // reads version nine. Every authentic PW142 TBH carries this prefix.
    let data = data.strip_prefix(b"MOXB").unwrap_or(data);
    let mut p = 0;
    let version = read_u32(data, &mut p)?;
    if version != 9 {
        return Err(format!(
            "unsupported TBH version {version}; native requires 9"
        ));
    }
    let count = read_u32(data, &mut p)? as usize;
    if count > 4096 {
        return Err(format!("TBH collision-object limit exceeded: {count}"));
    }
    let mut brushes = Vec::with_capacity(count);
    for _ in 0..count {
        skip_hull_payload(data, &mut p)?;
        brushes.push(CollisionBrush::read(data, &mut p)?);
    }
    if p != data.len() {
        return Err(format!("{} trailing TBH bytes", data.len() - p));
    }
    Ok(brushes)
}

fn tree_brushes(world: &mut CollisionWorld, element: &PathBuf, scene: &scene::Scene) -> TreeReport {
    let mut report = TreeReport::default();
    let result = (|| -> Result<(File, HashMap<String, pck::Entry>), String> {
        let mut file = File::open(element.join("trees.pck")).map_err(|e| e.to_string())?;
        let entries = pck::directory(&mut file)?
            .into_iter()
            .map(|entry| (entry.name.to_lowercase(), entry))
            .collect();
        Ok((file, entries))
    })();
    let (mut file, entries) = match result {
        Ok(value) => value,
        Err(error) => {
            report.failed = scene.tree_types.len();
            report.errors.push(format!("trees.pck: {error}"));
            return report;
        }
    };
    for (index, tree) in scene.tree_types.iter().enumerate() {
        let key = tree.tree.to_lowercase().replace('\\', "/");
        let Some(stem) = key.strip_suffix(".spt") else {
            report.failed += 1;
            report
                .errors
                .push(format!("{}: tree path is not SPT", tree.tree));
            continue;
        };
        let path = format!("{stem}.tbh");
        let Some(entry) = entries.get(&path) else {
            // CELTree::InitCDObjects treats a missing TBH as non-fatal.
            report.missing += 1;
            continue;
        };
        let result = pck::read_entry(&mut file, entry).and_then(|data| parse_tbh(&data));
        match result {
            Ok(brushes) => {
                world.add_tree_type(index, brushes);
                report.loaded_types += 1;
            }
            Err(error) => {
                report.failed += 1;
                if report.errors.len() < 4 {
                    report.errors.push(format!("{path}: {error}"));
                }
            }
        }
    }
    report
}

enum BuildingMessage {
    Batch {
        processed: usize,
        brushes: Vec<CollisionBrush>,
        decks: Vec<PropDeck>,
    },
    Done {
        failed: usize,
    },
    Fatal(String),
}

/// Largest half-extent a model may span and still count as a prop: buildings,
/// walls and terrain shells are wider and keep relying on their collision hulls.
const MAX_DECK_HALF_EXTENT: f32 = 12.0;
/// Tallest model that may provide a standing surface. Pillars, statues and trees
/// stay out, so nothing is lifted onto them by accident.
const MAX_DECK_HEIGHT: f32 = 8.0;
/// Edge of one deck tile. Upward-facing geometry is bucketed into tiles this
/// wide, which keeps a staircase, a ramp or a stepped platform usable: each tile
/// carries its own height instead of the model's highest point.
const DECK_TILE: f32 = 2.0;

/// Upward-facing standing surfaces of a world-baked lit model.
///
/// Lit-model vertices are already in world space, so no transform is needed.
/// Only faces that point up (`normal.y > 0.5`, the same threshold the hull trace
/// uses) contribute, which keeps a wagon's cargo or a stall's canopy from raising
/// the surface an actor actually stands on. The up-facing geometry is bucketed
/// into [`DECK_TILE`]-wide tiles, so the surface a placement gets is the one
/// directly under it rather than the tallest point of the whole model.
fn model_decks(model: &crate::bmd::Model) -> Vec<PropDeck> {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for mesh in &model.meshes {
        for vertex in &mesh.vertices {
            let position = Vec3::from_array(vertex.position);
            min = min.min(position);
            max = max.max(position);
        }
    }
    if max.y - min.y > MAX_DECK_HEIGHT {
        return Vec::new();
    }
    // Collect the up-facing triangles and their own XZ bounds first, so the tile
    // grid covers only the surface that can carry an actor.
    let mut faces = Vec::new();
    let mut area_min = Vec2::splat(f32::MAX);
    let mut area_max = Vec2::splat(f32::MIN);
    for mesh in &model.meshes {
        for triangle in mesh.indices.chunks_exact(3) {
            let Some(a) = mesh.vertices.get(triangle[0] as usize) else {
                continue;
            };
            let Some(b) = mesh.vertices.get(triangle[1] as usize) else {
                continue;
            };
            let Some(c) = mesh.vertices.get(triangle[2] as usize) else {
                continue;
            };
            let a = Vec3::from_array(a.position);
            let b = Vec3::from_array(b.position);
            let c = Vec3::from_array(c.position);
            let normal = (b - a).cross(c - a);
            if normal.length_squared() < 1e-8 || normal.normalize().y <= 0.5 {
                continue;
            }
            for point in [a, b, c] {
                area_min = area_min.min(Vec2::new(point.x, point.z));
                area_max = area_max.max(Vec2::new(point.x, point.z));
            }
            let centroid = Vec2::new(a.x + b.x + c.x, a.z + b.z + c.z) / 3.0;
            let top = a.y.max(b.y).max(c.y);
            faces.push((centroid, top));
        }
    }
    if faces.is_empty() {
        return Vec::new();
    }
    let extents = (area_max - area_min) * 0.5;
    if extents.x > MAX_DECK_HALF_EXTENT || extents.y > MAX_DECK_HALF_EXTENT {
        return Vec::new();
    }
    let columns = ((area_max.x - area_min.x) / DECK_TILE).ceil().max(1.0) as usize;
    let rows = ((area_max.y - area_min.y) / DECK_TILE).ceil().max(1.0) as usize;
    let mut tops = vec![f32::MIN; columns * rows];
    for (centroid, top) in faces {
        let column = (((centroid.x - area_min.x) / DECK_TILE) as usize).min(columns - 1);
        let row = (((centroid.y - area_min.y) / DECK_TILE) as usize).min(rows - 1);
        let slot = &mut tops[row * columns + column];
        *slot = slot.max(top);
    }
    let mut decks = Vec::new();
    for row in 0..rows {
        for column in 0..columns {
            let top = tops[row * columns + column];
            if top == f32::MIN {
                continue;
            }
            let x0 = area_min.x + column as f32 * DECK_TILE;
            let z0 = area_min.y + row as f32 * DECK_TILE;
            let x1 = (x0 + DECK_TILE).min(area_max.x);
            let z1 = (z0 + DECK_TILE).min(area_max.y);
            if x1 - x0 < 0.25 || z1 - z0 < 0.25 {
                continue;
            }
            decks.push(PropDeck {
                center: [(x0 + x1) * 0.5, (z0 + z1) * 0.5],
                extents: [(x1 - x0) * 0.5, (z1 - z0) * 0.5],
                top,
            });
        }
    }
    decks
}

fn building_worker(
    element: PathBuf,
    map_id: String,
    paths: Vec<String>,
    tx: SyncSender<BuildingMessage>,
) {
    let result = (|| -> Result<(), String> {
        let mut file = File::open(element.join("litmodels.pck")).map_err(|e| e.to_string())?;
        let entries: HashMap<_, _> = pck::directory(&mut file)?
            .into_iter()
            .map(|entry| (normalize(&entry.name), entry))
            .collect();
        let entry_names = entries.keys().cloned().collect::<Vec<_>>();
        let by_basename = litmodel_basename_index(&entry_names);
        let mut failed = 0;
        for start in (0..paths.len()).step_by(32) {
            let end = (start + 32).min(paths.len());
            let mut brushes = Vec::new();
            let mut decks = Vec::new();
            for path in &paths[start..end] {
                // EC-BSD paths are relative; retail archives partition scenery
                // under `litmodels/<map-id>/`, while older clients use the
                // legacy `litmodels/world/` root. Resolve within the selected
                // map first so another instance's same-named model is not used.
                let canonical = litmodel_key(path);
                let result = litmodel_entry_key(&canonical, &map_id, &entry_names, &by_basename)
                    .and_then(|key| {
                        let entry = entries
                            .get(&key)
                            .ok_or_else(|| format!("resolver/index mismatch for {key}"))?;
                        pck::read_entry(&mut file, entry)
                    })
                    .and_then(|data| {
                        crate::bmd::Model::parse(&data)
                            .map_err(|error| format!("{canonical}: {error}"))
                    });
                match result {
                    Ok(model) => {
                        decks.extend(model_decks(&model));
                        brushes.extend(model.collision);
                    }
                    Err(error) => {
                        failed += 1;
                        eprintln!("Walker object collision: {error}");
                    }
                }
            }
            if tx
                .send(BuildingMessage::Batch {
                    processed: end,
                    brushes,
                    decks,
                })
                .is_err()
            {
                return Ok(());
            }
        }
        let _ = tx.send(BuildingMessage::Done { failed });
        Ok(())
    })();
    if let Err(error) = result {
        let _ = tx.send(BuildingMessage::Fatal(error));
    }
}

/// Walker-owned object collision and its independent, lightweight BMD loader.
/// It does not depend on the whole-world texture/geometry preload setting.
pub struct Collision {
    element: Option<PathBuf>,
    building_paths: Vec<String>,
    building_map_id: String,
    receiver: Option<Receiver<BuildingMessage>>,
    world: CollisionWorld,
    tree_report: TreeReport,
    building_processed: usize,
    building_failed: usize,
    building_done: bool,
    fatal_error: Option<String>,
}

impl Default for Collision {
    fn default() -> Self {
        Self {
            element: None,
            building_paths: Vec::new(),
            building_map_id: "world".into(),
            receiver: None,
            world: CollisionWorld::default(),
            tree_report: TreeReport::default(),
            building_processed: 0,
            building_failed: 0,
            building_done: false,
            fatal_error: None,
        }
    }
}

impl Collision {
    pub fn new(element: PathBuf, scene: &scene::Scene, ornaments: &[scene::Ornament]) -> Self {
        let mut world = CollisionWorld::default();
        let tree_report = tree_brushes(&mut world, &element, scene);
        for block in &scene.blocks {
            for tree in &block.trees {
                world.add_tree(tree.tree_type as usize, Vec3::from_array(tree.position));
            }
        }
        let mut seen = HashSet::new();
        let building_paths = ornaments
            .iter()
            .map(|ornament| normalize(&ornament.path))
            .filter(|path| seen.insert(path.clone()))
            .collect();
        let building_map_id = ornaments
            .first()
            .map(|ornament| ornament.map_id.clone())
            .unwrap_or_else(|| "world".into());
        Self {
            element: Some(element),
            building_paths,
            building_map_id,
            world,
            tree_report,
            ..Self::default()
        }
    }

    pub fn ensure_loading(&mut self) {
        if self.receiver.is_some()
            || self.building_done
            || self.building_paths.is_empty()
            || self.fatal_error.is_some()
        {
            return;
        }
        let Some(element) = self.element.clone() else {
            return;
        };
        let (tx, rx) = mpsc::sync_channel(2);
        let paths = self.building_paths.clone();
        let map_id = self.building_map_id.clone();
        if std::thread::Builder::new()
            .name("walker-object-collision".into())
            .spawn(move || building_worker(element, map_id, paths, tx))
            .is_ok()
        {
            self.receiver = Some(rx);
        } else {
            self.fatal_error = Some("could not start object-collision loader".into());
        }
    }

    /// True while the loader still has batches to hand to the main thread.
    pub fn is_loading(&self) -> bool {
        self.receiver.is_some()
    }

    pub fn poll(&mut self) {
        let mut finished = false;
        let mut batches = 0;
        loop {
            if batches >= MAX_BATCHES_PER_POLL {
                break;
            }
            let message = match self.receiver.as_ref() {
                Some(receiver) => receiver.try_recv(),
                None => break,
            };
            match message {
                Ok(BuildingMessage::Batch {
                    processed,
                    brushes,
                    decks,
                }) => {
                    batches += 1;
                    self.building_processed = processed;
                    self.world.add_buildings(brushes);
                    self.world.add_decks(decks);
                }
                Ok(BuildingMessage::Done { failed }) => {
                    self.building_failed = failed;
                    self.building_done = true;
                    finished = true;
                    break;
                }
                Ok(BuildingMessage::Fatal(error)) => {
                    self.fatal_error = Some(error);
                    self.building_done = true;
                    finished = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.building_done = true;
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.receiver = None;
        }
    }

    pub fn resolve(&self, old_feet: Vec3, desired_feet: Vec3, extents: Vec3) -> ResolvedMove {
        self.world.resolve(old_feet, desired_feet, extents)
    }

    pub fn support(&self, feet: Vec3, extents: Vec3, max_down: f32) -> Option<Ground> {
        self.world.support(feet, extents, max_down)
    }

    /// The lowest walkable building floor at or above `feet`; see
    /// [`CollisionWorld::floor_above`].
    pub fn floor_above(&self, feet: Vec3, extents: Vec3, max_up: f32) -> Option<Ground> {
        self.world.floor_above(feet, extents, max_up)
    }

    /// Number of building hulls already streamed in, so a consumer can tell
    /// when a placement resolved against an incomplete world should be retried.
    pub fn buildings(&self) -> usize {
        self.world.buildings.len()
    }

    /// Model top surfaces that back placements no collision hull supports.
    pub fn decks(&self) -> usize {
        self.world.decks.len()
    }

    /// True when something solid stands between `from` (a camera) and `to` (an
    /// overhead name): the client hides a name it cannot see, so the label must
    /// never show through a building, a tree trunk or the terrain.
    /// True when an `extents`-sized box standing on `feet` already overlaps a
    /// building hull: the placement is inside a wall, a counter or a bridge
    /// arch. This is a zero-length sweep, so it answers "is this box inside the
    /// brush right now" and needs no motion. Tree hulls are excluded on purpose:
    /// standing under a canopy is not standing in a wall.
    pub fn solid_at(&self, feet: Vec3, extents: Vec3) -> bool {
        self.world
            .trace_buildings(feet + Vec3::Y * extents.y, Vec3::ZERO, extents)
            .is_some_and(|hit| hit.start_solid)
    }

    pub fn sight_blocked(&self, from: Vec3, to: Vec3) -> bool {
        let delta = to - from;
        if delta.length_squared() <= 1e-6 {
            return false;
        }
        match self.world.trace(from, delta, Vec3::splat(SIGHT_PROBE)) {
            Some(hit) => !hit.start_solid && hit.fraction < 1.0,
            None => false,
        }
    }

    pub fn status(&self) -> String {
        let loading = if self.building_paths.is_empty() {
            "no BMD placements".to_owned()
        } else if self.building_done {
            format!("{} BMD assets", self.building_processed)
        } else if self.receiver.is_some() {
            format!(
                "loading BMD {}/{}",
                self.building_processed,
                self.building_paths.len()
            )
        } else {
            "BMD loading starts with Walker".to_owned()
        };
        format!(
            "{} building hulls • {} model tops • {} tree instances ({}/{} tree hull types) • {} • terrain always on",
            self.world.buildings.len(),
            self.world.decks.len(),
            self.world.trees.len(),
            self.tree_report.loaded_types,
            self.tree_report.loaded_types + self.tree_report.missing + self.tree_report.failed,
            loading
        )
    }

    pub fn diagnostic(&self) -> Option<String> {
        if let Some(error) = &self.fatal_error {
            return Some(format!("Object collision loader: {error}"));
        }
        let mut details = Vec::new();
        if self.building_failed > 0 {
            details.push(format!("{} BMD assets failed", self.building_failed));
        }
        if self.tree_report.failed > 0 {
            details.push(format!(
                "{} tree hull types failed",
                self.tree_report.failed
            ));
        }
        if self.tree_report.missing > 0 {
            details.push(format!(
                "{} tree types have no TBH (native non-fatal path)",
                self.tree_report.missing
            ));
        }
        details.extend(self.tree_report.errors.iter().take(2).cloned());
        if details.is_empty() {
            None
        } else {
            Some(details.join(" • "))
        }
    }

    #[cfg(test)]
    fn from_parts(buildings: Vec<CollisionBrush>) -> Self {
        Self::from_parts_and_decks(buildings, Vec::new())
    }

    #[cfg(test)]
    fn from_parts_and_decks(buildings: Vec<CollisionBrush>, decks: Vec<PropDeck>) -> Self {
        let mut world = CollisionWorld::default();
        world.add_buildings(buildings);
        world.add_decks(decks);
        Self {
            world,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bmd::BrushSide, modules::walker::physics};

    const MASK: u32 = 0xa8937462;
    const CHECK: u32 = 0x59374231;
    fn put_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    /// A minimal uncompressed v2 PCK. Only its directory matters to the
    /// scenery-path recovery test; payloads need not be valid BMDs when the
    /// selected map namespace rejects a cross-map match.
    fn write_pck(path: &std::path::Path, entries: &[(&str, &[u8])]) {
        let mut bytes = Vec::new();
        put_u32(&mut bytes, 0x4dca23ef);
        put_u32(&mut bytes, 0);
        put_u32(&mut bytes, 0x56a089b7);
        let mut records = Vec::new();
        for (name, payload) in entries {
            records.push((bytes.len() as u32, *name, payload.len() as u32));
            bytes.extend_from_slice(payload);
        }
        let directory_at = bytes.len() as u32;
        for (offset, name, size) in records {
            let mut record = vec![0u8; 276];
            assert!(name.len() <= 260);
            record[..name.len()].copy_from_slice(name.as_bytes());
            record[260..264].copy_from_slice(&offset.to_le_bytes());
            record[264..268].copy_from_slice(&size.to_le_bytes());
            record[268..272].copy_from_slice(&size.to_le_bytes());
            put_u32(&mut bytes, 276 ^ MASK);
            put_u32(&mut bytes, 276 ^ MASK ^ CHECK);
            bytes.extend_from_slice(&record);
        }
        let end = bytes.len() as u32 + 280;
        let mut footer = vec![0u8; 280];
        footer[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
        footer[8..12].copy_from_slice(&(directory_at ^ MASK).to_le_bytes());
        footer[268..272].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
        footer[272..276].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        footer[276..280].copy_from_slice(&0x20002u32.to_le_bytes());
        bytes.extend_from_slice(&footer);
        bytes[4..8].copy_from_slice(&end.to_le_bytes());
        std::fs::write(path, &bytes).unwrap();
    }
    fn fixture_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "pw-collision-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
    #[test]
    fn scenery_collision_uses_the_selected_map_namespace() {
        let names = vec![
            "litmodels/a01/1/litmodel_1399.bmd".to_string(),
            "litmodels/a11/1/litmodel_1399.bmd".to_string(),
        ];
        let by_basename = litmodel_basename_index(&names);
        let canonical = litmodel_key("1/litmodel_1399.bmd");
        assert_eq!(
            litmodel_entry_key(&canonical, "a01", &names, &by_basename).unwrap(),
            "litmodels/a01/1/litmodel_1399.bmd"
        );
        assert!(litmodel_entry_key(&canonical, "a02", &names, &by_basename)
            .unwrap_err()
            .contains("refusing cross-map basename fallback"));
    }
    #[test]
    fn a_missing_map_scoped_collision_model_skips_only_that_prop() {
        let root = fixture_root("missing-map-scoped");
        write_pck(
            &root.join("litmodels.pck"),
            &[("litmodels/a11/1/litmodel_1399.bmd", b"unreadable")],
        );
        let (tx, rx) = mpsc::sync_channel(2);
        building_worker(
            root.clone(),
            "a01".into(),
            vec!["1/litmodel_1399.bmd".into()],
            tx,
        );
        let mut batches = 0;
        let mut failures = None;
        for message in rx {
            match message {
                BuildingMessage::Batch { brushes, decks, .. } => {
                    batches += 1;
                    assert!(brushes.is_empty());
                    assert!(decks.is_empty());
                }
                BuildingMessage::Done { failed } => {
                    failures = Some(failed);
                    break;
                }
                BuildingMessage::Fatal(error) => panic!("collision worker aborted: {error}"),
            }
        }
        assert_eq!(batches, 1);
        assert_eq!(failures, Some(1));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn box_brush(center: [f32; 3], extents: [f32; 3]) -> CollisionBrush {
        let sides = [
            ([1.0, 0.0, 0.0], center[0] + extents[0]),
            ([-1.0, 0.0, 0.0], extents[0] - center[0]),
            ([0.0, 1.0, 0.0], center[1] + extents[1]),
            ([0.0, -1.0, 0.0], extents[1] - center[1]),
            ([0.0, 0.0, 1.0], center[2] + extents[2]),
            ([0.0, 0.0, -1.0], extents[2] - center[2]),
        ]
        .into_iter()
        .map(|(normal, distance)| BrushSide {
            normal,
            distance,
            bevel: false,
        })
        .collect();
        CollisionBrush {
            center,
            extents,
            flags: 0,
            sides,
        }
    }

    fn cube() -> CollisionBrush {
        box_brush([0.0; 3], [1.0; 3])
    }

    #[test]
    fn native_sweep_expands_planes_by_player_aabb() {
        let hit = trace_brush(
            &cube(),
            Vec3::new(-3.0, 0.0, 0.0),
            Vec3::new(6.0, 0.0, 0.0),
            Vec3::splat(0.3),
        )
        .unwrap();
        assert!(!hit.start_solid);
        assert_eq!(hit.normal, Vec3::NEG_X);
        let expected = (1.7 - DIST_EPSILON) / 6.0;
        assert!((hit.fraction - expected).abs() < 1e-6);
    }

    #[test]
    fn object_collision_stops_walls_and_slides_along_them() {
        let collision = Collision::from_parts(vec![cube()]);
        let extents = Vec3::new(0.3, 0.9, 0.3);
        let old = Vec3::new(-3.0, 1.0, 0.0);
        let stopped = collision.resolve(old, Vec3::new(3.0, 1.0, 0.0), extents);
        assert!(stopped.blocked);
        assert!(stopped.feet.x <= -1.29, "{:?}", stopped);
        assert!((stopped.feet.z - old.z).abs() < 1e-6);

        let slide = collision.resolve(old, Vec3::new(3.0, 1.0, 2.0), extents);
        assert!(slide.blocked);
        assert!(slide.feet.x <= -1.29, "{:?}", slide);
        assert!(slide.feet.z > 1.0, "wall sliding was lost: {slide:?}");
    }

    #[test]
    fn skipped_building_hulls_do_not_block_but_tree_hulls_use_native_flags() {
        let mut skipped = cube();
        skipped.flags = CHDATA_SKIP_COLLISION;
        let collision = Collision::from_parts(vec![skipped]);
        let result = collision.resolve(
            Vec3::new(-3.0, 0.0, 0.0),
            Vec3::new(3.0, 0.0, 0.0),
            Vec3::splat(0.3),
        );
        assert!(!result.blocked);
        assert_eq!(result.feet, Vec3::new(3.0, 0.0, 0.0));
    }

    #[test]
    fn falling_sweep_and_down_trace_provide_object_top_support() {
        let collision = Collision::from_parts(vec![cube()]);
        let extents = Vec3::new(0.3, 0.9, 0.3);
        let landed = collision.resolve(Vec3::new(0.0, 3.0, 0.0), Vec3::new(0.0, 0.0, 0.0), extents);
        assert!(landed.blocked);
        assert_eq!(landed.normal, Vec3::Y);
        assert!((landed.feet.y - (1.0 + DIST_EPSILON)).abs() < 1e-5);

        let support = collision
            .support(Vec3::new(0.0, 1.5, 0.0), extents, 1.0)
            .expect("cube top should support the player AABB");
        assert_eq!(support.normal, Vec3::Y);
        assert!((support.height - (1.0 + DIST_EPSILON)).abs() < 1e-5);
        assert!(collision
            .support(Vec3::new(3.0, 1.5, 0.0), extents, 1.0)
            .is_none());
    }

    #[test]
    fn support_ignores_hulls_that_already_contain_the_player() {
        // A building interior/canopy brush that contains the AABB reports
        // `start_solid` with fraction 0. It must not veto the real floor plate
        // below, which used to leave the body airborne on every such frame.
        let enclosing = box_brush([0., 2., 0.], [4., 4., 4.]);
        let floor_plate = box_brush([0., 0.5, 0.], [2., 0.5, 2.]);
        let collision = Collision::from_parts(vec![enclosing, floor_plate]);
        let extents = Vec3::new(0.3, 0.9, 0.3);
        let feet = Vec3::new(0., 1.0 + DIST_EPSILON, 0.);
        let support = collision
            .support(feet, extents, 0.3)
            .expect("the floor plate is still a support plane");
        assert_eq!(support.normal, Vec3::Y);
        assert!((support.height - feet.y).abs() < 1e-5);
    }

    #[test]
    fn a_down_trace_never_lifts_the_feet() {
        let collision = Collision::from_parts(vec![cube()]);
        let extents = Vec3::new(0.3, 0.9, 0.3);
        // Exact contact used to report CONTACT_LIFT above the feet, which made
        // the body climb a centimetre every frame while standing on a roof.
        for feet_y in [1.0, 1.01, 1.02, 1.03, 1.0 + DIST_EPSILON] {
            let feet = Vec3::new(0., feet_y, 0.);
            let support = collision
                .support(feet, extents, 0.3)
                .expect("cube top should still support the feet");
            assert!(support.height <= feet_y + 1e-6, "at {feet_y}: {support:?}");
        }
    }

    #[test]
    fn tree_canopies_do_not_become_walkable_support() {
        let mut world = CollisionWorld::default();
        world.add_tree_type(0, vec![cube()]);
        world.add_tree(0, Vec3::new(0., 3., 0.));
        let collision = Collision {
            world,
            ..Collision::default()
        };
        let extents = Vec3::new(0.3, 0.9, 0.3);
        assert!(collision
            .support(Vec3::new(0., 5., 0.), extents, 1.0)
            .is_none());
        // The swept wall pass still sees the tree hull.
        assert!(
            collision
                .resolve(Vec3::new(-3., 3., 0.), Vec3::new(3., 3., 0.), extents)
                .blocked
        );
    }

    #[test]
    fn physics_floor_and_the_inherent_support_query_agree() {
        let collision = Collision::from_parts(vec![cube()]);
        let extents = Vec3::new(0.3, 0.9, 0.3);
        let feet = Vec3::new(0., 1.5, 0.);
        let direct = collision.support(feet, extents, 1.0).unwrap();
        let via_floor =
            <Collision as physics::ObjectSupport>::object_support(&collision, feet, extents, 1.0)
                .unwrap();
        assert_eq!(direct.height, via_floor.height);
        assert_eq!(direct.normal, via_floor.normal);
    }

    #[test]
    fn tbh_reader_skips_native_hull_payload_and_reads_shared_brush() {
        let brush = cube();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(b"V0.2======Convex_Hull_Data_Start=========");
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"abc");
        for value in brush.center.into_iter().chain(brush.extents) {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for value in [0.0f32; 6] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&brush.flags.to_le_bytes());
        bytes.extend_from_slice(&(brush.sides.len() as u32).to_le_bytes());
        for side in brush.sides {
            for value in side.normal {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            bytes.extend_from_slice(&side.distance.to_le_bytes());
            bytes.push(u8::from(side.bevel));
        }
        let mut wrapped = b"MOXB".to_vec();
        wrapped.extend_from_slice(&bytes);
        let parsed = parse_tbh(&wrapped).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].sides.len(), 6);
        assert_eq!(parsed[0].center, [0.0; 3]);
    }

    #[test]
    fn start_solid_can_escape_instead_of_trapping_the_player() {
        let collision = Collision::from_parts(vec![cube()]);
        let result = collision.resolve(Vec3::ZERO, Vec3::new(3.0, 0.0, 0.0), Vec3::splat(0.3));
        assert!(result.blocked);
        assert_eq!(result.feet, Vec3::new(3.0, 0.0, 0.0));
    }

    fn crate_deck(top: f32, half: f32) -> PropDeck {
        PropDeck {
            center: [0., 0.],
            extents: [half, half],
            top,
        }
    }

    fn up_quad(x0: f32, x1: f32, y: f32, z0: f32, z1: f32) -> ([[f32; 3]; 3], [[f32; 3]; 3]) {
        (
            [[x0, y, z0], [x1, y, z1], [x1, y, z0]],
            [[x0, y, z0], [x0, y, z1], [x1, y, z1]],
        )
    }

    fn model_with(quads: &[(([[f32; 3]; 3], [[f32; 3]; 3]))]) -> crate::bmd::Model {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (a, b) in quads {
            for triangle in [a, b] {
                for point in triangle {
                    indices.push(vertices.len() as u16);
                    vertices.push(crate::bmd::Vertex {
                        position: *point,
                        uv: [0., 0.],
                        normal: [0., 1., 0.],
                        diffuse: 0,
                        day_diffuse: 0,
                        night_diffuse: 0,
                    });
                }
            }
        }
        crate::bmd::Model {
            scale: [1., 1., 1.],
            direction: [0., 0., 1.],
            up: [0., 1., 0.],
            position: [0., 0., 0.],
            meshes: vec![crate::bmd::Mesh {
                name: String::new(),
                version: 0,
                material: None,
                texture: String::new(),
                vertices,
                indices,
            }],
            collision: Vec::new(),
        }
    }

    #[test]
    fn a_step_keeps_its_own_height_instead_of_the_models_tallest_point() {
        // Two steps of one model: a placement on the low step must not be lifted
        // to the height of the high one.
        let model = model_with(&[up_quad(0., 2., 0., 0., 2.), up_quad(2., 4., 2., 0., 2.)]);
        let decks = model_decks(&model);
        let at = |x: f32, z: f32| {
            decks
                .iter()
                .filter(|deck| {
                    (x - deck.center[0]).abs() <= deck.extents[0]
                        && (z - deck.center[1]).abs() <= deck.extents[1]
                })
                .map(|deck| deck.top)
                .fold(None::<f32>, |best, top| {
                    Some(best.map_or(top, |b: f32| b.max(top)))
                })
        };
        assert_eq!(at(1., 1.), Some(0.));
        assert_eq!(at(3., 1.), Some(2.));
    }

    #[test]
    fn a_tall_model_offers_no_standing_surface() {
        // A nine-unit-tall model (base and top) is scenery, not a prop.
        let model = model_with(&[up_quad(0., 2., 0., 0., 2.), up_quad(0., 2., 9., 0., 2.)]);
        assert!(model_decks(&model).is_empty());
    }

    #[test]
    fn a_model_top_supports_a_placement_that_has_no_hull() {
        // Props ship render geometry but often no collision hull, which used to
        // drop their NPCs onto the terrain under them.
        let collision = Collision::from_parts_and_decks(Vec::new(), vec![crate_deck(3., 2.)]);
        let ground = collision
            .floor_above(Vec3::new(0., 0.5, 0.), Vec3::new(0.4, 0.9, 0.4), 16.)
            .expect("the crate top is a floor");
        assert!((ground.height - 3.).abs() < 1e-5, "{ground:?}");
        // Outside the footprint the terrain stays the support.
        assert!(collision
            .floor_above(Vec3::new(5., 0.5, 0.), Vec3::new(0.4, 0.9, 0.4), 16.,)
            .is_none());
    }

    #[test]
    fn a_real_hull_floor_still_beats_a_model_top() {
        // The bridge deck is what the Walker walks on, so it also wins for a
        // placement standing over a prop that happens to be under the span.
        let collision = Collision::from_parts_and_decks(vec![cube()], vec![crate_deck(3., 2.)]);
        let ground = collision
            .floor_above(Vec3::new(0., -5., 0.), Vec3::new(0.4, 0.9, 0.4), 16.)
            .expect("the cube top is a floor");
        // The sweep starts one contact lift above the plane, so the surface it
        // reports sits that much above the cube's own top.
        assert!((ground.height - 1.).abs() < 0.05, "{ground:?}");
    }

    #[test]
    fn a_name_is_hidden_when_something_stands_between_camera_and_head() {
        let collision = Collision::from_parts(vec![cube()]);
        // Straight through the cube from one side to the other.
        assert!(collision.sight_blocked(Vec3::new(-4., 0., 0.), Vec3::new(4., 0., 0.)));
        // Clear line of sight beside it.
        assert!(!collision.sight_blocked(Vec3::new(-4., 5., 0.), Vec3::new(4., 5., 0.)));
        // A camera inside geometry is a clipping artefact, not an occlusion.
        assert!(!collision.sight_blocked(Vec3::new(0., 0., 0.), Vec3::new(4., 0., 0.)));
    }
}
