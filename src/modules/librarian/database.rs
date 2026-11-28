//! Initial v27 item index. Record interpretation remains deliberately separate
//! from the archive layer so the database can be tested with synthetic files.
use super::elements::visit_v27_records;
use encoding_rs::GBK;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Category {
    Weapons,
    Armor,
    Ornaments,
    Fashion,
    Flight,
    PetsAndMounts,
    Genies,
    Consumables,
    GemsBooksAmmo,
    Materials,
    QuestAndPacks,
}

#[derive(Debug)]
pub struct Item {
    pub id: u32,
    pub section: usize,
    pub category: Category,
    pub major: Option<u32>,
    pub subtype: Option<u32>,
    pub name: String,
    pub search_name: String,
    pub icon: String,
    pub model: String,
    pub attack_interval: Option<f32>,
    /// Raw v27 fields are retained for subsequent typed interpretation.
    pub record: Vec<u8>,
}

#[derive(Default)]
pub struct Database {
    pub items: Vec<Item>,
    pub by_id: HashMap<u32, usize>,
    pub category_names: HashMap<(usize, u32), String>,
    pub weapon_intervals: HashMap<u32, f32>,
    pub addons: HashMap<u32, Vec<u8>>,
    pub suites: HashMap<u32, Vec<u8>>,
    pub pets: HashMap<u32, Vec<u8>>,
    pub hair_skins: HashMap<u32, String>,
    pub face_textures: HashMap<u32, String>,
    pub descriptions: HashMap<u32, String>,
    pub addon_types: HashMap<u32, u32>,
    pub colors: HashMap<u32, u32>,
    pub skill_names: HashMap<u32, String>,
}

fn number(record: &[u8], offset: usize) -> u32 {
    record
        .get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .unwrap_or(0)
}
fn wide(record: &[u8], offset: usize) -> String {
    let Some(bytes) = record.get(offset..offset + 64) else {
        return String::new();
    };
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .take_while(|c| *c != 0)
        .collect();
    String::from_utf16_lossy(&units)
}
fn path(record: &[u8], offset: usize) -> String {
    let Some(bytes) = record.get(offset..offset + 128) else {
        return String::new();
    };
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    crate::ecmodel::normalize(&GBK.decode(&bytes[..end]).0)
}

// (category, major offset, subtype offset, name offset, icon offset, model offset)
fn layout(
    section: usize,
) -> Option<(
    Category,
    Option<usize>,
    Option<usize>,
    usize,
    usize,
    Option<usize>,
)> {
    use Category::*;
    let item = match section {
        3 => (Weapons, Some(4), Some(8), 12, 464, Some(80)),
        6 => (Armor, Some(4), Some(8), 12, 236, None),
        9 => (Ornaments, Some(4), Some(8), 12, 332, Some(76)),
        84 => (Fashion, Some(4), Some(8), 12, 236, None),
        12 => (Consumables, Some(4), Some(8), 12, 204, None),
        15 => (Materials, Some(4), Some(8), 12, 204, None),
        87 | 90 => (Consumables, Some(4), Some(8), 12, 204, None),
        17 | 19 | 22 | 34 | 36 => (GemsBooksAmmo, None, Some(4), 8, 200, None),
        32 => (GemsBooksAmmo, None, Some(4), 8, 328, Some(72)),
        123 => (Genies, None, Some(4), 8, 200, None),
        23 | 24 => (Flight, None, None, 4, 324, Some(68)),
        29 => (QuestAndPacks, None, None, 4, 68, None),
        30 => (Materials, None, None, 4, 324, Some(68)),
        75 | 76 | 110 | 126 | 127 => (QuestAndPacks, None, None, 4, 196, None),
        96 | 97 | 98 => (PetsAndMounts, None, None, 4, 196, None),
        114 => (GemsBooksAmmo, None, None, 4, 196, None),
        120 => (Fashion, None, None, 4, 196, None),
        121 => (Genies, None, None, 4, 708, Some(68)),
        124 => (Genies, None, None, 4, 196, None),
        125 => (QuestAndPacks, None, None, 4, 200, None),
        25 | 26 | 27 | 28 | 99 | 100 | 108 | 109 | 115 | 116 | 117 | 118 | 119 => {
            (Consumables, None, None, 4, 196, None)
        }
        _ => return None,
    };
    Some(item)
}

impl Database {
    pub fn parse_v27(bytes: &[u8]) -> Result<Self, String> {
        let mut db = Self::default();
        visit_v27_records(bytes, |section, record| {
            let id = number(record, 0);
            match section {
                0 => {
                    db.addons.insert(id, record.to_vec());
                }
                91 => {
                    db.suites.insert(id, record.to_vec());
                }
                95 => {
                    db.pets.insert(id, record.to_vec());
                }
                64 => {
                    db.hair_skins.insert(id, path(record, 68));
                }
                60 => {
                    db.face_textures.insert(id, path(record, 68));
                }
                1 | 2 | 4 | 5 | 7 | 8 | 10 | 11 | 13 | 14 | 21 | 31 | 35 | 82 | 83 | 94 | 122 => {
                    db.category_names.insert((section, id), wide(record, 4));
                    if section == 2 {
                        let interval = f32::from_bits(number(record, 344));
                        if interval.is_finite() && interval > 0. {
                            db.weapon_intervals.insert(id, interval);
                        }
                    }
                }
                _ => {}
            }
            if let Some((category, major, subtype, name, icon, model)) = layout(section) {
                let name = wide(record, name);
                let item = Item {
                    id,
                    section,
                    category,
                    major: major.map(|offset| number(record, offset)),
                    subtype: subtype.map(|offset| number(record, offset)),
                    search_name: name.to_lowercase(),
                    name,
                    icon: path(record, icon),
                    model: model.map(|offset| path(record, offset)).unwrap_or_default(),
                    attack_interval: None,
                    record: record.to_vec(),
                };
                let mut item = item;
                if section == 3 && (item.model.is_empty() || item.model == "0") {
                    item.model = path(record, 208);
                }
                db.by_id.entry(id).or_insert(db.items.len());
                db.items.push(item);
            }
            Ok(())
        })?;
        // Eggs point to the creature definition, whose model can be previewed.
        for item in &mut db.items {
            if item.section == 3 {
                item.attack_interval = item
                    .subtype
                    .and_then(|id| db.weapon_intervals.get(&id))
                    .copied();
            }
            if item.section == 96 {
                if let Some(pet) = db.pets.get(&number(&item.record, 324)) {
                    item.model = path(pet, 72);
                }
            }
        }
        Ok(db)
    }
    pub fn pet_info(&self, item: &Item) -> Option<(String, String, u32, f32)> {
        if item.section != 96 {
            return None;
        }
        let pet = self.pets.get(&number(&item.record, 324))?;
        let kind = number(pet, 4);
        let label = self
            .category_names
            .get(&(94, kind))
            .cloned()
            .unwrap_or_else(|| format!("Type {kind}"));
        Some((
            wide(pet, 8),
            label,
            number(pet, 336),
            f32::from_bits(number(pet, 384)),
        ))
    }
    pub fn addon_label(&self, addon_id: u32) -> String {
        let kind = self.addon_types.get(&addon_id).copied();
        let Some(record) = self.addons.get(&addon_id) else {
            return format!("Add-on {addon_id} (record missing)");
        };
        let count = number(record, 68).min(3) as usize;
        let params: Vec<i32> = (0..count)
            .map(|index| number(record, 72 + index * 4) as i32)
            .collect();
        if let Some(text) = kind.and_then(|kind| super::descriptions::describe_addon(kind, &params))
        {
            return format!("{text} (add-on {addon_id})");
        }
        if kind == Some(55) {
            if let Some(name) = params
                .first()
                .and_then(|id| u32::try_from(*id).ok())
                .and_then(|id| self.skill_names.get(&id.saturating_mul(10)))
            {
                let level = params
                    .get(1)
                    .filter(|&&level| level > 0)
                    .map(|level| format!(" Lv. {level}"))
                    .unwrap_or_default();
                return format!(
                    "Skill: {}{level} (add-on {addon_id})",
                    super::descriptions::plain_text(name)
                );
            }
        }
        format!(
            "Add-on {addon_id}{}: {:?} (raw)",
            kind.map(|value| format!(" · type {value}"))
                .unwrap_or_default(),
            params
        )
    }
    pub fn suite_addons(&self, suite_id: u32) -> Vec<String> {
        let Some(record) = self.suites.get(&suite_id) else {
            return Vec::new();
        };
        (0..11)
            .map(|index| number(record, 120 + index * 4))
            .filter(|id| *id != 0)
            .map(|id| self.addon_label(id))
            .collect()
    }
    pub fn suites_for(&self, id: u32) -> Vec<(u32, String, Vec<u32>)> {
        let mut result = Vec::new();
        if id == 0 {
            return result;
        }
        for (suite_id, record) in &self.suites {
            if !(0..12).any(|index| number(record, 72 + index * 4) == id) {
                continue;
            }
            let members: Vec<u32> = (0..12)
                .map(|index| number(record, 72 + index * 4))
                .filter(|member| *member != 0)
                .collect();
            result.push((*suite_id, wide(record, 4), members));
        }
        result.sort_unstable_by(|a, b| a.1.cmp(&b.1));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_and_truncated_files() {
        assert!(Database::parse_v27(&[]).is_err());
        // A neighbouring revision is refused with its own version number, so
        // the Librarian can say "v26" instead of "unexpected hex word".
        // `Database` has no `Debug`, so match rather than `unwrap_err()`.
        let error = match Database::parse_v27(&0x3000_001au32.to_le_bytes()) {
            Ok(_) => panic!("a v26 file must not parse as v27"),
            Err(error) => error,
        };
        assert!(error.contains("v26"), "{error}");
        assert!(error.contains("not the v27 layout"), "{error}");
        assert!(
            !error.contains("0x3000"),
            "family marker is not part of the version: {error}"
        );
        let mut data = 0x3000_001bu32.to_le_bytes().to_vec();
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(Database::parse_v27(&data).is_err());
    }
    #[test]
    fn reads_weapon_from_a_complete_synthetic_file() {
        let mut record = vec![0; 1412];
        record[0..4].copy_from_slice(&42u32.to_le_bytes());
        record[4..8].copy_from_slice(&1u32.to_le_bytes());
        record[8..12].copy_from_slice(&2u32.to_le_bytes());
        for (index, word) in "Sword".encode_utf16().enumerate() {
            record[12 + index * 2..14 + index * 2].copy_from_slice(&word.to_le_bytes());
        }
        record[464..472].copy_from_slice(b"test.dds");
        let bytes = super::super::elements::synthetic_file(3, &record);
        let db = Database::parse_v27(&bytes).unwrap();
        assert_eq!(db.items.len(), 1);
        let item = &db.items[db.by_id[&42]];
        assert_eq!(item.name, "Sword");
        assert_eq!(item.icon, "test.dds");
        assert_eq!(item.major, Some(1));
        assert_eq!(item.subtype, Some(2));
        assert_eq!(item.category, Category::Weapons);
    }
    #[test]
    fn weapon_addon_groups_keep_independent_weights() {
        let mut record = vec![0; 1412];
        record[728..732].copy_from_slice(&7u32.to_le_bytes());
        record[732..736].copy_from_slice(&0.25f32.to_le_bytes());
        record[984..988].copy_from_slice(&8u32.to_le_bytes());
        record[988..992].copy_from_slice(&0.5f32.to_le_bytes());
        let db = Database::parse_v27(&super::super::elements::synthetic_file(3, &record)).unwrap();
        let groups = db.items[0].addon_groups();
        assert_eq!(
            groups,
            vec![("Drop", vec![(7, 0.25)]), ("Craft", vec![(8, 0.5)])]
        );
    }
    #[test]
    fn egg_prices_and_loyalty_use_separate_fields() {
        let mut record = vec![0; 628];
        record[328..332].copy_from_slice(&100u32.to_le_bytes());
        record[336..340].copy_from_slice(&55u32.to_le_bytes());
        let db = Database::parse_v27(&super::super::elements::synthetic_file(96, &record)).unwrap();
        let attributes = db.items[0].attributes();
        assert!(attributes.contains(&("Incubation price", 100.)));
        assert!(attributes.contains(&("Loyalty", 55.)));
    }
    #[test]
    fn quiver_and_pet_item_links_are_preserved() {
        let mut quiver = vec![0; 340];
        quiver[328..332].copy_from_slice(&456u32.to_le_bytes());
        let db = Database::parse_v27(&super::super::elements::synthetic_file(34, &quiver)).unwrap();
        assert_eq!(db.items[0].contained_item(), Some(456));
        let mut egg = vec![0; 628];
        egg[352..356].copy_from_slice(&89u32.to_le_bytes());
        egg[356..360].copy_from_slice(&3u32.to_le_bytes());
        let db = Database::parse_v27(&super::super::elements::synthetic_file(96, &egg)).unwrap();
        assert_eq!(db.items[0].pet_skills(), vec![(89, 3)]);
    }
    #[test]
    fn dye_control_only_for_fashion_with_dye_count() {
        let mut fashion = vec![0; 412];
        fashion[376..380].copy_from_slice(&2u32.to_le_bytes());
        let db =
            Database::parse_v27(&super::super::elements::synthetic_file(84, &fashion)).unwrap();
        assert!(db.items[0].dyeable());
        let mut weapon = vec![0; 1412];
        weapon[376..380].copy_from_slice(&2u32.to_le_bytes());
        let db = Database::parse_v27(&super::super::elements::synthetic_file(3, &weapon)).unwrap();
        assert!(!db.items[0].dyeable());
    }

    #[test]
    fn tomes_and_dyes_are_in_their_relevant_categories() {
        assert_eq!(layout(114).unwrap().0, Category::GemsBooksAmmo);
        assert_eq!(layout(120).unwrap().0, Category::Fashion);
    }
    #[test]
    fn authentic_client_catalog_when_available() {
        let Ok(path) = std::env::var("PW_LIBRARIAN_ELEMENTS") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read PW_LIBRARIAN_ELEMENTS");
        let db = Database::parse_v27(&bytes).expect("parse authentic v27 elements.data");
        // Client builds may contain additional item tables or records. The
        // reference 1.4.2 catalog must include at least the requested 12,922.
        assert!(
            db.items.len() >= 12_922,
            "unexpectedly small v27 item catalog: {}",
            db.items.len()
        );
        assert_eq!(
            db.by_id.len(),
            db.items.len(),
            "duplicate item IDs in the reference catalog"
        );
        let weapon = &db.items[*db.by_id.get(&6).expect("weapon 6")];
        assert_eq!(weapon.category, Category::Weapons);
        assert!(!weapon.name.is_empty());
    }
    #[test]
    fn known_layouts_use_correct_name_and_icon_offsets() {
        let (category, _, _, name, icon, _) = layout(3).unwrap();
        assert_eq!(category, Category::Weapons);
        assert_eq!((name, icon), (12, 464));
        assert!(layout(1).is_none());
    }
}

pub fn section_name(section: usize) -> &'static str {
    match section {
        3 => "Weapons",
        6 => "Armor",
        9 => "Ornaments",
        12 => "Medicine",
        15 => "Materials",
        17 => "Weapon runes",
        19 => "Armor runes",
        22 => "Skill books",
        23 => "Fly swords",
        24 => "Wings",
        25 => "Town scrolls",
        26 => "Union scrolls",
        27 => "Revival scrolls",
        28 => "Elements",
        29 => "Quest items",
        30 => "Thrown items",
        32 => "Ammunition",
        34 => "Quivers",
        36 => "Gems",
        75 => "Task dice",
        76 => "Task items",
        84 => "Fashion",
        87 => "Appearance tickets",
        90 => "Appearance pills",
        96 => "Pet eggs",
        97 => "Pet food",
        98 => "Pet tickets",
        99 => "Fireworks",
        100 => "War machines",
        108 => "Skill items",
        109 => "Refine tickets",
        110 => "Destroy items",
        114 => "Tomes",
        115 => "Speakers",
        116 => "Auto HP",
        117 => "Auto MP",
        118 => "Double experience",
        119 => "Teleport scrolls",
        120 => "Dye",
        121 => "Genies",
        123 => "Genie equipment",
        124 => "Genie experience",
        125 => "Certificates",
        126 => "Target items",
        127 => "Information items",
        _ => "Other items",
    }
}
pub fn subtype_table(section: usize) -> Option<usize> {
    match section {
        3 => Some(2),
        6 => Some(5),
        9 => Some(8),
        12 => Some(11),
        15 => Some(14),
        22 => Some(21),
        32 => Some(31),
        36 => Some(35),
        84 => Some(83),
        123 => Some(122),
        _ => None,
    }
}
pub fn major_table(section: usize) -> Option<usize> {
    match section {
        3 => Some(1),
        6 => Some(4),
        9 => Some(7),
        12 => Some(10),
        15 => Some(13),
        84 => Some(82),
        _ => None,
    }
}

impl Category {
    pub const ALL: [Self; 11] = [
        Self::Weapons,
        Self::Armor,
        Self::Ornaments,
        Self::Fashion,
        Self::Flight,
        Self::PetsAndMounts,
        Self::Genies,
        Self::Consumables,
        Self::GemsBooksAmmo,
        Self::Materials,
        Self::QuestAndPacks,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Weapons => "Weapons",
            Self::Armor => "Armor",
            Self::Ornaments => "Ornaments",
            Self::Fashion => "Fashion",
            Self::Flight => "Flight",
            Self::PetsAndMounts => "Pets & Mounts",
            Self::Genies => "Genies",
            Self::Consumables => "Consumables",
            Self::GemsBooksAmmo => "Gems, Books & Ammo",
            Self::Materials => "Materials",
            Self::QuestAndPacks => "Quest & Packs",
        }
    }
}

impl Item {
    /// Only fields whose interpretation is known are exposed. Unknown fields
    /// are deliberately not guessed from their raw binary representation.
    /// Each entry is an ID and the client's authored selection weight.
    pub fn addon_groups(&self) -> Vec<(&'static str, Vec<(u32, f32)>)> {
        let groups: &[(&str, usize, usize)] = match self.section {
            3 => &[("Drop", 728, 32), ("Craft", 984, 32), ("Unique", 1240, 16)],
            6 => &[("Drop", 556, 32), ("Craft", 812, 32)],
            9 => &[("Drop", 608, 32), ("Craft", 864, 32)],
            _ => &[],
        };
        groups
            .iter()
            .filter_map(|&(name, start, count)| {
                let entries: Vec<_> = (0..count)
                    .filter_map(|index| {
                        let offset = start + index * 8;
                        let id = number(&self.record, offset);
                        let weight = f32::from_bits(number(&self.record, offset + 4));
                        if id == 0 {
                            None
                        } else {
                            Some((id, weight))
                        }
                    })
                    .collect();
                if entries.is_empty() {
                    None
                } else {
                    Some((name, entries))
                }
            })
            .collect()
    }

    pub fn contained_item(&self) -> Option<u32> {
        if self.section != 34 {
            return None;
        }
        let id = number(&self.record, 328);
        if id == 0 {
            None
        } else {
            Some(id)
        }
    }
    pub fn fixed_addons(&self) -> Vec<(&'static str, u32)> {
        let offsets: Vec<(&str, usize)> = match self.section {
            36 => vec![("Weapon", 352), ("Armor", 356)],
            114 => (0..10).map(|index| ("Tome", 324 + index * 4)).collect(),
            _ => Vec::new(),
        };
        offsets
            .into_iter()
            .filter_map(|(name, offset)| {
                let id = number(&self.record, offset);
                if id == 0 {
                    None
                } else {
                    Some((name, id))
                }
            })
            .collect()
    }
    pub fn pet_skills(&self) -> Vec<(u32, u32)> {
        if self.section != 96 {
            return Vec::new();
        }
        (0..32)
            .filter_map(|index| {
                let id = number(&self.record, 352 + 8 * index);
                let level = number(&self.record, 356 + 8 * index);
                if id == 0 {
                    None
                } else {
                    Some((id, level))
                }
            })
            .collect()
    }
    pub fn class_mask(&self) -> Option<u32> {
        match self.section {
            3 => Some(number(&self.record, 608)),
            6 => Some(number(&self.record, 388)),
            9 => Some(number(&self.record, 480)),
            23 => Some(number(&self.record, 500)),
            _ => None,
        }
    }
    pub fn fashion_gender(&self) -> Option<u32> {
        if self.section == 84 {
            Some(number(&self.record, 388))
        } else {
            None
        }
    }

    pub fn head_hair(&self) -> Option<(u32, u32)> {
        if number(&self.record, 364) != 9 {
            return None;
        }
        let (hair, texture) = match self.section {
            6 => (number(&self.record, 1100), number(&self.record, 1104)),
            84 => (number(&self.record, 392), number(&self.record, 396)),
            _ => return None,
        };
        (hair != 0).then_some((hair, texture))
    }

    pub fn dyeable(&self) -> bool {
        self.section == 84 && number(&self.record, 376) > 0
    }

    pub fn equipment_name(&self) -> Option<String> {
        if !matches!(self.section, 6 | 84) {
            return None;
        }
        let bytes = self.record.get(76..108)?;
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        let name = GBK.decode(&bytes[..end]).0.into_owned();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }

    pub fn attributes(&self) -> Vec<(&'static str, f64)> {
        let fields: &[(&str, usize)] = match self.section {
            3 => &[
                ("Strength", 592),
                ("Dexterity", 596),
                ("Magic", 600),
                ("Vitality", 604),
                ("Required level", 612),
                ("Grade", 616),
                ("Physical attack min", 624),
                ("Physical attack max (min)", 628),
                ("Physical attack max (max)", 632),
                ("Magic attack min", 636),
                ("Magic attack max (min)", 640),
                ("Magic attack max (max)", 644),
                ("Durability min", 656),
                ("Durability max", 660),
                ("Sell price", 672),
                ("Buy price", 676),
                ("Repair price", 680),
            ],
            6 => &[
                ("Grade", 368),
                ("Strength", 372),
                ("Dexterity", 376),
                ("Magic", 380),
                ("Vitality", 384),
                ("Required level", 392),
                ("Physical defense min", 400),
                ("Physical defense max", 404),
                ("Metal defense min", 408),
                ("Metal defense max", 412),
                ("Wood defense min", 416),
                ("Wood defense max", 420),
                ("Water defense min", 424),
                ("Water defense max", 428),
                ("Fire defense min", 432),
                ("Fire defense max", 436),
                ("Earth defense min", 440),
                ("Earth defense max", 444),
                ("MP min", 448),
                ("MP max", 452),
                ("HP min", 456),
                ("HP max", 460),
                ("Evasion min", 464),
                ("Evasion max", 468),
                ("Durability min", 472),
                ("Durability max", 476),
                ("Sell price", 488),
                ("Buy price", 492),
                ("Repair price", 496),
            ],
            9 => &[
                ("Grade", 460),
                ("Strength", 464),
                ("Dexterity", 468),
                ("Magic", 472),
                ("Vitality", 476),
                ("Required level", 484),
                ("Physical attack min", 492),
                ("Physical attack max", 496),
                ("Magic attack min", 500),
                ("Magic attack max", 504),
                ("Physical defense min", 508),
                ("Physical defense max", 512),
                ("Metal defense min", 516),
                ("Metal defense max", 520),
                ("Wood defense min", 524),
                ("Wood defense max", 528),
                ("Water defense min", 532),
                ("Water defense max", 536),
                ("Fire defense min", 540),
                ("Fire defense max", 544),
                ("Earth defense min", 548),
                ("Earth defense max", 552),
                ("Evasion min", 556),
                ("Evasion max", 560),
                ("Durability min", 564),
                ("Durability max", 568),
                ("Sell price", 580),
                ("Buy price", 584),
                ("Repair price", 588),
            ],
            84 => &[
                ("Item level", 368),
                ("Required level", 372),
                ("Dye count", 376),
                ("Sell price", 380),
                ("Buy price", 384),
                ("Stack limit", 400),
            ],
            12 => &[
                ("Required level", 332),
                ("Cooldown (ms)", 336),
                ("HP recovery", 340),
                ("MP recovery", 348),
                ("Sell price", 356),
                ("Buy price", 360),
                ("Stack limit", 364),
            ],
            15 => &[
                ("Sell price", 332),
                ("Buy price", 336),
                ("Stack limit", 356),
            ],
            23 => &[
                ("Sell price", 452),
                ("Buy price", 456),
                ("Grade", 460),
                ("Required level", 464),
            ],
            24 => &[
                ("Sell price", 452),
                ("Buy price", 456),
                ("Required level", 460),
            ],
            17 => &[
                ("Sell price", 332),
                ("Buy price", 336),
                ("Weapon level min", 340),
                ("Weapon level max", 344),
                ("Bonus", 348),
                ("Stack limit", 352),
            ],
            19 => &[
                ("Sell price", 588),
                ("Buy price", 592),
                ("Required level min", 596),
                ("Required level max", 600),
                ("Uses", 608),
                ("Stack limit", 612),
            ],
            22 => &[
                ("Sell price", 328),
                ("Buy price", 332),
                ("Stack limit", 336),
            ],
            25 | 26 => &[
                ("Sell price", 328),
                ("Buy price", 332),
                ("Stack limit", 336),
            ],
            27 => &[
                ("Cooldown (ms)", 328),
                ("Sell price", 332),
                ("Buy price", 336),
                ("Stack limit", 340),
            ],
            28 => &[
                ("Grade", 324),
                ("Sell price", 328),
                ("Buy price", 332),
                ("Stack limit", 336),
            ],
            29 => &[("Stack limit", 196)],
            34 => &[
                ("Contained item ID", 328),
                ("Amount min", 332),
                ("Amount max", 336),
            ],
            96 => &[
                ("Pet ID", 324),
                ("Incubation price", 328),
                ("Recovery price", 332),
                ("Loyalty", 336),
                ("Required level", 340),
                ("Pet experience", 344),
                ("Skill points", 348),
                ("Sell price", 608),
                ("Buy price", 612),
                ("Stack limit", 616),
            ],
            108 => &[
                ("Required level", 324),
                ("Skill ID", 328),
                ("Skill level", 332),
            ],
            114 => &[
                ("Sell price", 364),
                ("Buy price", 368),
                ("Stack limit", 380),
            ],
            115 => &[
                ("Sell price", 328),
                ("Buy price", 332),
                ("Stack limit", 336),
            ],
            116 => &[
                ("HP recovery", 324),
                ("Cooldown (ms)", 332),
                ("Sell price", 336),
                ("Buy price", 340),
                ("Stack limit", 344),
            ],
            117 => &[
                ("MP recovery", 324),
                ("Cooldown (ms)", 332),
                ("Sell price", 336),
                ("Buy price", 340),
                ("Stack limit", 344),
            ],
            118 => &[
                ("Duration", 324),
                ("Sell price", 328),
                ("Buy price", 332),
                ("Stack limit", 336),
            ],
            119 => &[
                ("Sell price", 324),
                ("Buy price", 328),
                ("Stack limit", 332),
            ],
            120 => &[
                ("Sell price", 348),
                ("Buy price", 352),
                ("Stack limit", 356),
            ],
            121 => &[
                ("Genie strength", 1224),
                ("Genie dexterity", 1228),
                ("Genie magic", 1232),
                ("Genie vitality", 1236),
                ("Max refine level", 1240),
                ("Sell price", 1244),
                ("Buy price", 1248),
            ],
            124 => &[
                ("Experience", 324),
                ("Required level", 328),
                ("Sell price", 332),
                ("Buy price", 336),
                ("Stack limit", 340),
            ],
            36 => &[
                ("Required level", 328),
                ("Sell price", 336),
                ("Buy price", 340),
                ("Stack limit", 424),
            ],
            123 => &[
                ("Required level", 332),
                ("Genie strength bonus", 336),
                ("Genie dexterity bonus", 340),
                ("Genie magic bonus", 344),
                ("Genie vitality bonus", 348),
                ("Genie metal affinity", 352),
                ("Genie wood affinity", 356),
                ("Genie water affinity", 360),
                ("Genie fire affinity", 364),
                ("Genie earth affinity", 368),
                ("Sell price", 372),
                ("Buy price", 376),
                ("Stack limit", 380),
            ],
            _ => &[],
        };
        let mut result: Vec<_> = fields
            .iter()
            .map(|(name, offset)| (*name, number(&self.record, *offset) as i32 as f64))
            .collect();
        if let Some(interval) = self.attack_interval {
            result.push(("Attack interval (s)", interval as f64));
            result.push(("Attacks per second", 1. / interval as f64));
        }
        let float_fields: &[(&str, usize)] = match self.section {
            3 => &[("Attack range", 648)],
            19 => &[("Damage reduction", 604)],
            24 => &[("Flight speed", 464)],
            25 | 26 | 27 => &[("Cast time", 324)],
            116 | 117 => &[("Trigger threshold", 328)],
            120 => &[
                ("Hue min", 324),
                ("Hue max", 328),
                ("Saturation min", 332),
                ("Saturation max", 336),
                ("Value min", 340),
                ("Value max", 344),
            ],
            121 => &[("Experience factor", 1220)],
            23 => &[
                ("Flight speed min", 468),
                ("Flight speed max", 472),
                ("Rush speed min", 476),
                ("Rush speed max", 480),
            ],
            _ => &[],
        };
        for (name, offset) in float_fields {
            let value = f32::from_bits(number(&self.record, *offset));
            if value.is_finite() {
                result.push((*name, value as f64));
            }
        }
        result
    }
}
