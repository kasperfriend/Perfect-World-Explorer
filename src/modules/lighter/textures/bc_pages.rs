//! Optional immutable terrain page plans. Pack block rectangles, never
//! decoded pixels. A tile is admitted whole or falls back to buffer sampling.
use super::{extent_at, Textures, Tile, BC_TERRAIN_1};

pub(in crate::modules::lighter) const SIDE: u32 = 256;
const BLOCKS: usize = SIDE as usize / 4;
const LAYERS: usize = 64;
const PAGED_BC: u32 = 1 << 8;

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
            rows: self
                .bc_pages
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
        let pages = Arc::make_mut(self.bc_pages.get_or_insert_with(Arc::default));
        pages.rows = trial.rows;
        pages.copies.extend(trial.copies);
        pages.tiles += 1;
        let mut stats = tile.1;
        stats.bc_terrain_saved_bytes = stats.bc_terrain_saved_bytes.saturating_sub((n + pad) * 4);
        self.mips.add(stats);
        Ok(addr as u32)
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
mod tests {
    use super::*;
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
