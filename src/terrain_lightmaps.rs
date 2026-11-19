//! Bounded T2LM v2 DDS records and subterrain-to-mask-area placement.
//! The offset table is indexed by mask area; payload order is not guaranteed.
//!
//! Availability is published to the grass shader as the low two bits of one
//! `u32` per mask area, so the numeric values below are an interface and are
//! pinned by tests. `0` means nothing is bound: the client's `ApplyLightMap`
//! leaves a terrain area unlit when its day record is missing, and only a
//! missing *night* record falls back to the day record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// No day record, so no lightmap is bound for this area.
    Unbound,
    /// Authored day record only; night falls back to the day tile.
    DayOnly,
    /// Both authored day and night records.
    Both,
}
impl Availability {
    /// Bits the grass shader tests with `& 3u`.
    pub const fn bits(self) -> u32 {
        match self {
            Self::Unbound => 0,
            Self::DayOnly => 1,
            Self::Both => 3,
        }
    }
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            0 => Some(Self::Unbound),
            1 => Some(Self::DayOnly),
            3 => Some(Self::Both),
            _ => None,
        }
    }
    /// Grass needs both authored maps: the terrain's day copy in the night slot
    /// is a terrain fallback, not an authored night map.
    pub const fn grass_ready(self) -> bool {
        matches!(self, Self::Both)
    }
}
/// Day is required for terrain. Only an absent night file falls back to day;
/// other I/O/format errors remain visible. Grass must inspect availability,
/// not mistake the terrain fallback copy for an authored night map.
pub struct Pair {
    day: Vec<u8>,
    night: Option<Vec<u8>>,
}
impl Pair {
    pub fn read(day: &std::path::Path, night: &std::path::Path) -> Result<Self, String> {
        Self::read_optional(day, night)?
            .ok_or_else(|| format!("{}: missing day lightmap", day.display()))
    }
    /// A missing *day* record leaves the area unbound (`Ok(None)`) instead of
    /// failing the whole world, matching the client's per-area `ApplyLightMap`
    /// result. Every other I/O or format error is still returned.
    pub fn read_optional(
        day: &std::path::Path,
        night: &std::path::Path,
    ) -> Result<Option<Self>, String> {
        let day_bytes = match std::fs::read(day) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", day.display())),
        };
        let night_bytes = match std::fs::read(night) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", night.display())),
        };
        Ok(Some(Self {
            day: day_bytes,
            night: night_bytes,
        }))
    }
    pub fn bytes(&self, night: bool) -> &[u8] {
        if night {
            self.night.as_deref().unwrap_or(&self.day)
        } else {
            &self.day
        }
    }
    pub fn availability(&self) -> Availability {
        if self.night.is_some() {
            Availability::Both
        } else {
            Availability::DayOnly
        }
    }
}
pub const TILES: usize = 16;

/// Edge length of one lightmap tile, in texels.
///
/// Two are shipped and both are measured, not guessed: every `login_*`,
/// `a91_*` and `a85_*` `.t2lm` in the staged clients is **128**, and
/// `world_8.t2lm` (the file the phase-1 record describes) is **256**. The
/// header states which, so the reader reads it rather than assuming one --
/// an assumption of 256 refuses every login map and 128-tile world map, and
/// one of 128 misreads every 256 tile.
pub fn tile_side(bytes: &[u8]) -> Result<usize, String> {
    let side = u32::from_le_bytes(
        bytes
            .get(12..16)
            .ok_or("truncated T2LM header")?
            .try_into()
            .unwrap(),
    ) as usize;
    match side {
        128 | 256 => Ok(side),
        other => Err(format!(
            "unsupported T2LM tile edge {other}; this build reads 128 and 256"
        )),
    }
}

/// Read the tile edge from a file's header without loading its payload.
///
/// The atlas has to be sized before the first upload, and a tile edge can only
/// be learned from a file, so the caller needs a cheap header probe. `Ok(None)`
/// means the file is absent -- the caller treats that as "no authored maps"
/// rather than as an error, matching `Pair::read_optional`.
pub fn side_of(path: &std::path::Path) -> Result<Option<usize>, String> {
    use std::io::Read;
    let mut head = [0u8; 16];
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    file.read_exact(&mut head)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    tile_side(&head)
        .map(Some)
        .map_err(|error| format!("{}: {error}", path.display()))
}

pub struct Tiles<'a> {
    bytes: &'a [u8],
    ranges: [(usize, usize); TILES],
    side: usize,
}
impl<'a> Tiles<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        const HEADER: usize = 24 + TILES * 4;
        let u = |p| -> Result<usize, String> {
            Ok(u32::from_le_bytes(
                bytes
                    .get(p..p + 4)
                    .ok_or("truncated T2LM header")?
                    .try_into()
                    .unwrap(),
            ) as usize)
        };
        if u(0)? != 0x54324c4d || u(4)? != 2 || u(8)? != TILES || u(16)? != 1 {
            return Err("unsupported T2LM version, tile count or DDS flags".into());
        }
        // The tile edge is read, not asserted: both 128 and 256 ship. See
        // `tile_side`, which also names the value when it is neither.
        let side = tile_side(bytes)?;
        let size = u(20)?;
        if size == 0 {
            return Err("empty T2LM tile".into());
        }
        let mut ranges = [(0, 0); TILES];
        for i in 0..TILES {
            let start = u(24 + i * 4)?;
            let end = start.checked_add(size).ok_or("T2LM tile size overflow")?;
            if start < HEADER || end > bytes.len() {
                return Err("T2LM tile outside payload".into());
            }
            if ranges[..i].iter().any(|&(a, b)| start < b && a < end) {
                return Err("overlapping T2LM tiles".into());
            }
            ranges[i] = (start, end);
        }
        Ok(Self {
            bytes,
            ranges,
            side,
        })
    }
    /// Edge length of every tile in this file, in texels (128 or 256).
    pub fn side(&self) -> usize {
        self.side
    }
    pub fn tile(&self, i: usize) -> Option<&'a [u8]> {
        self.ranges.get(i).map(|&(a, b)| &self.bytes[a..b])
    }
}
#[derive(Debug)]
pub struct Layout {
    areas_x: usize,
    subs_x: usize,
    sub_count: usize,
}
impl Layout {
    pub fn new(areas_x: usize, area_count: usize, sub_count: usize) -> Result<Self, String> {
        if areas_x == 0
            || areas_x % 4 != 0
            || area_count == 0
            || area_count % areas_x != 0
            || (area_count / areas_x) % 4 != 0
            || sub_count.checked_mul(TILES) != Some(area_count)
        {
            return Err("inconsistent lightmap subterrain/mask-area dimensions".into());
        }
        Ok(Self {
            areas_x,
            subs_x: areas_x / 4,
            sub_count,
        })
    }
    pub fn area(&self, sub: usize, tile: usize) -> Option<usize> {
        if sub >= self.sub_count || tile >= TILES {
            return None;
        }
        let x = (sub % self.subs_x) * 4 + tile % 4;
        let y = (sub / self.subs_x) * 4 + tile / 4;
        Some(y * self.areas_x + x)
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn missing_night_falls_back_for_terrain_without_marking_grass_ready() {
        let root = std::env::temp_dir().join(format!("pw-lm-night-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let day = root.join("day");
        let night = root.join("night");
        std::fs::write(&day, [1, 2, 3]).unwrap();
        let pair = super::Pair::read(&day, &night).unwrap();
        assert_eq!(pair.availability(), super::Availability::DayOnly);
        assert_eq!(pair.availability().bits(), 1);
        assert!(!pair.availability().grass_ready());
        assert_eq!(pair.bytes(true), pair.bytes(false));
        std::fs::write(&night, [4, 5, 6]).unwrap();
        let pair = super::Pair::read(&day, &night).unwrap();
        assert_eq!(pair.availability(), super::Availability::Both);
        assert_eq!(pair.availability().bits(), 3);
        assert!(pair.availability().grass_ready());
        assert_eq!(pair.bytes(true), [4, 5, 6]);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_day_and_non_missing_night_errors_are_not_hidden() {
        let root = std::env::temp_dir().join(format!("pw-lm-errors-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let day = root.join("day");
        let night = root.join("night");
        std::fs::write(&night, [1]).unwrap();
        assert!(super::Pair::read(&day, &night).is_err());
        std::fs::write(&day, [2]).unwrap();
        assert!(super::Pair::read(&day, &root).is_err()); // Directory is not a missing file.
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_day_record_leaves_the_area_unbound_instead_of_failing() {
        let root = std::env::temp_dir().join(format!("pw-lm-unbound-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let day = root.join("day");
        let night = root.join("night");
        std::fs::write(&night, [1]).unwrap();
        assert!(super::Pair::read_optional(&day, &night).unwrap().is_none());
        assert!(super::Pair::read(&day, &night).is_err());
        // An unreadable day path is an error, not an unbound area.
        assert!(super::Pair::read_optional(&root, &night).is_err());
        std::fs::write(&day, [2]).unwrap();
        let pair = super::Pair::read_optional(&day, &night).unwrap().unwrap();
        assert_eq!(pair.availability(), super::Availability::Both);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn published_availability_bits_round_trip_and_gate_grass() {
        for a in [
            super::Availability::Unbound,
            super::Availability::DayOnly,
            super::Availability::Both,
        ] {
            assert_eq!(super::Availability::from_bits(a.bits()), Some(a));
            assert_eq!(a.grass_ready(), a.bits() & 3 == 3);
        }
        assert_eq!(super::Availability::Unbound.bits(), 0);
        assert_eq!(super::Availability::from_bits(2), None);
        assert_eq!(super::Availability::from_bits(4), None);
    }

    use super::*;
    fn fixture() -> Vec<u8> {
        let mut b = vec![0; 88 + TILES * 4];
        for (i, v) in [0x54324c4du32, 2, 16, 256, 1, 4].into_iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        for i in 0..TILES {
            // Reverse order deliberately. A next-offset size heuristic fails.
            let p = 88 + (TILES - 1 - i) * 4;
            b[24 + i * 4..28 + i * 4].copy_from_slice(&(p as u32).to_le_bytes());
            b[p..p + 4].fill(i as u8);
        }
        b
    }
    #[test]
    fn independent_offsets_preserve_mask_indices() {
        let b = fixture();
        let t = Tiles::parse(&b).unwrap();
        for i in 0..TILES {
            assert_eq!(t.tile(i).unwrap(), [i as u8; 4]);
        }
        assert!(t.tile(TILES).is_none());
    }
    #[test]
    fn rejects_invalid_headers_ranges_and_overlap() {
        let original = fixture();
        for n in 0..original.len() {
            assert!(Tiles::parse(&original[..n]).is_err());
        }
        for (p, v) in [
            (0, 0),
            (4, 3),
            (8, 15),
            (12, 64),
            (16, 0),
            (16, 3),
            (20, 0),
            (20, u32::MAX),
            (24, 0),
            (24, 88),
            (24, 89),
        ] {
            let mut b = original.clone();
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
            assert!(Tiles::parse(&b).is_err(), "{p}/{v}");
        }
    }
    /// The atlas is sized from a header probe before any payload is read, so
    /// the probe has to agree with `parse` and has to distinguish an absent
    /// file from a malformed one.
    #[test]
    fn header_probe_reports_absent_and_malformed_differently() {
        let root = std::env::temp_dir().join(format!("pw-lm-probe-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for side in [128u32, 256] {
            let path = root.join(format!("probe{side}.t2lm"));
            let mut b = fixture();
            b[12..16].copy_from_slice(&side.to_le_bytes());
            std::fs::write(&path, &b).unwrap();
            assert_eq!(side_of(&path).unwrap(), Some(side as usize));
            // The probe and the parser must not disagree about the same file.
            assert_eq!(Tiles::parse(&b).unwrap().side(), side as usize);
        }
        // Absent is not an error: a subterrain with no authored map is normal.
        assert_eq!(side_of(&root.join("missing.t2lm")).unwrap(), None);
        // Malformed is, and the reason names the file.
        let bad = root.join("bad.t2lm");
        let mut b = fixture();
        b[12..16].copy_from_slice(&512u32.to_le_bytes());
        std::fs::write(&bad, &b).unwrap();
        let error = side_of(&bad).unwrap_err();
        assert!(error.contains("bad.t2lm"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Both tile edges ship, so both must read, and the side must be reported
    /// rather than assumed. `login_*`/`a91_*`/`a85_*` `.t2lm` are 128;
    /// `world_8.t2lm` is 256.
    #[test]
    fn reads_the_tile_edge_the_header_states() {
        for side in [128u32, 256] {
            let mut b = fixture();
            b[12..16].copy_from_slice(&side.to_le_bytes());
            let t = Tiles::parse(&b).unwrap();
            assert_eq!(t.side(), side as usize, "header side {side}");
            assert_eq!(t.tile(0).unwrap(), [0; 4]);
        }
        // A tile edge no client ships is refused by name, not silently read as
        // whichever of the two happens to be first in a `match`.
        let mut b = fixture();
        b[12..16].copy_from_slice(&512u32.to_le_bytes());
        let error = match Tiles::parse(&b) {
            Ok(_) => panic!("a 512 tile edge must be refused"),
            Err(error) => error,
        };
        assert!(error.contains("512"), "{error}");
        assert!(error.contains("128 and 256"), "{error}");
    }

    #[test]
    fn rectangular_world_layout_is_bijective() {
        for (x, y) in [(44, 32), (32, 44), (4, 4), (8, 12)] {
            let l = Layout::new(x, x * y, x * y / TILES).unwrap();
            let mut ids = vec![false; x * y];
            for sub in 0..x * y / TILES {
                for tile in 0..TILES {
                    let a = l.area(sub, tile).unwrap();
                    assert!(!ids[a]);
                    ids[a] = true;
                }
            }
            assert!(ids.into_iter().all(|x| x));
            assert_eq!(l.area(0, 0), Some(0));
            assert_eq!(l.area(0, 4), Some(x));
            if y > 4 {
                assert_eq!(l.area(x / 4, 0), Some(x * 4));
            }
            assert!(l.area(x * y / TILES, 0).is_none());
            assert!(l.area(0, TILES).is_none());
        }
    }
    #[test]
    fn rejects_inconsistent_layout() {
        for (x, n, s) in [
            (0, 0, 0),
            (3, 48, 3),
            (4, 0, 0),
            (4, 15, 1),
            (4, 20, 1),
            (8, 16, 1),
            (4, 16, 0),
            (4, 16, usize::MAX),
        ] {
            assert!(Layout::new(x, n, s).is_err());
        }
    }
}
