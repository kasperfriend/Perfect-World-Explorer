//! Rectangle packing for native-size scenery textures in 4096² GPU pages.
//! The 128-pixel grid avoids reducing large source textures to fixed 128² slots.
pub const SIDE: usize = 4096;
pub const UNIT: usize = 128;
const GRID: usize = SIDE / UNIT;

/// FNV-1a signature of an atlas *layout*: which texture owns which rectangle.
///
/// Both atlases in this viewer — the 4096² grass atlas on the world texture and
/// the 4096-wide page atlases of the streamed scenery — are rebuilt by the
/// background scene streamer for every block-window update, but their pixels are
/// a pure function of the layout (the packer is deterministic and the sources are
/// archive members). A layout that hashes the same therefore describes bytes the
/// renderer already has, which is what lets an update skip re-uploading them.
///
/// This is not a content hash and does not claim to be one: the caller only
/// compares keys that came from the same deterministic build path.
#[derive(Clone, Copy, Debug)]
pub struct LayoutKey(u64);

impl Default for LayoutKey {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutKey {
    /// FNV-1a 64-bit offset basis.
    pub fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub fn byte(&mut self, value: u8) {
        self.0 ^= value as u64;
        self.0 = self.0.wrapping_mul(0x100_0000_01b3);
    }

    pub fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    pub fn usize(&mut self, value: usize) {
        self.bytes(&(value as u64).to_le_bytes());
    }

    /// Add one texture's atlas rectangle, in packing order.
    pub fn slot(&mut self, name: &str, slot: Slot) {
        self.bytes(name.as_bytes());
        self.byte(0xff);
        self.usize(slot.x);
        self.usize(slot.y);
        self.usize(slot.w);
        self.usize(slot.h);
    }

    pub fn value(&self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}
pub struct Atlas {
    used: [u32; GRID],
    free: usize,
}
const _: () = assert!(GRID == u32::BITS as usize);
impl Atlas {
    pub fn new() -> Self {
        Self {
            used: [0; GRID],
            free: GRID * GRID,
        }
    }
    /// Allocate a native resolution rectangle if possible. `reserve` leaves at
    /// least one minimum slot per other texture; when needed, reduce this one.
    pub fn allocate(
        &mut self,
        width: usize,
        height: usize,
        reserve: usize,
        allow_reduce: bool,
    ) -> Option<Slot> {
        let mut w = width.clamp(UNIT, SIDE).next_power_of_two() / UNIT;
        let mut h = height.clamp(UNIT, SIDE).next_power_of_two() / UNIT;
        loop {
            if w * h <= self.free.saturating_sub(reserve) {
                for y in 0..=GRID - h {
                    // A set bit is free across every row of this rectangle.
                    let mut starts = !self.used[y..y + h].iter().fold(0, |a, &b| a | b);
                    // Widths are powers of two: each intersection doubles the
                    // contiguous run length represented by each surviving bit.
                    let mut run = 1;
                    while run < w && starts != 0 {
                        starts &= starts >> run;
                        run *= 2;
                    }
                    if starts == 0 {
                        continue;
                    }
                    let x = starts.trailing_zeros() as usize;
                    let mask = (u32::MAX >> (GRID - w)) << x;
                    for row in &mut self.used[y..y + h] {
                        *row |= mask;
                    }
                    self.free -= w * h;
                    return Some(Slot {
                        x: x * UNIT,
                        y: y * UNIT,
                        w: w * UNIT,
                        h: h * UNIT,
                    });
                }
            }
            if !allow_reduce || (w == 1 && h == 1) {
                return None;
            }
            if w >= h && w > 1 {
                w /= 2
            } else if h > 1 {
                h /= 2
            } else {
                w /= 2
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packs_rectangles_without_overlap() {
        let mut a = Atlas::new();
        let p = a.allocate(512, 256, 0, false).unwrap();
        let q = a.allocate(128, 512, 0, false).unwrap();
        assert!(p.x + p.w <= q.x || q.x + q.w <= p.x || p.y + p.h <= q.y || q.y + q.h <= p.y);
        assert_eq!((p.w, p.h), (512, 256));
    }
    #[test]
    fn reserve_prevents_starving_other_textures() {
        let mut a = Atlas::new();
        let p = a.allocate(4096, 4096, 1023, true).unwrap();
        assert_eq!((p.w, p.h), (128, 128));
        for _ in 0..1023 {
            assert!(a.allocate(128, 128, 0, false).is_some());
        }
        assert!(a.allocate(128, 128, 0, false).is_none());
    }
    #[test]
    fn layout_key_is_stable_and_separates_every_layout_field() {
        let slot = Slot {
            x: 128,
            y: 256,
            w: 512,
            h: 128,
        };
        let key = |name: &str, slot: Slot| {
            let mut key = LayoutKey::new();
            key.slot(name, slot);
            key.value()
        };
        assert_eq!(key("grass/a.dds", slot), key("grass/a.dds", slot));
        assert_ne!(key("grass/a.dds", slot), key("grass/b.dds", slot));
        for slot_change in [
            Slot { x: 0, ..slot },
            Slot { y: 0, ..slot },
            Slot { w: 128, ..slot },
            Slot { h: 512, ..slot },
        ] {
            assert_ne!(
                key("grass/a.dds", slot),
                key("grass/a.dds", slot_change),
                "every rectangle field participates in the key"
            );
        }
        // Packing order is part of the layout: two swaps must not collide.
        let two = |first: &str, second: &str| {
            let mut key = LayoutKey::new();
            key.slot(first, slot);
            key.slot(second, slot);
            key.value()
        };
        assert_ne!(two("a", "b"), two("b", "a"));
        assert_ne!(LayoutKey::new().value(), key("grass/a.dds", slot));
    }
}
