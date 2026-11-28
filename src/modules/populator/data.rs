//! Real population data for the Populator add-on.
//!
//! Two retail client files carry the shipped population:
//!
//! * `data/task_npc.data` — the main world's task-NPC table. The shipped file is
//!   18,876 bytes: `u32 total_size`, `u32 timestamp (0x4b39c937)`,
//!   `u16 version = 1`, `u16 count = 1,572`, then 1,572 records of
//!   `u32 npc_id`, `i16 x`, `i16 z`, `i16 height`, `u16 task_count`.
//!   1,057 of those records carry `x = z = 0`: they are the NPCs the server
//!   spawns for quests, and the client never places them, so the module does
//!   not draw them at the map origin — 515 records carry a real position.
//!   Every id resolves to an `NPC_ESSENCE` row and the two coordinate columns
//!   land on the client's own destination landmarks with the first column as
//!   world X and the second as world Z (Archosaur North
//!   `1470.490, 219.608, 1180.864` has 60 task NPCs within 400 units under that
//!   mapping and 17 under the swapped one; City of a Thousand Streams has 14
//!   against 0). `height` is 0 for 1,553 of the 1,572 records, which is the
//!   table's "stand on the terrain" encoding.
//! * `data/elements.data` v27 — `NPC_ESSENCE` (§58) and `MONSTER_ESSENCE` (§39)
//!   template tables, parsed by the existing zero-remainder v27 reader.
//!
//! Mob placement is **not** in the client: the server's `gamedata` owns
//! `npcgen.data`. [`Population`] therefore reports the real monster template
//! count next to an explicit "no placement source" note instead of inventing
//! coordinates.

use super::patrol::Route;
use crate::modules::designer::catalog::Catalog;
use std::{fs, path::Path};

/// Header version of the shipped `task_npc.data`.
pub const TASK_NPC_VERSION: u16 = 1;
/// Main-world half extents (`x -4096..4096`, `z -5632..5632`), used to reject a
/// table from another map or another client build.
const WORLD_LIMIT_X: f32 = 4096.;
const WORLD_LIMIT_Z: f32 = 5632.;

/// Where a populated record's template came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TemplateKind {
    Npc,
    /// Reachable as soon as a real mob placement source exists; the client
    /// ships none, so nothing constructs this variant today.
    #[allow(dead_code)]
    Monster,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskNpcEntry {
    pub npc_id: u32,
    /// World X of the spawn.
    pub x: i16,
    /// World Z of the spawn.
    pub z: i16,
    /// Authored absolute height; 0 means "use the terrain height".
    pub height: i16,
    /// Task links owned by this NPC in `tasks.data`.
    pub task_count: u16,
}

#[derive(Clone, Debug)]
pub struct TaskNpcTable {
    pub declared_size: u32,
    pub timestamp: u32,
    pub version: u16,
    pub entries: Vec<TaskNpcEntry>,
}

impl TaskNpcTable {
    /// Exact parser: the declared size, the record count and the record stride
    /// must all agree with the file length, so a table from another build can
    /// never be read as plausible noise.
    pub fn parse_exact(bytes: &[u8]) -> Result<Self, String> {
        let header = bytes
            .get(..12)
            .ok_or_else(|| "truncated task_npc.data header".to_string())?;
        let declared_size = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let timestamp = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let version = u16::from_le_bytes(header[8..10].try_into().unwrap());
        let count = u16::from_le_bytes(header[10..12].try_into().unwrap()) as usize;
        if declared_size as usize != bytes.len() {
            return Err(format!(
                "task_npc.data declares {declared_size} bytes, file has {}",
                bytes.len()
            ));
        }
        if version != TASK_NPC_VERSION {
            return Err(format!(
                "unsupported task_npc.data version {version}; expected {TASK_NPC_VERSION}"
            ));
        }
        let expected = 12usize
            .checked_add(
                count
                    .checked_mul(12)
                    .ok_or("task_npc.data count overflow")?,
            )
            .ok_or("task_npc.data size overflow")?;
        if expected != bytes.len() {
            return Err(format!(
                "{count} task_npc.data records need {expected} bytes, file has {}",
                bytes.len()
            ));
        }
        let mut entries = Vec::with_capacity(count);
        for (index, record) in bytes[12..].chunks_exact(12).enumerate() {
            let npc_id = u32::from_le_bytes(record[0..4].try_into().unwrap());
            let x = i16::from_le_bytes(record[4..6].try_into().unwrap());
            let z = i16::from_le_bytes(record[6..8].try_into().unwrap());
            let height = i16::from_le_bytes(record[8..10].try_into().unwrap());
            let task_count = u16::from_le_bytes(record[10..12].try_into().unwrap());
            if npc_id == 0 {
                return Err(format!("task_npc.data record {index} has a zero NPC id"));
            }
            if height < 0 || (x as f32).abs() > WORLD_LIMIT_X || (z as f32).abs() > WORLD_LIMIT_Z {
                return Err(format!(
                    "task_npc.data record {index} is outside the main world: {x}, {z}, {height}"
                ));
            }
            entries.push(TaskNpcEntry {
                npc_id,
                x,
                z,
                height,
                task_count,
            });
        }
        Ok(Self {
            declared_size,
            timestamp,
            version,
            entries,
        })
    }
}

/// One real placement: an `elements.data` template plus the authored anchor.
#[derive(Clone, Debug)]
pub struct Spawn {
    pub kind: TemplateKind,
    pub id: u32,
    pub name: String,
    /// Normalized `.ecm` path from the template table.
    pub model: String,
    /// World XZ of the feet position.
    pub anchor: [f32; 2],
    /// Authored absolute height, or `None` when the placement says "terrain".
    pub height: Option<f32>,
    pub task_count: u16,
    /// Index into [`Population::routes`], or `None` for a standing actor. The
    /// client's own placement table carries no waypoints, so this is always
    /// `None` until a server route source supplies one.
    pub route: Option<usize>,
}

/// A real population loaded from the client's own files.
#[derive(Clone, Debug, Default)]
pub struct Population {
    pub spawns: Vec<Spawn>,
    /// Records at `x = z = 0`, the table's "the server places this one" marker.
    pub unplaced: usize,
    /// Placements whose template was missing or not spawnable.
    pub skipped: usize,
    /// Real navigation paths, in spawn-index order of appearance. Empty for
    /// the shipped client data (see [`Population::route_source`]).
    pub routes: Vec<Route>,
    pub npc_templates: usize,
    pub monster_templates: usize,
    pub table_size: u32,
    pub table_timestamp: u32,
    /// Why no mob placement exists, shown verbatim in the settings window.
    pub mob_placement: String,
    /// Why no patrol route exists today, shown verbatim.
    pub route_source: String,
    pub provenance: Vec<String>,
}

impl Population {
    /// Loads `elements.data` and `task_npc.data` from a retail `element` folder.
    pub fn open(element: &Path) -> Result<Self, String> {
        let elements_path = element.join("data/elements.data");
        let bytes = fs::read(&elements_path)
            .map_err(|error| format!("{}: {error}", elements_path.display()))?;
        let catalog = Catalog::parse_v27(&bytes)
            .map_err(|error| format!("{}: {error}", elements_path.display()))?;
        let spawns_path = element.join("data/task_npc.data");
        let spawns = fs::read(&spawns_path)
            .map_err(|error| format!("{}: {error}", spawns_path.display()))?;
        let table = TaskNpcTable::parse_exact(&spawns)
            .map_err(|error| format!("{}: {error}", spawns_path.display()))?;
        Ok(Self::from_tables(
            &catalog,
            &table,
            format!(
                "{}: v27, {} NPC templates, {} monster templates",
                elements_path.display(),
                catalog.npcs.len(),
                catalog.monsters.len()
            ),
        ))
    }

    /// Joins the placement table onto the template tables. Every spawn keeps
    /// the template's authored name and rig; nothing is substituted.
    pub fn from_tables(catalog: &Catalog, table: &TaskNpcTable, elements_label: String) -> Self {
        let mut spawns = Vec::with_capacity(table.entries.len());
        let mut unplaced = 0usize;
        let mut skipped = 0usize;
        for entry in &table.entries {
            // Zero/zero is the table's own "no client position" encoding: the
            // entry is a quest NPC the server spawns, and drawing it would put
            // a thousand characters on the map origin.
            if entry.x == 0 && entry.z == 0 {
                unplaced += 1;
                continue;
            }
            let Some(npc) = catalog.npc(entry.npc_id) else {
                skipped += 1;
                continue;
            };
            if !npc.is_spawnable() {
                skipped += 1;
                continue;
            }
            spawns.push(Spawn {
                kind: TemplateKind::Npc,
                id: npc.id,
                name: npc.name.clone(),
                model: npc.model.clone(),
                anchor: [entry.x as f32, entry.z as f32],
                height: (entry.height > 0).then_some(entry.height as f32),
                task_count: entry.task_count,
                route: None,
            });
        }
        Self {
            provenance: vec![
                elements_label,
                format!(
                    "data/task_npc.data: v{}, {} B, {} records",
                    table.version,
                    table.declared_size,
                    table.entries.len()
                ),
            ],
            spawns,
            unplaced,
            skipped,
            routes: Vec::new(),
            npc_templates: catalog.npcs.len(),
            monster_templates: catalog.monsters.len(),
            table_size: table.declared_size,
            table_timestamp: table.timestamp,
            mob_placement: "MONSTER_ESSENCE templates are loaded, but mob placement is \
                            server-authoritative: the client ships no npcgen.data. Point the \
                            module at a server gamedata/npcgen.data to populate mobs."
                .to_string(),
            route_source: "no patrol route exists in the client data; routes arrive with the \
                           server npcgen.data"
                .to_string(),
        }
    }

    /// Number of real placements within `radius` of `camera`, for the status
    /// line: an empty radius is a data answer, not a rendering failure.
    pub fn count_within(&self, camera: [f32; 2], radius: f32) -> usize {
        if !radius.is_finite() || radius <= 0. {
            return 0;
        }
        let radius2 = radius * radius;
        self.spawns
            .iter()
            .filter(|spawn| {
                let dx = spawn.anchor[0] - camera[0];
                let dz = spawn.anchor[1] - camera[1];
                dx * dx + dz * dz <= radius2
            })
            .count()
    }

    /// Nearest spawns to `camera` within `radius`, closest first, capped at
    /// `limit`. Ties are broken by spawn order so the streamed set is stable.
    pub fn nearest(&self, camera: [f32; 2], radius: f32, limit: usize) -> Vec<(usize, f32)> {
        if !radius.is_finite() || radius <= 0. || limit == 0 {
            return Vec::new();
        }
        let radius2 = radius * radius;
        let mut candidates: Vec<(usize, f32)> = self
            .spawns
            .iter()
            .enumerate()
            .filter_map(|(index, spawn)| {
                let dx = spawn.anchor[0] - camera[0];
                let dz = spawn.anchor[1] - camera[1];
                let distance2 = dx * dx + dz * dz;
                (distance2 <= radius2).then_some((index, distance2))
            })
            .collect();
        let cmp = |a: &(usize, f32), b: &(usize, f32)| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0));
        if limit == 0 {
            candidates.clear();
        } else if candidates.len() > limit {
            candidates.select_nth_unstable_by(limit, cmp);
            candidates.truncate(limit);
            candidates.sort_unstable_by(cmp);
        } else {
            candidates.sort_unstable_by(cmp);
        }
        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u32, x: i16, z: i16, height: i16, tasks: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(12);
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&z.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&tasks.to_le_bytes());
        out
    }

    fn table(entries: &[Vec<u8>]) -> Vec<u8> {
        let size = 12 + entries.len() * 12;
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&0x4b39c937u32.to_le_bytes());
        out.extend_from_slice(&TASK_NPC_VERSION.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for entry in entries {
            out.extend_from_slice(entry);
        }
        out
    }

    #[test]
    fn exact_parser_reads_header_and_records() {
        let bytes = table(&[
            record(3124, 1778, 3447, 0, 70),
            record(3125, -985, 4204, 276, 66),
        ]);
        let parsed = TaskNpcTable::parse_exact(&bytes).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.declared_size as usize, bytes.len());
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(
            parsed.entries[0],
            TaskNpcEntry {
                npc_id: 3124,
                x: 1778,
                z: 3447,
                height: 0,
                task_count: 70
            }
        );
        assert_eq!(parsed.entries[1].height, 276);
        assert_eq!(parsed.entries[1].x, -985);
    }

    #[test]
    fn mismatched_size_count_and_stride_are_rejected() {
        let mut bytes = table(&[record(1, 0, 0, 0, 0)]);
        bytes[0] ^= 0xff;
        assert!(TaskNpcTable::parse_exact(&bytes).is_err());

        let mut bytes = table(&[record(1, 0, 0, 0, 0)]);
        bytes[10] = 2; // count says two records, file has one
        assert!(TaskNpcTable::parse_exact(&bytes).is_err());

        let mut bytes = table(&[record(1, 0, 0, 0, 0)]);
        bytes.push(0);
        let size = bytes.len() as u32;
        bytes[0..4].copy_from_slice(&size.to_le_bytes());
        assert!(TaskNpcTable::parse_exact(&bytes).is_err());
    }

    #[test]
    fn version_zero_ids_negative_heights_and_off_world_points_are_rejected() {
        let mut bytes = table(&[record(1, 0, 0, 0, 0)]);
        bytes[8] = 2;
        assert!(TaskNpcTable::parse_exact(&bytes).is_err());

        assert!(TaskNpcTable::parse_exact(&table(&[record(0, 0, 0, 0, 0)])).is_err());
        assert!(TaskNpcTable::parse_exact(&table(&[record(7, 0, 0, -1, 0)])).is_err());
        assert!(TaskNpcTable::parse_exact(&table(&[record(7, 9000, 0, 0, 0)])).is_err());
        assert!(TaskNpcTable::parse_exact(&table(&[record(7, 0, -9000, 0, 0)])).is_err());
    }

    #[test]
    fn nearest_is_closest_first_and_capped() {
        let population = Population {
            spawns: vec![
                spawn(1, [0., 0.]),
                spawn(2, [100., 0.]),
                spawn(3, [40., 0.]),
                spawn(4, [500., 0.]),
            ],
            ..Default::default()
        };
        let nearest = population.nearest([0., 0.], 200., 2);
        assert_eq!(
            nearest.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert!(population.nearest([0., 0.], 0., 4).is_empty());
        assert!(population.nearest([0., 0.], 200., 0).is_empty());
        assert_eq!(population.nearest([0., 0.], 200., 8).len(), 3);
    }

    fn spawn(id: u32, anchor: [f32; 2]) -> Spawn {
        Spawn {
            kind: TemplateKind::Npc,
            id,
            name: format!("npc {id}"),
            model: "models/npcs/npc/example/example.ecm".into(),
            anchor,
            height: None,
            task_count: 0,
            route: None,
        }
    }

    #[test]
    fn shipped_table_matches_the_known_counts() {
        let path = Path::new("google_drive/pw142/element/data/task_npc.data");
        if !path.exists() {
            return;
        }
        let bytes = fs::read(path).unwrap();
        let table = TaskNpcTable::parse_exact(&bytes).unwrap();
        assert_eq!(table.entries.len(), 1_572);
        assert_eq!(table.timestamp, 0x4b39c937);
        // Archosaur North District: the authored destination in
        // `maps/world/world_targets.txt`.
        let near = table
            .entries
            .iter()
            .filter(|entry| {
                let dx = entry.x as f32 - 1470.490;
                let dz = entry.z as f32 - 1180.864;
                dx * dx + dz * dz < 400. * 400.
            })
            .count();
        assert_eq!(near, 60);
        // City of a Thousand Streams.
        let near = table
            .entries
            .iter()
            .filter(|entry| {
                let dx = entry.x as f32 + 2825.774;
                let dz = entry.z as f32 - 3092.210;
                dx * dx + dz * dz < 400. * 400.
            })
            .count();
        assert_eq!(near, 14);
    }

    #[test]
    fn shipped_client_joins_every_placement_onto_an_npc_template() {
        let element = Path::new("google_drive/pw142/element");
        if !element.join("data/elements.data").exists()
            || !element.join("data/task_npc.data").exists()
        {
            return;
        }
        let population = Population::open(element).unwrap();
        assert_eq!(population.npc_templates, 2_838);
        assert_eq!(population.monster_templates, 4_208);
        // 1,572 records: 515 with a real position, 1,057 marked `0,0` for the
        // server to place. None is dropped for its template or its name.
        assert_eq!(population.spawns.len(), 515);
        assert_eq!(population.unplaced, 1_057);
        assert_eq!(population.skipped, 0);
        let apothecary = population
            .spawns
            .iter()
            .find(|spawn| spawn.id == 2109)
            .unwrap();
        assert_eq!(apothecary.name, "Apothecary Cheng");
        assert!(apothecary.model.ends_with("/男npc4.ecm"));
        // Every real placement resolves to an `.ecm`; 514 sit under
        // `models/npcs`, one is an animated scene creature under
        // `models/场景活物`.
        assert!(population
            .spawns
            .iter()
            .all(|spawn| spawn.model.ends_with(".ecm")));
        assert_eq!(
            population
                .spawns
                .iter()
                .filter(|spawn| spawn.model.starts_with("models/npcs/"))
                .count(),
            514
        );
        // 19 placed NPCs sit at an authored height instead of on the terrain.
        assert_eq!(
            population
                .spawns
                .iter()
                .filter(|spawn| spawn.height.is_some())
                .count(),
            19
        );
        // Archosaur North stays the densest cluster the client can place.
        assert_eq!(population.count_within([1470.490, 1180.864], 420.), 61);
    }
}
