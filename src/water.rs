//! Client EC-BSD water references and A3DWaterArea v0x10002000/0x10003000 grids.
//! Record layout derived from the supplied elementclient.exe at 0x780d30 and 0x781760.
use crate::scene::Scene;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// Instance maps name their EC-BSD after the map folder (`a01.ecbsd`), while
/// the main world and stand-alone fixtures keep `world.ecbsd`. This local
/// resolver keeps standalone diagnostic binaries independent of the viewer's
/// world catalog module.
fn ecbsd_path(dir: &Path) -> PathBuf {
    let stem = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("world");
    let named = dir.join(format!("{stem}.ecbsd"));
    if named.is_file() {
        named
    } else {
        dir.join("world.ecbsd")
    }
}

/// The EC-BSD manifest a map folder holds, for callers outside this module.
///
/// Diagnostic binaries need the same `<map id>.ecbsd`-then-`world.ecbsd`
/// resolution the loader uses; keeping one resolver means a survey cannot
/// disagree with the viewer about which file a map carries.
pub fn ecbsd_path_for_survey(dir: &Path) -> PathBuf {
    ecbsd_path(dir)
}

/// Authored shoreline control points and per-vertex day/night ARGB colors.
/// Version 0x10003000 serialization is read by client routine 0x782680.
#[derive(Debug, Clone)]
pub struct ShoreWave {
    pub capacity: usize,
    pub width: f32,
    pub points: Vec<[f32; 3]>,
    pub day_colors: Vec<u32>,
    pub night_colors: Vec<u32>,
}
#[derive(Debug, Clone)]
pub struct WaterArea {
    pub export_id: u32,
    pub color: u32,
    /// Selects the client's alternate simple-water area pass (record +44).
    pub simple: bool,
    pub height: f32,
    pub grid_size: f32,
    pub columns: usize,
    pub rows: usize,
    pub center: [f32; 3],
    pub mask: Vec<u8>,
    pub shore_waves: Vec<ShoreWave>,
}
pub(crate) fn u(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p.checked_add(4).ok_or("overflow")?)
            .ok_or("truncated water data")?
            .try_into()
            .unwrap(),
    ))
}
pub(crate) fn f(b: &[u8], p: usize) -> Result<f32, String> {
    let v = f32::from_bits(u(b, p)?);
    if !v.is_finite() {
        return Err("non-finite water value".into());
    }
    Ok(v)
}
pub(crate) fn path_string(b: &[u8], p: &mut usize) -> Result<String, String> {
    let len = u(b, *p)? as usize;
    *p += 4;
    if len > 1024 {
        return Err("invalid BSData name".into());
    }
    let s = b
        .get(*p..p.checked_add(len).ok_or("overflow")?)
        .ok_or("truncated BSData name")?;
    *p += len;
    let (s, _, bad) = encoding_rs::GBK.decode(s);
    if bad {
        return Err("bad BSData name".into());
    }
    let mut path = s.into_owned();
    if path.as_bytes().contains(&b'\\') {
        unsafe {
            for b in path.as_bytes_mut() {
                if *b == b'\\' {
                    *b = b'/';
                }
            }
        }
    }
    if path.contains("..") || path.starts_with('/') || path.contains(':') {
        return Err("unsafe BSData path".into());
    }
    Ok(path)
}
pub(crate) fn resolve_case_insensitive(
    root: &Path,
    path: &str,
) -> Result<std::path::PathBuf, String> {
    let mut result = root.to_path_buf();
    for component in path.split('/') {
        let mut found = None;
        for e in fs::read_dir(&result).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            if e.file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(component)
            {
                found = Some(e.path());
                break;
            }
        }
        result = found.ok_or_else(|| format!("missing BSData path {path}"))?;
    }
    Ok(result)
}
impl WaterArea {
    /// Whether this area has a water surface at `(x, z)`.
    ///
    /// The authored mask is *not* the surface footprint. The client's
    /// `A3DWaterArea::IsInWater` (`0x781c50`) threshold-tests a bilinearly
    /// interpolated height, so a zero mask byte marks a shallow (or dry) cell
    /// inside the area's bounding box, not a hole in the plane: the area still
    /// draws across its whole `columns x rows` grid. Deriving the drawn surface
    /// from `contains_xz` is what left ~21% of a 16x21 open-water area missing
    /// (`export=2538 … wet=265 of 336`) in both the 1.3.6 and 1.5.2 runs.
    ///
    /// `contains_xz` keeps its job — "is this point inside the area's grid" —
    /// and this adds the mask test on top for callers that need wet-vs-dry.
    pub fn has_surface_xz(&self, x: f32, z: f32) -> bool {
        if !self.is_over_xz(x, z) {
            return false;
        }
        let col = ((x - (self.center[0] - self.columns as f32 * self.grid_size * 0.5))
            / self.grid_size)
            .floor() as isize;
        let row = (((self.center[2] + self.rows as f32 * self.grid_size * 0.5) - z)
            / self.grid_size)
            .floor() as isize;
        if col < 0 || row < 0 || col >= self.columns as isize || row >= self.rows as isize {
            return false;
        }
        self.mask
            .get(row as usize * self.columns + col as usize)
            .is_none_or(|m| *m != 0)
    }

    /// Whether `(x, z)` falls inside this area's grid, mask ignored.
    pub fn is_over_xz(&self, x: f32, z: f32) -> bool {
        let half_x = self.columns as f32 * self.grid_size * 0.5;
        let half_z = self.rows as f32 * self.grid_size * 0.5;
        x.is_finite()
            && z.is_finite()
            && (x - self.center[0]).abs() < half_x
            && (z - self.center[2]).abs() < half_z
    }

    pub fn contains_xz(&self, x: f32, z: f32) -> bool {
        // 0x7807c6–0x780813 rejects the bounding box boundary before
        // 0x781c50 maps to and clamps the mask cell.
        let half_x = self.columns as f32 * self.grid_size * 0.5;
        let half_z = self.rows as f32 * self.grid_size * 0.5;
        if !x.is_finite()
            || !z.is_finite()
            || (x - self.center[0]).abs() >= half_x
            || (z - self.center[2]).abs() >= half_z
        {
            return false;
        }
        let col = ((x - (self.center[0] - self.columns as f32 * self.grid_size * 0.5))
            / self.grid_size)
            .floor() as isize;
        let row = (((self.center[2] + self.rows as f32 * self.grid_size * 0.5) - z)
            / self.grid_size)
            .floor() as isize;
        if col < 0 || row < 0 || col >= self.columns as isize || row >= self.rows as isize {
            return false;
        }
        self.mask
            .get(row as usize * self.columns + col as usize)
            .is_some_and(|m| *m != 0)
    }
    pub fn parse(b: &[u8], p: usize, export_id: u32) -> Result<Self, String> {
        let version = u(b, p)?;
        if !is_water_version(version) {
            return Err(format!("unsupported A3DWaterArea version {version:#x}"));
        }
        // The version word sits eight bytes before the area's own fields.
        Self::parse_fields(b, p + 8, export_id, version).map(|(area, _)| area)
    }

    /// Decode one area from the offset of its first field (`color`), returning
    /// the area plus the offset of the next field in the run.
    ///
    /// A run of areas shares one `version` word, so only the first area's
    /// fields follow it; the rest follow the previous area's shoreline arrays
    /// directly. The word after the version is not the number of areas (see
    /// `decode_areas`). `fb` is the offset of the first field, not of the
    /// version word, and where each field sits is [`WaterFields`]' business.
    fn parse_fields(
        b: &[u8],
        fb: usize,
        export_id: u32,
        version: u32,
    ) -> Result<(Self, usize), String> {
        // Only a revision whose field offsets were recovered can be walked; the
        // caller reports anything else by version instead of guessing at the
        // mask offset and silently reading a shifted grid.
        let fields = WaterFields::of(version).ok_or_else(|| {
            format!(
                "A3DWaterArea version {version:#x} has no recovered layout \
                 (this build reads 0x10002000, 0x10003000 and 4)"
            )
        })?;
        let color = u(b, fb + fields.color)?;
        let height = f(b, fb + fields.height)?;
        let columns = u(b, fb + fields.columns)? as usize;
        let rows = u(b, fb + fields.rows)? as usize;
        let grid_size = f(b, fb + fields.cell_size)?;
        let center = [
            f(b, fb + fields.center)?,
            f(b, fb + fields.center + 4)?,
            f(b, fb + fields.center + 8)?,
        ];
        // Bounds measured across every water patch staged for 1.5.2, 1.4.4 and
        // 1.8.4 (a07, a37 and the 1.8.4 world): at most 50 columns, 32 rows and
        // a cell size of 32 metres, with the 1.8.4 v4 layout reaching 60x64 and
        // 128x128 on instance maps. The limits below leave headroom over each
        // and exist only to stop a damaged record reserving memory; a record
        // rejected here costs its own area, never the map.
        const MAX_WATER_COLUMNS: usize = 4096;
        const MAX_WATER_ROWS: usize = 4096;
        const MAX_WATER_CELL_SIZE: f32 = 4096.;
        if columns == 0
            || rows == 0
            || columns > MAX_WATER_COLUMNS
            || rows > MAX_WATER_ROWS
            || grid_size <= 0.
            || grid_size > MAX_WATER_CELL_SIZE
        {
            return Err(format!(
                "invalid water grid: {columns}x{rows} cells of {grid_size} m \
                 (limits {MAX_WATER_COLUMNS} x {MAX_WATER_ROWS} cells, \
                 {MAX_WATER_CELL_SIZE} m)"
            ));
        }
        // The older v2 area has no simple-water flag byte; v3 inserts one
        // before the mask, which is the only difference between those two
        // revisions. The 1.8.4 v4 layout has no flag at all.
        let simple = match fields.simple {
            Some(at) => *b.get(fb + at).ok_or("truncated water flag")? != 0,
            None => false,
        };
        let n = columns.checked_mul(rows).ok_or("grid overflow")?;
        let start = fb.checked_add(fields.mask).ok_or("overflow")?;
        let mask = b
            .get(start..start.checked_add(n).ok_or("overflow")?)
            .ok_or("truncated water cell mask")?
            .to_vec();
        let wave_start = start.checked_add(n).ok_or("wave offset overflow")?;
        // The walk over `count` areas continues past the shoreline arrays, so
        // the reader reports where it stopped even when it gave up early. A
        // wave that cannot be read costs that wave only: the grid above is the
        // water surface, and losing it because one shoreline record named a
        // newer version is the "water is lost" failure this reader exists to
        // avoid.
        let (shore_waves, next) = match parse_shore_waves(b, wave_start) {
            Ok((waves, next)) => (waves, next),
            Err(error) => {
                crate::diag!(
                    SceneWater,
                    "water {export_id}: shoreline data unavailable: {error}"
                );
                let kept = parse_shore_waves_lenient(b, wave_start);
                crate::diag!(
                    SceneWater,
                    "water {export_id}: kept {} of the shoreline waves it could read",
                    kept.len()
                );
                (kept, b.len())
            }
        };
        Ok((
            Self {
                export_id,
                color,
                simple,
                height,
                grid_size,
                columns,
                rows,
                center,
                mask,
                shore_waves,
            },
            next,
        ))
    }
}

/// Water version words whose record layout this reader can decode.
///
/// A `0x100SSSSS` word carries its own layout revision in the low half, and the
/// client's `A3DWaterArea` loader (`0x780d30`) changes what it reads per
/// revision, so accepting a word it has never been shown would be a guess.
/// `water-record-survey` has found only these two in shipped data: 1.3.1,
/// 1.3.6, 1.4.2, 1.4.4, 1.5.2 and 1.8.4 all write `0x10003000`, and the older
/// `0x10002000` layout is the one the client still reads. Anything else is
/// named rather than dropped silently, and `parse_record` falls back to a
/// version-agnostic scan so a record carrying an unknown revision still has a
/// chance to yield its areas.
const WATER_VERSIONS: [u32; 2] = [0x10002000, 0x10003000];

/// The `dwVersion` a **1.8.4** BSData record stores.
///
/// It is not a `0x100SSSSS` word at all. In the shipped 1.8.4 client every water
/// record -- `BSData/7.dat` record 0 (a 32x32 area whose shoreline waves end at
/// `shaders\\textures\\water\\wave1.dds`), `4.dat` records 1 and 8, `44.dat`
/// record 9 -- opens with `u32 4`, and the shoreline waves embedded in the same
/// record still open with a `0x10003000` word. So the 431/`0x1af` incident was
/// not the only non-conforming version in this data: the area header's own word
/// is a plain revision number in this client.
///
/// Layout, measured against `7.dat` record 0 field by field, with every offset
/// taken from the record's `dwVersion`:
///
/// | offset | field | measured value |
/// |---|---|---|
/// | +0 | `dwVersion` | `4` |
/// | +4 | `dwColor` | `4` |
/// | +8 | `fHeight` | `3.0` |
/// | +16 | `dwColumns` | `32` |
/// | +20 | `dwRows` | `32` |
/// | +24 | `fCellSize` | `32.0` |
/// | +28 | `vCenter` | `2560, 216, 5120` |
/// | +124 | `byCellMask[columns*rows]` | 1024 bytes, 533 set |
/// | +1148 | `dwWaveCount` | `46` |
///
/// and the 46 waves follow immediately, ending exactly at the end of the
/// record. The 84 bytes between the centre vector and the mask are the fields
/// the v2/v3 layouts do not have, which is why this revision needs its own
/// offsets instead of being read as v3 with everything shifted.
const WATER_VERSION_V4: u32 = 4;

/// Whether `word` can open a water area.
///
/// Two spellings, because two clients write different things: the `0x100SSSSS`
/// word an `A3DWaterArea` prefix carries in 1.3.1 through 1.5.2, and the plain
/// revision `4` the 1.8.4 BSData records store.
fn is_water_version(word: u32) -> bool {
    WATER_VERSIONS.contains(&word) || word == WATER_VERSION_V4
}

/// Where a revision's fields sit, relative to the record's `dwVersion`.
///
/// Keeping them in one table is the point: a revision is a set of offsets, not a
/// special case spread through the walker, and the next one is a row here.
struct WaterFields {
    color: usize,
    height: usize,
    columns: usize,
    rows: usize,
    cell_size: usize,
    center: usize,
    simple: Option<usize>,
    mask: usize,
}

impl WaterFields {
    fn of(version: u32) -> Option<Self> {
        match version {
            0x10002000 => Some(Self {
                color: 0,
                height: 4,
                columns: 12,
                rows: 16,
                cell_size: 20,
                center: 24,
                simple: None,
                mask: 36,
            }),
            0x10003000 => Some(Self {
                color: 0,
                height: 4,
                columns: 12,
                rows: 16,
                cell_size: 20,
                center: 24,
                simple: Some(36),
                mask: 37,
            }),
            // 1.8.4 keeps the v2/v3 field order and inserts its extra fields
            // between the centre vector and the mask, so only the mask moves:
            // its own header is four bytes long instead of eight, but the
            // field block that follows it starts at the same place relative to
            // itself, and the mask sits 84 bytes past where v2 puts it.
            //
            // Measured on `7.dat` record 0, the basis for this row: with the
            // block starting at the version word + 4, `dwColor` is +0,
            // `fHeight` +4 (216.0, the same value the centre carries as its
            // y), `dwColumns`/`dwRows` +12/+16 (32/32), `fCellSize` +20 (32.0),
            // `vCenter` +24 (2560, 216, 5120), the cell mask +120 (533 wet of
            // 1024) and `dwWaveCount` 46 at +1144 with the waves running to the
            // end of the record.
            WATER_VERSION_V4 => Some(Self {
                color: 0,
                height: 4,
                columns: 12,
                rows: 16,
                cell_size: 20,
                center: 24,
                simple: None,
                mask: 120,
            }),
            _ => None,
        }
    }
}

/// Decode every `A3DWaterArea` stored in one BSData record.
///
/// A record is either the raw `version, count, areas…` block (the natural
/// payload for the small patches), or the zlib framing that the Angelica
/// binary container uses for records that are stored compressed:
/// `stored_len, raw_len, zlib(raw)` where `raw_len` is the window size — 32768
/// for the world and instance patches. Reading the zlib header as a version is
/// exactly the `unsupported A3DWaterArea version 0x1af` (431 = a stored length)
/// failure, so the framing has to be resolved before the version is trusted.
pub(crate) fn parse_record(record: &[u8], export_id: u32) -> Result<Vec<WaterArea>, String> {
    let first = u(record, 0)?;
    if is_water_version(first) {
        return decode_areas(record, export_id);
    }
    // A word in the water version family that this reader has no layout for.
    // Scanning the record for a revision it *does* know beats refusing the
    // whole record on its first four bytes: a record that opens with a newer
    // revision may still carry an older one further in, and the scan costs a
    // walk over at most one record.
    if (first & 0xffff_0000) == 0x1000_0000 {
        let mut from = 4usize;
        while let Some(offset) = find_version_word(record, from) {
            if let Ok(areas) = decode_areas(&record[offset..], export_id) {
                return Ok(areas);
            }
            from = offset + 4;
        }
        return Err(format!(
            "unsupported A3DWaterArea version {first:#x} (no recovered layout; \
             this build reads 0x10002000, 0x10003000 and 4)"
        ));
    }
    let stored = first as usize;
    let raw = u(record, 4)? as usize;
    // The rest of the record is the compressed payload. `stored` is the length
    // the writer declared and `raw` the size of the window it inflates to; a
    // wild word is a framing this reader does not know, not a size worth
    // trusting. Measured across the 160 framed water records in the 1.8.4
    // patches: every one declares a `stored` that fits inside its record, and
    // every one inflates to exactly `raw` with a real end marker. So neither
    // bound has to be widened -- refusing here costs only the records whose
    // framing really is unknown.
    let available = record.len().saturating_sub(8);
    if raw == 0 || raw > 1024 * 1024 || stored == 0 || stored > available {
        return Err(format!(
            "not a water record: the first word is {first:#x}, which is neither a \
             known A3DWaterArea version nor a {available}-byte zlib frame"
        ));
    }
    let payload = &record[8..8 + stored];
    let plain = crate::pck::inflate(payload, raw)
        .map_err(|error| format!("not a water record: the zlib frame does not inflate: {error}"))?;
    // Records are shared with the other BSData tables, so a water area can sit
    // anywhere in the window; find the first run that actually decodes.
    let mut from = 0usize;
    while let Some(offset) = find_version_word(&plain, from) {
        if let Ok(areas) = decode_areas(&plain[offset..], export_id) {
            return Ok(areas);
        }
        from = offset + 4;
    }
    // The frame inflated, so this is a readable record — it simply carries no
    // water area. That is a fact about the patch, not a fault in a version, and
    // saying "unsupported version 0x1af" here (431 was the *stored length*)
    // sent a whole investigation after a version that never existed.
    Err(format!(
        "no A3DWaterArea record in this patch: it inflates to {} bytes with no water \
         version word",
        plain.len()
    ))
}

/// First offset at or after `from` holding a water version word on a 4-byte
/// boundary.
fn find_version_word(b: &[u8], from: usize) -> Option<usize> {
    let mut p = (from + 3) & !3;
    while p + 8 <= b.len() {
        let word = u32::from_le_bytes(b[p..p + 4].try_into().ok()?);
        if is_water_version(word) {
            return Some(p);
        }
        p += 4;
    }
    None
}

/// Walk the `version, count, area…` list a record payload holds.
///
/// The version word opens a run of areas, and the word after it is **not** how
/// many areas follow. Every water record in the retail 1.3.1 world that decodes
/// at all holds exactly one area and declares 3, 8, 6 or 4: `21.dat` record 0
/// declares 3, `4.dat` record 1 declares 3 and `36.dat` record 9 declares 6,
/// and in all three cases the area's shoreline arrays end exactly on the last
/// byte of the record. The declared number counts something else (the client's
/// `A3DWaterArea` list capacity, not its length), so the walk is bounded by the
/// payload and a second area only exists if one really parses there.
///
/// This reader used to trust that word. A record declaring 3 held one area, the
/// second area failed on the bytes that follow, and the failure threw away the
/// area that *had* decoded — so nearly every record contributed no water at all
/// and only the handful whose declared count happened to be 1 survived. That is
/// what "water is broken in chunks" is: the surface is missing wherever a
/// record's count lied, not chopped by geometry.
fn decode_areas(b: &[u8], export_id: u32) -> Result<Vec<WaterArea>, String> {
    let mut areas = Vec::new();
    let mut p = 0usize;
    while p + 8 <= b.len() {
        let version = u(b, p)?;
        if !is_water_version(version) {
            break;
        }
        // The version word opens the run, so the first area's fields follow it
        // and every later area's fields follow the previous area's shoreline
        // arrays. Stop when the payload runs out or a word that cannot open an
        // area arrives; the declared count is not a bound.
        //
        // How far the fields sit behind the word depends on the revision: a
        // `0x100SSSSS` prefix reserves eight bytes before `dwColor`, while the
        // 1.8.4 v4 word sits immediately in front of it. Reading v4 through the
        // prefix offset shifted every field by four bytes, which turned a valid
        // 32x32 grid into nonsense and produced no water on any map.
        let mut cursor = match version {
            WATER_VERSION_V4 => p + 4,
            _ => p + 8,
        };
        let mut parsed = 0usize;
        loop {
            match WaterArea::parse_fields(b, cursor, export_id, version) {
                Ok((area, end)) if end > cursor => {
                    cursor = end;
                    areas.push(area);
                    parsed += 1;
                }
                _ => break,
            }
        }
        // Nothing opened here, so this was not a run after all: resume one word
        // later instead of stopping, so a real run further in is still found.
        p = if parsed > 0 { cursor } else { p + 4 };
        // A parsed run is followed by padding or the next run's version word;
        // either way the outer walk decides.
    }
    if areas.is_empty() {
        // Name the word that was actually there rather than the first four
        // bytes: a zlib-framed record's first dword is a stored length, and
        // "unsupported A3DWaterArea version 0x1af" mislabelled a framing
        // problem as a version problem for a long time. A word in the water
        // version family is quoted as a version; anything else is reported as
        // not being a water record at all.
        let first = u(b, 0).unwrap_or(0);
        return Err(if (first & 0xffff_0000) == 0x1000_0000 {
            format!(
                "unsupported A3DWaterArea version {first:#x} (no recovered layout; \
                 this build reads 0x10002000, 0x10003000 and 4)"
            )
        } else {
            format!("no A3DWaterArea record found; the first word is {first:#x}")
        });
    }
    Ok(areas)
}

/// Shoreline wave version words this reader has a recovered layout for.
///
/// The word's low half is the layout revision: v1 stores positions only and
/// lights both arrays to `0x80808080`, v2 stores the day ARGB array and derives
/// night as 0.3x it, v3 stores both arrays. All three are read from the client
/// at `0x782680`. A later revision is reported by version, and the wave — not
/// the whole map — is skipped.
const SHORE_WAVE_VERSIONS: [u32; 3] = [0x10001000, 0x10002000, 0x10003000];

/// Vertex ceiling for one wave: three vertices per control point, and the
/// viewer uploads u32 indices (`shore_gpu.rs`), so the client's u16 limit of
/// 21,845 points is not this reader's limit. This bound only stops a damaged
/// point count from reserving unbounded memory.
const SHORE_WAVE_MAX_POINTS: usize = 1 << 20;

/// Read every shoreline wave that can be read, stopping at the first that
/// cannot.
///
/// [`parse_shore_waves`] stops at the first bad wave because it has to report
/// where the array ended; this variant exists for the case where it did, so the
/// good waves before the bad one are still drawn. The two share one loop so a
/// change to the layout cannot make them disagree.
fn parse_shore_waves_lenient(b: &[u8], offset: usize) -> Vec<ShoreWave> {
    let mut cursor = offset;
    let Ok(count) = u(b, cursor) else {
        return Vec::new();
    };
    cursor += 4;
    let mut out = Vec::new();
    for _ in 0..count.min(65536) {
        match parse_one_shore_wave(b, cursor) {
            Ok((wave, next)) => {
                out.push(wave);
                cursor = next;
            }
            Err(_) => break,
        }
    }
    out
}

fn parse_shore_waves(b: &[u8], offset: usize) -> Result<(Vec<ShoreWave>, usize), String> {
    let mut cursor = offset;
    let count = u(b, cursor)? as usize;
    cursor += 4;
    if count > 65536 {
        return Err("invalid shoreline wave count".into());
    }
    let mut shore_waves = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let (wave, next) = parse_one_shore_wave(b, cursor)?;
        cursor = next;
        shore_waves.push(wave);
    }
    Ok((shore_waves, cursor))
}

/// Decode one shoreline wave at `cursor`, returning it and the offset of the
/// next wave.
fn parse_one_shore_wave(b: &[u8], mut cursor: usize) -> Result<(ShoreWave, usize), String> {
    let version = u(b, cursor)?;
    if !SHORE_WAVE_VERSIONS.contains(&version) {
        return Err(format!(
            "unsupported shoreline wave version {version:#x} (this build reads \
             0x10001000, 0x10002000 and 0x10003000)"
        ));
    }
    let capacity = u(b, cursor + 4)? as usize;
    let width = f(b, cursor + 8)?;
    let points_count = u(b, cursor + 12)? as usize;
    cursor = cursor.checked_add(16).ok_or("wave offset overflow")?;
    // `capacity` is the client's authored buffer size and is not a bound
    // this reader may enforce beyond sanity: the arrays are read by point
    // count, and `points_count > capacity` only means the author grew the
    // wave after the capacity was written.
    if points_count > SHORE_WAVE_MAX_POINTS || width < 0.0 {
        return Err(format!(
            "invalid shoreline wave dimensions: {} points (limit {SHORE_WAVE_MAX_POINTS}) \
             at capacity {capacity}",
            points_count
        ));
    }
    let bytes = points_count.checked_mul(12).ok_or("wave size overflow")?;
    let arrays = match version {
        0x10001000 => 1,
        0x10002000 => 2,
        _ => 3,
    };
    let end = cursor
        .checked_add(bytes.checked_mul(arrays).ok_or("wave size overflow")?)
        .ok_or("wave offset overflow")?;
    if end > b.len() {
        return Err("truncated shoreline wave".into());
    }
    let mut points = Vec::with_capacity(points_count);
    for i in 0..points_count {
        let pos = cursor + i * 12;
        points.push([f(b, pos)?, f(b, pos + 4)?, f(b, pos + 8)?]);
    }
    cursor += bytes;
    let mut day_colors = vec![0x80808080; points_count * 3];
    let mut night_colors = vec![0x80808080; points_count * 3];
    if version >= 0x10002000 {
        for (i, color) in day_colors.iter_mut().enumerate() {
            *color = u(b, cursor + i * 4)?;
        }
        cursor += bytes;
        if version == 0x10003000 {
            for (i, color) in night_colors.iter_mut().enumerate() {
                *color = u(b, cursor + i * 4)?;
            }
            cursor += bytes;
        } else {
            // 0x782863-0x7828b2 converts all ARGB components to floats,
            // scales by 0.3, and packs them back for the night array.
            for (dst, src) in night_colors.iter_mut().zip(&day_colors) {
                let mut out = 0u32;
                for shift in [0, 8, 16, 24] {
                    let v = ((src >> shift) & 255) as f32;
                    out |= ((v * 0.3) as u32) << shift;
                }
                *dst = out;
            }
        }
    }
    Ok((
        ShoreWave {
            capacity,
            width,
            points,
            day_colors,
            night_colors,
        },
        cursor,
    ))
}
pub fn load(scene: &Scene, dir: &Path) -> Result<Vec<WaterArea>, String> {
    // Name the file. `e.to_string()` alone is "No such file or directory
    // (os error 2)", which reached the compatibility matrix as a refusal with no
    // path in it -- a reader cannot tell whether the map ships no water manifest
    // or whether the viewer looked in the wrong place. The loader's own contract
    // is that a refusal names what it refused.
    let manifest = ecbsd_path(dir);
    let b = fs::read(&manifest).map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    if let Err(why) = crate::scene::ecbsd_header(&b) {
        // Same reasoning: say which file carried the bad header.
        crate::load_status::dropped(
            crate::load_status::Category::Water,
            format!("{}: {why}", manifest.display()),
        );
        return Err(format!("{}: {why}", manifest.display()));
    }
    // One check owns the EC-BSD identity and revision, so water, scenery and
    // grass cannot disagree about which revisions a map may carry. It ran above,
    // so that a refusal can name the file it refused.
    // Measured: the largest manifest staged is the 1.8.4 world's, with 88
    // BSData paths. The limit leaves two orders of magnitude of headroom and
    // only stops a damaged header reserving memory.
    const MAX_BSDATA_FILES: usize = 10_000;
    let count = u(&b, 12)? as usize;
    if count > MAX_BSDATA_FILES {
        return Err(format!(
            "invalid BSData file count: {count} paths (limit {MAX_BSDATA_FILES})"
        ));
    }
    // The name block starts at 0x4c with the *length* of the first name, not
    // with the name: every entry is `u32 length` then `length` bytes of GBK
    // path, and 0x4c is where the first of those dwords sits. The paths count
    // comes from the header at +12, so the block ends after exactly that many
    // strings and the water records begin there.
    let mut p = 0x4c;
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        files.push(path_string(&b, &mut p)?)
    }
    let mut loaded_index = None;
    let mut loaded = Vec::new();
    let est: usize = scene.blocks.iter().map(|bl| bl.water.len()).sum();
    let mut records = Vec::with_capacity(est);
    let mut seen = HashMap::<u32, u32>::with_capacity(est);
    let mut areas = Vec::with_capacity(est);
    for block in &scene.blocks {
        for r in &block.water {
            if let Some(previous) = seen.insert(r.export_id, r.offset) {
                if previous != r.offset {
                    return Err(format!("water {} has inconsistent offsets", r.export_id));
                }
                continue;
            }
            records.push((u(&b, r.offset as usize)? as usize, records.len(), r));
        }
    }
    records.sort_unstable_by_key(|r| r.0);
    for (index, order, r) in records {
        let off = r.offset as usize;
        let (Ok(record), Some(path)) = (u(&b, off + 4), files.get(index)) else {
            continue;
        };
        let record = record as usize;
        if loaded_index != Some(index) {
            // A patch this map references but the client does not ship costs
            // its own references, not the world's whole water supply.
            let resolved = match resolve_case_insensitive(dir, path) {
                Ok(path) => path,
                Err(error) => {
                    crate::diag!(
                        SceneWater,
                        "water {}: BSData {index} unavailable: {error}",
                        r.export_id
                    );
                    loaded_index = Some(index);
                    loaded.clear();
                    continue;
                }
            };
            drop(std::mem::take(&mut loaded));
            match fs::read(resolved) {
                Ok(bytes) => loaded = bytes,
                Err(error) => {
                    crate::diag!(
                        SceneWater,
                        "water {}: BSData {index} unavailable: {error}",
                        r.export_id
                    );
                    loaded_index = Some(index);
                    continue;
                }
            }
            loaded_index = Some(index);
        }
        let data = &loaded;
        let (Ok(magic), Ok(version)) = (u(data, 0), u(data, 4)) else {
            continue;
        };
        if magic != 0x42584f4d || version != 2 {
            crate::diag!(
                SceneWater,
                "water {}: BSData {index} has an unexpected header",
                r.export_id
            );
            continue;
        }
        let Some(tail) = data.len().checked_sub(4) else {
            continue;
        };
        let Ok(entries) = u(data, tail) else {
            crate::diag!(
                SceneWater,
                "water {}: BSData {index} too short to hold its entry count",
                r.export_id
            );
            continue;
        };
        let entries = entries as usize;
        let Some(table) = entries
            .checked_mul(4)
            .and_then(|bytes| tail.checked_sub(bytes))
        else {
            // The entry table cannot fit inside the file, so this patch is
            // unreadable rather than empty. Say so: an unqualified `continue`
            // here loses a whole water surface and prints nothing.
            crate::diag!(
                SceneWater,
                "water {}: BSData {index} entry table of {entries} entries does not fit \
                 in {} bytes",
                r.export_id,
                data.len()
            );
            continue;
        };
        // Every bound below costs this reference only. `load` returning an
        // error means "this map has no water" to the caller, so one stale index
        // must never reach it: 1.3.1's world references 111 records across 72
        // patches, and a single out-of-range index took the whole surface away.
        if record >= entries {
            crate::diag!(
                SceneWater,
                "water {}: BSData {index} record {record} past {entries} entries",
                r.export_id
            );
            continue;
        }
        let Ok(rec) = u(data, table + record * 4) else {
            crate::diag!(
                SceneWater,
                "water {}: BSData {index} has no offset for record {record}",
                r.export_id
            );
            continue;
        };
        let rec = rec as usize;
        let mut end = table;
        for i in 0..entries {
            let Ok(other) = u(data, table + i * 4) else {
                continue;
            };
            let other = other as usize;
            if other > rec && other < end {
                end = other;
            }
        }
        if rec >= end {
            crate::diag!(
                SceneWater,
                "water {}: BSData {index} record {record} has no boundary",
                r.export_id
            );
            continue;
        }
        // One unreadable record costs its own areas, not the whole world: the
        // caller treats a `load` error as "this map has no water", which drops
        // every other area on the map with it.
        match parse_record(&data[rec..end], r.export_id) {
            Ok(batch) => areas.extend(batch.into_iter().map(|area| (order, area))),
            // Name the patch as well as its index: a report that says
            // "BSData 7 record 31" cannot be acted on without the manifest,
            // and the manifest entry is what the author would fix.
            Err(error) => crate::diag!(
                SceneWater,
                "water {}: BSData {index} ({name}) record {record} skipped: {error}",
                r.export_id,
                name = files.get(index).map(String::as_str).unwrap_or("<no name>")
            ),
        }
    }
    areas.sort_by_key(|a| a.0);
    let areas: Vec<WaterArea> = areas.into_iter().map(|(_, area)| area).collect();
    // A world that *references* water and yields none is a loss, not an empty
    // map: the manifest was read, its records were found, and every one of them
    // was skipped above. Recording the count here is what makes the difference
    // between "this map has no water" and "this map's water did not load"
    // visible in the status counters -- the report that started this was
    // "0 water areas, 0 cells, 0 of them wet" with nothing beside it.
    //
    // A world with no water references at all is the honest empty case, and it
    // is common: instance maps author none.
    let referenced = scene.blocks.iter().map(|b| b.water.len()).sum::<usize>();
    if areas.is_empty() && referenced > 0 {
        crate::load_status::dropped(
            crate::load_status::Category::Water,
            format!(
                "{} water references across {} blocks, and not one record parsed",
                referenced,
                scene.blocks.len()
            ),
        );
    }
    Ok(areas)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn underwater_uses_active_cells_and_world_orientation() {
        let a = WaterArea {
            export_id: 1,
            color: 0,
            simple: false,
            height: 5.,
            grid_size: 2.,
            columns: 2,
            rows: 2,
            center: [0., 0., 0.],
            mask: vec![1, 0, 0, 2],
            shore_waves: vec![],
        };
        assert!(a.contains_xz(-1., 1.));
        assert!(!a.contains_xz(1., 1.));
        assert!(a.contains_xz(1., -1.));
        assert!(!a.contains_xz(3., 0.));
        assert!(!a.contains_xz(-2., 1.));
        assert!(!a.contains_xz(-1., 2.));
        assert!(!a.contains_xz(2., -1.));
        assert!(!a.contains_xz(1., -2.));
        assert!(!a.contains_xz(f32::NAN, 0.));
    }
    #[test]
    fn rejects_invalid_version() {
        assert!(WaterArea::parse(&[0; 64], 0, 1).is_err())
    }

    /// A refusal has to name the file it refused.
    ///
    /// This is the defect the compatibility matrix caught: `map_err(|e|
    /// e.to_string())` produced "No such file or directory (os error 2)" as the
    /// whole reason, so the matrix cell said water was refused but not what was
    /// missing or where the loader had looked for it. Fails on the old code,
    /// which returned the bare `io::Error` text.
    #[test]
    fn a_missing_water_manifest_is_named_in_the_refusal() {
        let scene = Scene {
            width: 1.,
            length: 1.,
            block_size: 1.,
            rows: 1,
            cols: 1,
            tree_types: Vec::new(),
            grass_types: Vec::new(),
            blocks: Vec::new(),
        };
        let dir = std::env::temp_dir().join("pwe-water-missing-manifest-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let err = load(&scene, &dir).expect_err("no manifest in an empty directory");
        std::fs::remove_dir_all(&dir).ok();
        // The path the loader tried, by name, in the message.
        assert!(
            err.contains("world.ecbsd"),
            "the refusal must name the manifest it wanted: {err}"
        );
        assert!(
            err.contains("cannot read"),
            "the refusal must say what failed: {err}"
        );
    }
    #[test]
    fn decodes_uncompressed_argb_normal_dds() {
        let mut b = vec![0u8; 132];
        b[..4].copy_from_slice(b"DDS ");
        for (p, v) in [
            (4, 124u32),
            (12, 1),
            (16, 1),
            (76, 32),
            (88, 32),
            (92, 0x00ff0000),
            (96, 0x0000ff00),
            (100, 0x000000ff),
            (104, 0xff000000),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[128..132].copy_from_slice(&[120, 254, 131, 255]);
        assert_eq!(
            decode_water_dds(&b).unwrap().get_pixel(0, 0).0,
            [131, 254, 120, 255]
        );
        assert!(decode_water_dds(&b[..131]).is_err());
    }
    #[test]
    fn preserves_authored_water_mips_and_rejects_truncation() {
        let mut b = vec![0; 128 + 4 * 4 * 4 + 2 * 2 * 4 + 4];
        b[..4].copy_from_slice(b"DDS ");
        for (p, v) in [
            (4, 124u32),
            (8, 0x20000),
            (12, 4),
            (16, 4),
            (28, 3),
            (76, 32),
            (88, 32),
            (92, 0x00ff0000),
            (96, 0x0000ff00),
            (100, 0x000000ff),
            (104, 0xff000000),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        let level2 = 128 + 4 * 4 * 4 + 2 * 2 * 4;
        b[level2..level2 + 4].copy_from_slice(&[10, 20, 30, 255]);
        let m = decode_water_dds_mips(&b).unwrap();
        assert_eq!(
            m.iter().map(|x| x.dimensions()).collect::<Vec<_>>(),
            vec![(4, 4), (2, 2), (1, 1)]
        );
        assert_eq!(m[2].get_pixel(0, 0).0, [30, 20, 10, 255]);
        assert!(decode_water_dds_mips(&b[..b.len() - 1]).is_err());
        b[28..32].copy_from_slice(&4u32.to_le_bytes());
        assert!(decode_water_dds_mips(&b).is_err());
    }
    #[test]
    fn shoreline_wave_reads_authored_arrays_and_rejects_truncation() {
        let mut b = vec![0u8; 45 + 1 + 4 + 16 + 36];
        for (p, v) in [
            (0, 0x10003000u32),
            (20, 1),
            (24, 1),
            (28, 32f32.to_bits()),
            (46, 1),
            (50, 0x10003000),
            (54, 1),
            (58, 3f32.to_bits()),
            (62, 1),
            (66, 1f32.to_bits()),
            (70, 2f32.to_bits()),
            (74, 3f32.to_bits()),
            (78, 0xff112233),
            (82, 0xff445566),
            (86, 0xff778899),
            (90, 0xff010203),
            (94, 0xff040506),
            (98, 0xff070809),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        let area = WaterArea::parse(&b, 0, 42).unwrap();
        let wave = &area.shore_waves[0];
        assert_eq!(wave.points, [[1., 2., 3.]]);
        assert_eq!(wave.width, 3.);
        assert_eq!(wave.day_colors, [0xff112233, 0xff445566, 0xff778899]);
        assert_eq!(wave.night_colors, [0xff010203, 0xff040506, 0xff070809]);
        assert!(parse_shore_waves(&b[..b.len() - 1], 46).is_err());
    }
    #[test]
    fn older_shoreline_versions_follow_client_defaults() {
        let mut record = vec![0u8; 16 + 12 + 12];
        for (p, v) in [
            (0, 0x10002000u32),
            (4, 1),
            (8, 2f32.to_bits()),
            (12, 1),
            (16, 1f32.to_bits()),
            (20, 2f32.to_bits()),
            (24, 3f32.to_bits()),
            (28, 0xff112233),
            (32, 0xff445566),
            (36, 0xff778899),
        ] {
            record[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut bytes = 1u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&record);
        let wave = parse_shore_waves(&bytes, 0).unwrap().0.remove(0);
        assert_eq!(wave.day_colors[0], 0xff112233);
        assert_eq!(wave.night_colors[0], 0x4c050a0f);
        bytes[4..8].copy_from_slice(&0x10001000u32.to_le_bytes());
        bytes.truncate(4 + 16 + 12);
        let wave = parse_shore_waves(&bytes, 0).unwrap().0.remove(0);
        assert_eq!(wave.day_colors, [0x80808080; 3]);
        assert_eq!(wave.night_colors, [0x80808080; 3]);
    }

    /// The 1.8.4 water record, rebuilt byte for byte from the shipped data.
    ///
    /// This is `BSData/7.dat` record 0 as the client stores it: `dwVersion` 4,
    /// no `0x100SSSSS` prefix, the v2/v3 field order, then 84 bytes of fields
    /// the older revisions do not have, the 32x32 cell mask at +124, and 46
    /// shoreline waves that end the record. Two separate faults made this area
    /// unreadable and neither was a wrong offset in the old layout:
    ///
    /// the revision word is `4`, so `parse_record` never recognised the record
    /// and reported "the first word is 0x4, which is neither a known
    /// A3DWaterArea version nor a plausible zlib frame". It was then read as a
    /// zlib frame four bytes long, which is the other half of the same miss.
    ///
    /// A whole world of refs like this is why the 1.8.4 client had no water.
    #[test]
    fn the_184_record_layout_is_read_without_a_version_prefix() {
        let mut b = Vec::new();
        b.extend_from_slice(&4u32.to_le_bytes()); // dwVersion
        b.extend_from_slice(&4u32.to_le_bytes()); // dwColor
        b.extend_from_slice(&3.0f32.to_bits().to_le_bytes()); // fHeight
        b.extend_from_slice(&0u32.to_le_bytes()); // unused at +12
        b.extend_from_slice(&32u32.to_le_bytes()); // dwColumns
        b.extend_from_slice(&32u32.to_le_bytes()); // dwRows
        b.extend_from_slice(&32.0f32.to_bits().to_le_bytes()); // fCellSize
        for centre in [2560.0f32, 216.0, 5120.0] {
            b.extend_from_slice(&centre.to_bits().to_le_bytes());
        }
        // The 84 bytes between the centre vector and the mask, as the real
        // record stores them, up to the last field before the mask.
        b.extend_from_slice(&[0u8; 84]);
        assert_eq!(b.len(), 124, "the mask starts at +124");
        let mut mask = vec![0u8; 32 * 32];
        for cell in [0usize, 1, 7, 33, 1023] {
            mask[cell] = 1;
        }
        b.extend_from_slice(&mask);
        b.extend_from_slice(&0u32.to_le_bytes()); // dwWaveCount
        let area = parse_record(&b, 42)
            .expect("a 1.8.4 record is a water area, not a refusal")
            .remove(0);
        assert_eq!((area.columns, area.rows), (32, 32));
        assert_eq!(area.height, 3.0);
        assert_eq!(area.grid_size, 32.0);
        assert_eq!(area.center, [2560.0, 216.0, 5120.0]);
        assert_eq!(area.mask.iter().filter(|m| **m != 0).count(), 5);
        assert!(!area.simple);
        assert_eq!(area.export_id, 42);
    }

    /// A record whose framing is not water is refused, and the refusal says
    /// what it saw and what it expected.
    ///
    /// The temptation is to widen this into "try the bytes as an area anyway",
    /// and it has been tried: a record whose first word is any small integer
    /// then reads as a plausible grid, because a mask of zero bytes is valid,
    /// and BSData records for unrelated tables start exactly that way. Guessing
    /// turned `21.dat` records 15 and 25 -- which inflate to 32768 bytes with no
    /// water word in them, because they are other tables -- into water areas.
    #[test]
    fn a_record_whose_framing_is_not_water_names_both_sides() {
        // 0x1af is a stored length, not a version; this is the shape that sent
        // an investigation after a version that never existed.
        let mut b = 0x1afu32.to_le_bytes().to_vec();
        b.extend_from_slice(&32768u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 16]);
        let error = parse_record(&b, 1).expect_err("not a water record");
        assert!(error.contains("0x1af"), "{error}");
        assert!(error.contains("zlib frame"), "{error}");
        // A declared length past the end of the record is the same kind of
        // refusal, and it is the one that would read raw record bytes as cells.
        let mut b = 4096u32.to_le_bytes().to_vec();
        b.extend_from_slice(&32768u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 16]);
        assert!(parse_record(&b, 1).is_err());
    }

    /// Build one v3 area followed by `waves` shoreline entries.
    fn area_with_waves(waves: &[(u32, usize)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0x10003000u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        // Fields start eight bytes past the version word. Offsets within the
        // field block: color +0, height +4, an unused dword at +8, columns +12,
        // rows +16, grid size +20, centre +24..+36, v3 flag +36, mask +37.
        for v in [
            0u32,             // color
            1.0f32.to_bits(), // height
            0,                // unused
            1,                // columns
            1,                // rows
            2.0f32.to_bits(), // grid size
            0,                // centre x
            0,                // centre y
            0,                // centre z
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(0); // v3 simple flag
        b.push(1); // one 1x1 cell, wet
        b.extend_from_slice(&(waves.len() as u32).to_le_bytes());
        for (version, points) in waves {
            b.extend_from_slice(&version.to_le_bytes());
            b.extend_from_slice(&(*points as u32).to_le_bytes());
            b.extend_from_slice(&2.0f32.to_le_bytes());
            b.extend_from_slice(&(*points as u32).to_le_bytes());
            for i in 0..*points {
                for k in 0..3 {
                    b.extend_from_slice(&((i + k) as f32).to_le_bytes());
                }
            }
            // `arrays` counts the position array too, so the colour arrays are
            // one fewer: v1 has none, v2 the day array, v3 day and night.
            let arrays = match version {
                0x10001000 => 1usize,
                0x10002000 => 2,
                _ => 3,
            };
            for _ in 0..((arrays - 1) * points * 3) {
                b.extend_from_slice(&0xff112233u32.to_le_bytes());
            }
        }
        b
    }

    /// The surface survives a shoreline record this build cannot read.
    ///
    /// `parse_shore_waves` used to abandon the whole array on the first
    /// unreadable entry, and the caller then dropped every wave on the area.
    /// A newer wave revision is a reason to skip that wave and say so, never a
    /// reason to take the water away.
    #[test]
    fn a_newer_shoreline_version_keeps_the_surface_and_the_other_waves() {
        let b = area_with_waves(&[(0x10003000, 2), (0x10009000, 1), (0x10003000, 2)]);
        let area = WaterArea::parse(&b, 0, 1).expect("the grid still decodes");
        // The first wave is kept; the unknown revision stops the strict walk,
        // and the lenient walk hands back what it read.
        assert_eq!(area.shore_waves.len(), 1, "the first wave is kept");
        assert_eq!(area.shore_waves[0].points.len(), 2);
        // The waves begin after the header, the flag byte and the 1x1 cell mask:
        // 8 (version + count) + 36 (fields) + 1 (flag) + 1 (mask) = 46.
        let kept = parse_shore_waves_lenient(&b, 46);
        assert_eq!(kept.len(), 1, "a bad wave does not remove the good ones");
        // And the revision is named, not dropped silently.
        let err = parse_shore_waves(&b, 46).expect_err("the strict walk reports");
        assert!(err.contains("0x10009000"), "{err}");
    }

    /// An unknown water revision is reported by version, not by first dword.
    #[test]
    fn an_unknown_water_version_is_named_as_a_version() {
        let mut b = vec![0u8; 64];
        b[..4].copy_from_slice(&0x10004000u32.to_le_bytes());
        let err = decode_areas(&b, 1).expect_err("no layout for 0x10004000");
        assert!(err.contains("0x10004000"), "{err}");
        assert!(err.contains("no recovered layout"), "{err}");
        // A record that does not start with a water word says so instead of
        // pretending the word was a version.
        let err = decode_areas(&[0u8; 64], 1).expect_err("not a water record");
        assert!(err.contains("no A3DWaterArea record found"), "{err}");
    }

    /// A record that opens with a newer revision still yields an older run.
    #[test]
    fn an_unknown_opening_version_does_not_hide_a_known_run_inside() {
        let mut b = 0x10004000u32.to_le_bytes().to_vec();
        b.extend_from_slice(&[0u8; 12]);
        let area = area_with_waves(&[]);
        b.extend_from_slice(&area);
        let areas = parse_record(&b, 7).expect("the v3 run inside is found");
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].export_id, 7);
    }

    /// The version word picks the offsets, and a revision with no row is
    /// reported rather than guessed at.
    #[test]
    fn the_layout_follows_the_version_word() {
        let v2 = WaterFields::of(0x10002000).expect("v2 has a layout");
        assert_eq!((v2.simple, v2.mask), (None, 36));
        let v3 = WaterFields::of(0x10003000).expect("v3 has a layout");
        assert_eq!((v3.simple, v3.mask), (Some(36), 37));
        // 1.8.4 keeps the v2/v3 field order and moves only the mask, behind the
        // 84 bytes of fields the older revisions do not have.
        let v4 = WaterFields::of(WATER_VERSION_V4).expect("v4 has a layout");
        assert_eq!((v4.simple, v4.mask), (None, 120));
        assert_eq!(
            (v4.color, v4.height, v4.columns, v4.rows),
            (v3.color, v3.height, v3.columns, v3.rows)
        );
        assert!(WaterFields::of(0x10004000).is_none());
        assert!(!is_water_version(0x10004000));
        assert!(is_water_version(WATER_VERSION_V4));
    }

    /// A wave larger than the client's u16 index range is read, not refused.
    ///
    /// The old cap was 21,845 points because the client indexes its triangles
    /// with u16 and each point makes three vertices. The viewer uploads u32
    /// indices, so that limit only ever dropped real data.
    #[test]
    fn a_wave_larger_than_the_u16_index_limit_still_parses() {
        let points = 22_000usize;
        let b = area_with_waves(&[(0x10003000, points)]);
        let area = WaterArea::parse(&b, 0, 1).expect("a 22,000-point wave is not a fault");
        assert_eq!(area.shore_waves.len(), 1);
        assert_eq!(area.shore_waves[0].points.len(), points);
        assert_eq!(area.shore_waves[0].capacity, points);
    }

    #[test]
    fn actual_69_water_if_available() {
        let Ok(path) = std::env::var("PW_WATER_SAMPLE_69") else {
            return;
        };
        let Ok(b) = std::fs::read(path) else {
            return;
        };
        let area = WaterArea::parse(&b[..0x9df4], 0x3d17, 0).unwrap();
        assert_eq!(area.shore_waves.len(), 8);
        assert_eq!(
            area.shore_waves
                .iter()
                .map(|w| w.points.len())
                .collect::<Vec<_>>(),
            [52, 45, 41, 37, 40, 132, 286, 34]
        );
        assert_eq!(area.shore_waves[0].day_colors[0], 0xff647f94);
        assert_eq!(area.shore_waves[0].night_colors[0], 0xff235175);
    }
    /// The five 1.8.4 water records that ship in the client, decoded end to end.
    ///
    /// `tests/fixtures/water-184/water.dat` is a patch package rebuilt from the
    /// client's own bytes: the five records named below, byte for byte, with a
    /// real 4-byte offset table and trailer. Nothing about the layout is
    /// re-implemented, so a change to the record framing breaks this test --
    /// which is the point. It is committed rather than staged because the whole
    /// 1.8.4 failure ("0 water areas, 0 cells, 0 of them wet") was green in CI
    /// for as long as the only corpus test was skipped for want of a file.
    ///
    /// Expected: `4.dat` 1 (32x32, 17 wet), `4.dat` 8 (8x8, 39), `7.dat` 0
    /// (32x32, 533), `21.dat` 0 (60x64, 900) and `44.dat` 9 (8x32, 99).
    #[test]
    fn the_shipped_184_water_records_decode() {
        let path = match std::env::var_os("PW_WATER_184_DAT") {
            Some(value) => std::path::PathBuf::from(value),
            None => {
                crate::pck::corpus_file_absent(Path::new("water-184/water.dat"));
                return;
            }
        };
        if !path.is_file() {
            crate::pck::corpus_file_absent(&path);
            return;
        }
        let data = std::fs::read(&path).expect("the staged water.dat corpus");
        let offsets = records_of(&data);
        let table = table_of(&data);
        assert_eq!(offsets.len(), 5, "the fixture holds five records");
        let mut decoded = Vec::new();
        for (i, &offset) in offsets.iter().enumerate() {
            let end = offsets.get(i + 1).copied().unwrap_or(table);
            match parse_record(&data[offset..end], 1) {
                Ok(areas) => decoded.extend(areas),
                Err(why) => panic!("record {i} at {offset} was refused: {why}"),
            }
        }
        let shape: Vec<(usize, usize, usize)> = decoded
            .iter()
            .map(|a| {
                (
                    a.columns,
                    a.rows,
                    a.mask.iter().filter(|m| **m != 0).count(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (32, 32, 17),
                (8, 8, 39),
                (32, 32, 533),
                (60, 64, 900),
                (8, 32, 99)
            ]
        );
        // The 1.8.4 grid is a real surface, not a bare mask: the centre and cell
        // size have to come through too.
        assert_eq!(decoded[2].center, [2560.0, 216.0, 5120.0]);
        assert_eq!(decoded[2].grid_size, 32.0);
    }

    /// The whole path a client uses: a real world file, its `world.ecbsd`
    /// manifest, and the patches the manifest names, loaded through `load`.
    ///
    /// This is the test the 1.8.4 report needed and did not have. The record
    /// test above proves `parse_record` reads the bytes; this one proves the
    /// loader that the viewer actually calls finds the manifest, resolves each
    /// `BSData/N.dat` name case-insensitively, and hands back the areas. Every
    /// step of that chain was broken at once for 1.8.4, and each step is a
    /// separate way to end up with "0 water areas".
    ///
    /// The staged world retains the client's 1.8.4 header, 22,528-block grid,
    /// all non-water block payloads, and both type tables. Blocks were
    /// re-emitted after reducing the water list to four references, one per
    /// selected patch, so the real tree and grass counts remain measurable too.
    /// The matching manifest and referenced `BSData/N.dat` files are staged
    /// beside the world file, as its manifest paths require.
    #[test]
    fn the_staged_184_client_loads_water_through_the_real_loader() {
        let element = match std::env::var_os("PW_CLIENT_184_DIR") {
            Some(value) => std::path::PathBuf::from(value),
            None => {
                crate::pck::corpus_file_absent(Path::new("pw-184/maps/world/world.ecwld"));
                return;
            }
        };
        let world = element.join("maps/world/world.ecwld");
        if !world.is_file() {
            crate::pck::corpus_file_absent(&world);
            return;
        }
        let scene = Scene::load(&world).expect("the staged 1.8.4 world parses");
        let refs: usize = scene.blocks.iter().map(|b| b.water.len()).sum();
        assert_eq!(refs, 4, "one water reference per staged patch");
        // The directory the world file lives in, which is what the viewer passes
        // (`main.rs` hands `map.dir`). The manifest sits beside `world.ecwld`.
        let world_dir = world.parent().expect("the world file has a directory");
        let areas = load(&scene, world_dir).expect("the staged client's water loads");
        assert_eq!(areas.len(), 4, "one area per reference");
        let wet: usize = areas
            .iter()
            .map(|a| a.mask.iter().filter(|m| **m != 0).count())
            .sum();
        // 4.dat record 1 (17), 7.dat record 0 (533), 21.dat record 0 (900) and
        // 44.dat record 9 (99).
        assert_eq!(wet, 17 + 533 + 900 + 99);
        let mut shapes: Vec<(usize, usize)> = areas.iter().map(|a| (a.columns, a.rows)).collect();
        shapes.sort_unstable();
        assert_eq!(shapes, vec![(8, 32), (32, 32), (32, 32), (60, 64)]);
    }

    #[test]
    fn synthetic_patch_package_with_zlib_records_decodes() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        let specs = [
            (2u32, 2u32, [10.0f32, 30.0, -10.0]),
            (4, 2, [64.0, 216.0, 128.0]),
            (3, 3, [-32.0, 12.0, 96.0]),
        ];
        let mut package = Vec::new();
        let mut offsets = Vec::new();
        for &(cols, rows, center) in &specs {
            offsets.push(package.len() as u32);
            let payload = v3_payload(cols, rows, center);
            let mut plain = vec![0u8; 32768];
            plain[..payload.len()].copy_from_slice(&payload);
            let mut compressed = Vec::new();
            let mut encoder = ZlibEncoder::new(&mut compressed, Compression::fast());
            encoder.write_all(&plain).unwrap();
            encoder.finish().unwrap();
            package.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            package.extend_from_slice(&32768u32.to_le_bytes());
            package.extend_from_slice(&compressed);
        }
        for offset in &offsets {
            package.extend_from_slice(&offset.to_le_bytes());
        }
        package.extend_from_slice(&(offsets.len() as u32).to_le_bytes());

        let parsed_offsets = records_of(&package);
        let table = table_of(&package);
        assert_eq!(parsed_offsets.len(), specs.len());
        let mut decoded = Vec::new();
        for (i, &offset) in parsed_offsets.iter().enumerate() {
            let end = parsed_offsets.get(i + 1).copied().unwrap_or(table);
            decoded.extend(parse_record(&package[offset..end], i as u32).unwrap());
        }
        assert_eq!(decoded.len(), specs.len());
        assert_eq!(decoded[1].columns, 4);
        assert_eq!(decoded[1].rows, 2);
        assert_eq!(decoded[1].center, [64.0, 216.0, 128.0]);
    }

    /// The record offsets a patch package's trailer table holds, sorted.
    fn records_of(data: &[u8]) -> Vec<usize> {
        let Some(tail) = data.len().checked_sub(4) else {
            return Vec::new();
        };
        let Some(entries) = u(data, tail).ok().map(|n| n as usize) else {
            return Vec::new();
        };
        let Some(table) = tail.checked_sub(entries * 4) else {
            return Vec::new();
        };
        let mut offsets: Vec<usize> = (0..entries)
            .filter_map(|i| u(data, table + i * 4).ok().map(|o| o as usize))
            .collect();
        offsets.sort_unstable();
        offsets
    }

    /// Where a patch package's record table starts.
    fn table_of(data: &[u8]) -> usize {
        let tail = data.len().saturating_sub(4);
        let entries = u(data, tail).unwrap_or(0) as usize;
        tail.saturating_sub(entries * 4)
    }

    /// Every water area in a real BSData file from a shipped client, when one is
    /// available. `PW_WATER_BSDATAS` is an `os.pathsep`-separated list of
    /// `.dat` files; the point is that records shipped compressed (`stored,
    /// raw, zlib`) decode instead of failing on their length word.
    #[test]
    fn real_bsdata_records_decode_when_a_corpus_is_available() {
        let Some(list) = std::env::var_os("PW_WATER_BSDATAS") else {
            return;
        };
        let mut areas = 0usize;
        for path in std::env::split_paths(&list) {
            let Ok(data) = std::fs::read(&path) else {
                continue;
            };
            let Some(tail) = data.len().checked_sub(4) else {
                continue;
            };
            let entries = u(&data, tail).unwrap() as usize;
            let Some(table) = tail.checked_sub(entries * 4) else {
                continue;
            };
            let mut offsets: Vec<usize> = (0..entries)
                .filter_map(|i| u(&data, table + i * 4).ok().map(|o| o as usize))
                .collect();
            offsets.sort_unstable();
            offsets.dedup();
            for (i, &offset) in offsets.iter().enumerate() {
                let end = offsets.get(i + 1).copied().unwrap_or(table);
                if offset >= end || end > data.len() {
                    continue;
                }
                if let Ok(batch) = parse_record(&data[offset..end], 1) {
                    areas += batch.len();
                }
            }
        }
        assert!(
            areas > 0,
            "no water areas decoded from {}",
            std::env::var("PW_WATER_BSDATAS").unwrap_or_default()
        );
    }

    /// A one-area v3 payload with an authored shoreline, the shape `8.dat`
    /// record 6 holds in the shipped `a37` patch.
    ///
    /// `version, count` open the payload, then the area's fields follow: colour
    /// at +8, height at +12, grid at +20, centre at +32, the simple flag at
    /// +44, and the mask at +45.
    fn v3_payload(columns: u32, rows: u32, center: [f32; 3]) -> Vec<u8> {
        let mask = (columns * rows) as usize;
        // header, mask, wave count, then one wave: its 16-byte header, one
        // point, and the three-colour arrays for a single point.
        let mut b = vec![0u8; 45 + mask + 4 + 16 + 12 + 12 + 12];
        for (p, v) in [
            (0, 0x10003000u32),
            (4, 1),
            (8, 0xffc0c0c0),
            (12, 30f32.to_bits()),
            (20, columns),
            (24, rows),
            (28, 32f32.to_bits()),
            (32, center[0].to_bits()),
            (36, center[1].to_bits()),
            (40, center[2].to_bits()),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[44] = 1;
        b[45] = 1;
        let wave = 45 + mask;
        b[wave..wave + 4].copy_from_slice(&1u32.to_le_bytes());
        b[wave + 4..wave + 8].copy_from_slice(&0x10003000u32.to_le_bytes());
        b[wave + 8..wave + 12].copy_from_slice(&1u32.to_le_bytes());
        b[wave + 12..wave + 16].copy_from_slice(&1.5f32.to_le_bytes());
        b[wave + 16..wave + 20].copy_from_slice(&1u32.to_le_bytes());
        b[wave + 20..wave + 24].copy_from_slice(&1f32.to_le_bytes());
        b[wave + 24..wave + 28].copy_from_slice(&2f32.to_le_bytes());
        b[wave + 28..wave + 32].copy_from_slice(&3f32.to_le_bytes());
        b
    }

    #[test]
    fn reads_a_raw_record_without_its_own_header() {
        let payload = v3_payload(2, 2, [10., 30., -10.]);
        let areas = parse_record(&payload, 7).unwrap();
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].mask, [1, 0, 0, 0]);
        assert_eq!(areas[0].center, [10., 30., -10.]);
        assert!(areas[0].simple);
        assert_eq!(areas[0].shore_waves.len(), 1);
    }

    /// The shipped patches store records as `stored, raw, zlib(raw)` with a
    /// 32768-byte window. Reading `stored` as a version word is the
    /// `unsupported A3DWaterArea version 0x1af` (431) failure seen on the main
    /// world, so the framing has to be resolved before the version is trusted.
    #[test]
    fn reads_a_zlib_framed_record_instead_of_treating_the_length_as_a_version() {
        let mut plain = vec![0u8; 32768];
        let payload = v3_payload(2, 2, [-1536., 30., 0.]);
        plain[..payload.len()].copy_from_slice(&payload);
        let mut compressed = Vec::new();
        {
            use flate2::{write::ZlibEncoder, Compression};
            use std::io::Write;
            let mut encoder = ZlibEncoder::new(&mut compressed, Compression::fast());
            encoder.write_all(&plain).unwrap();
            encoder.finish().unwrap();
        }
        let mut record = Vec::new();
        record.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        record.extend_from_slice(&32768u32.to_le_bytes());
        record.extend_from_slice(&compressed);
        // 431 is what the length word reads as when it is mistaken for one.
        assert_eq!(u(&record, 0).unwrap(), compressed.len() as u32);
        let areas = parse_record(&record, 8).unwrap();
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].center, [-1536., 30., 0.]);
        assert_eq!(areas[0].height, 30.);
    }

    #[test]
    fn a_run_of_areas_shares_one_version_word() {
        let first = v3_payload(1, 1, [0., 1., 0.]);
        let second = v3_payload(2, 1, [32., 1., 0.]);
        let mut b = Vec::new();
        b.extend_from_slice(&0x10003000u32.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&first[8..]);
        b.extend_from_slice(&second[8..]);
        let areas = decode_areas(&b, 9).unwrap();
        assert_eq!(areas.len(), 2);
        assert_eq!(areas[1].columns, 2);
        assert_eq!(areas[1].center, [32., 1., 0.]);
    }

    /// The word after the version is not a count, and believing it cost nearly
    /// every water area in the shipped world.
    ///
    /// Every record in the retail 1.3.1 patches that decodes at all holds
    /// exactly one area and declares 3, 8, 6 or 4 (`21.dat` record 0 declares
    /// 3, `4.dat` record 1 declares 3, `36.dat` record 9 declares 6), and the
    /// area's shoreline arrays end exactly on the record's last byte. The old
    /// walk read the declared number, failed on the bytes that follow the one
    /// real area, and threw that area away with it — 0 of 11 record-payload
    /// areas survived in the sample the fix was measured on.
    #[test]
    fn a_lying_area_count_does_not_discard_the_area_that_decodes() {
        let payload = v3_payload(3, 2, [64., 30., -64.]);
        let mut b = payload[..8].to_vec();
        // Claim four areas where the payload holds exactly one.
        b[4..8].copy_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&payload[8..]);
        let areas = decode_areas(&b, 11).unwrap();
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].center, [64., 30., -64.]);
        assert_eq!(areas[0].columns, 3);
        assert_eq!(areas[0].mask.len(), 6);
    }

    #[test]
    fn a_broken_area_does_not_discard_its_record() {
        let good = v3_payload(1, 1, [0., 1., 0.]);
        let mut b = Vec::new();
        b.extend_from_slice(&0x10003000u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&good[8..]);
        // A second run whose header is fine but whose grid is impossible.
        b.extend_from_slice(&0x10003000u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 16]);
        let areas = decode_areas(&b, 10).unwrap();
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].center, [0., 1., 0.]);
    }

    #[test]
    fn parses_legacy_v2_water_without_simple_flag() {
        // Instance v2 records are a 44-byte header followed immediately by the
        // cell mask; there is no simple-water flag at +44.
        let mut b = vec![0; 44 + 4 + 4];
        for (p, v) in [
            (0, 0x10002000u32),
            (8, 0xff123456),
            (12, 216f32.to_bits()),
            (20, 2),
            (24, 2),
            (28, 32f32.to_bits()),
            (32, (-32f32).to_bits()),
            (36, 216f32.to_bits()),
            (40, 32f32.to_bits()),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[44..48].copy_from_slice(&[1, 0, 0, 1]);
        let area = WaterArea::parse(&b, 0, 19).unwrap();
        assert_eq!(area.mask, [1, 0, 0, 1]);
        assert!(!area.simple);
        assert_eq!(area.columns, 2);
        assert_eq!(area.center, [-32., 216., 32.]);
        assert!(area.shore_waves.is_empty());
    }

    #[test]
    fn parses_simple_water() {
        let mut b = vec![0; 53];
        for (p, v) in [
            (0, 0x10003000u32),
            (8, 0xff123456),
            (12, 10f32.to_bits()),
            (20, 2),
            (24, 2),
            (28, 32f32.to_bits()),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes())
        }
        b[45..49].copy_from_slice(&[1, 0, 0, 1]);
        let w = WaterArea::parse(&b, 0, 4).unwrap();
        assert!(!w.simple);
        b[44] = 1;
        assert!(WaterArea::parse(&b, 0, 4).unwrap().simple);
        assert_eq!(w.mask, [1, 0, 0, 1]);
        assert_eq!(w.grid_size, 32.);
    }
}

/// Load the retail client's animated water-normal sequence from its texture package.
pub fn load_normals(element: &Path) -> Result<Vec<Vec<image::RgbaImage>>, String> {
    use std::collections::HashMap;
    use std::fs::File;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<Vec<image::RgbaImage>>>>> = OnceLock::new();
    let cache_key = element.to_path_buf();
    if let Ok(guard) = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        if let Some(cached) = guard.get(&cache_key) {
            return Ok(cached.clone());
        }
    }

    let mut file = File::open(element.join("textures.pck")).map_err(|e| e.to_string())?;
    let entries = crate::pck::directory(&mut file)?;
    let mut targets: Vec<(usize, String)> = (0..29)
        .map(|i| {
            (
                i,
                format!("textures/water/waternormal/water_normal{i:02}.dds"),
            )
        })
        .collect();
    let mut matched: Vec<Option<crate::pck::Entry>> = vec![None; targets.len()];
    let mut remaining = targets.len();
    for e in &entries {
        if e.name.len() < 40 {
            continue;
        }
        for (slot, (_, name)) in targets.iter().enumerate() {
            if matched[slot].is_none() && e.name.eq_ignore_ascii_case(name) {
                matched[slot] = Some(e.clone());
                remaining -= 1;
                break;
            }
        }
        if remaining == 0 {
            break;
        }
    }
    let resolved: Vec<(String, crate::pck::Entry)> = targets
        .drain(..)
        .zip(matched)
        .map(|((_, name), entry)| {
            let entry = entry.ok_or_else(|| format!("missing {name}"))?;
            Ok((name, entry))
        })
        .collect::<Result<_, String>>()?;

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(resolved.len())
        .min(32);
    let decode_one = |name: &str,
                      entry: &crate::pck::Entry|
     -> Result<Vec<image::RgbaImage>, String> {
        let raw = crate::pck::read_entry_at(&file, entry).map_err(|e| e.to_string())?;
        match decode_water_dds_mips(&raw) {
            Ok(levels) => Ok(levels),
            Err(err) => {
                // A malformed or unsupported lower mip must not prevent a
                // client that previously loaded its base level from starting.
                let mut base = raw.clone();
                if base.len() >= 12 {
                    let flags = u32::from_le_bytes(base[8..12].try_into().unwrap()) & !0x20000;
                    base[8..12].copy_from_slice(&flags.to_le_bytes());
                }
                let levels = decode_water_dds_mips(&base).map_err(|e| format!("{name}: {e}"))?;
                eprintln!("{name}: {err}; using base level");
                Ok(levels)
            }
        }
    };
    let mut frames: Vec<Vec<image::RgbaImage>> = if workers > 1 {
        let chunk = resolved.len().div_ceil(workers);
        std::thread::scope(|s| -> Result<Vec<Vec<image::RgbaImage>>, String> {
            let handles: Vec<_> = resolved
                .chunks(chunk)
                .map(|slice| {
                    let decode = &decode_one;
                    s.spawn(move || {
                        slice
                            .iter()
                            .map(|(name, entry)| decode(name, entry))
                            .collect::<Result<Vec<_>, String>>()
                    })
                })
                .collect();
            let mut out = Vec::with_capacity(resolved.len());
            for h in handles {
                out.extend(h.join().unwrap()?);
            }
            Ok(out)
        })?
    } else {
        resolved
            .iter()
            .map(|(name, entry)| decode_one(name, entry))
            .collect::<Result<Vec<_>, String>>()?
    };

    if let Some(first_dims) = frames.first().map(|f| f[0].dimensions()) {
        if frames
            .iter()
            .skip(1)
            .any(|f| f[0].dimensions() != first_dims)
        {
            return Err("water normal sequence dimensions differ".into());
        }
    }
    // A texture array needs a common chain length; keep every authored level
    // shared by all frames rather than failing on an unusual archive.
    let levels = frames.iter().map(Vec::len).min().unwrap_or(1);
    for frame in &mut frames {
        frame.truncate(levels);
    }
    if let Ok(mut guard) = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        guard.insert(cache_key, frames.clone());
    }
    Ok(frames)
}

// The retail water-normal DDS is uncompressed 32-bit ARGB, which image's DDS
// decoder does not accept. Preserve all authored mip levels when present.
#[cfg(test)]
fn decode_water_dds(b: &[u8]) -> Result<image::RgbaImage, String> {
    Ok(decode_water_dds_mips(b)?.remove(0))
}
pub(crate) fn decode_water_dds_mips(b: &[u8]) -> Result<Vec<image::RgbaImage>, String> {
    if b.get(..4) != Some(b"DDS ")
        || u(b, 4)? != 124
        || u(b, 76)? != 32
        || u(b, 84)? != 0
        || u(b, 88)? != 32
    {
        return Err("unsupported water DDS layout".into());
    }
    let width = u(b, 16)?;
    let height = u(b, 12)?;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("invalid water DDS dimensions".into());
    }
    let masks = [u(b, 92)?, u(b, 96)?, u(b, 100)?, u(b, 104)?];
    if masks[..3].iter().any(|m| *m == 0) {
        return Err("invalid water DDS masks".into());
    }
    let maximum = 32 - width.max(height).leading_zeros();
    let flags = u(b, 8)?;
    let count = if flags & 0x20000 != 0 {
        u(b, 28)?.max(1)
    } else {
        1
    };
    if count > maximum {
        return Err("invalid water DDS mip count".into());
    }
    let mut levels = Vec::with_capacity(count as usize);
    let mut offset = 128usize;
    for level in 0..count {
        let w = (width >> level).max(1);
        let h = (height >> level).max(1);
        let n = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or("water DDS overflow")?;
        let end = offset.checked_add(n).ok_or("water DDS overflow")?;
        let src = b.get(offset..end).ok_or("truncated water DDS mip")?;
        let mut out = vec![0u8; n];
        let shifts = [
            if masks[0] != 0 {
                masks[0].trailing_zeros()
            } else {
                0
            },
            if masks[1] != 0 {
                masks[1].trailing_zeros()
            } else {
                0
            },
            if masks[2] != 0 {
                masks[2].trailing_zeros()
            } else {
                0
            },
            if masks[3] != 0 {
                masks[3].trailing_zeros()
            } else {
                0
            },
        ];
        let maxes = [
            if masks[0] != 0 {
                masks[0] >> shifts[0]
            } else {
                0
            },
            if masks[1] != 0 {
                masks[1] >> shifts[1]
            } else {
                0
            },
            if masks[2] != 0 {
                masks[2] >> shifts[2]
            } else {
                0
            },
            if masks[3] != 0 {
                masks[3] >> shifts[3]
            } else {
                0
            },
        ];
        if maxes[0] == 255
            && maxes[1] == 255
            && maxes[2] == 255
            && (maxes[3] == 255 || maxes[3] == 0)
        {
            let s0 = shifts[0];
            let s1 = shifts[1];
            let s2 = shifts[2];
            let s3 = shifts[3];
            let has_a = maxes[3] == 255;
            for (pix, dst) in src.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                let v = u32::from_le_bytes([pix[0], pix[1], pix[2], pix[3]]);
                dst[0] = (v >> s0) as u8;
                dst[1] = (v >> s1) as u8;
                dst[2] = (v >> s2) as u8;
                dst[3] = if has_a { (v >> s3) as u8 } else { 255 };
            }
        } else {
            for (pix, dst) in src.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                let v = u32::from_le_bytes([pix[0], pix[1], pix[2], pix[3]]);
                for i in 0..4 {
                    let m = masks[i];
                    if m == 0 {
                        dst[i] = if i == 3 { 255 } else { 0 };
                    } else {
                        let val = (v & m) >> shifts[i];
                        dst[i] = ((val as u64 * 255) / (maxes[i] as u64)) as u8;
                    }
                }
            }
        }
        levels.push(image::RgbaImage::from_raw(w, h, out).ok_or("invalid water DDS pixels")?);
        offset = end;
    }
    Ok(levels)
}

/// Decode the archive's authored DDS mip chain. The image crate decodes only
/// the first level, so each block-compressed level gets a single-level header.
pub(crate) fn decode_texture_mips(raw: &[u8]) -> Result<Vec<image::RgbaImage>, String> {
    let first = match image::load_from_memory_with_format(raw, image::ImageFormat::Dds) {
        Ok(image) => image.to_rgba8(),
        Err(error) => {
            return match decode_water_dds_mips(raw) {
                Ok(levels) => Ok(levels),
                Err(_) => {
                    let mut base = raw.to_vec();
                    if base.len() >= 12 {
                        let flags = u32::from_le_bytes(base[8..12].try_into().unwrap()) & !0x20000;
                        base[8..12].copy_from_slice(&flags.to_le_bytes());
                    }
                    decode_water_dds_mips(&base).map_err(|_| format!("water texture: {error}"))
                }
            };
        }
    };
    let get = |at: usize| {
        raw.get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let flags = get(8).unwrap_or(0);
    let count = if flags & 0x20000 != 0 {
        get(28).unwrap_or(1).max(1)
    } else {
        1
    };
    let fourcc = get(84).unwrap_or(0);
    let block = match fourcc {
        0x31545844 => 8usize,
        0x33545844 | 0x35545844 => 16,
        _ => return Ok(vec![first]),
    };
    let mut levels = vec![first];
    let mut offset = 128usize;
    let (mut w, mut h) = levels[0].dimensions();
    let max_levels = 32 - w.max(h).leading_zeros();
    for i in 0..count.min(max_levels) {
        let size = ((w as usize + 3) / 4)
            .checked_mul((h as usize + 3) / 4)
            .and_then(|n| n.checked_mul(block))
            .ok_or("wave mip size overflow")?;
        let end = offset.checked_add(size).ok_or("wave mip offset overflow")?;
        let Some(data) = raw.get(offset..end) else {
            break;
        };
        if i > 0 {
            let mut dds = raw[..128].to_vec();
            dds[8..12].copy_from_slice(&(flags & !0x20000).to_le_bytes());
            // The image DXT decoder requires multiples of four even for
            // legal 1x1 / 2x2 mip blocks; decode padded blocks, then crop.
            let padded_h = (h + 3) & !3;
            let padded_w = (w + 3) & !3;
            dds[12..16].copy_from_slice(&padded_h.to_le_bytes());
            dds[16..20].copy_from_slice(&padded_w.to_le_bytes());
            dds[20..24].copy_from_slice(&(size as u32).to_le_bytes());
            dds[28..32].copy_from_slice(&1u32.to_le_bytes());
            dds.extend_from_slice(data);
            match image::load_from_memory_with_format(&dds, image::ImageFormat::Dds) {
                Ok(img) => {
                    levels.push(image::imageops::crop_imm(&img.to_rgba8(), 0, 0, w, h).to_image())
                }
                Err(_) => break,
            }
        }
        offset = end;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    Ok(levels)
}

/// Load the client's 30-frame alternate-water texture sequence.
pub fn load_simple_frames(element: &Path) -> Result<Vec<Vec<image::RgbaImage>>, String> {
    use std::collections::HashMap;
    use std::fs::File;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<Vec<image::RgbaImage>>>>> = OnceLock::new();
    let cache_key = element.to_path_buf();
    if let Ok(guard) = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        if let Some(cached) = guard.get(&cache_key) {
            return Ok(cached.clone());
        }
    }

    let mut file = File::open(element.join("textures.pck")).map_err(|e| e.to_string())?;
    let entries = crate::pck::directory(&mut file)?;
    let targets: Vec<String> = (0..30)
        .map(|i| format!("textures/water/l/l{i}.dds"))
        .collect();
    let mut matched: Vec<Option<crate::pck::Entry>> = vec![None; targets.len()];
    let mut remaining = targets.len();
    for e in &entries {
        if e.name.len() < 22 || e.name.len() > 26 {
            continue;
        }
        for (slot, name) in targets.iter().enumerate() {
            if matched[slot].is_none() && e.name.eq_ignore_ascii_case(name) {
                matched[slot] = Some(e.clone());
                remaining -= 1;
                break;
            }
        }
        if remaining == 0 {
            break;
        }
    }
    let resolved: Vec<(String, crate::pck::Entry)> = targets
        .into_iter()
        .zip(matched)
        .map(|(name, entry)| {
            let entry = entry.ok_or_else(|| format!("missing {name}"))?;
            Ok((name, entry))
        })
        .collect::<Result<_, String>>()?;

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(resolved.len())
        .min(32);
    let decode_one =
        |name: &str, entry: &crate::pck::Entry| -> Result<Vec<image::RgbaImage>, String> {
            let data = crate::pck::read_entry_at(&file, entry).map_err(|e| e.to_string())?;
            decode_texture_mips(&data).map_err(|e| format!("{name}: {e}"))
        };
    let mut frames: Vec<Vec<image::RgbaImage>> = if workers > 1 {
        let chunk = resolved.len().div_ceil(workers);
        std::thread::scope(|s| -> Result<Vec<Vec<image::RgbaImage>>, String> {
            let handles: Vec<_> = resolved
                .chunks(chunk)
                .map(|slice| {
                    let decode = &decode_one;
                    s.spawn(move || {
                        slice
                            .iter()
                            .map(|(name, entry)| decode(name, entry))
                            .collect::<Result<Vec<_>, String>>()
                    })
                })
                .collect();
            let mut out = Vec::with_capacity(resolved.len());
            for h in handles {
                out.extend(h.join().unwrap()?);
            }
            Ok(out)
        })?
    } else {
        resolved
            .iter()
            .map(|(name, entry)| decode_one(name, entry))
            .collect::<Result<Vec<_>, String>>()?
    };

    if let Some(first_dims) = frames.first().map(|f| f[0].dimensions()) {
        for (i, frame) in frames.iter().enumerate().skip(1) {
            if frame[0].dimensions() != first_dims {
                return Err(format!("textures/water/l/l{i}.dds: dimensions differ"));
            }
        }
    }
    let count = frames.iter().map(Vec::len).min().unwrap_or(1);
    for frame in &mut frames {
        frame.truncate(count);
    }
    if let Ok(mut guard) = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        guard.insert(cache_key, frames.clone());
    }
    Ok(frames)
}
