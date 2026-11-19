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
pub struct Tiles<'a> {
    bytes: &'a [u8],
    side: usize,
    ranges: [(usize, usize); TILES],
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
        let [magic, version, count, side, flags] = [u(0)?, u(4)?, u(8)?, u(12)?, u(16)?];
        if magic != 0x54324c4d {
            return Err("invalid T2LM magic".into());
        }
        if version != 2 || count != TILES || !matches!(side, 128 | 256) || flags != 1 {
            return Err(format!(
                "unsupported T2LM version={version} tiles={count} side={side} flags=0x{flags:x}"
            ));
        }
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
            side,
            ranges,
        })
    }
    pub fn side(&self) -> usize {
        self.side
    }

    pub fn tile(&self, i: usize) -> Option<&'a [u8]> {
        self.ranges.get(i).map(|&(a, b)| &self.bytes[a..b])
    }
}
/// Place a 128² BC1 tile in the legacy 256² atlas slot without re-encoding
/// endpoints. Each selector is repeated over a 2×2 texel neighbourhood.
/// This is storage expansion, not authored higher-resolution lighting.
pub fn expand_bc1_128(src: &[u8]) -> Result<Vec<u8>, String> {
    const BLOCKS: usize = 32;
    if src.len() != BLOCKS * BLOCKS * 8 {
        return Err("wrong BC1 byte count for 128px T2LM tile".into());
    }
    let mut dst = vec![0; 4 * src.len()];
    for by in 0..BLOCKS {
        for bx in 0..BLOCKS {
            let here = (by * BLOCKS + bx) * 8;
            let block = &src[here..here + 8];
            let selectors = u32::from_le_bytes(block[4..8].try_into().unwrap());
            for jy in 0..2 {
                for jx in 0..2 {
                    let there = ((by * 2 + jy) * BLOCKS * 2 + bx * 2 + jx) * 8;
                    let mut expanded = 0u32;
                    for y in 0..4 {
                        for x in 0..4 {
                            let source = (2 * jy + y / 2) * 4 + 2 * jx + x / 2;
                            expanded |= ((selectors >> (source * 2)) & 3) << ((y * 4 + x) * 2);
                        }
                    }
                    dst[there..there + 4].copy_from_slice(&block[..4]);
                    dst[there + 4..there + 8].copy_from_slice(&expanded.to_le_bytes());
                }
            }
        }
    }
    Ok(dst)
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
    #[test]
    fn accepts_retail_128_header_and_expands_bc1_selectors_without_encoding() {
        let mut record = fixture();
        record[12..16].copy_from_slice(&128u32.to_le_bytes());
        assert_eq!(Tiles::parse(&record).unwrap().side(), 128);
        let src: Vec<_> = (0..32u32 * 32)
            .flat_map(|i| [i.to_le_bytes(), (0x6e1839e4u32 ^ i).to_le_bytes()].concat())
            .collect();
        let dst = super::expand_bc1_128(&src).unwrap();
        let read = |bytes: &[u8], side: usize, x: usize, y: usize| {
            let i = ((y / 4) * side / 4 + x / 4) * 8;
            let bits = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap());
            (
                bytes[i..i + 4].to_vec(),
                (bits >> ((y % 4 * 4 + x % 4) * 2)) & 3u32,
            )
        };
        for y in 0..256 {
            for x in 0..256 {
                assert_eq!(read(&src, 128, x / 2, y / 2), read(&dst, 256, x, y));
            }
        }
        assert!(super::expand_bc1_128(&src[1..]).is_err());
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
