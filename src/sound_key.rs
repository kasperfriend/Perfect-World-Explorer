//! Archive key for an authored sound path.
//!
//! Two very different records name the same audio files:
//!
//! - `configs/sound.txt` (the client's event-id table) writes them as
//!   `SFX\Item\EquipSound\DownGem.wav`;
//! - a GFX element-170 sound writes them as `Item\EquipSound\DownGem.wav`,
//!   without the prefix.
//!
//! `sfx.pck` stores them under `sfx/...`, lower-cased. So the key has to add the
//! prefix when it is missing and leave it alone when it is already there —
//! otherwise the same file gets two keys, one of which resolves and one of which
//! does not, and which one you get depends on which record type named it.
//!
//! Both sides are measured, not assumed: the `sound.txt` rows above come from
//! the 1.8.4 `configs.pck`, and the element-170 rows from the 355 records in the
//! staged 1.3.1 `gfx.pck`. They meet at `sfx/item/equipsound/downgem.wav`.

/// Archive key for an authored sound path, or `None` when the path is not a
/// plain relative path inside the archive.
///
/// Rejects traversal, absolute paths, drive letters, `//` and empty segments.
/// The result is always lower-case and always under `sfx/`.
pub fn asset_key(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.starts_with('/')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.contains(':'))
    {
        return None;
    }
    let path = path.to_lowercase();
    Some(if path.starts_with("sfx/") {
        path
    } else {
        format!("sfx/{path}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two record types that name the same file must produce the same key.
    /// This is the whole point of the module: `sound.txt` writes the `SFX\`
    /// prefix, a GFX element-170 record does not, and `sfx.pck` stores one entry.
    #[test]
    fn both_record_types_key_the_same_file_identically() {
        assert_eq!(
            asset_key("Item\\EquipSound\\DownGem.wav").as_deref(),
            Some("sfx/item/equipsound/downgem.wav"),
            "a GFX element-170 path, no prefix"
        );
        assert_eq!(
            asset_key("SFX\\Item\\EquipSound\\DownGem.wav").as_deref(),
            Some("sfx/item/equipsound/downgem.wav"),
            "the same file as configs/sound.txt writes it"
        );
    }

    /// The five top-level folders the element-170 records actually use, from the
    /// staged `gfx.pck`: `Skill`, `Effect`, `Creature`, `Item`, `Ambience`.
    #[test]
    fn the_five_authored_roots_all_key_under_sfx() {
        let authored = [
            (
                "Skill\\01Fighter\\霸王龙飞_C.wav",
                "sfx/skill/01fighter/霸王龙飞_c.wav",
            ),
            (
                "Effect\\Player\\魔力燃烧.wav",
                "sfx/effect/player/魔力燃烧.wav",
            ),
            (
                "Creature\\Skill\\冰锥术_A.wav",
                "sfx/creature/skill/冰锥术_a.wav",
            ),
            (
                "Item\\EquipSound\\DownGem.wav",
                "sfx/item/equipsound/downgem.wav",
            ),
            (
                "Ambience\\3DimL\\16_Waterfall.wav",
                "sfx/ambience/3diml/16_waterfall.wav",
            ),
        ];
        for (raw, expected) in authored {
            assert_eq!(asset_key(raw).as_deref(), Some(expected), "{raw}");
        }
    }

    /// A path that could escape the archive, or that is not a relative path at
    /// all, is refused rather than normalised into something that might exist.
    #[test]
    fn keys_reject_traversal_and_absolute_paths() {
        for path in [
            "../secret.wav",
            "Effect/../secret.wav",
            "/sfx/a.wav",
            "C:/a.wav",
            "effect//a.wav",
            "",
            ".",
        ] {
            assert!(asset_key(path).is_none(), "{path:?} must be refused");
        }
    }

    /// Mixed separators and upper case are the normal case in these files, and
    /// the key folds both. `SFX/` and `sfx\` must not produce two entries.
    #[test]
    fn separators_and_case_fold() {
        assert_eq!(
            asset_key("SFX/Music/A.OGG").as_deref(),
            Some("sfx/music/a.ogg")
        );
        assert_eq!(
            asset_key("sfx\\Music\\a.ogg").as_deref(),
            Some("sfx/music/a.ogg")
        );
    }
}
