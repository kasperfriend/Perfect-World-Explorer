//! NPC and mob templates from the shipped v27 `elements.data`.
//!
//! `NPC_ESSENCE` (§58, 852 B) and `MONSTER_ESSENCE` (§39, 1528 B) are read with
//! the offsets of the client's own v27 element layout, including the service
//! block §58@736..848 and the §39 tail 1228..1528.
//!
//! Neither section carries a `file_icon` field: the client draws an NPC or a mob
//! from its modelled rig. The only artwork these records reach is the pet egg a
//! mob is captured as (`id_pet_egg_captured` → `PET_EGG_ESSENCE` `file_icon`),
//! which belongs to the pet system rather than to the template, so the tab shows
//! the rig and does not present an egg as the NPC's own icon.
use super::elements::visit_v27_records;
use encoding_rs::GBK;
use std::collections::HashMap;

/// The scene action the client asks a placed NPC for. `站立` is the standing
/// loop 1,674 of the 1,871 shipped NPC rigs carry; the client falls back to the
/// literal `休闲` when a rig has no match, which `EcModel::scene_action` does.
/// See `Findings/npc-idle-evidence.md`.
pub const SCENE_IDLE: &str = "站立";

const SECTION_MONSTER_ADDON: usize = 37;
const SECTION_MONSTER_TYPE: usize = 38;
const SECTION_MONSTER: usize = 39;
const SECTION_NPC_TYPE: usize = 57;
const SECTION_NPC: usize = 58;

/// `NPC_ESSENCE` service-id fields: (label, record offset, service table).
///
/// The tables are addressed by their position in the file, which departs from
/// the numbering of the plain-text `NPC_WAR_TOWERBUILD_SERVICE` block for every
/// section after it.
const NPC_SERVICES: [(&str, usize, usize); 27] = [
    ("Talk", 736, 40),
    ("Sell goods", 740, 41),
    ("Buy goods", 744, 42),
    ("Repair", 748, 43),
    ("Install stones", 752, 44),
    ("Remove stones", 756, 45),
    ("Offer tasks", 760, 47),
    ("Turn in tasks", 764, 46),
    ("Task items", 768, 48),
    ("Teach skills", 772, 49),
    ("Heal", 776, 50),
    ("Teleport", 780, 51),
    ("Transport", 784, 52),
    ("Proxy sale", 788, 53),
    ("Storage", 792, 54),
    ("Craft", 796, 55),
    ("Decompose", 800, 56),
    ("Identify", 804, 81),
    ("War tower build", 808, 102),
    ("Reset attributes", 812, 104),
    ("Rename pet", 816, 105),
    ("Learn pet skills", 820, 106),
    ("Forget pet skills", 824, 107),
    ("Bind equipment", 828, 111),
    ("Destroy equipment", 832, 112),
    ("Cancel equipment destruction", 836, 113),
    ("Goblin skills", 840, 49),
];

/// `combined_services` bits, from the client's own field comment. Bit 2 and the
/// unused high bits stay unnamed rather than guessed.
const COMBINED_SERVICES: [(u32, &str); 29] = [
    (0, "Forget a production skill"),
    (1, "Punch a socket"),
    (3, "Staging post discovery"),
    (4, "Faction services"),
    (5, "Change appearance"),
    (6, "Mail"),
    (7, "Auction"),
    (8, "Double-experience punch card"),
    (9, "Hatch a pet egg"),
    (10, "Restore a pet egg"),
    (11, "City war management"),
    (12, "Leave the battlefield"),
    (13, "Consign game-time cards"),
    (14, "Upgrade equipment"),
    (15, "Dye"),
    (16, "Reverse fortune"),
    (17, "Reset goblin attributes"),
    (18, "Reset goblin skill points"),
    (19, "Forget a goblin skill"),
    (20, "Refine a goblin"),
    (21, "Split a goblin"),
    (22, "Remove goblin equipment"),
    (23, "Repair broken equipment"),
    (24, "Trading platform"),
    (25, "Immortal/demon conversion"),
    (26, "Marriage"),
    (27, "Open a territory base"),
    (28, "Upgrade or pay into a base"),
    (29, "Exchange base materials"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Npc,
    Monster,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Npc => "NPC",
            Self::Monster => "Monster",
        }
    }
}

#[derive(Debug)]
pub struct Entry {
    pub id: u32,
    pub kind: Kind,
    pub name: String,
    pub search_name: String,
    /// Authored `.ecm`, already normalised to the archive's separators.
    pub model: String,
    pub type_id: u32,
    pub type_name: String,
    pub record: Vec<u8>,
}

fn number(record: &[u8], offset: usize) -> u32 {
    record
        .get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .unwrap_or(0)
}
fn decimal(record: &[u8], offset: usize) -> f32 {
    f32::from_bits(number(record, offset))
}
/// Fixed-width UTF-16 field, terminated by a zero unit.
fn wide(record: &[u8], offset: usize, bytes: usize) -> String {
    let Some(field) = record.get(offset..offset + bytes) else {
        return String::new();
    };
    let units: Vec<u16> = field
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}
/// Fixed-width GBK field, terminated by a zero byte.
fn encoded(record: &[u8], offset: usize, bytes: usize) -> String {
    let Some(field) = record.get(offset..offset + bytes) else {
        return String::new();
    };
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    GBK.decode(&field[..end]).0.into_owned()
}
fn model_path(record: &[u8], offset: usize) -> String {
    let path = encoded(record, offset, 128);
    if path.eq_ignore_ascii_case("n/a") || path == "0" {
        String::new()
    } else {
        crate::ecmodel::normalize(&path)
    }
}

fn format_number(value: f64) -> String {
    if value.fract() == 0. {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

/// One authored service a template offers, resolved through its own table.
#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    pub label: &'static str,
    /// `NPC_ESSENCE` field the id came from.
    pub offset: usize,
    /// Service table the id was resolved in, `None` for a bit-only service.
    pub table: Option<usize>,
    pub id: u32,
    pub name: String,
    pub detail: String,
}

#[derive(Default)]
pub struct Database {
    pub entries: Vec<Entry>,
    pub by_id: HashMap<u32, usize>,
    /// Service tables keyed by file position, then by record id.
    services: HashMap<usize, HashMap<u32, Vec<u8>>>,
    monster_addons: HashMap<u32, (String, Vec<i32>)>,
    /// `MONSTER_TYPE` add-on rolls, keyed by monster type.
    monster_type_addons: HashMap<u32, Vec<(u32, f32)>>,
    /// Main-world placement counts, when `task_npc.data` was readable.
    placements: HashMap<u32, u32>,
    pub placements_loaded: bool,
}

/// Distinct service tables the `NPC_ESSENCE` fields reference.
fn service_tables() -> Vec<usize> {
    let mut tables: Vec<usize> = Vec::new();
    for (_, _, table) in NPC_SERVICES {
        if !tables.contains(&table) {
            tables.push(table);
        }
    }
    tables
}

impl Database {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut db = Self::default();
        let mut npc_types: HashMap<u32, String> = HashMap::with_capacity(64);
        let mut monster_types: HashMap<u32, String> = HashMap::with_capacity(64);
        let wanted = service_tables();
        visit_v27_records(bytes, |section, record| {
            let id = number(record, 0);
            match section {
                SECTION_NPC_TYPE | SECTION_MONSTER_TYPE => {
                    let name = wide(record, 4, 64).trim().to_string();
                    if section == SECTION_NPC_TYPE {
                        npc_types.insert(id, name);
                    } else {
                        monster_types.insert(id, name);
                        let addons = (0..16)
                            .filter_map(|index| {
                                let addon = number(record, 68 + index * 8);
                                let chance = decimal(record, 72 + index * 8);
                                (addon != 0).then_some((addon, chance))
                            })
                            .collect();
                        db.monster_type_addons.insert(id, addons);
                    }
                }
                SECTION_MONSTER_ADDON => {
                    let name = wide(record, 4, 64).trim().to_string();
                    let count = number(record, 68).min(3) as usize;
                    let params: Vec<i32> = (0..count)
                        .map(|index| number(record, 72 + index * 4) as i32)
                        .collect();
                    db.monster_addons.insert(id, (name, params));
                }
                SECTION_NPC => {
                    let name = wide(record, 4, 64).trim().to_string();
                    db.by_id.entry(id).or_insert(db.entries.len());
                    db.entries.push(Entry {
                        id,
                        kind: Kind::Npc,
                        search_name: name.to_lowercase(),
                        name,
                        model: model_path(record, 80),
                        type_id: number(record, 68),
                        type_name: String::new(),
                        record: record.to_vec(),
                    });
                }
                SECTION_MONSTER => {
                    let name = wide(record, 8, 64).trim().to_string();
                    db.by_id.entry(id).or_insert(db.entries.len());
                    db.entries.push(Entry {
                        id,
                        kind: Kind::Monster,
                        search_name: name.to_lowercase(),
                        name,
                        model: model_path(record, 144),
                        type_id: number(record, 4),
                        type_name: String::new(),
                        record: record.to_vec(),
                    });
                }
                _ => {
                    if wanted.contains(&section) {
                        db.services
                            .entry(section)
                            .or_default()
                            .insert(id, record.to_vec());
                    }
                }
            }
            Ok(())
        })?;
        for entry in &mut db.entries {
            let table = if entry.kind == Kind::Npc {
                &npc_types
            } else {
                &monster_types
            };
            entry.type_name = table.get(&entry.type_id).cloned().unwrap_or_default();
        }
        Ok(db)
    }

    /// Counts how often each NPC is placed by the client's own `task_npc.data`.
    ///
    /// The header and stride are checked the same way the Populator reads this
    /// file, so a table from another build is ignored instead of counted.
    pub fn attach_placements(&mut self, bytes: &[u8]) -> bool {
        let Some(header) = bytes.get(..12) else {
            return false;
        };
        let declared = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let version = u16::from_le_bytes(header[8..10].try_into().unwrap());
        let count = u16::from_le_bytes(header[10..12].try_into().unwrap()) as usize;
        if declared != bytes.len() || version != 1 || 12 + count * 12 != bytes.len() {
            return false;
        }
        for record in bytes[12..].chunks_exact(12) {
            let id = u32::from_le_bytes(record[0..4].try_into().unwrap());
            if id != 0 {
                *self.placements.entry(id).or_insert(0) += 1;
            }
        }
        self.placements_loaded = true;
        true
    }

    pub fn placements(&self, id: u32) -> Option<u32> {
        self.placements.get(&id).copied()
    }

    pub fn count(&self, kind: Kind) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.kind == kind)
            .count()
    }
}

impl Entry {
    fn numeric(&self, offset: usize) -> f64 {
        number(&self.record, offset) as f64
    }
    fn real(&self, offset: usize) -> f64 {
        decimal(&self.record, offset) as f64
    }

    /// The NPC's authored greeting; the client prints this when the dialog opens.
    pub fn greeting(&self) -> String {
        if self.kind != Kind::Npc {
            return String::new();
        }
        wide(&self.record, 216, 512).trim().to_string()
    }

    /// The mob's authored elemental trait line, e.g. `+WT-ER`.
    pub fn trait_line(&self) -> String {
        if self.kind != Kind::Monster {
            return String::new();
        }
        wide(&self.record, 104, 32).trim().to_string()
    }

    /// Mob whose attributes replace this NPC's own (the client's own rule).
    pub fn source_monster(&self) -> Option<u32> {
        let source = number(&self.record, 212);
        (self.kind == Kind::Npc && source != 0).then_some(source)
    }

    pub fn egg(&self) -> Option<u32> {
        let egg = number(&self.record, 552);
        (self.kind == Kind::Monster && egg != 0).then_some(egg)
    }

    /// Numeric fields, formatted for display and reused by the comparison.
    pub fn attributes(&self) -> Vec<(&'static str, f64)> {
        match self.kind {
            Kind::Npc => {
                let mut values = vec![
                    ("Refresh time (s)", self.real(72)),
                    // The authored field is already a percentage: the shipped
                    // records hold 0.05, 3.7616, 8, 17 and 100.
                    ("Tax rate (%)", self.real(208)),
                    ("Domain related", self.numeric(732)),
                ];
                let discover = number(&self.record, 728);
                if discover != 0 {
                    values.push(("Staging post", discover as f64));
                }
                values
            }
            Kind::Monster => {
                let mut values = vec![
                    ("Level", self.numeric(544)),
                    ("Size", self.real(528)),
                    ("Life", self.numeric(556)),
                    ("Defence", self.numeric(560)),
                ];
                for index in 0..5 {
                    let value = number(&self.record, 564 + index * 4);
                    if value != 0 {
                        let name: &'static str = [
                            "Magic defence 1",
                            "Magic defence 2",
                            "Magic defence 3",
                            "Magic defence 4",
                            "Magic defence 5",
                        ][index];
                        values.push((name, value as f64));
                    }
                }
                values.extend([
                    ("Attack", self.numeric(612)),
                    ("Armor", self.numeric(616)),
                    ("Damage min", self.numeric(620)),
                    ("Damage max", self.numeric(624)),
                    ("Magic damage min", self.numeric(676)),
                    ("Magic damage max", self.numeric(680)),
                    ("Attack range (m)", self.real(668)),
                    ("Attack speed (attacks/s)", self.real(672)),
                    ("Aggro range (m)", self.real(708)),
                    ("Aggro time (s)", self.real(712)),
                    ("Sight range (m)", self.numeric(608)),
                    ("HP regenerate", self.numeric(692)),
                    ("Experience", self.numeric(588)),
                    ("Skill points", self.numeric(592)),
                    ("Money average", self.numeric(596)),
                    ("Money variance", self.numeric(600)),
                    ("Walk speed", self.real(728)),
                    ("Run speed", self.real(732)),
                ]);
                for (name, offset) in [("Fly speed", 736usize), ("Swim speed", 740)] {
                    let value = decimal(&self.record, offset);
                    if value != 0. {
                        values.push((name, value as f64));
                    }
                }
                values
            }
        }
    }

    /// Named client enumerations, kept apart from the numeric attributes
    /// because they are categories rather than quantities.
    pub fn behaviour(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        if self.kind == Kind::Npc {
            values.push((
                "Attackable",
                match number(&self.record, 76) {
                    0 => "No; other players cannot attack this NPC".to_owned(),
                    1 => "Yes; the attacker's name turns pink".to_owned(),
                    2 => "Yes; the attacker's name turns red".to_owned(),
                    other => format!("Unknown rule {other}"),
                },
            ));
            return values;
        }
        values.push((
            "Aggressive",
            match number(&self.record, 696) {
                0 => "Passive".to_owned(),
                1 => "Aggressive".to_owned(),
                other => format!("Unknown mode {other}"),
            },
        ));
        // sELedit labels 0 as ranged and 1 as close range; the server turns a
        // non-zero value into the 4 m close-range attack distance.
        values.push((
            "Attack kind",
            match number(&self.record, 604) {
                0 => "Ranged".to_owned(),
                1 => "Close range".to_owned(),
                2 => "Assassin close range".to_owned(),
                other => format!("Unknown kind {other}"),
            },
        ));
        // `inhabit_type` is an enum, not a mask: the server's own placement
        // switch maps 0 and unknown values to the ground, 1 to water, 2 to the
        // air, 3 to water, 5 to the air, and 4 and 6 to the ground.
        values.push((
            "Habitat",
            match number(&self.record, 716) {
                0 => "Ground".to_owned(),
                1 => "Water".to_owned(),
                2 => "Air".to_owned(),
                3 => "Ground+Water".to_owned(),
                4 => "Ground+Air".to_owned(),
                5 => "Water+Sky".to_owned(),
                6 => "Ground+Water+Air".to_owned(),
                other => format!("Unknown habitat {other}"),
            },
        ));
        // The header names these 站立, 双足站立 and 悬空站立: standing, standing
        // on two legs, and standing in the air.
        values.push((
            "Stance",
            match number(&self.record, 724) {
                0 => "Standing".to_owned(),
                1 => "Biped standing".to_owned(),
                2 => "Hovering".to_owned(),
                other => format!("Unknown stance {other}"),
            },
        ));
        values.push(("Patrol mode", format_number(self.numeric(720))));
        // The header lists "no effect", 自爆 (self-destruct) and a third mode
        // that the record pairs with `after_death_summon_monster_*`.
        values.push((
            "On death",
            match number(&self.record, 968) {
                0 => "Nothing".to_owned(),
                1 => "Explodes".to_owned(),
                2 => "Summons".to_owned(),
                other => format!("Unknown outcome {other}"),
            },
        ));
        values.push((
            "Highest strategy frequency",
            if number(&self.record, 1508) != 0 {
                "Yes".to_owned()
            } else {
                "No".to_owned()
            },
        ));
        let stealth = (
            number(&self.record, 1516) as i32,
            number(&self.record, 1520) as i32,
        );
        if stealth != (0, 0) {
            values.push((
                "Stealth levels",
                format!("hides at {} · sees through {}", stealth.0, stealth.1),
            ));
        }
        values.push((
            "Accepts player buffs",
            if number(&self.record, 1512) != 0 {
                "No".to_owned()
            } else {
                "Yes".to_owned()
            },
        ));
        values.push((
            "Auto fight",
            if number(&self.record, 1524) != 0 {
                "Stays out of combat".to_owned()
            } else {
                "Enters combat".to_owned()
            },
        ));
        values
    }

    /// Services this template offers, resolved through their own tables.
    pub fn services(&self, db: &Database) -> Vec<Service> {
        let mut services = Vec::new();
        if self.kind != Kind::Npc {
            return services;
        }
        for (label, offset, table) in NPC_SERVICES {
            let id = number(&self.record, offset);
            if id == 0 {
                continue;
            }
            let record = db.services.get(&table).and_then(|table| table.get(&id));
            services.push(Service {
                label,
                offset,
                table: Some(table),
                id,
                name: record
                    .map(|record| wide(record, 4, 64).trim().to_string())
                    .unwrap_or_default(),
                detail: record
                    .map(|record| service_detail(table, record))
                    .unwrap_or_default(),
            });
        }
        let combined = number(&self.record, 844);
        for (bit, label) in COMBINED_SERVICES {
            if combined & (1 << bit) != 0 {
                services.push(Service {
                    label: "Combined service",
                    offset: 844,
                    table: None,
                    id: 1 << bit,
                    name: label.to_owned(),
                    detail: String::new(),
                });
            }
        }
        services
    }

    /// Drop table entries: (item id, probability).
    pub fn drops(&self) -> Vec<(u32, f32)> {
        let mut drops = Vec::new();
        if self.kind != Kind::Monster {
            return drops;
        }
        for index in 0..32 {
            let id = number(&self.record, 1252 + index * 8);
            if id != 0 {
                drops.push((id, decimal(&self.record, 1256 + index * 8)));
            }
        }
        drops
    }

    pub fn drop_counts(&self) -> (u32, [f32; 4]) {
        (
            number(&self.record, 1244),
            [0, 1, 2, 3].map(|index| decimal(&self.record, 1228 + index * 4)),
        )
    }

    /// (skill id, level, when) for the primary skill and the three
    /// life-threshold tables plus the general skill list.
    pub fn skills(&self) -> Vec<(u32, u32, &'static str)> {
        let mut skills = Vec::new();
        if self.kind != Kind::Monster {
            return skills;
        }
        let primary = number(&self.record, 684);
        if primary != 0 {
            skills.push((primary, number(&self.record, 688), "Primary"));
        }
        for (base, when) in [
            (788usize, "Below 75% life"),
            (848, "Below 50% life"),
            (908, "Below 25% life"),
        ] {
            for index in 0..5 {
                let id = number(&self.record, base + index * 12);
                if id != 0 {
                    skills.push((id, number(&self.record, base + index * 12 + 4), when));
                }
            }
        }
        for index in 0..32 {
            let id = number(&self.record, 972 + index * 8);
            if id != 0 {
                skills.push((id, number(&self.record, 976 + index * 8), "Known"));
            }
        }
        skills
    }

    /// Add-ons the mob's own `MONSTER_TYPE` rolls, as (name, chance, params).
    ///
    /// The parameters are the authored add-on arguments; they are printed
    /// rather than converted, because their meaning depends on the add-on.
    pub fn addons(&self, db: &Database) -> Vec<(String, f32, Vec<i32>)> {
        if self.kind != Kind::Monster {
            return Vec::new();
        }
        db.monster_type_addons
            .get(&self.type_id)
            .map(|addons| {
                addons
                    .iter()
                    .map(|(id, chance)| {
                        let (name, params) = db
                            .monster_addons
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| (format!("Add-on {id}"), Vec::new()));
                        (name, *chance, params)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Everything the details pane shows except the lists that carry links.
    pub fn sections(&self, db: &Database) -> Vec<(&'static str, Vec<String>)> {
        let mut sections: Vec<(&'static str, Vec<String>)> = Vec::new();
        let mut overview = vec![
            format!(
                "Template: {}_ESSENCE",
                if self.kind == Kind::Npc {
                    "NPC"
                } else {
                    "MONSTER"
                }
            ),
            format!(
                "Type: {} ({})",
                if self.type_name.is_empty() {
                    "unnamed".to_owned()
                } else {
                    self.type_name.clone()
                },
                self.type_id
            ),
        ];
        if let Some(placed) = db.placements(self.id) {
            overview.push(format!("Placed in the main world: {placed}"));
        } else if db.placements_loaded && self.kind == Kind::Npc {
            overview.push("Placed in the main world: not by task_npc.data".to_owned());
        }
        if let Some(source) = self.source_monster() {
            overview.push(format!(
                "Attributes taken from monster {source}: {}",
                db.by_id
                    .get(&source)
                    .map(|&index| db.entries[index].name.clone())
                    .unwrap_or_else(|| "not in this client".to_owned())
            ));
        }
        if let Some(egg) = self.egg() {
            overview.push(format!("Captured as pet egg {egg}"));
        }
        if self.model.is_empty() {
            overview.push("Model: this record names no rig".to_owned());
        } else {
            overview.push(format!("Model: {}", self.model));
        }
        let greeting = self.greeting();
        if !greeting.is_empty() {
            overview.push(format!(
                "Greeting: {}",
                super::descriptions::plain_text(&greeting)
            ));
        }
        let trait_line = self.trait_line();
        if !trait_line.is_empty() {
            overview.push(format!("Trait line: {trait_line}"));
        }
        sections.push(("Overview", overview));

        let attributes: Vec<String> = self
            .attributes()
            .into_iter()
            .map(|(name, value)| format!("{name}: {}", format_number(value)))
            .collect();
        if !attributes.is_empty() {
            sections.push(("Stats", attributes));
        }

        let behaviour: Vec<String> = self
            .behaviour()
            .into_iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect();
        if !behaviour.is_empty() {
            sections.push(("Behaviour", behaviour));
        }

        let services: Vec<String> = self
            .services(db)
            .into_iter()
            .map(|service| {
                let name = if service.name.is_empty() {
                    format!("service {} (missing from table)", service.id)
                } else {
                    format!("{} · {}", service.name, service.id)
                };
                let detail = if service.detail.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", service.detail)
                };
                format!("{}: {name}{detail}", service.label)
            })
            .collect();
        if !services.is_empty() {
            sections.push(("Services", services));
        }

        let addons: Vec<String> = self
            .addons(db)
            .into_iter()
            .map(|(name, chance, params)| {
                format!(
                    "{name} ({:.1}% of rolls){}{}",
                    chance * 100.,
                    if params.is_empty() {
                        ""
                    } else {
                        " — params "
                    },
                    params
                        .iter()
                        .map(i32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect();
        if !addons.is_empty() {
            sections.push(("Type add-ons", addons));
        }
        sections
    }
}

/// Short human summary of what a service table row offers, counted from its own
/// record rather than described in prose.
fn plural(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

fn service_detail(table: usize, record: &[u8]) -> String {
    // Number of `count` groups, `stride` bytes apart, whose `width` int fields
    // starting at `field` hold anything but zero.
    let groups = |start: usize, stride: usize, count: usize, field: usize, width: usize| {
        (0..count)
            .filter(|index| {
                (0..width)
                    .any(|slot| number(record, start + index * stride + field + slot * 4) != 0)
            })
            .count()
    };
    // A title-bearing page counts even when it lists no goods.
    let pages = |start: usize, stride: usize, count: usize| {
        (0..count)
            .filter(|page| {
                let base = start + page * stride;
                record
                    .get(base..base.saturating_add(16))
                    .is_some_and(|title| title.iter().any(|byte| *byte != 0))
                    || (0..32).any(|item| number(record, base + 16 + item * 4) != 0)
            })
            .count()
    };
    let dialog = number(record, record.len().saturating_sub(4));
    match table {
        41 => format!(
            "{} on {}",
            plural(groups(84, 144, 8, 0, 32), "good", "goods"),
            plural(pages(68, 144, 8), "page", "pages")
        ),
        44 | 45 => format!("{} of 32 goods slots", groups(68, 4, 32, 0, 1)),
        46 | 47 => plural(groups(68, 4, 256, 0, 1), "task", "tasks"),
        48 => format!(
            "{} with item requirements",
            plural(groups(68, 36, 16, 4, 8), "task", "tasks")
        ),
        49 | 106 => format!("{} of 128 skills", groups(68, 4, 128, 0, 1)),
        51 => plural(groups(72, 12, 32, 0, 1), "destination", "destinations"),
        52 => plural(groups(68, 8, 32, 0, 1), "route", "routes"),
        55 => format!(
            "{} on {}",
            plural(groups(92, 144, 8, 0, 32), "good", "goods"),
            plural(pages(76, 144, 8), "page", "pages")
        ),
        81 => format!("fee {}", number(record, 68)),
        102 => plural(groups(68, 20, 4, 0, 5), "build recipe", "build recipes"),
        104 => plural(
            groups(68, 20, 15, 0, 5),
            "attribute reset",
            "attribute resets",
        ),
        105 | 107 | 111 | 112 | 113 => format!(
            "token {} · price {}",
            number(record, 68),
            number(record, 72)
        ),
        40 | 42 | 43 | 50 | 53 => format!("dialog {dialog}"),
        56 => format!("decompose skill {}", number(record, 68)),
        _ => String::new(),
    }
}

/// Aligns numeric attributes across records, as the item comparison does.
pub fn compare_rows(
    entries: &[&Entry],
) -> std::collections::BTreeMap<&'static str, Vec<Option<f64>>> {
    let mut rows = std::collections::BTreeMap::new();
    for (column, entry) in entries.iter().enumerate() {
        for (name, value) in entry.attributes() {
            let values = rows
                .entry(name)
                .or_insert_with(|| vec![None; entries.len()]);
            values[column] = Some(value);
        }
    }
    rows
}

/// Fields where a smaller value is the better one; everything else rewards a
/// larger number.
pub fn best(name: &str, values: &[Option<f64>]) -> Option<f64> {
    if matches!(
        name,
        "Refresh time (s)" | "Tax rate (%)" | "Domain related" | "Staging post"
    ) {
        values.iter().flatten().copied().reduce(f64::min)
    } else {
        values.iter().flatten().copied().reduce(f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::super::elements;
    use super::*;

    fn npc_record(id: u32, name: &str) -> Vec<u8> {
        let mut record = vec![0; 852];
        record[0..4].copy_from_slice(&id.to_le_bytes());
        for (index, unit) in name.encode_utf16().enumerate() {
            record[4 + index * 2..6 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        record[68..72].copy_from_slice(&3214u32.to_le_bytes());
        let model = b"Models\\NPCs\\npc\\men4\\men4.ecm";
        record[80..80 + model.len()].copy_from_slice(model);
        record[212..216].copy_from_slice(&900u32.to_le_bytes());
        for (index, unit) in "Hello there".encode_utf16().enumerate() {
            record[216 + index * 2..218 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        record[740..744].copy_from_slice(&700u32.to_le_bytes());
        record[764..768].copy_from_slice(&701u32.to_le_bytes());
        record[844..848].copy_from_slice(&(1u32 << 6 | 1 << 14).to_le_bytes());
        record
    }

    fn monster_record(id: u32) -> Vec<u8> {
        let mut record = vec![0; 1528];
        record[0..4].copy_from_slice(&id.to_le_bytes());
        record[4..8].copy_from_slice(&14u32.to_le_bytes());
        for (index, unit) in "Tauroc".encode_utf16().enumerate() {
            record[8 + index * 2..10 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        for (index, unit) in "+WT-ER".encode_utf16().enumerate() {
            record[104 + index * 2..106 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let model = b"Models\\NPCs\\mob\\ox.ecm";
        record[144..144 + model.len()].copy_from_slice(model);
        record[544..548].copy_from_slice(&48u32.to_le_bytes());
        // The captured pet egg, not the life total, sits at 552.
        record[552..556].copy_from_slice(&960u32.to_le_bytes());
        record[556..560].copy_from_slice(&10_234u32.to_le_bytes());
        record[696..700].copy_from_slice(&1u32.to_le_bytes());
        record[1252..1256].copy_from_slice(&42u32.to_le_bytes());
        record[1256..1260].copy_from_slice(&0.25f32.to_le_bytes());
        record[1244..1248].copy_from_slice(&1u32.to_le_bytes());
        record
    }

    fn synthetic() -> Vec<u8> {
        let mut type_record = vec![0; 68];
        type_record[0..4].copy_from_slice(&3214u32.to_le_bytes());
        for (index, unit) in "Normal".encode_utf16().enumerate() {
            type_record[4 + index * 2..6 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let mut monster_type = vec![0; 196];
        monster_type[0..4].copy_from_slice(&14u32.to_le_bytes());
        for (index, unit) in "Ox".encode_utf16().enumerate() {
            monster_type[4 + index * 2..6 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        monster_type[68..72].copy_from_slice(&503u32.to_le_bytes());
        monster_type[72..76].copy_from_slice(&0.5f32.to_le_bytes());
        let mut addon = vec![0; 84];
        addon[0..4].copy_from_slice(&503u32.to_le_bytes());
        for (index, unit) in "Scud".encode_utf16().enumerate() {
            addon[4 + index * 2..6 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        addon[68..72].copy_from_slice(&1u32.to_le_bytes());
        addon[72..76].copy_from_slice(&1u32.to_le_bytes());
        let mut sell = vec![0; 1224];
        sell[0..4].copy_from_slice(&700u32.to_le_bytes());
        for (index, unit) in "Goods".encode_utf16().enumerate() {
            sell[4 + index * 2..6 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        sell[84..88].copy_from_slice(&42u32.to_le_bytes());
        let mut task_in = vec![0; 1092];
        task_in[0..4].copy_from_slice(&701u32.to_le_bytes());
        task_in[68..72].copy_from_slice(&439u32.to_le_bytes());
        elements::synthetic_records(&[
            (37, vec![addon]),
            (38, vec![monster_type]),
            (39, vec![monster_record(900)]),
            (41, vec![sell]),
            (46, vec![task_in]),
            (57, vec![type_record]),
            (58, vec![npc_record(2109, "Apothecary Cheng")]),
        ])
    }

    #[test]
    fn reads_npc_fields_services_and_mob_link() {
        let db = Database::parse(&synthetic()).unwrap();
        let npc = &db.entries[db.by_id[&2109]];
        assert_eq!(npc.kind, Kind::Npc);
        assert_eq!(npc.name, "Apothecary Cheng");
        assert_eq!(npc.model, "models/npcs/npc/men4/men4.ecm");
        assert_eq!(npc.type_name, "Normal");
        assert_eq!(npc.greeting(), "Hello there");
        assert_eq!(npc.source_monster(), Some(900));
        let services = npc.services(&db);
        let sell = services.iter().find(|s| s.label == "Sell goods").unwrap();
        assert_eq!(sell.name, "Goods");
        assert_eq!(sell.detail, "1 good on 1 page");
        let turn_in = services
            .iter()
            .find(|s| s.label == "Turn in tasks")
            .unwrap();
        assert_eq!(turn_in.detail, "1 task");
        let combined: Vec<&str> = services
            .iter()
            .filter(|s| s.table.is_none())
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            combined,
            vec!["Mail", "Upgrade equipment"],
            "bits 6 and 14 are the authored combined services"
        );
    }

    #[test]
    fn reads_monster_life_from_its_own_field() {
        let db = Database::parse(&synthetic()).unwrap();
        let monster = &db.entries[db.by_id[&900]];
        assert_eq!(monster.kind, Kind::Monster);
        assert_eq!(monster.name, "Tauroc");
        assert_eq!(monster.model, "models/npcs/mob/ox.ecm");
        assert_eq!(monster.type_name, "Ox");
        assert_eq!(monster.trait_line(), "+WT-ER");
        assert_eq!(monster.egg(), Some(960));
        let life = monster
            .attributes()
            .into_iter()
            .find(|(name, _)| *name == "Life")
            .unwrap();
        assert_eq!(life.1, 10_234.);
        assert_eq!(monster.drops(), vec![(42, 0.25)]);
        assert_eq!(monster.addons(&db), vec![("Scud".to_owned(), 0.5, vec![1])]);
    }

    #[test]
    fn placements_come_from_the_clients_own_table() {
        let mut db = Database::parse(&synthetic()).unwrap();
        let bytes = |count: u16| {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&(12 + 12 * count as u32).to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&count.to_le_bytes());
            for _ in 0..count {
                bytes.extend_from_slice(&2109u32.to_le_bytes());
                bytes.extend_from_slice(&[0; 8]);
            }
            bytes
        };
        assert!(db.attach_placements(&bytes(2)));
        assert_eq!(db.placements(2109), Some(2));
        // A table from another build is refused rather than counted.
        let mut truncated = bytes(2);
        truncated.pop();
        assert!(!db.attach_placements(&truncated));
    }

    #[test]
    fn habitat_labels_follow_the_servers_own_switch() {
        // Values 0-6 are what the shipped data uses; the labels are the
        // client's own strings, not bit arithmetic.
        let mut record = monster_record(900);
        let mut labels = Vec::new();
        for habitat in 0..7u32 {
            record[716..720].copy_from_slice(&habitat.to_le_bytes());
            let entry = Entry {
                id: 900,
                kind: Kind::Monster,
                name: String::new(),
                search_name: String::new(),
                model: String::new(),
                type_id: 14,
                type_name: String::new(),
                record: record.clone(),
            };
            let line = entry
                .behaviour()
                .into_iter()
                .find(|(name, _)| *name == "Habitat")
                .map(|(_, value)| value)
                .expect("habitat is always listed");
            labels.push(line);
        }
        assert_eq!(
            labels,
            vec![
                "Ground",
                "Water",
                "Air",
                "Ground+Water",
                "Ground+Air",
                "Water+Sky",
                "Ground+Water+Air",
            ]
        );
        // The tax field is already a percentage, not a fraction.
        let mut npc = npc_record(2109, "Taxed");
        npc[208..212].copy_from_slice(&3.7616f32.to_le_bytes());
        let entry = Entry {
            id: 2109,
            kind: Kind::Npc,
            name: String::new(),
            search_name: String::new(),
            model: String::new(),
            type_id: 3214,
            type_name: String::new(),
            record: npc,
        };
        let tax = entry
            .attributes()
            .into_iter()
            .find(|(name, _)| *name == "Tax rate (%)")
            .map(|(_, value)| value);
        assert!((tax.expect("tax is always listed") - 3.7616).abs() < 1e-4);
    }

    #[test]
    fn comparison_aligns_only_the_fields_a_record_has() {
        let db = Database::parse(&synthetic()).unwrap();
        let npc = &db.entries[db.by_id[&2109]];
        let monster = &db.entries[db.by_id[&900]];
        let rows = compare_rows(&[npc, monster]);
        assert_eq!(rows["Life"], vec![None, Some(10_234.)]);
        assert_eq!(rows["Tax rate (%)"], vec![Some(0.), None]);
        assert_eq!(best("Life", &rows["Life"]), Some(10_234.));
        assert_eq!(best("Refresh time (s)", &[Some(5.), Some(2.)]), Some(2.));
    }
}
