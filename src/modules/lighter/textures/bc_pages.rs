//! Optional immutable sampled page plans. Copies are block rectangles. A
//! tile is admitted whole or falls back to the portable buffer sampler.
use super::{extent_at, Textures, Tile, BC_TERRAIN_1};

pub(in crate::modules::lighter) const SIDE: u32 = 256;
const BLOCKS: usize = SIDE as usize / 4;
const LAYERS: usize = 64;
const PAGED_BC: u32 = 1 << 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::modules::lighter) enum Kind {
    Terrain,
    Normal,
    Color,
}

impl Kind {
    pub fn index(self) -> usize {
        self as usize
    }
}

pub(in crate::modules::lighter) type Plans = [Option<std::sync::Arc<Pages>>; 3];
pub(in crate::modules::lighter) const KINDS: [Kind; 3] = [Kind::Terrain, Kind::Normal, Kind::Color];

#[derive(Clone, Copy)]
pub(in crate::modules::lighter) struct Region {
    pub offset: usize,
    pub origin: [u32; 3],
    pub extent: [u32; 2],
}

#[derive(Clone, Default)]
pub(in crate::modules::lighter) struct Pages {
    rows: Vec<[u8; BLOCKS]>, // rightmost used block of each row, per layer
    pub copies: Vec<Region>,
    pub tiles: usize,
}

impl Pages {
    pub fn layers(&self) -> u32 {
        self.rows.len() as u32
    }

    fn place(&mut self, [w, h]: [u32; 2]) -> Option<[u32; 3]> {
        let [w, h] = [w.div_ceil(4) as usize, h.div_ceil(4) as usize];
        if w > BLOCKS || h > BLOCKS {
            return None;
        }
        for layer in 0..=self.rows.len().min(LAYERS - 1) {
            if layer == self.rows.len() {
                self.rows.push([0; BLOCKS]);
            }
            let rows = &mut self.rows[layer];
            // Prefer the existing layer. Mip tails often fit in the unused
            // block rows of an NPOT texture and need no extra page.
            if let Some((y, x)) = (0..=BLOCKS - h)
                .filter_map(|y| {
                    let x = *rows[y..y + h].iter().max().unwrap() as usize;
                    (x + w <= BLOCKS).then_some((y, x))
                })
                .min_by_key(|&(y, x)| (x, y))
            {
                rows[y..y + h].fill((x + w) as u8);
                return Some([4 * x as u32, 4 * y as u32, layer as u32]);
            }
        }
        None
    }
}

impl Textures {
    pub(crate) fn insert_paged_terrain(&mut self, tile: &Tile) -> Result<u32, String> {
        use std::sync::Arc;
        let n = tile.0[2] as usize;
        assert_eq!(tile.0[3], BC_TERRAIN_1);
        let size = [tile.0[0], tile.0[1]];
        // Copy only the small placement cursors until every mip fits.
        let mut trial = Pages {
            rows: self.bc_pages[Kind::Terrain.index()]
                .as_ref()
                .map_or_else(Vec::new, |p| p.rows.clone()),
            ..Pages::default()
        };
        let mut placements = Vec::with_capacity(n);
        for lod in 0..n {
            let size = extent_at(size, lod as u32);
            let Some(at) = trial.place(size) else {
                return self.insert(tile);
            };
            placements.push((at, size.map(|n| n.div_ceil(4) * 4)));
        }
        // VkBufferImageCopy's source must be block-size aligned. Retain the
        // same compressed words as the portable fallback; no host mirror or
        // separate upload is needed to initialize the image.
        let len = self.byte_len() / 4;
        let pad = (len + 4 + 2 * n) % 2;
        let words = tile.0.len() + n + pad;
        if words * 4 > self.remaining_bytes() {
            return self.insert(tile);
        }
        let addr = self.reserve(words)? + pad;
        self.write_words(addr, &[size[0], size[1], n as u32, BC_TERRAIN_1 | PAGED_BC]);
        self.write_words(addr + 4 + 2 * n, &tile.0[4 + n..]);
        for (i, ([x, y, layer], extent)) in placements.into_iter().enumerate() {
            let offset = tile.0[4 + i] + n as u32;
            self.write_words(addr + 4 + i, &[offset]);
            self.write_words(addr + 4 + n + i, &[x | (y << 8) | (layer << 16)]);
            trial.copies.push(Region {
                offset: (addr + offset as usize) * 4,
                origin: [x, y, layer],
                extent,
            });
        }
        let pages =
            Arc::make_mut(self.bc_pages[Kind::Terrain.index()].get_or_insert_with(Arc::default));
        pages.rows = trial.rows;
        pages.copies.extend(trial.copies);
        pages.tiles += 1;
        let mut stats = tile.1;
        stats.bc_terrain_saved_bytes = stats.bc_terrain_saved_bytes.saturating_sub((n + pad) * 4);
        self.mips.add(stats);
        Ok(addr as u32)
    }

    /// At world startup cache a small authored material set. Keep a decoded
    /// tile for Software, unsupported formats and failed Vulkan admission.
    /// Only authored levels with the same logical extents can take this path;
    /// generated/coverage-adjusted mips must use the portable buffer bytes.
    pub(crate) fn insert_paged_material(
        &mut self,
        tile: &Tile,
        raw: &[u8],
        budget: usize,
    ) -> Result<Option<u32>, String> {
        use crate::model_dds::{Dds, Format};
        use std::sync::Arc;
        let [w, h, count, flags] = tile.0[..4] else {
            unreachable!("tile header")
        };
        if tile.1.generated != 0 || count == 0 || tile.bytes() > budget {
            return Ok(None);
        }
        let Ok(dds) = Dds::parse(raw) else {
            return Ok(None);
        };
        let kind = match dds.format {
            Format::Bc5 if flags == super::LINEAR_DATA | super::NORMAL_RG => Kind::Normal,
            // These are opacity-bearing BC7 BMD materials. An opaque-colour
            // tile with alpha forced to one isn't interchangeable with raw DDS.
            Format::Bc7 if flags == 0 => Kind::Color,
            _ => return Ok(None),
        };
        let n = count as usize;
        let Some(start) = dds.mips.iter().position(|m| [m.width, m.height] == [w, h]) else {
            return Ok(None);
        };
        let Some(mips) = dds.mips.get(start..start + n) else {
            return Ok(None);
        };
        let mut trial = Pages {
            rows: self.bc_pages[kind.index()]
                .as_ref()
                .map_or_else(Vec::new, |p| p.rows.clone()),
            ..Pages::default()
        };
        let mut placements = Vec::with_capacity(n);
        for (lod, m) in mips.iter().enumerate() {
            let dims = [m.width, m.height];
            if dims != extent_at([w, h], lod as u32) {
                return Ok(None);
            }
            let Some(at) = trial.place(dims) else {
                return Ok(None);
            };
            placements.push((at, dims.map(|s| s.div_ceil(4) * 4)));
        }
        let base_words = tile.0.len() + n;
        // BC5/BC7 VkBufferImageCopy offsets need 16-byte alignment. Suffix
        // offsets for the decoded tile are rebased by the small page table.
        let pad = (4 - (self.byte_len() / 4 + base_words) % 4) % 4;
        let words = base_words + pad + mips.iter().map(|m| m.bytes.len() / 4).sum::<usize>();
        if words * 4 > budget.min(self.remaining_bytes()) {
            return Ok(None);
        }
        let addr = self.reserve(words)?;
        self.write_words(
            addr,
            &[w, h, count, flags | PAGED_BC | (kind.index() as u32) << 9],
        );
        self.write_words(addr + 4 + 2 * n, &tile.0[4 + n..]);
        let mut compressed = addr + base_words + pad;
        for (lod, (mip, ([x, y, layer], extent))) in mips.iter().zip(placements).enumerate() {
            self.write_words(addr + 4 + lod, &[tile.0[4 + lod] + n as u32]);
            self.write_words(addr + 4 + n + lod, &[x | (y << 8) | (layer << 16)]);
            let blocks: Vec<_> = mip
                .bytes
                .chunks_exact(4)
                .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
                .collect();
            self.write_words(compressed, &blocks);
            trial.copies.push(Region {
                offset: compressed * 4,
                origin: [x, y, layer],
                extent,
            });
            compressed += blocks.len();
        }
        let pages = Arc::make_mut(self.bc_pages[kind.index()].get_or_insert_with(Arc::default));
        pages.rows = trial.rows;
        pages.copies.extend(trial.copies);
        pages.tiles += 1;
        self.mips.add(tile.1);
        Ok(Some(addr as u32))
    }
}

/// BC2/3 always use four colors. On opaque terrain their separate alpha
/// plane is unused. Reorder endpoints and toggle the low selector bit to
/// preserve the same RGB palette in BC1's four-color mode.
pub(super) fn four_color(words: &[u8]) -> [u32; 2] {
    let end = u32::from_le_bytes(words[..4].try_into().unwrap());
    let sel = u32::from_le_bytes(words[4..8].try_into().unwrap());
    if end as u16 > (end >> 16) as u16 {
        [end, sel]
    } else if end as u16 == (end >> 16) as u16 {
        [end, 0]
    } else {
        [end.rotate_left(16), sel ^ 0x5555_5555]
    }
}

#[cfg(test)]
pub(in crate::modules::lighter) fn material_fixture(kind: Kind) -> (Vec<u8>, Tile) {
    let mut raw = vec![0; 148];
    raw[..4].copy_from_slice(b"DDS ");
    raw[84..88].copy_from_slice(b"DX10");
    let (dxgi, filter) = match kind {
        Kind::Normal => (83u32, super::Filter::NormalRg),
        Kind::Color => (99u32, super::Filter::Alpha),
        Kind::Terrain => unreachable!(),
    };
    // NPOT source: its 7x5 base needs four blocks; its 3x2 tail needs one.
    for (i, n) in [
        (4, 124u32),
        (12, 5),
        (16, 7),
        (28, 2),
        (76, 32),
        (80, 4),
        (128, dxgi),
        (132, 3),
        (140, 1),
    ] {
        raw[i..i + 4].copy_from_slice(&n.to_le_bytes());
    }
    for i in 0..5 {
        if kind == Kind::Normal {
            raw.extend([
                160 + i * 8,
                160 + i * 8,
                0,
                0,
                0,
                0,
                0,
                0,
                128,
                128,
                0,
                0,
                0,
                0,
                0,
                0,
            ]);
        } else {
            // BC7 mode 6, equal endpoints per block, different sRGB and
            // straight alpha per block. Each channel uses an even endpoint.
            let mut block = 1u128 << 6;
            let mut shift = 7;
            for v in [
                230 - i * 36,
                70 + i * 30,
                126,
                if i % 2 == 0 { 210 } else { 138 },
            ] {
                for _ in 0..2 {
                    block |= u128::from(v / 2) << shift;
                    shift += 7;
                }
            }
            raw.extend(block.to_le_bytes());
        }
    }
    let (images, _) = crate::model_texture_gpu::decode_for_rays(&raw).unwrap();
    let tile = Tile::images(&images, 8, filter).unwrap();
    (raw, tile)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn material_pages_keep_authored_mips_and_a_byte_exact_buffer_fallback() {
        let mut bank = Textures::default();
        for kind in [Kind::Normal, Kind::Color] {
            let (raw, tile) = material_fixture(kind);
            assert!(bank
                .insert_paged_material(&tile, &raw, tile.bytes())
                .unwrap()
                .is_none());
            assert!(
                bank.bc_pages[kind.index()].is_none(),
                "failed reservation left a partial plan"
            );
            let addr = bank
                .insert_paged_material(&tile, &raw, 1 << 20)
                .unwrap()
                .unwrap();
            let words = bank.tile_words(addr);
            let pages = bank.bc_pages[kind.index()].as_ref().unwrap();
            for (i, copy) in pages.copies.iter().enumerate() {
                assert_eq!(copy.offset % 16, 0);
                assert_eq!(copy.extent, if i == 0 { [8, 8] } else { [4, 4] });
                let n = (tile.0[0] >> i) * (tile.0[1] >> i);
                let (a, b) = (tile.0[4 + i] as usize, words[4 + i] as usize);
                assert_eq!(tile.0[a..a + n as usize], words[b..b + n as usize]);
            }
            assert_eq!(pages.tiles, 1);
        }
        bank.seal();
        let fork = bank.fork_prefix();
        for kind in [Kind::Normal, Kind::Color] {
            let slot = kind.index();
            assert!(std::sync::Arc::ptr_eq(
                bank.bc_pages[slot].as_ref().unwrap(),
                fork.bc_pages[slot].as_ref().unwrap()
            ));
        }
    }

    #[test]
    fn fills_pages_without_overlapping_block_regions_and_stops_at_cap() {
        let mut p = Pages::default();
        let mut occupied = std::collections::HashSet::new();
        for _ in 0..4 {
            for size in [[33, 19], [16, 9], [8, 4], [4, 2], [2, 1], [1, 1]] {
                let [x, y, l] = p.place(size).unwrap();
                assert_eq!((x % 4, y % 4), (0, 0));
                for by in y / 4..y / 4 + size[1].div_ceil(4) {
                    for bx in x / 4..x / 4 + size[0].div_ceil(4) {
                        assert!(occupied.insert((bx, by, l)));
                    }
                }
            }
        }
        assert_eq!(p.layers(), 1);
        for _ in 1..LAYERS {
            assert!(p.place([SIDE; 2]).is_some());
        }
        assert!(p.place([SIDE; 2]).is_none());
        assert!(p.place([SIDE + 1, SIDE]).is_none());
    }
}
