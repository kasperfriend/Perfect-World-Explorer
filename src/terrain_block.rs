//! T2BK block layout: where each block's sections start and end.
//!
//! Kept free of compression so it can be tested without external crates; the
//! caller inflates the ranges this module reports.
//!
//! ## Why the third section exists
//!
//! `world_39.t2bk` is v5 with 256 blocks and header flags `0x35`
//! (`0x1 | 0x4 | 0x10 | 0x20`). Parsing only the height and normal sections
//! leaves 1.7-1.9 kB unaccounted for in *every* block and reads past the end of
//! the file on the last one (the block ends at 1,758,080 in a 1,761,150-byte
//! file). Accounting for a third zlib section of `(grid+1)^2 * 4` bytes after the
//! normals makes the block spans sum to exactly the file length minus the offset
//! table, in all 256 blocks:
//!
//! ```text
//! sum of spans  = 1,760,110
//! file - table  = 1,761,150 - (16 + 256*4) = 1,760,110
//! ```
//!
//! So the format is: 32-byte block head, heights, normals, then per-vertex RGBA.
//! In the retail file the section is present in all 256 blocks, is exactly 1089
//! vertices of 4 bytes per block once inflated, and its alpha is uniformly 255 —
//! the colour is a per-vertex tint (RGB 93..254), not a hole mask.

const MAGIC: u32 = 0x5442_4b42;

/// T2BK layout revisions this reader implements.
///
/// The header grows at 3, 4 and 6, and the pre-v5 flag byte is read as a
/// has-data byte. Every `.t2bk` surveyed across all six staged clients is
/// version 6 (`tools/version-gates.py`), so the top of the range is what
/// shipped data uses.
pub const KNOWN_VERSIONS: std::ops::RangeInclusive<u32> = 2..=6;

/// Per-vertex RGBA section present after the normals.
pub const FLAG_VERTEX_COLOUR: u8 = 0x1;
/// Heights and normals are zlib-compressed.
pub const FLAG_COMPRESSED: u8 = 0x4;
/// Normals are zlib-compressed.
pub const FLAG_COMPRESSED_NORMALS: u8 = 0x10;
/// Normals are in azimuth/elevation form rather than 12-byte vectors.
pub const FLAG_POLAR_NORMALS: u8 = 0x20;

/// Header flags the format defines. Any other set bit is a format this reader has
/// not seen, and is an explicit error rather than a silent misread.
const KNOWN_FLAGS: u8 =
    FLAG_VERTEX_COLOUR | FLAG_COMPRESSED | FLAG_COMPRESSED_NORMALS | FLAG_POLAR_NORMALS;

/// Section byte ranges for one block, relative to the block offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sections {
    pub header_len: usize,
    /// Byte ranges `[start, end)` inside the file.
    pub heights: (usize, usize),
    pub normals: (usize, usize),
    /// `Some` only when `FLAG_VERTEX_COLOUR` is set.
    pub colour: Option<(usize, usize)>,
}

/// The parsed shape of a T2BK file: version, flags and per-block sections.
#[derive(Debug)]
pub struct Layout {
    pub version: u32,
    pub flags: u8,
    pub header_len: usize,
    pub blocks: Vec<Sections>,
}

fn u32at(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4).ok_or("truncated")?.try_into().unwrap(),
    ))
}

/// The fields of a `.trn2` header that place the map in the world.
///
/// ## There is no world transform here
///
/// An instance map ships its own `.trn2`, and the question was whether reading
/// its `precinct.clt` polygons needs a translation applied first -- whether the
/// map is authored around the origin and placed somewhere in the world by a
/// matrix stored in the terrain header. It is not. The header is 96 bytes of
/// geometry and counters:
///
/// | offset | field | measured (`a37.trn2`, 3,457 B) |
/// |---|---|---|
/// | 0  | magic `2NRT` (`0x54524e32`) | — |
/// | 4  | version | 1 |
/// | 8  | width in metres | 6,144.0 |
/// | 12 | length in metres | 3,072.0 |
/// | 16 | block size in metres | 64.0 |
/// | 20 | cells per block edge | 32 |
/// | 24 | block count | 4,608 (= 48 x 96) |
/// | 28, 32 | rows, columns | 48, 96 |
/// | 36..76 | section counts and offsets | — |
/// | 68 | grid spacing in metres | 2.0 |
/// | 76, 80 | loader id, loader offset | 1, 3,282 |
///
/// Every word either matches the map's own geometry or indexes the sections
/// that follow, and dwords 84..96 are zero in both staged copies of `a37.trn2`.
/// So there is **no stored translation, rotation or scale** for placing the map
/// in the world, and nothing left over in the header that could be one.
///
/// What that does *not* settle is the coordinate frame itself. The extent here
/// is x 0..6,144 and z 0..3,072 (48 rows x 96 columns x 64 m), while the zone
/// polygons in the world's `precinct.clt` reach z = 5,637 and negative x. Two
/// readings remain open: the polygons are in a frame whose origin is not the
/// terrain corner (a centred or offset frame, which would still need no
/// per-map transform because every map would share it), or there is a shared
/// world offset this header does not carry. The instance `.trn2` matching its
/// own `.ecwld` exactly (same 48 x 96 x 64 m in both) is what rules out a
/// *per-instance* placement; the frame origin is a separate question and is
/// recorded as open in `Findings/Unsupported-client-paths.md` rather than
/// guessed at here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trn2Header {
    pub version: u32,
    pub width: f32,
    pub length: f32,
    pub block_size: f32,
    pub grid: u32,
    pub blocks: u32,
    pub rows: u32,
    pub columns: u32,
    pub grid_spacing: f32,
}

/// Read and sanity-check a `.trn2` header.
pub fn trn2_header(data: &[u8]) -> Result<Trn2Header, String> {
    const MAGIC: u32 = 0x5452_4e32; // "2NRT"
    if u32at(data, 0)? != MAGIC {
        return Err("invalid TRN2".into());
    }
    let f = |p: usize| -> Result<f32, String> { Ok(f32::from_bits(u32at(data, p)?)) };
    let header = Trn2Header {
        version: u32at(data, 4)?,
        width: f(8)?,
        length: f(12)?,
        block_size: f(16)?,
        grid: u32at(data, 20)?,
        blocks: u32at(data, 24)?,
        rows: u32at(data, 28)?,
        columns: u32at(data, 32)?,
        grid_spacing: f(68)?,
    };
    if header.version > 1 {
        return Err(format!("unsupported TRN2 version {}", header.version));
    }
    for (name, value) in [
        ("width", header.width),
        ("length", header.length),
        ("block size", header.block_size),
        ("grid spacing", header.grid_spacing),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(format!("invalid TRN2 {name} {value}"));
        }
    }
    if header.grid == 0
        || header.rows == 0
        || header.columns == 0
        || header.rows.checked_mul(header.columns) != Some(header.blocks)
    {
        return Err("TRN2 block grid does not add up".into());
    }
    Ok(header)
}

/// Whether a world-space point lies inside a map's own terrain extent.
///
/// **The frame is centred on the origin, and this was measured, not assumed.**
/// The extent words are sizes, not a corner: a map `width` x `length` covers
/// `-width/2 ..= width/2` in x and the same in z. The measurement that settles
/// it is `world.ecwld`, whose 7,737 placed records fill the centred box to
/// 99.5 % in x and 97.5 % in z with **0** points outside it; if the box were
/// anchored at a corner, every one of the 5,461 world units of negative z would
/// be outside. The same holds for the `a37` and `a07` instances (0 outside) and
/// for the 383 zone centres of the world's `precinct.clt` (0 outside, 92.3 % of
/// half-width). See `Findings/Unsupported-client-paths.md`.
///
/// An earlier version of this function tested `0.0..=width`, which made every
/// genuinely-placed point in the negative half fail. Nothing caught it because
/// the only caller was a test asserting a point that happened to be positive.
pub fn contains_world_point(header: &Trn2Header, x: f32, z: f32) -> bool {
    let half_width = header.width / 2.0;
    let half_length = header.length / 2.0;
    (-half_width..=half_width).contains(&x) && (-half_length..=half_length).contains(&z)
}

/// Parse the offset table and derive every block's section ranges.
///
/// `grid` is the cell count per block edge (`world.trn2` header dword @20); it is
/// only used to size the colour section so a wrong size is caught here rather than
/// surfacing as a corrupt texture.
pub fn layout(data: &[u8], grid: usize) -> Result<Layout, String> {
    if u32at(data, 0)? != MAGIC {
        return Err("invalid T2BK".into());
    }
    let version = u32at(data, 4)?;
    // Versions 2..=6 are the layout revisions this reader was built against:
    // the header grows at 3, 4 and 6, and the pre-v5 flag byte is read as a
    // has-data byte. Above 6 there is no recovered layout, and guessing one
    // would silently shift every section offset below.
    //
    // Surveyed, not assumed: every `.t2bk` staged from the 1.3.1, 1.3.6, 1.4.2,
    // 1.4.4, 1.5.2 and 1.8.4 installs is version 6
    // (`tools/version-gates.py`). So the top of the range is what shipped data
    // uses, and a version above it is a client this reader has never seen
    // rather than a client it ought to handle. `main.rs` consults the same
    // range, so the scan and the loader cannot disagree about a block.
    if !KNOWN_VERSIONS.contains(&version) {
        return Err(format!(
            "unsupported T2BK version {version} (this build reads {}..={}); \
             the block is skipped by name, not the map",
            KNOWN_VERSIONS.start(),
            KNOWN_VERSIONS.end()
        ));
    }
    let count = u32at(data, 8)? as usize;
    let flags = if version < 5 {
        // Pre-v5 files only have a "has data" byte; the loader has always read it
        // as 0x15 (compressed + polar normals + vertex colour).
        if data.get(12).copied().unwrap_or(0) != 0 {
            0x15
        } else {
            0
        }
    } else {
        data.get(12).copied().ok_or("truncated flags")?
    };
    if flags & !KNOWN_FLAGS != 0 {
        return Err(format!(
            "T2BK flags 0x{flags:02x} include an undefined bit (known mask 0x{KNOWN_FLAGS:02x})"
        ));
    }
    let header_len = if version >= 6 {
        36
    } else if version >= 4 {
        32
    } else if version >= 3 {
        28
    } else {
        24
    };
    let nv = (grid + 1) * (grid + 1);
    let table_end = 16 + count * 4;
    let mut blocks = Vec::with_capacity(count);
    for bi in 0..count {
        let off = u32at(data, 16 + bi * 4)? as usize;
        // The offset table is the only length record for a block: the next block's
        // offset bounds it, and the last block runs to EOF.
        let next = if bi + 1 < count {
            u32at(data, 16 + (bi + 1) * 4)? as usize
        } else {
            data.len()
        };
        if off < table_end || next > data.len() || next < off {
            return Err(format!("block {bi} offset out of range"));
        }
        let hs = u32at(data, off + 16)? as usize;
        let ns = u32at(data, off + 20)? as usize;
        let hend = off
            .checked_add(header_len)
            .and_then(|x| x.checked_add(hs))
            .ok_or("block range overflow")?;
        let nend = hend.checked_add(ns).ok_or("block range overflow")?;
        if nend > next {
            return Err(format!(
                "block {bi}: heights+normals end at {nend}, past the next block at {next}"
            ));
        }
        let colour = if flags & FLAG_VERTEX_COLOUR != 0 {
            let raw = next - nend;
            // An empty section cannot inflate to anything, so it is an error.
            //
            // The compressed size is deliberately NOT compared to `nv * 4`: zlib
            // output can be smaller than its input. Blocks 152 and 168 of the
            // retail `world_39.t2bk` carry 4678 and 4709 compressed bytes that
            // inflate to exactly 4356, so a "must be smaller than the output"
            // bound would have rejected real client data. Only the inflated length
            // proves the section is well formed, and the caller checks it.
            if raw == 0 {
                return Err(format!("block {bi}: empty colour section"));
            }
            Some((nend, next))
        } else {
            if nend != next {
                return Err(format!(
                    "block {bi}: {} trailing bytes but the colour flag is clear",
                    next - nend
                ));
            }
            None
        };
        blocks.push(Sections {
            header_len,
            heights: (off + header_len, hend),
            normals: (hend, nend),
            colour,
        });
    }
    Ok(Layout {
        version,
        flags,
        header_len,
        blocks,
    })
}

#[cfg(test)]
mod tests {
    /// The staged instance header, verbatim: these are the bytes of
    /// `a37.trn2`'s first 96 bytes, and the test asserts what the doc comment
    /// above claims about them.
    fn a37_header() -> Vec<u8> {
        let mut b = vec![0u8; 96];
        b[0..4].copy_from_slice(&0x5452_4e32u32.to_le_bytes());
        for (offset, value) in [
            (4usize, 1u32),
            (8, 6144.0f32.to_bits()),
            (12, 3072.0f32.to_bits()),
            (16, 64.0f32.to_bits()),
            (20, 32),
            (24, 4608),
            (28, 48),
            (32, 96),
            (68, 2.0f32.to_bits()),
            (76, 1),
            (80, 3282),
        ] {
            b[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        b
    }

    #[test]
    fn trn2_header_has_no_world_transform() {
        let header = trn2_header(&a37_header()).unwrap();
        assert_eq!(header.width, 6144.0);
        assert_eq!(header.length, 3072.0);
        assert_eq!((header.rows, header.columns, header.blocks), (48, 96, 4608));
        assert_eq!(header.grid, 32);
        // The header describes the map's own extent and nothing else: the tail
        // words are zero, so there is no matrix hidden in it. The frame is
        // centred on the origin, which is measured on the neighbouring
        // `world.ecwld` rather than read out of this header -- see
        // `contains_world_point`.
        assert_eq!(header.width * header.length, 6144.0 * 3072.0);
        // Inside, on both sides of the origin.
        assert!(contains_world_point(&header, 3070.3, 1500.0));
        assert!(contains_world_point(&header, -3070.3, -1500.0));
        assert!(contains_world_point(&header, 0.0, 0.0));
        assert!(
            contains_world_point(&header, -3072.0, -1536.0),
            "the corner is inside"
        );
        // Outside by a hair, in each direction.
        assert!(!contains_world_point(&header, 3072.1, 0.0));
        assert!(!contains_world_point(&header, 0.0, -1536.1));
        // The mistake the old corner-anchored version made: a point a precinct
        // polygon really names on the neighbouring world must not read as
        // "outside this map's extent".
        assert!(
            contains_world_point(&header, -1936.0, -428.3),
            "a37's own placement"
        );
        let bytes = a37_header();
        assert_eq!(
            &bytes[84..96],
            &[0u8; 12],
            "nothing follows the loader offset"
        );
    }

    #[test]
    fn trn2_header_rejects_nonsense() {
        let mut b = a37_header();
        b[0] = 0;
        assert!(trn2_header(&b).is_err());
        let mut b = a37_header();
        b[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(trn2_header(&b).unwrap_err().contains("version 2"));
        let mut b = a37_header();
        b[28..32].copy_from_slice(&47u32.to_le_bytes());
        assert!(trn2_header(&b).is_err(), "rows x columns must be the count");
        let mut b = a37_header();
        b[8..12].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
        assert!(trn2_header(&b).is_err());
    }

    use super::*;

    /// Build a synthetic T2BK with uncompressible filler of the given sizes.
    fn build(grid: usize, blocks: usize, flags: u8, colour_len: Option<usize>) -> Vec<u8> {
        let nv = (grid + 1) * (grid + 1);
        let header_len = 32usize;
        let table_end = 16 + blocks * 4;
        let mut out = vec![0u8; table_end];
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..8].copy_from_slice(&5u32.to_le_bytes());
        out[8..12].copy_from_slice(&(blocks as u32).to_le_bytes());
        out[12] = flags;
        let mut pos = table_end;
        for bi in 0..blocks {
            out[16 + bi * 4..20 + bi * 4].copy_from_slice(&(pos as u32).to_le_bytes());
            let hs = nv * 2;
            let ns = nv;
            let mut head = vec![0u8; header_len];
            head[16..20].copy_from_slice(&(hs as u32).to_le_bytes());
            head[20..24].copy_from_slice(&(ns as u32).to_le_bytes());
            out.extend_from_slice(&head);
            out.extend(std::iter::repeat(0xAA).take(hs + ns));
            if let Some(cl) = colour_len {
                out.extend(std::iter::repeat(0xBB).take(cl));
            }
            pos = out.len();
        }
        out
    }

    #[test]
    fn the_retail_flag_byte_is_fully_defined() {
        // world_39.t2bk header byte 12.
        let flags = 0x35u8;
        assert_eq!(flags & !KNOWN_FLAGS, 0);
        assert_eq!(flags & FLAG_VERTEX_COLOUR, FLAG_VERTEX_COLOUR);
    }

    #[test]
    fn an_undefined_flag_bit_is_an_explicit_error() {
        let mut d = build(4, 2, 0x4, None);
        d[12] = 0x4 | 0x2; // bit 0x2 is not part of the format
        let err = layout(&d, 4).unwrap_err();
        assert!(err.contains("undefined bit"), "{err}");
    }

    #[test]
    fn the_colour_range_runs_from_the_normals_to_the_next_block() {
        let nv = 25usize;
        let d = build(4, 3, 0x35, Some(nv * 4 - 8));
        let l = layout(&d, 4).unwrap();
        assert_eq!(l.blocks.len(), 3);
        for b in &l.blocks {
            let (cs, ce) = b.colour.expect("colour section");
            assert_eq!(ce - cs, nv * 4 - 8);
            // Follows the normals with no padding and precedes the next block.
            assert_eq!(b.normals.1, cs);
        }
    }

    #[test]
    fn spans_cover_the_file_exactly_once_the_colour_section_is_counted() {
        let nv = 25usize;
        let d = build(4, 3, 0x35, Some(nv * 4 - 8));
        let l = layout(&d, 4).unwrap();
        let total: usize = l
            .blocks
            .iter()
            .map(|b| {
                let end = b.colour.map(|c| c.1).unwrap_or(b.normals.1);
                end - b.heights.0 + b.header_len
            })
            .sum();
        assert_eq!(total, d.len() - (16 + 3 * 4));
    }

    #[test]
    fn a_trailing_section_without_the_flag_is_an_error() {
        // The defect behind the missing section: bytes past the normals that the
        // flag says nothing about used to be silently skipped.
        let mut d = build(4, 2, 0x4, Some(40));
        d[12] = 0x4;
        let err = layout(&d, 4).unwrap_err();
        assert!(err.contains("trailing bytes"), "{err}");
    }

    #[test]
    fn a_colour_section_larger_than_its_inflated_size_is_accepted() {
        // Blocks 152 and 168 of the retail world_39.t2bk are exactly this: more
        // compressed bytes than the 4356 the grid inflates to. Only the inflated
        // length proves the section valid, so layout() must not bound the input.
        let nv = 25usize;
        let d = build(4, 2, 0x35, Some(nv * 4 + 322));
        let l = layout(&d, 4).unwrap();
        for b in &l.blocks {
            let (cs, ce) = b.colour.expect("colour section");
            assert_eq!(ce - cs, nv * 4 + 322);
        }
    }

    #[test]
    fn an_empty_colour_section_is_an_error() {
        let d = build(4, 2, 0x35, None);
        let err = layout(&d, 4).unwrap_err();
        assert!(err.contains("empty colour section"), "{err}");
    }

    #[test]
    fn sections_are_contiguous_and_inside_their_block() {
        let nv = 25usize;
        let d = build(4, 2, 0x35, Some(nv * 4 - 8));
        let l = layout(&d, 4).unwrap();
        for b in &l.blocks {
            assert_eq!(b.heights.1, b.normals.0);
            if let Some((s, e)) = b.colour {
                assert_eq!(s, b.normals.1);
                assert!(e > s);
            }
        }
    }

    #[test]
    fn bad_magic_and_versions_are_rejected() {
        let mut d = build(4, 1, 0x35, Some(90));
        d[0] = 0;
        assert!(layout(&d, 4).unwrap_err().contains("invalid T2BK"));
        let mut d = build(4, 1, 0x35, Some(90));
        d[4] = 9;
        assert!(layout(&d, 4).unwrap_err().contains("version"));
    }

    #[test]
    fn a_block_overrunning_the_next_offset_is_an_error() {
        let nv = 25usize;
        let mut d = build(4, 2, 0x35, Some(nv * 4 - 8));
        // Claim the first block's normals run to the end of the file.
        let off0 = u32at(&d, 16).unwrap() as usize;
        d[off0 + 20..off0 + 24].copy_from_slice(&(9999u32).to_le_bytes());
        let err = layout(&d, 4).unwrap_err();
        assert!(err.contains("past the next block"), "{err}");
    }
}
