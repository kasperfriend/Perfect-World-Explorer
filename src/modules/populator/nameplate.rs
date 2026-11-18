//! Overhead-name font taken from the client's own table.
//!
//! The client does not decide its label look in code: `configs/fonts.txt`
//! (UTF-16LE, inside `configs.pck`) is a table of `"family" height bold italic
//! sample // comment` rows, and the 1.4.2 table shipped with this client reads
//!
//! ```text
//! "Arial Unicode MS"  12  0  0  W  //  玩家和NPC头顶称谓文字
//! "宋体"              20  0  0  W  //  弹出的伤害值的文字
//! //"UseAUIFont" 0 0 0 W // 控制使用AUIFont作为头顶文字
//! ```
//!
//! that is, the first row is explicitly the font of the player and NPC names
//! above their heads, the second is the floating damage number font, and
//! `UseAUIFont` - which would switch the labels to the interface font from
//! `fonts.pck` - is commented out in the shipped file.
//!
//! So the reader below asks the client for that row rather than inventing a
//! look: family and size come from the shipped table, the typeface is resolved
//! to a real font file (the client renders through GDI, which finds "Arial
//! Unicode MS" in the system font directory), and the client's own CJK font is
//! stacked behind it so the non-Latin names still resolve on a machine without
//! that system font.

use super::archive::Archive;
use std::path::{Path, PathBuf};

/// egui family name reserved for the client's overhead-name font.
pub const FAMILY: &str = "pw-client-overhead";

/// What the shipped table says, used when `configs.pck` cannot be read.
const FALLBACK_FAMILY: &str = "Arial Unicode MS";
const FALLBACK_SIZE: f32 = 12.;

/// Client archives and loose folders that may carry a font file.
const CLIENT_FONT_FOLDERS: [&str; 3] = ["data", "font", ""];

/// System font files for the families the shipped table can name. The client
/// reaches these through GDI, which resolves the family by name; a viewer needs
/// the file, so the table's families are mapped to their shipped file names.
const FAMILY_FILES: [(&str, &[&str]); 7] = [
    ("arial unicode", &["arialuni.ttf", "ARIALUNI.TTF"]),
    ("arial", &["arial.ttf", "ARIAL.TTF"]),
    ("宋体", &["simsun.ttc", "SIMSUN.TTC"]),
    ("simsun", &["simsun.ttc", "SIMSUN.TTC"]),
    ("微软雅黑", &["msyh.ttc", "MSYH.TTC"]),
    ("黑体", &["simhei.ttf", "SIMHEI.TTF"]),
    ("楷体", &["simkai.ttf", "SIMKAI.TTF"]),
];

/// Client fonts kept as a fallback until the table's family resolves, in the
/// order `configs/font.ini` lists them (`FontPath1`, then `FontPath2`).
const CLIENT_FALLBACKS: [(&str, &str); 2] = [
    ("fzl2jw.ttf", "fonts/fzl2jw.ttf"),
    ("fzxh1jw.ttf", "fonts/fzxh1jw.ttf"),
];

/// One registered font file: label for the settings window, bytes for egui.
pub struct FontFile {
    pub label: String,
    pub bytes: Vec<u8>,
}

/// The client's overhead-name font, resolved once per session.
pub struct NameFont {
    /// Family name the shipped table asks for.
    pub family: String,
    /// Pixel height the shipped table asks for.
    pub size: f32,
    /// Where the family and size were read from.
    pub source: String,
    /// Registered files, primary first.
    pub files: Vec<FontFile>,
    /// Set once [`NameFont::install`] has handed the files to egui.
    installed: bool,
}

impl NameFont {
    /// Read the client's font table and resolve the typeface it names.
    pub fn load(element: &Path) -> Self {
        let (family, size, source) = match read_font_table(element) {
            Some((family, size)) => (family, size, "Configs\\fonts.txt (configs.pck)".to_owned()),
            None => (
                FALLBACK_FAMILY.to_owned(),
                FALLBACK_SIZE,
                "built-in default (configs.pck unavailable)".to_owned(),
            ),
        };
        let mut files = Vec::new();
        if let Some(file) = system_font_file(&family).or_else(|| client_cjk_font(element)) {
            files.push(file);
        }
        // The client's own CJK font goes behind the table's typeface so the
        // Chinese and symbol names always resolve, exactly like the client's
        // own font set (it registers both UI fonts at start-up).
        if let Some(file) = client_cjk_font(element) {
            if files
                .first()
                .map(|primary| primary.label != file.label)
                .unwrap_or(true)
            {
                files.push(file);
            }
        }
        Self {
            family,
            size,
            source,
            files,
            installed: false,
        }
    }

    /// True when at least one real font file backs the family.
    pub fn resolved(&self) -> bool {
        !self.files.is_empty()
    }

    /// Search path summary for the settings window.
    pub fn describe(&self) -> String {
        if !self.resolved() {
            format!(
                "\"{}\" {} px per {} — no font file found, egui default used",
                self.family, self.size, self.source
            )
        } else {
            format!(
                "\"{}\" {} px per {}, from {}",
                self.family,
                self.size,
                self.source,
                self.files
                    .iter()
                    .map(|file| file.label.as_str())
                    .collect::<Vec<_>>()
                    .join(" + ")
            )
        }
    }

    /// Register the files with egui once, keeping the viewer's own fonts as the
    /// last resort for glyphs they lack.
    ///
    /// Returns whether the family can be drawn with in the frame that calls
    /// this: egui only picks new definitions up at the start of the next frame,
    /// so the very first call reports `false` and the caller keeps its own font
    /// for that one frame.
    pub fn install(&mut self, ctx: &egui::Context) -> bool {
        if !self.resolved() || self.installed {
            return true;
        }
        super::super::extend_fonts(ctx, |fonts| {
            let mut family = Vec::new();
            for (index, file) in self.files.iter().enumerate() {
                let key = format!("{FAMILY}-{index}");
                fonts
                    .font_data
                    .insert(key.clone(), egui::FontData::from_owned(file.bytes.clone()));
                family.push(key);
            }
            if let Some(defaults) = fonts.families.get(&egui::FontFamily::Proportional) {
                family.extend(defaults.iter().cloned());
            }
            fonts
                .families
                .insert(egui::FontFamily::Name(FAMILY.into()), family);
        });
        self.installed = true;
        false
    }

    /// The `FontId` the client's table asks for.
    pub fn font_id(&self) -> egui::FontId {
        if self.files.is_empty() {
            egui::FontId::proportional(self.size)
        } else {
            egui::FontId::new(self.size, egui::FontFamily::Name(FAMILY.into()))
        }
    }
}

/// Read `Configs\fonts.txt` out of the client and parse it.
fn read_font_table(element: &Path) -> Option<(String, f32)> {
    let mut archive = Archive::open(&element.join("configs.pck")).ok()?;
    let bytes = archive.read("configs/fonts.txt").ok()?;
    parse_font_table(&decode_utf16(&bytes))
}

/// Parse the first non-comment row of the client's font table.
fn parse_font_table(text: &str) -> Option<(String, f32)> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let mut parts = line.split('"');
        parts.next()?;
        let family = parts.next()?.trim().to_owned();
        let rest = parts.next()?;
        let mut tokens = rest.split_whitespace();
        let size: f32 = tokens.next()?.parse().ok()?;
        if family.is_empty() || size <= 0. {
            continue;
        }
        return Some((family, size));
    }
    None
}

/// UTF-16LE with an optional byte-order mark, as the shipped table is stored.
fn decode_utf16(bytes: &[u8]) -> String {
    let start = if bytes.starts_with(&[0xff, 0xfe]) {
        2
    } else {
        0
    };
    let units: Vec<u16> = bytes[start..]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The file a system family lives in, searched where Windows keeps fonts.
fn system_font_file(family: &str) -> Option<FontFile> {
    let names = family_file_names(family);
    for folder in system_font_folders() {
        for name in &names {
            let path = folder.join(name);
            if let Ok(bytes) = std::fs::read(&path) {
                return Some(FontFile {
                    label: path.display().to_string(),
                    bytes,
                });
            }
        }
    }
    None
}

fn family_file_names(family: &str) -> Vec<String> {
    let lowered = family.to_lowercase();
    let mut names = Vec::new();
    for (key, files) in FAMILY_FILES {
        if lowered.contains(key) {
            names.extend(files.iter().map(|file| (*file).to_owned()));
        }
    }
    if names.is_empty() {
        // Unknown family: try its own collapsed name before giving up.
        let collapsed: String = lowered.chars().filter(|c| c.is_alphanumeric()).collect();
        if !collapsed.is_empty() {
            names.push(format!("{collapsed}.ttf"));
            names.push(format!("{collapsed}.ttc"));
        }
    }
    names
}

fn system_font_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Ok(windir) = std::env::var("WINDIR") {
        folders.push(PathBuf::from(windir).join("Fonts"));
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        folders.push(
            PathBuf::from(local)
                .join("Microsoft")
                .join("Windows")
                .join("Fonts"),
        );
    }
    folders
}

/// The client's own CJK font, for the whole interface rather than one label.
///
/// The viewer's egui context starts with the built-in Latin faces only, so any
/// HUD string the client authored in Chinese draws as one tofu box per glyph —
/// the zone banner over a Chinese-named area shows `Entering \u{25a1}\u{25a1}\u{25a1}`.
/// Nameplates installed this font for themselves, which is why the overhead
/// names read correctly while the banner did not. Registering it once on the
/// shared context puts it behind the default faces for every widget, and it
/// stays the last resort so Latin text keeps the built-in metrics.
pub fn install_client_cjk(ctx: &egui::Context, element: &Path) {
    let Some(file) = client_cjk_font(element) else {
        return;
    };
    const KEY: &str = "pw-client-cjk-ui";
    super::super::extend_fonts(ctx, |fonts| {
        if fonts.font_data.contains_key(KEY) {
            return;
        }
        fonts
            .font_data
            .insert(KEY.into(), egui::FontData::from_owned(file.bytes));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(KEY.into());
        }
    });
}

/// A font the client itself ships, read from the element folder or `fonts.pck`.
fn client_cjk_font(element: &Path) -> Option<FontFile> {
    for (name, entry) in CLIENT_FALLBACKS {
        for folder in CLIENT_FONT_FOLDERS {
            let path = element.join(folder).join(name);
            if let Ok(bytes) = std::fs::read(&path) {
                return Some(FontFile {
                    label: path.display().to_string(),
                    bytes,
                });
            }
        }
        if let Ok(mut archive) = Archive::open(&element.join("fonts.pck")) {
            if let Ok(bytes) = archive.read(entry) {
                return Some(FontFile {
                    label: format!("fonts.pck:{entry}"),
                    bytes,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xfe];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn the_shipped_table_names_the_overhead_font_first() {
        let bytes = utf16(
            "//  字体名 ｜ 大小 ｜ 粗体 ｜ 斜体 ｜字符样本\r\n\r\n\
             \"Arial Unicode MS\"  12  0  0  W\t//  玩家和NPC头顶称谓文字\r\n\
             \"宋体\"  20  0  0  W\t//  弹出的伤害值的文字\r\n\
             //\"UseAUIFont\" 0 0 0 W // 控制使用AUIFont作为头顶文字\r\n",
        );
        assert_eq!(
            parse_font_table(&decode_utf16(&bytes)),
            Some(("Arial Unicode MS".to_owned(), 12.))
        );
    }

    #[test]
    fn a_commented_only_table_yields_no_font() {
        let bytes = utf16("//\"UseAUIFont\" 0 0 0 W // 控制使用AUIFont作为头顶文字\r\n");
        assert_eq!(parse_font_table(&decode_utf16(&bytes)), None);
    }

    #[test]
    fn the_table_family_maps_to_its_system_file() {
        assert!(family_file_names("Arial Unicode MS").contains(&"arialuni.ttf".to_owned()));
        assert!(family_file_names("宋体").contains(&"simsun.ttc".to_owned()));
        assert!(family_file_names("Unknown Family").contains(&"unknownfamily.ttf".to_owned()));
    }

    /// The shipped client, when its archives are staged locally: the table the
    /// client parses must be the one this module reads, and the fonts it ships
    /// must be there for the fallback path.
    #[test]
    fn the_shipped_client_archive_names_the_same_overhead_font() {
        let element = Path::new("google_drive/pw142/element");
        if !element.join("configs.pck").exists() {
            return;
        }
        let (family, size) = read_font_table(element).expect("configs/fonts.txt parses");
        assert_eq!(family, "Arial Unicode MS");
        assert_eq!(size, 12.0);
        // The client's own CJK font is the fallback for machines without the
        // system family, and it comes out of either data/ or fonts.pck.
        let fallback = client_cjk_font(element).expect("the client ships its font");
        assert!(fallback.bytes.len() > 100_000, "{}", fallback.label);
    }

    #[test]
    fn an_unresolved_font_still_draws_with_the_viewer_default() {
        let font = NameFont {
            family: FALLBACK_FAMILY.to_owned(),
            size: FALLBACK_SIZE,
            source: "test".to_owned(),
            files: Vec::new(),
            installed: false,
        };
        assert!(!font.resolved());
        assert!(font.describe().contains("egui default"));
        assert_eq!(font.font_id(), egui::FontId::proportional(FALLBACK_SIZE));
    }
}
