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
