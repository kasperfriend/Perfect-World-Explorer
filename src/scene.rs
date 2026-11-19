//! ECWLD v12 scene blocks from the Perfect World client.
//! Layout informed by https://forum.ragezone.com/threads/map-editor.1252114/
use std::{collections::HashSet, fs, path::Path};
#[derive(Clone, Debug)]
pub struct TreeType {
    pub tree: String,
    pub composite: String,
}
#[derive(Clone, Debug)]
pub struct GrassType {
    pub path: String,
    // Raw 4-byte serialized slot: only its low byte is the bool; padding
    // contains nonzero legacy bytes. Retail grass initialization forces false.
    pub alpha_blend: i32,
    pub alpha_ref: i32,
    pub size: f32,
    pub size_variation: f32,
    pub softness: f32,
    pub sight_range: f32,
}
#[derive(Clone, Debug)]
pub struct Tree {
    pub tree_type: u32,
    pub position: [f32; 3],
}
#[derive(Clone, Debug)]
pub struct Reference {
    pub export_id: u32,
    pub offset: u32,
}
/// Tree types a world file may declare. The largest staged table is 59 (1.8.4).
pub const MAX_TREE_TYPES: usize = 4096;

/// Grass types a world file may declare. The largest staged table is 244 (1.8.4).
pub const MAX_GRASS_TYPES: usize = 4096;

#[derive(Clone, Debug)]
pub struct SceneModel {
    pub export_id: u32,
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub up: [f32; 3],
    /// The record's own scale (`fScale`). Version 12 has no such field and
    /// every model is 1.0; a later layout inserts it between `vUp` and the
    /// model's path, so a reader that stops at `vUp` takes the scale for a
    /// string length and reads the rest of the block as garbage.
    pub scale: f32,
    pub path: String,
    pub action: String,
}
#[derive(Clone, Debug)]
pub struct Effect {
    /// Position of this record in its block's effect list, for diagnostics.
    pub index: u32,
    /// The record's own id word (`record[0]`). It is not the block list index:
    /// the **2,971** placements of the shipped `world.ecwld` carry 2,971
    /// distinct values.
    ///
    /// (This said 2,275 for a while. That was an earlier reader's count, left
    /// behind when the record's leading id word was recovered and every block
    /// stopped losing its last record; re-measured against the staged file.)
    pub export_id: u32,
    pub scale: f32,
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub up: [f32; 3],
    pub speed: f32,
    pub valid_time: i32,
    pub alpha: f32,
    pub attenuation: bool,
    pub path: String,
}
impl Effect {
    /// The effect record, re-derived against every placement the shipped map
    /// authors.
    ///
    /// The record opens with an **id word** and the payload follows it; an
    /// earlier reading started at the payload and so landed every field four
    /// bytes early. A shift like that is invisible in a round-trip test and
    /// fatal in the world: with the old offsets each day-only placement read its
    /// day/night word out of its alpha, its play speed out of its day/night word
    /// (a float bit pattern, so 1.06e9 — a clock that jumps and freezes), and its
    /// position out of the middle of its own direction vector.
    ///
    /// The shipped `world.ecwld` decides between the two readings on all **2,971**
    /// records with no exceptions. Under this one `[20..32)` and `[32..44)` are
    /// unit vectors and perpendicular to each other on 2,971 of 2,971 records,
    /// the play speeds span 0.030..12.000 with no non-positive value, the uniform
    /// scales span 0.054..380.953, and the day/night word takes only the three
    /// authored modes with the split 2,004 day-and-night / 787 day-only / 180
    /// night-only. Under the old one the same fields are a mixture of ids and
    /// float bit patterns and the direction is non-unit on almost every record.
    /// `corpus_effect_records_agree_on_one_layout` holds the map to that.
    ///
    /// The copy block at `0x5b3436..0x5b34a6` carries the same field order: the
    /// position triple first (with the caller's x/z addends), then the two
    /// vectors, then the three dwords and the attenuation byte. The x/z addends
    /// itself is not a block origin: the only caller that supplies it
    /// (`0x41bb30`) pushes two zeros, so record positions are absolute world
    /// coordinates and no origin term belongs in this reader.
    fn read(data: &[u8], p: &mut usize) -> Result<Self, String> {
        let start = *p;
        let bytes = data
            .get(start..start.checked_add(60).ok_or("effect offset overflow")?)
            .ok_or("short scene effect record")?;
        *p += 60;
        Ok(Self {
            index: 0,
            export_id: u32at(bytes, 0)?,
            scale: f32at(bytes, 4)?,
            position: vec3(bytes, 8)?,
            direction: vec3(bytes, 20)?,
            up: vec3(bytes, 32)?,
            speed: f32at(bytes, 44)?,
            valid_time: u32at(bytes, 48)? as i32,
            alpha: f32at(bytes, 52)?,
            attenuation: bytes[56] != 0,
            path: string(data, p)?,
        })
    }
    /// 0x5b18a1..0x5b18d4: both day-only and night-only effects are
    /// invalid at exactly 0.5. The first x87 C0 test treats unordered as day.
    pub fn valid_at(&self, night_factor: f32) -> bool {
        effect_visible(self.valid_time, night_factor)
    }
}
/// ECWLD effect time eligibility, shared with the runtime light owners so the
/// loaded and published states cannot diverge.
pub fn effect_visible(valid_time: i32, night_factor: f32) -> bool {
    valid_time == 2
        || (valid_time == 0 && (night_factor < 0.5 || night_factor.is_nan()))
        || (valid_time == 1 && night_factor > 0.5)
}

#[derive(Clone, Debug)]
pub struct Sound {
    pub export_id: u32,
    pub position: [f32; 3],
    /// Native sound-buffer near/far distance parameters (ECWLD offsets 16/20).
    pub min_distance: f32,
    pub max_distance: f32,
    /// Serialized word at offset 24, not copied by the inspected client loader.
    pub reserved: u32,
    /// Native owner mode at offset 28; its complete meaning is not yet proven.
    pub mode: i32,
    pub path: String,
}
impl Sound {
    fn read(data: &[u8], p: &mut usize) -> Result<Self, String> {
        let end = p.checked_add(32).ok_or("sound offset overflow")?;
        let bytes = data.get(*p..end).ok_or("short scene sound record")?;
        *p = end;
        Ok(Self {
            export_id: u32at(bytes, 0)?,
            position: vec3(bytes, 4)?,
            min_distance: f32at(bytes, 16)?,
            max_distance: f32at(bytes, 20)?,
            reserved: u32at(bytes, 24)?,
            mode: u32at(bytes, 28)? as i32,
            path: string(data, p)?,
        })
    }
}
#[derive(Clone, Debug)]
pub struct Block {
    pub row: u16,
    pub col: u16,
    pub trees: Vec<Tree>,
    pub water: Vec<Reference>,
    pub grass: Vec<Reference>,
    pub ornaments: Vec<Reference>,
    pub boxes: Vec<Reference>,
    pub effects: Vec<Effect>,
    pub models: Vec<SceneModel>,
    pub critters: Vec<Reference>,
    pub beziers: Vec<Reference>,
    pub sounds: Vec<Sound>,
}
pub struct Scene {
    pub width: f32,
    pub length: f32,
    pub block_size: f32,
    pub rows: usize,
    pub cols: usize,
    pub tree_types: Vec<TreeType>,
    pub grass_types: Vec<GrassType>,
    pub blocks: Vec<Block>,
}
fn u32at(b: &[u8], p: usize) -> Result<u32, String> {
    let end = p.checked_add(4).ok_or("overflow")?;
    Ok(u32::from_le_bytes(
        b.get(p..end).ok_or("truncated ECWLD")?.try_into().unwrap(),
    ))
}
fn f32at(b: &[u8], p: usize) -> Result<f32, String> {
    let f = f32::from_bits(u32at(b, p)?);
    if !f.is_finite() {
        return Err("non-finite ECWLD float".into());
    }
    Ok(f)
}
fn string(b: &[u8], p: &mut usize) -> Result<String, String> {
    let n = u32at(b, *p)? as usize;
    let at = *p;
    *p += 4;
    if n > 4096 {
        // A length word this size is almost never a string: it is the reader
        // standing in the wrong field, which happens when a newer client's
        // record carries a field this layout does not know about. Report where,
        // because "oversized ECWLD string" alone cannot be acted on.
        return Err(format!(
            "oversized ECWLD string: a {n}-byte length at offset {at} (this build's record layout does not match the file)"
        ));
    }
    let e = p.checked_add(n).ok_or("string overflow")?;
    let bytes = b
        .get(*p..e)
        .ok_or_else(|| format!("truncated ECWLD string of {n} bytes at offset {at}"))?;
    *p = e;
    let (s, _, bad) = encoding_rs::GBK.decode(bytes);
    if bad {
        return Err("invalid GBK scene string".into());
    }
    Ok(if s.contains('\\') {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    })
}
/// The EC-BSD header word that has stayed put, and the version this reader's
/// record layout was measured on.
///
/// `MOXB`/`FDSB` (little-endian `0x42584f4d` / `0x42534446`) identify the file;
/// the third word is its revision. The revision has moved -- 1.5.2's world
/// writes 12 and 1.8.4's writes 18 -- while everything below the header stayed
/// byte-identical, which the two shipped maps prove rather than suggest:
///
/// | client | EC-BSD | ECWLD | ornament refs readable | water refs |
/// |---|---|---|---|---|
/// | 1.5.2 | 12 | 12 | 22,981 of 22,981 | 111 of 111 |
/// | 1.8.4 | 18 | 17 | 23,068 of 23,068 | 111 of 111 |
///
/// Both files keep the name table at `+0x4c` with the same `u32 length` framing,
/// and both end it at exactly the same offset, so the revision word gates
/// nothing this reader touches. Refusing a revision other than 12 turned 1.8.4's
/// map into "unsupported EC-BSD header" and, through the viewer's fallback,
/// into a box that gave the user no map at all.
///
/// The revision is still read and reported, because a future layout change has
/// to start somewhere; it just is not a reason to abandon a map whose records
/// parse.
pub const ECBSD_VERSION: u32 = 12;
pub const ECBSD_IDENTITY: (u32, u32) = (0x42584f4d, 0x42534446);

/// Validate an EC-BSD's identity and revision.
///
/// Pass the revision only when the caller needs to know it; a file whose
/// revision is below [`ECBSD_VERSION`] is still read, because nothing in the
/// record layout below depends on the revision having reached it.
pub fn ecbsd_header(b: &[u8]) -> Result<u32, String> {
    let identity = (u32at(b, 0)?, u32at(b, 4)?);
    if identity != ECBSD_IDENTITY {
        return Err(format!(
            "this is not an EC-BSD file: it starts with {:#010x} {:#010x}",
            identity.0, identity.1
        ));
    }
    u32at(b, 8)
}

fn vec3(b: &[u8], p: usize) -> Result<[f32; 3], String> {
    Ok([f32at(b, p)?, f32at(b, p + 4)?, f32at(b, p + 8)?])
}
fn refs(b: &[u8], p: &mut usize, n: usize) -> Result<Vec<Reference>, String> {
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        out.push(Reference {
            export_id: u32at(b, *p)?,
            offset: u32at(b, *p + 4)?,
        });
        *p += 8;
    }
    Ok(out)
}
/// First ECWLD version whose static-model record carries a scale between its
/// up vector and its path strings.
///
/// The client header this recovery works from documents the field only from
/// `ECWDFILEECMODEL` onward, and the shipped files place the boundary above 12:
/// 1.8.4 writes version 17, which has the field, and a 1.5.2-era map writes 12,
/// which does not.
pub const MODEL_SCALE_VERSION: u32 = 13;

impl Scene {
    pub fn load(path: &Path) -> Result<Self, String> {
        let b = fs::read(path).map_err(|e| e.to_string())?;
        Self::parse(&b)
    }
    pub fn parse(b: &[u8]) -> Result<Self, String> {
        let version = match (u32at(b, 0), u32at(b, 4)) {
            (Ok(0x45435744), Ok(version)) => version,
            (Ok(identity), _) => {
                return Err(format!(
                    "this is not a world file: it starts with {identity:#010x}, not the ECWLD id"
                ))
            }
            _ => return Err("the world file is shorter than its header".into()),
        };
        // The header grew by fields up to v12 and has not changed since, so
        // anything at or above it parses with the same layout, and `p !=
        // data.len()` below still catches a block list that does not add up.
        // 1.5.2 and 1.8.4 both write 12; a gate that named one exact number
        // read "only ECWLD version 12 is supported" for a later client that
        // changed it.
        if version < 12 {
            return Err(format!(
                "this world file is ECWLD version {version}; this build reads version 12 and later"
            ));
        }
        let width = f32at(b, 8)?;
        let length = f32at(b, 12)?;
        let block_size = f32at(b, 16)?;
        let count = u32at(b, 24)? as usize;
        let rows = u32at(b, 28)? as usize;
        let cols = u32at(b, 32)? as usize;
        let tree_off = u32at(b, 36)? as usize;
        let grass_off = u32at(b, 40)? as usize;
        let tree_count = u32at(b, 44)? as usize;
        let grass_count = u32at(b, 48)? as usize;
        if count == 0
            || rows.checked_mul(cols) != Some(count)
            || count > 1_000_000
            // Measured across every staged client: the largest block grid is
            // the 1.8.4 world's 22,528 blocks (176x128), its tree table holds
            // 59 types and its grass table 244. Each limit carries headroom
            // over those and exists to catch a damaged header, not to police
            // what a newer client may author.
            || tree_count > MAX_TREE_TYPES
            || grass_count > MAX_GRASS_TYPES
            || tree_off > grass_off
            || grass_off > b.len()
        {
            return Err("invalid ECWLD header".into());
        }
        let offsets: Vec<usize> = (0..count)
            .map(|i| u32at(b, 116 + i * 4).map(|v| v as usize))
            .collect::<Result<_, _>>()?;
        let mut order = offsets.clone();
        order.sort_unstable();
        if order.iter().any(|&x| x < 116 + 4 * count || x >= tree_off)
            || order.windows(2).any(|w| w[0] == w[1])
        {
            return Err("invalid ECWLD block offsets".into());
        }
        let mut boundaries = std::collections::HashMap::with_capacity(offsets.len());
        for i in 0..count {
            boundaries.insert(
                order[i],
                if i + 1 < count {
                    order[i + 1]
                } else {
                    tree_off
                },
            );
        }
        let mut tp = tree_off;
        let mut tree_types = Vec::with_capacity(tree_count);
        for _ in 0..tree_count {
            tree_types.push(TreeType {
                tree: string(b, &mut tp)?,
                composite: string(b, &mut tp)?,
            });
        }
        if tp != grass_off {
            return Err("tree type table does not end at grass table".into());
        }
        let mut gp = grass_off;
        let mut grass_types = Vec::with_capacity(grass_count);
        for _ in 0..grass_count {
            let g = GrassType {
                alpha_blend: u32at(b, gp)? as i32,
                alpha_ref: u32at(b, gp + 4)? as i32,
                size: f32at(b, gp + 8)?,
                size_variation: f32at(b, gp + 12)?,
                softness: f32at(b, gp + 16)?,
                sight_range: f32at(b, gp + 20)?,
                path: {
                    gp += 24;
                    string(b, &mut gp)?
                },
            };
            grass_types.push(g)
        }
        if gp != b.len() {
            return Err(format!(
                "unparsed bytes after grass type table: {}",
                b.len() - gp
            ));
        }
        // Which model record layout this file uses. The scale field arrived
        // after version 12; 12 itself has none, which the corpus proves both
        // ways (1.8.4's v17 needs it in 65 blocks and reads in all 22,528,
        // while a v12 map misreads 34 blocks if it is added there).
        let model_has_scale = version >= MODEL_SCALE_VERSION;
        let mut blocks = Vec::with_capacity(count);
        let mut coords = HashSet::with_capacity(offsets.len());
        for off in offsets {
            let end = boundaries[&off];
            let data = &b[off..end];
            let row = u16::from_le_bytes(data.get(0..2).ok_or("short block")?.try_into().unwrap());
            let col = u16::from_le_bytes(data.get(2..4).ok_or("short block")?.try_into().unwrap());
            if row as usize >= rows || col as usize >= cols || !coords.insert((row, col)) {
                return Err("invalid or duplicate block coordinates".into());
            }
            let mut n = [0usize; 10];
            for i in 0..10 {
                n[i] = u32at(data, 4 + i * 4)? as usize;
                if n[i] > 100_000 {
                    return Err("invalid block object count".into());
                }
            }
            let mut p = 44;
            let mut trees = Vec::with_capacity(n[0]);
            for _ in 0..n[0] {
                let tree_type = u32at(data, p)?;
                if tree_type as usize >= tree_count {
                    return Err("invalid tree type".into());
                }
                trees.push(Tree {
                    tree_type,
                    position: vec3(data, p + 4)?,
                });
                p += 16;
            }
            let water = refs(data, &mut p, n[1])?;
            let ornaments = refs(data, &mut p, n[3])?;
            let boxes = refs(data, &mut p, n[4])?;
            let grass = refs(data, &mut p, n[2])?;
            let mut effects = Vec::with_capacity(n[5]);
            for index in 0..n[5] {
                let mut effect = Effect::read(data, &mut p)?;
                effect.index = index as u32;
                effects.push(effect);
            }
            let mut models = Vec::with_capacity(n[6]);
            for _ in 0..n[6] {
                let export_id = u32at(data, p)?;
                let position = vec3(data, p + 4)?;
                let direction = vec3(data, p + 16)?;
                let up = vec3(data, p + 28)?;
                p += 40;
                // The scale sits between `vUp` and the two strings from the
                // layout that added it, and it is the difference between a
                // readable block and a bogus string length: on 1.8.4's v17
                // `world.ecwld` every one of the 65 blocks holding a model
                // misreads without it, and all 22,528 read exactly with it.
                let scale = if model_has_scale {
                    let scale = f32at(data, p)?;
                    p += 4;
                    scale
                } else {
                    1.0
                };
                let path = string(data, &mut p)?;
                let action = string(data, &mut p)?;
                models.push(SceneModel {
                    export_id,
                    position,
                    direction,
                    up,
                    scale,
                    path,
                    action,
                })
            }
            let critters = refs(data, &mut p, n[7])?;
            let beziers = refs(data, &mut p, n[8])?;
            let mut sounds = Vec::with_capacity(n[9]);
            for _ in 0..n[9] {
                sounds.push(Sound::read(data, &mut p)?)
            }
            if p != data.len() {
                return Err(format!(
                    "block ({row},{col}) length mismatch: consumed {p}, actual {}",
                    data.len()
                ));
            }
            blocks.push(Block {
                row,
                col,
                trees,
                water,
                grass,
                ornaments,
                boxes,
                effects,
                models,
                critters,
                beziers,
                sounds,
            });
        }
        Ok(Self {
            width,
            length,
            block_size,
            rows,
            cols,
            tree_types,
            grass_types,
            blocks,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Ornament {
    pub export_id: u32,
    pub offset: u32,
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub up: [f32; 3],
    pub extra: u32,
    pub path: String,
    /// Map namespace used by the retail scenery archive (`litmodels/a01/...`).
    /// A folder name that is not a safe ID falls back to the legacy main-world
    /// namespace rather than becoming part of an archive path.
    pub map_id: String,
}
fn map_id_from_ecbsd_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("world")
        .to_ascii_lowercase();
    if !stem.is_empty()
        && stem.len() <= 64
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        stem
    } else {
        "world".into()
    }
}
impl Scene {
    /// Read placed lit-model references in the ECWLD's associated EC-BSD file.
    /// Blocks can refer to the same export ID, so each instance is returned once.
    pub fn load_ornaments(&self, path: &Path) -> Result<Vec<Ornament>, String> {
        let b = fs::read(path).map_err(|e| e.to_string())?;
        ecbsd_header(&b)?;
        let map_id = map_id_from_ecbsd_path(path);
        let mut seen = std::collections::HashMap::<u32, u32>::new();
        let mut result = Vec::new();
        for block in &self.blocks {
            for r in &block.ornaments {
                if let Some(old) = seen.insert(r.export_id, r.offset) {
                    if old != r.offset {
                        return Err(format!("ornament {} has inconsistent offsets", r.export_id));
                    }
                    continue;
                }
                let p = r.offset as usize;
                let position = vec3(&b, p)?;
                let direction = vec3(&b, p + 12)?;
                let up = vec3(&b, p + 24)?;
                let extra = u32at(&b, p + 36)?;
                let mut pos = p + 40;
                let path = string(&b, &mut pos)?;
                if path.is_empty() || !path.to_lowercase().ends_with(".bmd") {
                    return Err(format!(
                        "ornament {} has unexpected BMD path {path:?}",
                        r.export_id
                    ));
                }
                result.push(Ornament {
                    export_id: r.export_id,
                    offset: r.offset,
                    position,
                    direction,
                    up,
                    extra,
                    path,
                    map_id: map_id.clone(),
                });
            }
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Both shipped client revisions' EC-BSD files are the same layout.
    ///
    /// This is the test that keeps the version gate honest. The revision word
    /// moved (1.5.2 writes 12, 1.8.4 writes 18) while nothing below the header
    /// changed, so the reader must not refuse either one. These are the real
    /// files, staged in `google_drive/`; where they are absent the test says so
    /// rather than passing quietly.
    #[test]
    fn both_ecbsd_revisions_share_one_layout() {
        // The corpus lives at `google_drive/` beside the repository, which is
        // the working directory under both Cargo and the offline harness.
        let root = Path::new("google_drive");
        let cases = [
            (
                "ecbsd/world-1.5.2.ecbsd",
                "worlds/world-1.5.2.ecwld",
                12u32,
                12u32,
                22981,
            ),
            (
                "ecbsd/world-1.8.4.ecbsd",
                "scenecorpus/world-2.85M.ecwld",
                18,
                17,
                23068,
            ),
        ];
        let mut checked = 0;
        for (ecbsd, ecwld, want_ecbsd, want_ecwld, want_ornaments) in cases {
            let (ecbsd_path, ecwld_path) = (root.join(ecbsd), root.join(ecwld));
            let (Ok(bytes), Ok(world)) = (std::fs::read(&ecbsd_path), std::fs::read(&ecwld_path))
            else {
                println!("SKIPPED: {} or {} is not staged", ecbsd, ecwld);
                continue;
            };
            // The header check accepts the revision and reports it.
            let revision = ecbsd_header(&bytes).expect("an EC-BSD this build reads");
            assert_eq!(revision, want_ecbsd, "{ecbsd}: revision");
            let scene = Scene::parse(&world).expect("the paired world file parses");
            assert_eq!(
                u32at(&world, 4).unwrap(),
                want_ecwld,
                "{ecwld}: ECWLD revision"
            );
            // Every ornament reference in that world resolves to a record in
            // this EC-BSD: a BMD path, a finite position and no other payload.
            let ornaments = scene
                .load_ornaments(&ecbsd_path)
                .unwrap_or_else(|e| panic!("{ecbsd}: {e}"));
            assert_eq!(ornaments.len(), want_ornaments, "{ecbsd}: ornament count");
            assert!(
                ornaments
                    .iter()
                    .all(|o| o.path.to_lowercase().ends_with(".bmd")),
                "{ecbsd}: every ornament names a BMD"
            );
            checked += 1;
        }
        if checked == 0 {
            println!("SKIPPED: no EC-BSD pair is staged");
        }
    }

    #[test]
    fn scenery_map_namespace_comes_from_the_map_ecbsd_filename() {
        assert_eq!(
            map_id_from_ecbsd_path(Path::new("element/maps/a01/a01.ecbsd")),
            "a01"
        );
        assert_eq!(
            map_id_from_ecbsd_path(Path::new("element/maps/world/world.ecbsd")),
            "world"
        );
        assert_eq!(
            map_id_from_ecbsd_path(Path::new("element/maps/unsafe map/unsafe map.ecbsd")),
            "world"
        );
    }
    fn effect_bytes() -> Vec<u8> {
        let mut b = vec![0; 60];
        // Corrected record layout, measured on the shipped world.ecwld: id at
        // [0], scale at [4], position at [8..20), direction at [20..32), up at
        // [32..44), speed at [44], valid_time at [48], alpha at [52],
        // attenuation byte at [56], and [57..60) is padding.
        for (o, v) in [
            (4, 2.5f32),
            (8, 11.),
            (12, 12.),
            (16, 13.),
            (20, 0.),
            (24, 0.),
            (28, -1.),
            (32, 0.),
            (36, 1.),
            (40, 0.),
            (44, 0.75),
            (52, 0.25),
        ] {
            b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[0..4].copy_from_slice(&77u32.to_le_bytes());
        b[48..52].copy_from_slice(&2i32.to_le_bytes());
        b[56] = 1;
        // Padding is not another flag or part of the path length.
        b[57..60].fill(0xff);
        b.extend_from_slice(&5u32.to_le_bytes());
        b.extend_from_slice(b"a.gfx");
        b
    }
    #[test]
    fn preserves_complete_scene_effect_owner_record() {
        let b = effect_bytes();
        let mut p = 0;
        let e = Effect::read(&b, &mut p).unwrap();
        assert_eq!(p, b.len());
        assert_eq!(e.export_id, 77);
        assert_eq!(e.scale, 2.5);
        assert_eq!(e.position, [11., 12., 13.]);
        assert_eq!(e.direction, [0., 0., -1.]);
        assert_eq!(e.up, [0., 1., 0.]);
        assert_eq!(e.speed, 0.75);
        assert_eq!(e.valid_time, 2);
        assert_eq!(e.alpha, 0.25);
        assert!(e.attenuation);
        assert_eq!(e.path, "a.gfx");
        for n in 0..b.len() {
            assert!(Effect::read(&b[..n], &mut 0).is_err(), "length {n}");
        }
    }
    #[test]
    fn retains_scene_sound_distance_mode_and_path() {
        let mut b = vec![0; 32];
        b[0..4].copy_from_slice(&23u32.to_le_bytes());
        for (offset, value) in [(4, 1.0f32), (8, 2.0), (12, 3.0), (16, 5.0), (20, 50.0)] {
            b[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        b[24..28].copy_from_slice(&0x1234u32.to_le_bytes());
        b[28..32].copy_from_slice(&2i32.to_le_bytes());
        b.extend_from_slice(&5u32.to_le_bytes());
        b.extend_from_slice(b"x.wav");
        let mut p = 0;
        let s = Sound::read(&b, &mut p).unwrap();
        assert_eq!(p, b.len());
        assert_eq!((s.export_id, s.position), (23, [1., 2., 3.]));
        assert_eq!(
            (s.min_distance, s.max_distance, s.reserved, s.mode),
            (5., 50., 0x1234, 2)
        );
        assert_eq!(s.path, "x.wav");
        for n in 0..b.len() {
            assert!(Sound::read(&b[..n], &mut 0).is_err(), "length {n}");
        }
    }
    #[test]
    fn effect_validity_retains_native_strict_half_day_boundary() {
        let mut e = Effect::read(&effect_bytes(), &mut 0).unwrap();
        for (mode, expected) in [
            (0, [true, false, false, true]),
            (1, [false, false, true, false]),
            (2, [true, true, true, true]),
            (-1, [false, false, false, false]),
        ] {
            e.valid_time = mode;
            for (factor, expected) in [0., 0.5, 1., f32::NAN].into_iter().zip(expected) {
                assert_eq!(e.valid_at(factor), expected);
            }
        }
    }
    #[test]
    /// The corrected record layout, re-measured against the shipped map.
    ///
    /// A wrong field order is invisible in a synthetic fixture: every byte is
    /// self-consistent whichever way the reader slices it. It is not invisible
    /// here. With the id word counted, every placement agrees at once — the two
    /// vectors are unit and perpendicular on every record, the play speeds are
    /// all positive, and the day/night word lands on the three authored values.
    /// Skip when the archive is not staged beside the checkout.
    ///
    /// Three shipped `world.ecwld` builds have been measured, and all three pass
    /// every invariant below; only the totals differ, because they are different
    /// maps of the same client:
    ///
    /// | build | bytes | placements | day/night split |
    /// |---|---|---|---|
    /// | the 2.78 MB world the layout was first measured on | 2,780,000 | 2,275 | 154 / 676 / 1,445 |
    /// | the 2.78 MB world in `docs/cross-version-matrix.md` | ~2,780,000 | 3,535 | not recorded |
    /// | the 2.67 MB 1.8.4 world staged today | 2,668,870 | 2,971 | 180 / 787 / 2,004 |
    ///
    /// The count is asserted for the file actually present so a layout that
    /// shifts by one field cannot pass by being self-consistent about the wrong
    /// numbers, but the invariants are the real test.
    #[test]
    fn corpus_effect_records_agree_on_one_layout() {
        let path = std::path::Path::new("google_drive/world.ecwld");
        if !path.is_file() {
            crate::pck::corpus_absent("`google_drive/world.ecwld`");
            return;
        }
        let scene = Scene::load(path).unwrap();
        let mut ids = std::collections::HashSet::new();
        let mut modes = [0usize; 3];
        let mut total = 0;
        let mut bad_speed = 0;
        let mut bad_scale = 0;
        let mut bad_basis = 0;
        for block in &scene.blocks {
            for e in &block.effects {
                total += 1;
                ids.insert(e.export_id);
                match e.valid_time {
                    0 | 1 | 2 => modes[e.valid_time as usize] += 1,
                    _ => return panic!("valid_time {} is not an authored mode", e.valid_time),
                }
                if !e.speed.is_finite() || e.speed <= 0. {
                    bad_speed += 1;
                }
                if !e.scale.is_finite() || e.scale <= 0. {
                    bad_scale += 1;
                }
                let [dx, dy, dz] = e.direction;
                let [ux, uy, uz] = e.up;
                let dl = (dx * dx + dy * dy + dz * dz).sqrt();
                let ul = (ux * ux + uy * uy + uz * uz).sqrt();
                let dot = (dx * ux + dy * uy + dz * uz) / (dl * ul).max(f32::MIN_POSITIVE);
                if (dl - 1.).abs() > 0.01 || (ul - 1.).abs() > 0.01 || dot.abs() > 0.01 {
                    bad_basis += 1;
                }
            }
        }
        // The 2.67 MB 1.8.4 build staged in `google_drive/`. The 2.78 MB build
        // measured first gives 2,275 and 154/676/1,445; see the doc comment.
        assert_eq!(total, 2_971, "shipped world.ecwld placement count");
        assert_eq!(ids.len(), total, "every placement carries its own id");
        assert_eq!(modes, [180, 787, 2_004], "authored day/night split");
        assert_eq!((bad_speed, bad_scale, bad_basis), (0, 0, 0));
    }
    /// A static-model record is `vUp` then a scale then the path: the scale is
    /// read from the layout that added it, and leaving it out shifts every
    /// string after it by four bytes.
    ///
    /// The fixture is a minimal block carrying one model, built twice, with and
    /// without the field, so each version reads the one meant for it and the
    /// wrong one is caught. On the shipped files the difference is not subtle:
    /// 1.8.4's v17 `world.ecwld` misreads 65 of its 22,528 blocks without the
    /// field, and a v12 map misreads 34 of its blocks with it.
    fn world_with_a_model(version: u32, with_scale: bool) -> Vec<u8> {
        let mut block = Vec::new();
        block.extend_from_slice(&0u16.to_le_bytes()); // row
        block.extend_from_slice(&0u16.to_le_bytes()); // col
        let counts = [0u32, 0, 0, 0, 0, 0, 1, 0, 0, 0]; // one EC model
        for c in counts {
            block.extend_from_slice(&c.to_le_bytes());
        }
        block.extend_from_slice(&7u32.to_le_bytes()); // export id
        for v in [1.5f32, 2.5, 3.5, 0., 1., 0., 0., 0., 1.] {
            block.extend_from_slice(&v.to_le_bytes());
        }
        if with_scale {
            block.extend_from_slice(&1.75f32.to_le_bytes());
        }
        let path = b"Models\\tree.ecm";
        block.extend_from_slice(&(path.len() as u32).to_le_bytes());
        block.extend_from_slice(path);
        block.extend_from_slice(&0u32.to_le_bytes()); // empty action
        let nblocks = 1usize;
        let block_off = 116 + 4 * nblocks;
        let tree_off = block_off + block.len();
        let mut b = Vec::new();
        b.extend_from_slice(&0x45435744u32.to_le_bytes());
        b.extend_from_slice(&version.to_le_bytes());
        for v in [64.0f32, 64.0, 64.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in [32u32, nblocks as u32, 1, 1] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        // Tree and grass tables: both empty, so both offsets point at the end.
        b.extend_from_slice(&(tree_off as u32).to_le_bytes());
        b.extend_from_slice(&(tree_off as u32).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.resize(116, 0);
        b.extend_from_slice(&(block_off as u32).to_le_bytes());
        b.extend_from_slice(&block);
        b
    }
    #[test]
    fn a_model_scale_is_read_from_the_version_that_has_one() {
        for (version, with_scale) in [(12u32, false), (17u32, true)] {
            let scene = Scene::parse(&world_with_a_model(version, with_scale))
                .unwrap_or_else(|e| panic!("version {version}: {e}"));
            let model = &scene.blocks[0].models[0];
            assert_eq!(model.path, "Models/tree.ecm");
            assert_eq!(model.action, "");
            assert_eq!(model.export_id, 7);
            assert_eq!(
                model.scale,
                if with_scale { 1.75 } else { 1.0 },
                "version {version}"
            );
        }
        // The mismatches are refused rather than silently accepted, which is
        // what "the block does not add up" has to mean.
        assert!(Scene::parse(&world_with_a_model(17, false)).is_err());
        assert!(Scene::parse(&world_with_a_model(12, true)).is_err());
    }
    /// Every staged `.ecwld` parses, whatever version wrote it.
    ///
    /// A block that does not add up is refused by the per-block length check,
    /// so a file that parses has had every record of every block walked at the
    /// right stride. That is exactly the check the model-scale field needs: one
    /// wrong stride turns a scale into a string length and the block stops
    /// adding up. The staged corpus is the real thing — the reported failure
    /// came from a version-17 file, which the model-scale fix has to read.
    #[test]
    fn every_staged_world_file_parses() {
        let dir = std::path::Path::new("google_drive/scenecorpus");
        let mut files: Vec<_> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ecwld"))
                .collect(),
            Err(_) => Vec::new(),
        };
        if files.is_empty() {
            crate::pck::corpus_absent("a staged `google_drive/scenecorpus`");
            return;
        }
        files.sort();
        let mut versions = std::collections::BTreeSet::new();
        let mut models = 0usize;
        for path in &files {
            let scene = Scene::load(path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.file_name().unwrap().to_string_lossy()));
            let version = u32at(&std::fs::read(path).unwrap(), 4).unwrap();
            versions.insert(version);
            assert!(!scene.blocks.is_empty());
            for block in &scene.blocks {
                for model in &block.models {
                    models += 1;
                    // The scale is a real authored number, which a misread
                    // string length would not be. The first shipped value is
                    // exactly 1.0, so the bound has to be inclusive.
                    assert!(
                        model.scale.is_finite() && model.scale >= 0.05 && model.scale <= 100.,
                        "{}: model scale {}",
                        path.display(),
                        model.scale
                    );
                }
            }
        }
        assert!(
            versions.len() >= 2,
            "the corpus is only one version; it proves nothing about the boundary: {versions:?}"
        );
        assert!(
            models > 0,
            "no models were staged to check the stride against"
        );
    }
    #[test]
    fn rejects_bad_header() {
        assert!(Scene::parse(&[0; 116]).is_err());
    }
    #[test]
    fn the_version_gate_says_which_version_it_saw() {
        // 1.8.4 was refused with the bare sentence "only ECWLD version 12 is
        // supported", which does not say what the file held or what the build
        // wanted. The gate now names both, and anything at or above 12 is
        // attempted rather than refused by number.
        // `Result::unwrap_err` would need `Scene: Debug`, which this module does
        // not implement and should not gain just for a test.
        fn error(b: &[u8]) -> String {
            match Scene::parse(b) {
                Err(error) => error,
                Ok(_) => panic!("this header should not parse"),
            }
        }
        let mut b = vec![0u8; 116];
        b[0..4].copy_from_slice(&0x45435744u32.to_le_bytes());
        b[4..8].copy_from_slice(&13u32.to_le_bytes());
        let newer = error(&b);
        assert!(!newer.contains("only ECWLD"), "{newer}");
        assert!(!newer.contains("version 13"), "13 is attempted: {newer}");

        b[4..8].copy_from_slice(&7u32.to_le_bytes());
        let older = error(&b);
        assert!(older.contains("version 7"), "{older}");
        assert!(older.contains("version 12"), "{older}");

        b[0..4].copy_from_slice(&0xdeadbeefu32.to_le_bytes());
        let not_a_world = error(&b);
        assert!(not_a_world.contains("not a world file"), "{not_a_world}");
    }
    #[test]
    fn rejects_short_string() {
        let mut p = 0;
        assert!(string(&[9, 0, 0, 0, 1], &mut p).is_err());
    }
    #[test]
    fn decodes_chinese_path() {
        let mut p = 0;
        let path = string(&[4, 0, 0, 0, 0xd1, 0xa9, 0xb5, 0xd8], &mut p).unwrap();
        assert_eq!(path, "雪地");
    }
}
