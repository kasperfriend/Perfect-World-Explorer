//! Text resources in configs.pck. Lines with explicit numeric IDs are parsed
//! independently, so malformed or comment lines never shift another item's ID.
use std::collections::HashMap;

fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        encoding_rs::GBK.decode(bytes).0.into_owned()
    }
}

pub fn numeric_pairs(bytes: &[u8]) -> HashMap<u32, u32> {
    decode(bytes)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .collect()
}

pub fn keyed_strings(bytes: &[u8]) -> HashMap<u32, String> {
    let text = decode(bytes);
    let mut result = HashMap::with_capacity(text.len() / 32);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with('/') {
            continue;
        }
        let Some(start) = line.find('"') else {
            continue;
        };
        let Some(end) = line[start + 1..].rfind('"') else {
            continue;
        };
        let Ok(id) = line[..start].trim().parse::<u32>() else {
            continue;
        };
        result.entry(id).or_insert_with(|| {
            let slice = &line[start + 1..start + 1 + end];
            if !slice.contains('\\') {
                slice.to_owned()
            } else {
                slice.replace("\\r", "\n").replace("\\n", "\n")
            }
        });
    }
    result
}

/// The game's colour escape is a caret followed by exactly six hex digits.
pub fn plain_text(text: &str) -> String {
    if !text.contains('^') {
        return text.to_owned();
    }
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '^' {
            let rest = chars.as_str().as_bytes();
            if rest.len() >= 6 && rest[..6].iter().all(|c| c.is_ascii_hexdigit()) {
                for _ in 0..6 {
                    chars.next();
                }
                continue;
            }
        }
        result.push(ch);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_id_and_quoted_value_without_shifting_on_comments() {
        let values = keyed_strings(b"# comment\n42 \"^ff0000Sword\\nFine\"\ninvalid\n");
        assert_eq!(plain_text(&values[&42]), "Sword\nFine");
        assert_eq!(plain_text("a^oops"), "a^oops");
    }
    #[test]
    fn addon_blocks_ignore_comments() {
        let types = addon_types(b"/* type 7 {999} */ type: 2 { 41,42 } // 5 { 1 }\ntype: 3 {43}");
        assert_eq!(types.get(&41), Some(&2));
        assert_eq!(types.get(&43), Some(&3));
        assert!(!types.contains_key(&999));
    }
    #[test]
    fn formats_known_and_rejects_nonfinite_addons() {
        assert_eq!(
            describe_addon(41, &[3, 5]).as_deref(),
            Some("Strength: +3 to +5")
        );
        assert_eq!(
            describe_addon(45, &[0.05f32.to_bits() as i32]).as_deref(),
            Some("Critical rate: +5.00%")
        );
        assert_eq!(
            describe_addon(16, &[0.35f32.to_bits() as i32, 0.45f32.to_bits() as i32]).as_deref(),
            Some("Metal defense: +35.00% to +45.00%")
        );
        assert_eq!(
            describe_addon(9, &[0.05f32.to_bits() as i32]).as_deref(),
            Some("Attack interval: -0.05")
        );
        assert_eq!(describe_addon(45, &[f32::NAN.to_bits() as i32]), None);
        assert_eq!(
            describe_addon(6, &[8, 4]).as_deref(),
            Some("Physical defense: +8; Physical attack: -4")
        );
        assert_eq!(
            describe_addon(150, &[]).as_deref(),
            Some("Critical rate: +1%")
        );
        assert_eq!(describe_addon(900, &[3]), None);
    }
    #[test]
    fn authentic_config_text_when_available() {
        let Ok(root) = std::env::var("PW_LIBRARIAN_ELEMENT_ROOT") else {
            return;
        };
        let mut assets = super::super::archive::LibrarianAssets::open(std::path::Path::new(&root));
        let descriptions = keyed_strings(
            &assets
                .read_config("configs/item_ext_desc.txt")
                .expect("item descriptions"),
        );
        assert!(!plain_text(descriptions.get(&10037).expect("description 10037")).is_empty());
        let addons = addon_types(
            &assets
                .read_config("configs/item_ext_prop.txt")
                .expect("add-on types"),
        );
        assert_eq!(addons.get(&411), Some(&0));
    }

    #[test]
    fn reads_utf16_text() {
        let mut bytes = vec![0xff, 0xfe];
        for unit in "42 \"Name\"".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            keyed_strings(&bytes).get(&42).map(String::as_str),
            Some("Name")
        );
    }
}

/// Parse the client type blocks in item_ext_prop.txt. Comments are removed
/// before tokenizing so example numbers do not become addon IDs.
pub fn addon_types(bytes: &[u8]) -> HashMap<u32, u32> {
    let raw = decode(bytes);
    let mut clean = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            clean.push(' ');
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            for c in chars.by_ref() {
                if previous == '*' && c == '/' {
                    break;
                }
                previous = c;
            }
            clean.push(' ');
        } else {
            clean.push(ch);
        }
    }
    let clean = clean.replace('{', " { ").replace('}', " } ");
    let tokens: Vec<_> = clean
        .split(|c: char| !c.is_ascii_digit() && c != '{' && c != '}')
        .filter(|part| !part.is_empty())
        .collect();
    let mut types = HashMap::new();
    let mut index = 0;
    while index + 1 < tokens.len() {
        let Ok(kind) = tokens[index].parse::<u32>() else {
            index += 1;
            continue;
        };
        if tokens[index + 1] != "{" {
            index += 1;
            continue;
        }
        index += 2;
        while index < tokens.len() && tokens[index] != "}" {
            if let Ok(id) = tokens[index].parse::<u32>() {
                types.entry(id).or_insert(kind);
            }
            index += 1;
        }
        index += 1;
    }
    types
}

/// Interpret a conservative subset of v27 property types. Unknown types retain
/// their raw parameters in the UI rather than being assigned a guessed meaning.
pub fn describe_addon(kind: u32, params: &[i32]) -> Option<String> {
    if (150..=155).contains(&kind) {
        return Some(format!("Critical rate: +{}%", kind - 149));
    }
    if let Some((gain, loss, percent)) = match kind {
        6 => Some(("Physical defense", "Physical attack", false)),
        7 => Some(("Physical attack", "Physical defense", false)),
        8 => Some(("Magic attack", "Magic defense", false)),
        25 | 30 => Some(("Metal defense", "Fire defense", kind < 30)),
        26 | 31 => Some(("Wood defense", "Metal defense", kind < 30)),
        27 | 32 => Some(("Water defense", "Earth defense", kind < 30)),
        28 | 33 => Some(("Fire defense", "Water defense", kind < 30)),
        29 | 34 => Some(("Earth defense", "Wood defense", kind < 30)),
        _ => None,
    } {
        let (&first, rest) = params.split_first()?;
        let second = *rest.first()?;
        return if percent {
            let first = f32::from_bits(first as u32);
            let second = f32::from_bits(second as u32);
            (first.is_finite() && second.is_finite()).then(|| {
                format!(
                    "{gain}: {:+.2}%; {loss}: {:+.2}%",
                    first * 100.,
                    -second * 100.
                )
            })
        } else {
            Some(format!(
                "{gain}: {first:+}; {loss}: {:+}",
                -i64::from(second)
            ))
        };
    }
    let (label, percent, floating) = match kind {
        0 => ("Physical attack", false, false),
        1 => ("Maximum physical attack", false, false),
        2 => ("Physical attack", true, true),
        3 => ("Magic attack", false, false),
        4 => ("Maximum magic attack", false, false),
        5 => ("Magic attack", true, true),
        9 => ("Attack interval", false, true),
        10 => ("Attack range", false, true),
        11 => ("Channeling time", true, true),
        12 => ("Physical defense", false, false),
        13 => ("Physical defense", true, true),
        14 => ("Elemental defense", false, false),
        15 => ("Metal defense", false, false),
        16 => ("Metal defense", true, true),
        17 => ("Wood defense", false, false),
        18 => ("Wood defense", true, true),
        19 => ("Water defense", false, false),
        20 => ("Water defense", true, true),
        21 => ("Fire defense", false, false),
        22 => ("Fire defense", true, true),
        23 => ("Earth defense", false, false),
        24 => ("Earth defense", true, true),
        35 => ("HP", false, false),
        36 => ("MP", false, false),
        37 => ("HP", true, true),
        38 => ("MP", true, true),
        39 => ("HP recovery", false, false),
        40 => ("MP recovery", false, false),
        41 => ("Strength", false, false),
        42 => ("Dexterity", false, false),
        43 => ("Magic", false, false),
        44 => ("Vitality", false, false),
        45 => ("Critical rate", true, true),
        46 => ("Accuracy", false, false),
        47 => ("Accuracy", true, true),
        48 => ("Movement speed", false, true),
        49 => ("Movement speed", true, true),
        50 => ("Evasion", false, false),
        51 => ("Evasion", true, true),
        52 => ("Durability", false, false),
        53 => ("Durability", true, true),
        54 => ("Physical immunity", true, true),
        56 => ("Equipment requirement reduction", true, true),
        58 => ("Experience bonus", true, true),
        59 => ("Attack level", false, false),
        60 => ("Defense level", false, false),
        61 => ("Elemental defense", true, true),
        200 => ("Refine physical attack", false, false),
        201 => ("Refine magic attack", false, false),
        202 => ("Refine physical defense", false, false),
        203 => ("Refine metal defense", false, false),
        204 => ("Refine wood defense", false, false),
        205 => ("Refine water defense", false, false),
        206 => ("Refine fire defense", false, false),
        207 => ("Refine earth defense", false, false),
        208 => ("Refine HP", false, false),
        209 => ("Refine evasion", false, false),
        210 => ("Refine elemental defense", false, false),
        211 => ("Refine physical and magic attack", false, false),
        _ => return None,
    };
    let raw = *params.first()?;
    if floating {
        let convert = |raw: i32| {
            let value = f32::from_bits(raw as u32);
            let scale =
                if matches!(kind, 9 | 11) { -1. } else { 1. } * if percent { 100. } else { 1. };
            value.is_finite().then_some(value * scale)
        };
        let first = convert(raw)?;
        let second = params.get(1).and_then(|raw| convert(*raw));
        let suffix = if percent { "%" } else { "" };
        if let Some(second) = second.filter(|value| *value != 0. && *value != first) {
            Some(format!(
                "{label}: {:+.2}{suffix} to {:+.2}{suffix}",
                first.min(second),
                first.max(second)
            ))
        } else {
            Some(format!("{label}: {first:+.2}{suffix}"))
        }
    } else if let Some(&high) = params.get(1).filter(|&&high| high != raw && high != 0) {
        Some(format!("{label}: {raw:+} to {high:+}"))
    } else {
        Some(format!("{label}: {raw:+}"))
    }
}
