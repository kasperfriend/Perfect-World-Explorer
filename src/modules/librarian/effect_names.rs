//! English labels are presentation data. The GFX path and binding names must
//! reach the engine untouched. See Modules/Librarian/names/README.md.
use std::{borrow::Cow, collections::HashMap};

const TERMS: &str = include_str!("../../../Modules/Librarian/names/effects.tsv");
const SKILLS: &str = include_str!("../../../Modules/Librarian/names/skills.tsv");

#[derive(Debug, PartialEq)]
pub(super) struct Label {
    pub text: String,
    pub untranslated: bool,
}

pub(super) struct Names {
    terms: HashMap<&'static str, &'static str>,
    skills: HashMap<&'static str, Cow<'static, str>>,
    max_chars: usize,
}
impl Names {
    pub fn new(client_strings: &HashMap<u32, String>) -> Self {
        // A Latin-script locale need not be English. Only prefer the installed
        // spellings after recognizing its name table, never its tooltip rows.
        let english = [
            (50, "Drake's Ray"),
            (600, "Vacuous Palm"),
            (11290, "Glacial Shards"),
        ]
        .iter()
        .any(|(id, name)| {
            client_strings.get(id).is_some_and(|s| {
                super::descriptions::plain_text(s)
                    .trim()
                    .eq_ignore_ascii_case(name)
            })
        });
        let skills: HashMap<_, _> = SKILLS
            .lines()
            .map(|line| {
                let mut fields = line.split('\t');
                let zh = fields.next().unwrap();
                let id: u32 = fields.next().unwrap().parse().unwrap();
                let fallback = fields.next().unwrap();
                let en = english
                    .then(|| client_strings.get(&(id * 10)))
                    .flatten()
                    .map(|s| super::descriptions::plain_text(s).trim().to_owned())
                    .filter(|s| {
                        !s.is_empty() && s.is_ascii() && s.bytes().any(|b| b.is_ascii_alphabetic())
                    })
                    .map(Cow::Owned)
                    .unwrap_or(Cow::Borrowed(fallback));
                (zh, en)
            })
            .collect();
        let terms: HashMap<_, _> = TERMS
            .lines()
            .map(|l| l.split_once('\t').expect("invalid effect glossary"))
            .collect();
        let max_chars = terms
            .keys()
            .chain(skills.keys())
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(0);
        Self {
            terms,
            skills,
            max_chars,
        }
    }
    pub fn is_skill_path(path: &str) -> bool {
        path.split('/').any(|p| {
            p.contains("技能") || p.eq_ignore_ascii_case("skill") || p.eq_ignore_ascii_case("sgc")
        })
    }
    pub fn label(&self, mut source: &str, skill: bool) -> Label {
        let mut text = String::with_capacity(source.len());
        let mut untranslated = false;
        let mut after_word = false;
        while let Some(ch) = source.chars().next() {
            // Weapon filenames carry the grade before 品. In English it is
            // "Grade 18", not "18 Grade"; other numeric suffixes stay intact.
            if ch.is_ascii_digit() {
                let digits = source.bytes().take_while(u8::is_ascii_digit).count();
                if let Some(rest) = source[digits..].strip_prefix('品') {
                    if text.ends_with(char::is_alphanumeric) {
                        text.push(' ');
                    }
                    text.push_str("Grade ");
                    text.push_str(&source[..digits]);
                    source = rest;
                    after_word = true;
                    continue;
                }
            }
            if !is_han(ch) {
                if after_word && ch.is_alphanumeric() {
                    text.push(' ');
                }
                text.push(ch);
                source = &source[ch.len_utf8()..];
                after_word = false;
                continue;
            }
            // Match compounds before their prefixes; never split a UTF-8 character.
            let mut end = source
                .char_indices()
                .nth(self.max_chars)
                .map_or(source.len(), |(n, _)| n);
            let mut found = None;
            while end > 0 {
                let word = &source[..end];
                if let Some(en) = skill
                    .then(|| self.skills.get(word))
                    .flatten()
                    .map(Cow::as_ref)
                    .or_else(|| self.terms.get(word).copied())
                {
                    found = Some((end, en));
                    break;
                }
                end -= word.chars().next_back().unwrap().len_utf8();
            }
            let (len, en) = found.unwrap_or_else(|| {
                untranslated = true;
                (ch.len_utf8(), &source[..ch.len_utf8()])
            });
            if text.ends_with(char::is_alphanumeric) {
                text.push(' ');
            }
            text.push_str(en);
            source = &source[len..];
            after_word = true;
        }
        Label { text, untranslated }
    }
}
fn is_han(c: char) -> bool {
    matches!(c as u32,0x3400..=0x4dbf|0x4e00..=0x9fff|0xf900..=0xfaff|0x20000..=0x323af)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skill_localization_domain_terms_and_unknown_custom_names() {
        let names = Names::new(&HashMap::from([
            (50, "Drake's Ray".into()),
            (11290, "^ffffffGlacial Shards (Classic)".into()),
        ]));
        for (path, en) in [
            (
                "gfx/场景/新种族地图/树林日光多道暖色.gfx",
                "gfx/Scene/Tideborn Region/Warm Forest Sunbeams.gfx",
            ),
            (
                "gfx/人物/技能/巫师/冷凝术_击中2.gfx",
                "gfx/Character/Skill/Psychic/Glacial Shards (Classic)_Impact 2.gfx",
            ),
            (
                "gfx/npc/女npc_蝴蝶子效果.gfx",
                "gfx/npc/Female NPC_Butterfly Child Effect.gfx",
            ),
            (
                "gfx/18品双剑_剑柄.gfx",
                "gfx/Grade 18 Dual Blades_Sword Hilt.gfx",
            ),
            ("gfx/mixedCase/Beam_02.GFX", "gfx/mixedCase/Beam_02.GFX"),
        ] {
            assert_eq!(
                names.label(path, Names::is_skill_path(path)),
                Label {
                    text: en.into(),
                    untranslated: false
                }
            );
        }
        assert_eq!(names.label("斩杀", true).text, "Slipstream Strike");
        assert_eq!(names.label("斩杀", false).text, "Execution");
        let outside = names.label("龘", false);
        assert!(outside.untranslated);
        assert_eq!(outside.text, "龘");
    }
}
