//! English names for the Chinese zone names the retail precinct files carry.
//!
//! `precinct.clt` stores its zone names in whichever language the client was
//! localised for: a Chinese client writes `刺骨之地`, an English one writes
//! `The Harshlands`. Without a font that carries CJK the Chinese strings render
//! as boxes, which is what the zone banner did for a 1.5.2 world, and the names
//! are unreadable to most of the people reading this one either way.
//!
//! The table is not typed in by hand. It is the join of two shipped files for
//! the **same map**: the Chinese `precinct.clt` from the 1.5.2 client in
//! `maps/world` and the English one from the 1.8.4 client for the same map,
//! matched zone by zone on their polygon vertices. 382 zones in the Chinese
//! file, 378 of them found in the English file, and every one of those 378
//! agrees on a single English name for its Chinese one: 243 distinct names,
//! no conflicts. The four zones the English file does not carry are dropped
//! rather than guessed.
//!
//! Matching on the polygon rather than on order or file offset is what makes
//! this trustworthy: the zone order and the numeric headers differ between the
//! two files, but a zone's shape does not move between client versions.

/// `(Chinese, English)` pairs, sorted by the English name.
///
/// Only three zones repeat (`未开放`, `无极海`, `离离草原`); everything else is
/// one name to one name.
pub const ZONE_NAMES: &[(&str, &str)] = &[
    ("联军营地", "Allies Camp"),
    ("逆天神坛", "Altar of Disbelief"),
    ("梦徊之间", "Altar of Dreams"),
    ("紫晶村", "Amethyst Village"),
    ("残山", "Ancient Peak"),
    ("古长城", "Ancient Wall"),
    ("渔村", "Angler's Village"),
    ("祖龙城北郊", "Archosaur, Northern Outskirts"),
    ("祖龙城南郊", "Archosaur, Southern Outskirt"),
    ("祖龙城西郊", "Archosaur, Western Outskirts"),
    ("踏箭山庄", "Arrowhead Manor"),
    ("踏箭峰", "Arrowhead Peak"),
    ("雪崩峡谷", "Avalanche Canyon"),
    ("雪崩山", "Avalanche Mountain"),
    ("雪崩岭", "Avalanche Ridge"),
    ("雪崩村", "Avalanche Village"),
    ("月照竹林", "Bamboo Forest of the Shining Moon"),
    ("倚竹村", "Bamboo Village"),
    ("海狱岛", "Barrier Island"),
    ("海域岛", "Barrier Island"),
    ("落缨村", "Battlemark Village"),
    ("白虎岭", "Bengal Ridge"),
    ("黑山", "Black Mountain"),
    ("烈风谷", "Blazing Wind Valley"),
    ("天荒城遗址", "Bleakhaven Ruins"),
    ("血沙滩", "Bloody Beach"),
    ("苍茫草原", "Boundless Grasslands"),
    ("埋骨之地", "Brd Bns"),
    ("断弓山", "Broken Bow Bluff"),
    ("断桥村", "Broken Bridge Village"),
    ("破阵平原", "Broken Plain"),
    ("破浪营地", "Camp Wave Breaker"),
    ("远征营地", "Camp of the Intrepid"),
    ("海妖洞窟", "Caves of Menaces"),
    ("媚眼湖", "Charmed Eye Lake"),
    ("万劫城", "City of Misfortune"),
    ("惊涛城", "City of Raging Tides"),
    ("万流城", "City of a Thousand Streams"),
    ("万化城", "City of the Lost"),
    ("碎冰岩", "Cliff of Shattered Ice"),
    ("腐败之地", "Corrupted Marshes"),
    ("无忧小道", "Courage Path"),
    ("赤寨", "Crimson Stronghold"),
    ("夜郎村", "Cromagnon Village"),
    ("枉凝归路", "Crooked Pathway"),
    ("朝啼岛", "Dawn's Cry Isle"),
    ("赢霄城", "Dawnglory"),
    ("通灵之野", "Decaying Woods"),
    ("回音戈壁", "Desert of Echoes"),
    ("沙漠海", "Deserted Sea"),
    ("仙踪岭", "Distant Ranges"),
    ("鲸涛部落", "Dragon Hunters Tribe"),
    ("煮龙部落", "Dragon Slayers Tribe"),
    ("龙战之野", "Dragon Wilderness"),
    ("应龙谷", "Dragon's Breath Valley"),
    ("绝龙坡", "Dragon's End"),
    ("龙牙村", "Dragonfang Village"),
    ("弱水之源", "Drbl Sp Spring"),
    ("南柯寨", "Dreaming Stronghold"),
    ("魍休林", "Dreamrest Woods"),
    ("寻梦港", "Dreamweaver Port"),
    ("寻梦港郊外", "Dreamweaver Port, Outskirts"),
    ("东屏镇", "East Barrier Village"),
    ("灵音小径", "Echoing Path"),
    ("极乐村", "Elysium Village"),
    ("青龙湾", "Emerald Dragon Gulf"),
    ("无极海", "Endless Sea"),
    ("剑仙城", "Etherblade City"),
    ("剑仙湖", "Etherblade Lake"),
    ("剑仙城郊外", "Etherblade Outskirts"),
    ("剑仙城城南", "Etherblade, South District"),
    ("影幽林地", "Evernight Ruins"),
    ("祭旗坡", "Flag Hill"),
    ("丛林遗迹", "Forest Ruins"),
    ("迷雾丛林", "Forest of Haze"),
    ("踏歌林", "Forest of Songs"),
    ("羽嘉森林", "Forest of the Plume"),
    ("远古神殿", "Forgotten Sanctuary"),
    ("逆潮关", "Fort Countertide"),
    ("飞鹰堡", "Fortress of the Talon"),
    ("暗香坡", "Fragrant Hills"),
    ("企鹅码头", "Frigid Bay"),
    ("企鹅海", "Frigid Sea"),
    ("乘风原", "Galerider Plains"),
    ("风吼镇", "Galeroar Town"),
    ("古风口", "Gate of Antiquity"),
    ("神迹台", "Gelid Plateau"),
    ("悼梦林", "Ghostmourn Woods"),
    ("卫戌营", "Guardian's Watch"),
    ("幽冥古道", "Haunted Path"),
    ("侠隐村", "Hidden Heroes Village"),
    ("幽兰村", "Hidden Orchid"),
    ("幽兰谷", "Hidden Orchid"),
    ("东幽兰谷", "Hidden Orchid Valley, East"),
    ("禹王坡", "Hill of the Old King"),
    ("蚁聚山", "Hill of the Swarm"),
    ("冰脊走廊", "Icespine Pass"),
    ("沐火村", "Immolation Camp"),
    ("鹏程客栈", "Inn of the Eagle"),
    ("碎梦岛", "Island of Broken Dreams"),
    ("残云岛", "Island of Shattered Cloud"),
    ("禹王鼎", "King's Feast"),
    ("映血湖", "Lake of Blood"),
    ("无忧寒潭", "Lake of No Worries"),
    ("垂泪湖", "Lake of Woe"),
    ("积羽湖", "Lake of the Plume"),
    ("安息之地", "Land of Eternal Sleep"),
    ("暗语之地", "Land of Whispers"),
    ("焚心之地", "Land of the Burning Heart"),
    ("灵蜥部落", "Lizard Stalker Tribe"),
    ("月涌湖", "Loch Nocturne"),
    ("孤云礁", "Lonely Cloud Reef"),
    ("望月关", "Lunar Pass"),
    ("祭剑坛", "Lunastone Altar"),
    ("雾幻山", "Mist Mountain"),
    ("朝灵谷", "Morning Star Valley"),
    ("布尔罕山", "Mount Burhan"),
    ("灯笼山", "Mount Lantern"),
    ("玄武山", "Mountain of the Tortoise God"),
    ("颤魂岭", "Mountains of Despair"),
    ("无名岛", "Nameless Isle"),
    ("地泉河营地", "Netherspring Camp"),
    ("夜火坛", "Nightfire Altar"),
    ("月魇斜谷", "Nightmare Cleft"),
    ("轮回殿入口", "Nirvana Palace Entrance"),
    ("北屏镇", "North Barrier Village"),
    ("城北", "North City"),
    ("北石阵荒野", "Northern Desolace"),
    ("兰若寺", "Orchid Temple"),
    ("桃花坞", "Peach Blossom Grove"),
    ("穿心湖", "Pierced Heart Lake"),
    ("离离草原", "Plain of Farewells"),
    ("积羽湖畔", "Plume Lakeside"),
    ("积羽城", "Plume Officer"),
    ("獠牙镇", "Plume Officer"),
    ("幻臆流鸿", "Pools of Fantasy"),
    ("古代废墟", "Primal Remains"),
    ("眠魂之地", "Restless Outlands"),
    ("眠魂遗迹", "Restless Remnants"),
    ("云梦岭", "Ridge of the Dreaming Cloud"),
    ("棋盘谷", "Rook's Valley"),
    ("血肉之池", "Rotflesh Trench"),
    ("罪恶囚地旧址", "Ruins of the Exiled"),
    ("桃源镇", "Sanctuary"),
    ("鸣沙村旧址", "Sandsong Ruins"),
    ("鸣沙村", "Sandsong Village"),
    ("风沙谷", "Sandstorm Valley"),
    ("野人农地", "Savage Fields"),
    ("绯色祭坛", "Scarlet Altar"),
    ("蛇蝎谷", "Scorpion Valley"),
    ("寂寞海", "Sea of Isolation"),
    ("无妄海", "Sea of Reality"),
    ("南风原", "Sealed Basin"),
    ("英雄冢", "Secret Passage"),
    ("棺椟之地", "Serpent Cliffs"),
    ("暗潮大帐", "Shadowtide Camp"),
    ("鲨骨塔", "Sharkscale Spire"),
    ("玉碎滩", "Shattered Jade Shore"),
    ("碧涛林", "Shining Tidewood"),
    ("奉灵殿", "Shrine of the Ancestors"),
    ("谪仙村", "Shrine of the Immortals"),
    ("潮汐神殿", "Shrine of the Tides"),
    ("盘丝岭", "Silk Ridge"),
    ("胡姬村", "Sirry Wine Camp"),
    ("骷髅山", "Skull Mountain"),
    ("雪龙冰原", "Snowdragon Heights"),
    ("雪霏村", "Snowy Village"),
    ("玄魅坛", "Solstone Altar"),
    ("南屏镇", "South Barrier Village"),
    ("城南", "South City"),
    ("南石阵荒野", "Southern Desolace"),
    ("南屏山", "Southern Hold"),
    ("惊涛城南郊", "Southern Outskirts"),
    ("南柯崖", "Southern Stronghold"),
    ("雀画滩", "Sparrowmark Beach"),
    ("星栖苑", "Spire of Awakening"),
    ("摩天崖", "Stairs to Heavn"),
    ("星垂之野", "Starry Wilds"),
    ("风云堑", "Stormcloud Chasm"),
    ("苏木尔营", "Sumor Camp"),
    ("落日高原", "Sundown Highlnd"),
    ("落日镇", "Sundown Town"),
    ("疾风部落", "Swiftwind Tribe"),
    ("太极滩", "Tai Chi Shore"),
    ("灵犀城", "Tellus City"),
    ("迷离界", "Temple of Shadows"),
    ("折戟台", "The Blunted Lance Stand"),
    ("断桥", "The Broken Bridge"),
    ("幻灵谷", "The Fissure"),
    ("禁咒之海", "The Forbidden Sea"),
    ("履霜城", "The Forgotten Frostland"),
    ("冰封古道", "The Frozen Path"),
    ("上古冰河", "The Frozen River"),
    ("通天湖", "The Great Lake"),
    ("刺骨之地", "The Harshlands"),
    ("矿场", "The Mines"),
    ("北关营地", "The Northern Pass"),
    ("吟叶知秋", "The Shattered Boughs"),
    ("镜湖居", "The Silver Pool"),
    ("无为村", "The Village of Naught"),
    ("白岭", "The White Ridge"),
    ("万流城北郊", "Thousand Streams, Northern Outskirt"),
    ("万流城南郊", "Thousand Streams, Southern Outskirt"),
    ("万木岭", "Thousand Wood Ridge"),
    ("望汐桥", "Tidalwave Bridge"),
    ("伐木场", "Timberfield"),
    ("碧涛陵", "Tomb of the Shining Tide"),
    ("玄冥塔", "Tower of Darkness"),
    ("飞来镇", "Town of Arrivals"),
    ("信风镇", "Tradewind Village"),
    ("无忧河西岸", "Tranquility, Western Shore"),
    ("水盈居", "Treehollow Lake"),
    ("伤麒森林", "Unicorn Forest"),
    ("未开放", "Upcoming"),
    ("七杀谷", "Valley of Death"),
    ("破碎蚁穴", "Valley of the Ants"),
    ("天沟", "Valley of the Gods"),
    ("伤痕谷", "Valley of the Scarred"),
    ("幽魂塔", "Veiled Tower"),
    ("回风峡谷", "Verdant Ravine"),
    ("惊鸿岭", "Vibrant Cliffs"),
    ("野人村", "Village of Brutes"),
    ("碎冰村", "Village of Shattered Ice"),
    ("云梦村", "Village of the Dreaming Cloud"),
    ("迷失村", "Village of the Lost"),
    ("抱残村", "Village of the Oppressed"),
    ("炼狱火山", "Volcanic Purgatory"),
    ("四方寨", "Walled Stronghold"),
    ("横断山", "Wanderlust Peak"),
    ("海龙滩", "Waterbeast Shore"),
    ("破浪崖", "Wave Breaker Cliffs"),
    ("邻水镇", "Wellspring"),
    ("城西", "West City"),
    ("惊涛西径", "Western Crossroads"),
    ("砥石寨", "Whetstone Keep"),
    ("白沙滩", "White Sand Beach"),
    ("望乡海岸", "Widow's Coast"),
    ("疾风草原", "Windswept Grasslands"),
    ("风陵峡谷", "Windy Tomb Canyon"),
    ("狼图腾谷", "Wolf Totem Valley"),
    ("悲歌之野", "Woodland Elegy"),
    ("怨灵之门", "Wraithgate"),
    ("怨灵沼泽", "Wrth Sw"),
];

/// The English name for a zone, or the name itself when the table has none.
///
/// A lookup miss is deliberately not an error: a zone a later client adds, a
/// hand-edited `precinct.clt`, or a map of the user's own all keep their
/// authored name, which is better than a placeholder.
pub fn english(name: &str) -> &str {
    let trimmed = name.trim();
    ZONE_NAMES
        .iter()
        .chain(INSTANCE_NAMES.iter())
        .find(|(zh, _)| *zh == trimmed)
        .map(|(_, en)| *en)
        .unwrap_or(trimmed)
}

// BEGIN GENERATED instance aliases -- tools/instance-aliases.py
/// `(Chinese instance name, the English label for that folder)`.
///
/// Generated from the client's own `configs/instance.txt` joined with
/// `world_catalog::english_name`, so the Chinese is the client's text and the
/// English is the table the map picker already shows. Regenerate with
/// `python3 tools/instance-aliases.py <configs.pck> --write`.
pub const INSTANCE_NAMES: &[(&str, &str)] = &[
    ("万化城竞技场", "Lost Arena"),
    ("万蛇岛", "Snake Isle"),
    ("不赦堂", "Unyielding Hall"),
    ("丛林遗迹", "Forest Ruins"),
    ("五帝争霸", "Nation Wars Base Camp"),
    ("人界", "Primal World"),
    ("人界故事模式", "Home of Stealth"),
    ("仙", "Lothranis (Heaven)"),
    ("仙幻天", "Eden"),
    ("伏波堂", "Snowfall Valley"),
    ("凤鸣谷", "Phoenix Valley"),
    ("剑仙城竞技场", "Etherblade Arena"),
    ("土蝼洞穴", "Earth Cave"),
    ("城战1", "Territory War"),
    ("城战2", "Territory War"),
    ("城战3", "Territory War"),
    ("城战4", "Territory War"),
    ("城战5", "Territory War"),
    ("城战6", "Territory War"),
    ("天劫谷", "Valley of Disaster"),
    ("天怒神罚", "Chrono City"),
    ("天界炼狱", "Seat of Torment"),
    ("完美大陆", "Perfect World — Mainland"),
    ("履霜城", "Frostcover City"),
    ("履霜秘境", "Secret Frostcover Grounds"),
    ("帮派基地", "Faction Base"),
    ("幻沙蜃境", "Random Map 03"),
    ("幽冥居", "Gate of Delirium"),
    ("怨灵之门", "Wraithgate"),
    ("战神谷", "Wargod Gulch"),
    ("挂机岛", "Realm of Reflection"),
    ("新手村", "Celestial Vale"),
    ("无定宙", "Endless Universe"),
    ("楼兰古都", "Hallucinatory Trench"),
    ("流银宫", "Flowsilver Palace"),
    ("测试场景", "Nation Wars Capture the Flag"),
    ("海盗岛", "Nightscream Island"),
    ("火岩洞穴", "Firecrag Grotto"),
    ("狂狼巢穴", "Den of Rabid Wolves"),
    ("盘丝洞", "Silk Cave"),
    ("祖龙城竞技场", "Archosaur Arena"),
    ("神威狱", "Palace of Nirvana"),
    ("神无谷", "Valley of Reciprocity"),
    ("神月谷", "Lunar Glade"),
    ("积羽村竞技场", "Plume Arena"),
    ("穹世界", "Place of Origination"),
    ("结婚场景", "Perfect Chapel"),
    ("英雄冢", "Hero's Tomb / Secret Passage"),
    ("落日谷", "Sunset Valley"),
    ("蓬明洞", "Nightfall Hall"),
    ("蓬莱世界", "Morai"),
    ("蓬莱地宫", "Blighted Chamber"),
    ("蛇蝎洞", "Cave of the Vicious"),
    ("蜂巢密室", "Cube of Fate"),
    ("迷沙幻境", "Random Map 03"),
    ("青衣冢", "Hall of Deception"),
    ("飘渺城", "Warsong City"),
    ("高级无定宙", "Advanced Endless Universe"),
    ("鬼城", "City of Abominations / Ghost City"),
    ("鬼狱幻境", "Cave of Sadistic Glee"),
    ("魔", "Momaganon (Hell)"),
    ("魔域桃源", "Abaddon"),
    ("魔幻天", "Brimstone Pit"),
    ("黄昏圣殿", "Twilight Temple"),
    ("龙宫", "Temple of the Dragon"),
    ("龙隐窟", "Hidden Dragon Den"),
];
// END GENERATED instance aliases

/// Whether the name has anything to localise: cheap check for the caller so a
/// mostly-ASCII file costs no lookups.
pub fn is_localized(name: &str) -> bool {
    name.trim()
        .chars()
        .any(|c| !c.is_ascii() && !c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_zones_have_english_names() {
        // The names a 1.5.2 world shows first, which is what the report was
        // about: three squares where a zone name should be.
        assert_eq!(english("未开放"), "Upcoming");
        assert_eq!(english("刺骨之地"), "The Harshlands");
        assert_eq!(english("雪霏村"), "Snowy Village");
    }

    #[test]
    fn an_unknown_or_ascii_name_is_its_own_answer() {
        assert_eq!(english("My Own Map"), "My Own Map");
        assert_eq!(
            english("  未开放  "),
            "Upcoming",
            "surrounding space is trimmed"
        );
        assert_eq!(english(""), "");
        // A name the English file never carried stays as authored rather than
        // becoming a placeholder.
        assert_eq!(english("祖龙城"), "祖龙城");
    }

    /// An instance map's zone names come from its own `precinct.clt`, which is
    /// Chinese in a Chinese client. They must reach English through the instance
    /// table, not just the world one.
    #[test]
    fn instance_zone_names_are_localised() {
        // Every instance row whose folder `world_catalog::english_name` knows,
        // which the generator reports as all of them.
        assert!(INSTANCE_NAMES.len() >= 60, "{}", INSTANCE_NAMES.len());
        assert_eq!(english("万蛇岛"), "Snake Isle");
        assert_eq!(english("不赦堂"), "Unyielding Hall");
        assert_eq!(english("蛇蝎洞"), "Cave of the Vicious");
        // The world table still answers, and the two do not collide.
        assert_eq!(english("刺骨之地"), "The Harshlands");
        // No instance name may map to two labels.
        let mut seen = std::collections::HashSet::new();
        for (zh, _) in INSTANCE_NAMES {
            assert!(seen.insert(*zh), "{zh} appears twice");
        }
    }

    /// The final bar: a zone name that carries non-ASCII and has an English
    /// entry must not be shown as authored. This is the test that fails when a
    /// new client's names are added to the data but not to a table.
    #[test]
    fn every_localisable_zone_name_has_english() {
        let mut missing = Vec::new();
        for (zh, _) in ZONE_NAMES.iter().chain(INSTANCE_NAMES.iter()) {
            if is_localized(zh) && english(zh) == *zh {
                missing.push(*zh);
            }
        }
        assert!(missing.is_empty(), "no English entry for: {missing:?}");
    }

    #[test]
    fn no_chinese_name_maps_to_two_english_ones() {
        // The join was conflict-free, so the table must stay that way: a
        // duplicate would make the answer depend on row order.
        let mut seen = std::collections::HashSet::new();
        for (zh, _) in ZONE_NAMES {
            assert!(seen.insert(*zh), "{zh} appears twice");
        }
        assert_eq!(ZONE_NAMES.len(), 243);
    }

    #[test]
    fn only_names_that_need_it_are_localized() {
        assert!(is_localized("刺骨之地"));
        assert!(!is_localized("The Harshlands"));
        assert!(!is_localized("  "));
    }
}
