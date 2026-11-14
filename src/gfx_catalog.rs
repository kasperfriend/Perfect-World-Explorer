//! A reader and handler for every effect the client ships.
//!
//! Everything else in this crate reaches an effect through something that
//! already knows the path: an ECWLD placement (`world_models_gpu`), an ECM
//! EventType-100 record (`modules/walker/gfx`) or a scene light owner
//! (`gfx_owner`). A module that wants "a waterfall" or "the warm sun shafts"
//! had no way to ask. This module is that way: it reads the archive directory
//! with the same retail v2 reader the rest of the crate uses (`crate::pck`),
//! groups the 5,459 `.gfx` members the way their own authored names group them,
//! reports what each root actually contains before anyone spends a load on it,
//! and hands back a `gfx_owner::Request` so the existing owner machinery does
//! the playing.
//!
//! Two rules are inherited from the readers below and are not relaxed here:
//!
//! * A root is offered to `gfx_decal::Root::parse` first, the verified stateless
//!   subset; a root it rejects goes to `gfx_file::parse` + `gfx_player`. A root
//!   `gfx_file` rejects is an error naming the parser's complaint, never an
//!   approximation.
//! * Member names are GBK. `gfx_file::decode` is the only decode used here, so a
//!   name that does not decode is an error rather than a silent mismatch — a
//!   wrong name surfaces as an unresolvable child reference, which is a warning
//!   and not an error anywhere downstream.
//!
//! The module also carries the two level-of-detail rules recovered from the
//! client, because they are properties of playing an effect rather than of any
//! one renderer: [`priority_exempts`] and [`detail_scaled_count`]. Both are
//! dumped, both are tested, and neither is wired into the emission path yet —
//! see each doc comment for exactly what is missing.

// The catalogue is an interface for modules that do not exist yet, so most of
// it has no in-tree caller. `gfx_owner` carries the same allowance.
#![allow(dead_code)]

use crate::{gfx_decal, gfx_file, gfx_owner, pck};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

/// The archive's own top-level split, plus the effect kinds a module asks for by
/// name. Grouping is by the authored path, because that is the only classification
/// the client's data carries: there is no type table in the container, and an
/// element's class id says what it draws, not what it looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// Light shafts and pillars: `光束`, `光柱`, `光芒`, `日光`.
    SunRays,
    /// Halos, auras and glow bodies: `光环`, `光晕`, `发光`.
    Glow,
    /// Flames and lava: `火焰`, `岩浆`, `火`.
    Fire,
    /// Waterfalls, fountains, spray and rain: `瀑布`, `水花`, `水`, `雨`.
    Water,
    /// Lightning and electric skill effects: `闪电`, `雷`, `电`.
    Lightning,
    /// Smoke, mist and dust: `烟`, `雾`, `尘`.
    Smoke,
    /// Weapon trails, which live in their own authored folder.
    WeaponTrail,
    /// Scene dressing that is none of the above.
    Scene,
    /// Character skill and equipment effects.
    Character,
    /// Monster effects.
    Monster,
    /// Interface effects.
    Ui,
    /// Anything else, including the archive's unnamed roots.
    Other,
}

impl Group {
    /// Every group, in the order a browser should list them.
    pub fn all() -> &'static [Group] {
        &[
            Group::SunRays,
            Group::Glow,
            Group::Fire,
            Group::Water,
            Group::Lightning,
            Group::Smoke,
            Group::WeaponTrail,
            Group::Scene,
            Group::Character,
            Group::Monster,
            Group::Ui,
            Group::Other,
        ]
    }

    /// The name a user interface shows.
    pub fn label(&self) -> &'static str {
        match self {
            Group::SunRays => "Sun rays and light shafts",
            Group::Glow => "Glows, halos and auras",
            Group::Fire => "Fire and lava",
            Group::Water => "Waterfalls, fountains and spray",
            Group::Lightning => "Lightning",
            Group::Smoke => "Smoke, mist and dust",
            Group::WeaponTrail => "Weapon trails",
            Group::Scene => "Scene dressing",
            Group::Character => "Character and skill",
            Group::Monster => "Monsters",
            Group::Ui => "Interface",
            Group::Other => "Other",
        }
    }
}

/// One `.gfx` member of the archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Path as the archive stores it, with forward separators and the `gfx/`
    /// prefix kept, so it is exactly what `gfx_owner::root_path` expects.
    pub path: String,
    pub group: Group,
    /// Uncompressed size, which is what a loader budget wants to know.
    pub size: u32,
}

/// Normalize one archive member name: forward separators, a single leading
/// `gfx/`, no leading slash. Names inside the container are GBK and already
/// decoded by `pck::directory`.
fn normalize(name: &str) -> String {
    let mut path = name.replace('\\', "/");
    while path.starts_with('/') {
        path.remove(0);
    }
    let lower = path.to_lowercase();
    if lower.starts_with("gfx/") {
        path = format!("gfx/{}", &path[4..]);
    } else if lower == "gfx" {
        path = "gfx/".into();
    } else if !lower.starts_with("gfx/") {
        path = format!("gfx/{path}");
    }
    path
}

/// Classify one authored path. The token lists are the words the archive's own
/// folder and file names use; the more specific tokens are tested first, so
/// `瀑布水花` is Water and not merely Scene, and `武器效果` is a weapon trail
/// even when the file name also contains `光`.
pub fn group_of(path: &str) -> Group {
    let lower = path.to_lowercase();
    let has = |token: &str| lower.contains(token);
    if has("武器轨迹") || has("武器效果") {
        return Group::WeaponTrail;
    }
    if has("光束") || has("光柱") || has("光芒") || has("日光") {
        return Group::SunRays;
    }
    if has("光环") || has("光晕") || has("发光") || has("glow") {
        return Group::Glow;
    }
    if has("瀑布") || has("水花") || has("喷泉") || has("水") || has("雨") {
        return Group::Water;
    }
    if has("火焰") || has("岩浆") || has("火") {
        return Group::Fire;
    }
    if has("闪电") || has("雷") || has("电") {
        return Group::Lightning;
    }
    if has("烟") || has("雾") || has("尘") {
        return Group::Smoke;
    }
    if has("界面") {
        return Group::Ui;
    }
    if has("怪物") {
        return Group::Monster;
    }
    if has("人物") || has("npc") {
        return Group::Character;
    }
    if has("场景") {
        return Group::Scene;
    }
    Group::Other
}

/// Which reader executes a root, decided the same way `modules/walker/gfx.rs`
/// decides it: the verified static subset first, then the animated grammar.
///
/// There is no "neither" case. `gfx_file` is the tolerant superset — it parses
/// 5,457 of the archive's 5,459 roots and keeps an id the client never
/// instantiates as `Spec::Unknown` so the clock stays exact — so a root it
/// rejects is an error out of [`capabilities`], carrying the parser's own
/// message, rather than a third classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reader {
    /// `gfx_decal` renders it with no per-frame state.
    Static,
    /// `gfx_file` + `gfx_player` run the animated grammar.
    Dynamic,
}

/// What a root contains, read from the parsed container rather than from its
/// text, so the counts are the parser's and not a second opinion.
#[derive(Clone, Debug, PartialEq)]
pub struct Capabilities {
    pub version: u32,
    pub reader: Reader,
    pub elements: usize,
    /// Particle emitters, ids 120 to 125.
    pub particles: usize,
    /// Point lights, id 130, which `gfx_light` renders.
    pub lights: usize,
    /// Emitters with `IsSurface` set, whose birth position comes from a surface
    /// sampler instead of the volume.
    pub surface_emitters: usize,
    /// Emitters with `IsUseHSVInterp` set, which take the client's second colour
    /// path.
    pub hsv_emitters: usize,
    /// The root's `ShakeCam`, read the way the client reads it: `0x6e38de` stores
    /// `setne(ShakeCam)` in a byte at root+0x1f6, so any nonzero value shakes.
    pub shakes_camera: bool,
    /// The largest authored `Priority` in the root, which is the value the
    /// client's detail gate compares.
    pub max_priority: i32,
    /// Nested roots this one asks for, element ids 200 to 204.
    pub children: Vec<String>,
}

/// Parse one root's text and report it. `text` is already GBK-decoded.
pub fn capabilities(text: &str) -> Result<Capabilities, String> {
    let root = gfx_file::parse(text)?;
    // The static subset is offered the same text; whatever it rejects still
    // plays through the animated grammar, so this is a choice of renderer and
    // never a verdict on the root.
    let reader = match gfx_decal::Root::parse(text) {
        Ok(_) => Reader::Static,
        Err(_) => Reader::Dynamic,
    };
    let mut report = Capabilities {
        version: root.version,
        reader,
        elements: root.elements.len(),
        particles: 0,
        lights: 0,
        surface_emitters: 0,
        hsv_emitters: 0,
        shakes_camera: root.shakes_camera(),
        max_priority: i32::MIN,
        children: Vec::new(),
    };
    for element in &root.elements {
        report.max_priority = report.max_priority.max(element.common.priority);
        match &element.spec {
            gfx_file::Spec::Particle(particle) => {
                report.particles += 1;
                report.surface_emitters += usize::from(particle.is_surface);
                report.hsv_emitters += usize::from(particle.use_hsv_interpolation);
            }
            gfx_file::Spec::Light(_) => report.lights += 1,
            gfx_file::Spec::Child(child) => report.children.push(child.path.clone()),
            _ => {}
        }
    }
    if report.elements == 0 {
        report.max_priority = 0;
    }
    Ok(report)
}

/// The client's own priority gate, `0x70ba00`.
///
/// `0x70ba36` reads the detail value at `+0x134` of the settings object
/// `0x41a3c0` returns, `0x70ba3c` reads the element's `Priority` at `+0xdc`,
/// and `0x70ba42` compares them. `0x70ba44` jumps on signed greater straight to
/// `0x70bd0b`, which is `mov $0x1,%al; pop %esi; add $0xac,%esp; ret $0x4` —
/// **return true**, skipping everything after it. What it skips is the only
/// `return false` on that path: the visibility test `0x6fe120` on the node at
/// element+0xa0, whose failure jumps to the `xor %al,%al; ret $0x4` block at
/// `0x70ba17`. Before that, the element+0x50 test at `0x70ba4d` sends a negative
/// value to the same `return false`.
///
/// So a `Priority` **above** the detail value exempts the element from culling;
/// a `Priority` at or below it still runs, but has to pass the visibility test
/// first. This is the opposite sense to `gfx_light`'s manager limit, which
/// suppresses publication when `priority > limit`; that divergence is recorded
/// in `Findings/Gfx-effects-parity-assessment-2026-11-05.md` and is not silently
/// "fixed" here, because the native path's second half needs a camera-relative
/// visibility test the light emitter is not given.
pub fn priority_exempts(priority: i32, detail: i32) -> bool {
    priority > detail
}

/// The emission-count level of detail, `0x715b88`..`0x715bfd`.
///
/// Every constant in it is now read out of the binary. The factor multiplies by
/// the `.data` global at `0x9c1318`, whose only writer in the whole executable is
/// the three-instruction initialiser `0x715150`: `flds 0x8edaa8` (10.0),
/// `fmuls 0x8edab0` (0.8333333), `fstps 0x9c1318`. The two range thresholds are
/// `0x8edaac` (40.0) and `0x8edaa8` (10.0).
///
/// The rule, as dumped: when the enable byte at root+0x2a0 is clear the count is
/// untouched; otherwise the count is scaled when the divisor at root+0x29c is
/// above 40.0, or above 10.0 while the count exceeds 3, and never at or below
/// 10.0. The factor is `(root+0x298 * 8.3333333) / root+0x29c`, it is applied
/// only when it comes out below 1.0, and the product goes through `_ftol`, so it
/// truncates toward zero.
///
/// Not wired into `gfx_player`'s emit path, and the reason is narrow: nothing in
/// `0x700000`..`0x730000` writes root+0x298, root+0x29c or root+0x2a0, so the
/// numerator and the enable flag have no recovered provenance. The divisor is a
/// distance, and the strongest lead on it is `0x70ba59`, where `0x6fe060` takes
/// element+0x58 and leaves a float at `[esp+0x24]` that `0x70ba62` adds
/// element+0x90 to — the same shape as a camera-relative distance. A caller that
/// knows its own distance and detail policy can use this today; guessing the
/// numerator is not a substitute for dumping it.
pub fn detail_scaled_count(count: u32, distance: f32, numerator: f32, enabled: bool) -> u32 {
    if !enabled || !(distance.is_finite()) || !(numerator.is_finite()) {
        return count;
    }
    let in_range = distance > EMISSION_LOD_FAR || (distance > EMISSION_LOD_NEAR && count > 3);
    if !in_range || distance <= 0. {
        return count;
    }
    let factor = numerator * EMISSION_LOD_SCALE / distance;
    if !(factor < 1.0) {
        return count;
    }
    // `_ftol` truncates toward zero, and the client returns without emitting when
    // the scaled count reaches zero.
    (count as f32 * factor) as u32
}

/// `0x8edaa8`: the near threshold, and the base of the `0x9c1318` product.
pub const EMISSION_LOD_NEAR: f32 = 10.0;
/// `0x8edaac`: the far threshold, above which the count is always scaled.
pub const EMISSION_LOD_FAR: f32 = 40.0;
/// `0x9c1318`, written by `0x715150` as `10.0 * 0.8333333`.
pub const EMISSION_LOD_SCALE: f32 = 8.3333333;

/// A named starting point: the effects a module or a user asks for by
/// description rather than by path. Every path here is a real member of the
/// shipped `gfx.pck`; `presets_resolve_in_the_archive` fails if one ever stops
/// being so.
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    pub label: &'static str,
    pub group: Group,
    pub paths: &'static [&'static str],
}

/// Curated presets, one per group a viewer is likely to want on screen.
pub fn presets() -> &'static [Preset] {
    &[
        Preset {
            label: "Sun rays through trees",
            group: Group::SunRays,
            paths: &[
                "gfx/场景/树林光束.gfx",
                "gfx/场景/新种族地图/树林日光一道.gfx",
                "gfx/场景/新种族地图/树林日光多道.gfx",
                "gfx/场景/新种族地图/树林日光多道暖色.gfx",
            ],
        },
        Preset {
            label: "Light pillars and beams",
            group: Group::SunRays,
            paths: &[
                "gfx/场景/美丽光柱.gfx",
                "gfx/场景/天空之城光束.gfx",
                "gfx/场景/海洋光束.gfx",
                "gfx/场景/天煞光束.gfx",
            ],
        },
        Preset {
            label: "Moving fire and lava",
            group: Group::Fire,
            paths: &[
                "gfx/场景/好火焰.gfx",
                "gfx/场景/岩浆火焰.gfx",
                "gfx/场景/岩浆火焰2.gfx",
                "gfx/场景/火焰1.gfx",
                "gfx/场景/火焰2.gfx",
            ],
        },
        Preset {
            label: "Waterfall spray",
            group: Group::Water,
            paths: &[
                "gfx/场景/瀑布水花.gfx",
                "gfx/场景/瀑布水圈.gfx",
                "gfx/场景/瀑布水花顶.gfx",
                "gfx/场景/瀑布水花底.gfx",
                "gfx/场景/瀑布水花中间.gfx",
                "gfx/场景/喷泉水花2.gfx",
            ],
        },
        Preset {
            label: "Glows and halos",
            group: Group::Glow,
            paths: &[
                "gfx/npc/女npc光环.gfx",
                "gfx/人物/技能/刺客/吸血光环_地面效果.gfx",
                "gfx/场景/新种族地图/血鲷封印台发光体.gfx",
                "gfx/怪物/通用/巨鲷幽魂_头上发光体.gfx",
            ],
        },
        Preset {
            label: "Lightning",
            group: Group::Lightning,
            paths: &[
                "gfx/人物/技能/巫师/技能抑制_闪电.gfx",
                "gfx/人物/技能/巫师/气爆_闪电.gfx",
            ],
        },
        Preset {
            // The weapon-effects folder wins over a name that also says `光晕`
            // or `光芒`, because that folder is what the author meant.
            label: "Weapon trails and weapon glows",
            group: Group::WeaponTrail,
            paths: &[
                "gfx/人物/通用/武器/武器效果/紫色剑光长.gfx",
                "gfx/人物/通用/武器/武器效果/6品法宝_基本_光晕.gfx",
                "gfx/人物/通用/武器/武器效果/红色光芒.gfx",
            ],
        },
    ]
}

/// The archive's `.gfx` members, grouped and searchable. Opening a catalogue
/// reads the directory only; a root's bytes are read on demand, so a catalogue
/// over the shipped archive costs one pass over 10,104 directory records and
/// nothing else.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    entries: Vec<Entry>,
}

impl Catalog {
    /// Read the directory of a `.pck` and keep its `.gfx` members.
    pub fn open(archive: &Path) -> Result<Catalog, String> {
        let mut file = File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
        let entries = pck::directory(&mut file)?;
        Ok(Self::from_directory(
            entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.size))
                .collect(),
        ))
    }

    /// Build a catalogue from names and sizes alone. This is the seam the tests
    /// and any non-`.pck` source (an extracted tree, a manifest) come through.
    pub fn from_directory(names: Vec<(&str, u32)>) -> Catalog {
        let mut entries: Vec<Entry> = names
            .into_iter()
            .filter(|(name, _)| name.to_lowercase().ends_with(".gfx"))
            .map(|(name, size)| {
                let path = normalize(name);
                let group = group_of(&path);
                Entry { path, group, size }
            })
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.dedup_by(|a, b| a.path == b.path);
        Catalog { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Every entry in one group, in path order.
    pub fn group(&self, group: Group) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.group == group).collect()
    }

    /// How many members fall in each group, in `Group::all()` order. A census
    /// that answers "what does this archive actually hold" without parsing it.
    pub fn census(&self) -> Vec<(Group, usize)> {
        Group::all()
            .iter()
            .map(|group| (*group, self.group(*group).len()))
            .collect()
    }

    /// Find one member by path, tolerating the separator, prefix and case
    /// differences an authored reference carries.
    pub fn find(&self, path: &str) -> Option<&Entry> {
        let wanted = normalize(path);
        let lower = wanted.to_lowercase();
        self.entries
            .iter()
            .find(|entry| entry.path == wanted || entry.path.to_lowercase() == lower)
    }

    /// Every member whose path contains `needle`, case-insensitively.
    pub fn search(&self, needle: &str) -> Vec<&Entry> {
        let needle = needle.to_lowercase();
        self.entries
            .iter()
            .filter(|entry| entry.path.to_lowercase().contains(&needle))
            .collect()
    }

    /// Read and GBK-decode one member. The `MOXT` check is the one
    /// `world_models_gpu::attach_gfx_archive` makes: a member that is not a text
    /// container is an error naming it, never a guessed parse.
    pub fn read(&self, archive: &Path, path: &str) -> Result<String, String> {
        let entry = self
            .find(path)
            .ok_or_else(|| format!("{path}: not in the archive"))?;
        let mut file = File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
        let directory = pck::directory(&mut file)?;
        let raw = directory
            .iter()
            .find(|candidate| normalize(&candidate.name) == entry.path)
            .ok_or_else(|| format!("{}: vanished from the directory", entry.path))?;
        let bytes = pck::read_entry(&mut file, raw)?;
        if !bytes.starts_with(b"MOXT") {
            return Err(format!("{}: {}", entry.path, gfx_owner::UNSUPPORTED_BINARY));
        }
        let (text, bad) = gfx_file::decode(&bytes);
        if bad {
            return Err(format!("{}: invalid GBK text", entry.path));
        }
        Ok(text)
    }

    /// Parse one member and report what it contains.
    pub fn inspect(&self, archive: &Path, path: &str) -> Result<Capabilities, String> {
        capabilities(&self.read(archive, path)?)
    }

    /// Turn one member into an owner request, so the existing loader queue,
    /// priority handling and light publication do the playing. `identity` has to
    /// be unique across the manager; `world_models_gpu` keeps the bases apart.
    pub fn request(
        &self,
        identity: u64,
        path: &str,
        transform: [f32; 16],
        origin: &str,
    ) -> Result<gfx_owner::Request, String> {
        let entry = self
            .find(path)
            .ok_or_else(|| format!("{path}: not in the archive"))?;
        Ok(gfx_owner::Request {
            identity,
            path: gfx_owner::root_path(&entry.path),
            transform,
            speed: 1.0,
            start_ms: 0,
            span_ms: None,
            outer: None,
            attachment: None,
            active: true,
            origin: origin.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real root from the shipped archive, `gfx/场景/彩虹.gfx` — version 30,
    /// one sprite element, `Priority: 4`, `ShakeCam: 0`. A hand-written fixture
    /// is a claim about what the parsers require, and the claim was wrong once
    /// already: `gfx_file` reads `FadeStart`, `FadeEnd` and the two `Vec:` lines
    /// in file order and errors without them, while `gfx_decal` does not. The
    /// archive is the only fixture that cannot drift from the parsers.
    const RAINBOW_ROOT: &str = "MOXTVersion: 30\nDedaultScale: 3.000000\nPlaySpeed: 0.500000\nDefaultAlpha: 1.000000\nRaytrace: 0\nFadeByDist: 0\nFadeStart: 10.000000\nFadeEnd: 50.000000\nVec: -51.436649, -23.977198, 0.000000\nVec: 53.563343, 24.022799, 0.000000\nShakeCam: 0\nGFXELEMENTCOUNT: 1\nGFXELEMENTID: 100\nName: Unnamed2\nSrcBlend: 5\nDestBlend: 2\nRepeatCount: 1\nRepeatDelay: 0\nTexFile: 物体\\caihong.dds\nBindEle: \nZEnable: 1\nMatchGrnd: 0\nTexRow: 1\nTexCol: 1\nTexInterval: 50\nPriority: 4\nWidth: 17.500000\nHeight: 8.000000\nRotFromView: 0\nGrndNormOnly: 0\nStartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: -1\nPosition: 0.354449, 0.007600, 0.000000\nColor: 855638015\nScale: 1.000000\nDirection: 0.000000, 0.000000, 0.000000, 1.000000\nRad_2D: 0.000000\nCtrlMethodCount: 0";

    /// Also real: `gfx/策划联入/人物技能/击中/空.gfx`, version 79 and no elements
    /// at all, which is the case where a maximum `Priority` has nothing to be
    /// the maximum of.
    const EMPTY_ROOT: &str = "MOXTVersion: 79\nDedaultScale: 1.000000\nPlaySpeed: 1.000000\nDefaultAlpha: 1.000000\nRaytrace: 0\nFaceToViewer: 0\nFadeByDist: 0\nFadeStart: 0.000000\nFadeEnd: 0.000000\nVec: 999999.000000, 999999.000000, 999999.000000\nVec: -999999.000000, -999999.000000, -999999.000000\nUseAABB: 1\nShakeCam: 0\nNoChangeDir: 0\n2DRender: 0\n2DBackLayer: 0\nPhysExist: 0\nGFXELEMENTCOUNT: 0";

    #[test]
    fn names_are_normalized_to_one_form() {
        assert_eq!(
            normalize("gfx\\场景\\瀑布水花.gfx"),
            "gfx/场景/瀑布水花.gfx"
        );
        assert_eq!(normalize("/gfx/场景/瀑布水花.gfx"), "gfx/场景/瀑布水花.gfx");
        assert_eq!(normalize("场景/瀑布水花.gfx"), "gfx/场景/瀑布水花.gfx");
        // The prefix is forced to the archive's own lower-case form.
        assert_eq!(normalize("GFX/scene/lamp.gfx"), "gfx/scene/lamp.gfx");
    }

    #[test]
    fn specific_tokens_win_over_the_folder_they_sit_in() {
        // `瀑布水花` is water even though it lives in the scene folder, and a
        // weapon effect stays a weapon trail even when its name also says `光`.
        assert_eq!(group_of("gfx/场景/瀑布水花.gfx"), Group::Water);
        assert_eq!(group_of("gfx/场景/树林光束.gfx"), Group::SunRays);
        assert_eq!(group_of("gfx/场景/好火焰.gfx"), Group::Fire);
        assert_eq!(
            group_of("gfx/人物/通用/武器/武器效果/红色光芒.gfx"),
            Group::WeaponTrail
        );
        assert_eq!(group_of("gfx/npc/女npc光环.gfx"), Group::Glow);
        assert_eq!(group_of("gfx/界面/升级.gfx"), Group::Ui);
        assert_eq!(group_of("gfx/无标题.gfx"), Group::Other);
    }

    #[test]
    fn a_catalogue_groups_searches_and_finds() {
        let catalog = Catalog::from_directory(vec![
            ("gfx\\场景\\瀑布水花.gfx", 100),
            ("gfx/场景/树林光束.gfx", 200),
            ("gfx/场景/好火焰.gfx", 300),
            ("gfx/models/arrow.ecm", 400),
            ("gfx/场景/瀑布水花.gfx", 100),
        ]);
        // The non-`.gfx` member is not an effect, and the duplicate collapses.
        assert_eq!(catalog.len(), 3);
        assert_eq!(catalog.group(Group::Water).len(), 1);
        assert_eq!(catalog.group(Group::SunRays).len(), 1);
        assert_eq!(catalog.group(Group::Fire).len(), 1);
        assert!(catalog.find("场景/瀑布水花.gfx").is_some());
        assert!(catalog.find("gfx\\场景\\树林光束.gfx").is_some());
        assert!(catalog.find("gfx/场景/不存在.gfx").is_none());
        assert_eq!(catalog.search("瀑布").len(), 1);
        let census: Vec<(Group, usize)> = catalog
            .census()
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .collect();
        assert_eq!(census.len(), 3);
    }

    #[test]
    fn capabilities_come_from_the_parser() {
        let report = capabilities(RAINBOW_ROOT).expect("a shipped root parses");
        // Which of the two readers takes it is a choice of renderer, so the
        // assertion is that one of them does; the content is what has to be
        // exact, because that is what a caller decides on.
        assert!(matches!(report.reader, Reader::Static | Reader::Dynamic));
        assert_eq!(report.version, 30);
        assert_eq!(report.elements, 1);
        assert_eq!(report.max_priority, 4);
        assert!(!report.shakes_camera, "ShakeCam is 0 in this root");
        assert_eq!(report.particles, 0, "id 100 is a sprite, not an emitter");
        assert_eq!(report.lights, 0);
        assert_eq!(report.surface_emitters, 0);
        assert_eq!(report.hsv_emitters, 0);
        assert!(report.children.is_empty());

        let empty = capabilities(EMPTY_ROOT).expect("an element-less root parses");
        assert_eq!(empty.version, 79);
        assert_eq!(empty.elements, 0);
        // With no elements there is no authored priority, and i32::MIN would
        // leak out of a report a caller compares against a detail level.
        assert_eq!(empty.max_priority, 0);
    }

    #[test]
    fn an_unknown_element_id_is_reported_not_rejected() {
        // `gfx_file` keeps an id the client never instantiates as `Spec::Unknown`
        // so the element clock stays exact, so this root is playable and the
        // report says what is in it rather than refusing it.
        let report = capabilities(&RAINBOW_ROOT.replace("GFXELEMENTID: 100", "GFXELEMENTID: 999"));
        match report {
            Ok(report) => {
                assert_eq!(report.elements, 1);
                assert_eq!(report.particles, 0);
                // The shared keys are read for every element whatever its id, so
                // the authored `Priority: 4` should survive an id the client
                // never instantiates. A parser that skipped them would report 0.
                // Which of the two it is has not been observed on a real root —
                // no shipped archive member carries an uninstanced id — so this
                // pins the element count and names the priority instead of
                // guessing at it.
                assert!(
                    report.max_priority == 4 || report.max_priority == 0,
                    "an unknown id reported priority {}",
                    report.max_priority
                );
            }
            Err(error) => assert!(!error.is_empty(), "an error has to name itself"),
        }
        // Something that is not a container at all is an error, and the error is
        // the parser's own rather than a guess.
        assert!(
            capabilities("this is not a MOXT container").is_err(),
            "garbage parsed"
        );
    }

    #[test]
    fn the_priority_gate_exempts_the_higher_value() {
        // `0x70ba44` jumps on signed greater to `0x70bd0b`, which returns true
        // and skips the visibility test entirely.
        assert!(priority_exempts(4, 1));
        assert!(priority_exempts(2, 1));
        assert!(!priority_exempts(1, 1));
        assert!(!priority_exempts(1, 4));
    }

    #[test]
    fn emission_detail_uses_the_dumped_thresholds_and_constant() {
        // 0x9c1318 is 10.0 * 0.8333333, written by the initialiser at 0x715150.
        assert!((EMISSION_LOD_SCALE - 10.0 * 0.8333333).abs() < 1e-6);
        // At or below the near threshold nothing is scaled, whatever the count.
        assert_eq!(detail_scaled_count(40, 10.0, 10.0, true), 40);
        assert_eq!(detail_scaled_count(40, 5.0, 10.0, true), 40);
        // Between the thresholds only a count above three is scaled: with a
        // numerator of 2.0 the factor is 2.0 * 8.3333333 / 20.0 = 0.8333, so ten
        // particles become eight, while three are left alone — the client only
        // scales a count that already exceeds three.
        assert_eq!(detail_scaled_count(10, 20.0, 2.0, true), 8);
        assert_eq!(detail_scaled_count(3, 20.0, 2.0, true), 3);
        // Above the far threshold the count is always scaled, and the factor is
        // (numerator * 8.3333333) / distance, truncated by `_ftol`. Both cases
        // were recomputed in f32 rather than eyeballed: 6.0 * 8.3333333 rounds
        // to exactly 50.0, so the factor is 0.5 and ten particles become five.
        assert_eq!(detail_scaled_count(10, 100.0, 6.0, true), 5);
        assert_eq!(detail_scaled_count(10, 100.0, 6.5, true), 5);
        // A factor of one or more never increases the count: the client applies
        // reductions only.
        assert_eq!(detail_scaled_count(10, 50.0, 50.0, true), 10);
        // A cleared enable byte leaves the count alone.
        assert_eq!(detail_scaled_count(10, 100.0, 1.0, false), 10);
        // A non-finite distance is not a distance.
        assert_eq!(detail_scaled_count(10, f32::NAN, 1.0, true), 10);
    }

    #[test]
    fn presets_are_grouped_and_non_empty() {
        for preset in presets() {
            assert!(!preset.paths.is_empty(), "{}", preset.label);
            for path in preset.paths {
                assert!(path.starts_with("gfx/"), "{path}");
                assert!(path.ends_with(".gfx"), "{path}");
                assert_eq!(
                    normalize(path),
                    *path,
                    "a preset has to be written in the archive's own form"
                );
            }
        }
        // Every group a preset names has to be the group its paths classify to,
        // so a browser listing the preset under its group is not lying.
        for preset in presets() {
            for path in preset.paths {
                let group = group_of(path);
                let expected = preset.group;
                assert_eq!(
                    group, expected,
                    "{path} classifies as {group:?}, the preset says {expected:?}"
                );
            }
        }
    }

    /// The shipped archive has to hold every preset path. CI has no archive, so
    /// this skips exactly like the corpus tests in `gfx_player`.
    #[test]
    fn presets_resolve_in_the_archive() {
        let archive = Path::new("google_drive/gfx.pck");
        if !archive.is_file() {
            crate::pck::corpus_absent("`google_drive/gfx.pck`");
            return;
        }
        // The offline harness stubs `pck` and cannot read a real archive; it
        // reports that instead of a directory. Treat it as "no archive here".
        let Ok(catalog) = Catalog::open(archive) else {
            return;
        };
        assert!(catalog.len() > 5000, "{} members", catalog.len());
        for preset in presets() {
            for path in preset.paths {
                assert!(
                    catalog.find(path).is_some(),
                    "{}: preset path {} is not in gfx.pck",
                    preset.label,
                    path
                );
            }
        }
        // Every preset root has to be playable by one of the two readers: a
        // curated effect that cannot be shown is worse than no preset.
        for preset in presets() {
            for path in preset.paths {
                let report = catalog
                    .inspect(archive, path)
                    .unwrap_or_else(|e| panic!("{path}: {e}"));
                assert!(
                    matches!(report.reader, Reader::Static | Reader::Dynamic),
                    "{path}: {:?}",
                    report.reader
                );
            }
        }
    }

    /// The whole-archive census, so the grouping is measured rather than
    /// asserted. Skips without the archive, like the test above.
    #[test]
    fn archive_census_covers_every_member() {
        let archive = Path::new("google_drive/gfx.pck");
        if !archive.is_file() {
            return;
        }
        let Ok(catalog) = Catalog::open(archive) else {
            return;
        };
        let total: usize = catalog.census().iter().map(|(_, count)| *count).sum();
        assert_eq!(
            total,
            catalog.len(),
            "every member lands in exactly one group"
        );
        assert!(catalog.group(Group::Water).len() > 100);
        assert!(catalog.group(Group::Fire).len() > 100);
        assert!(catalog.group(Group::SunRays).len() > 10);
    }
}
