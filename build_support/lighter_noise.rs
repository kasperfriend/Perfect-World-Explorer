//! Deterministic toroidal void-and-cluster rank tile, built alongside WGSL.
use std::{fmt::Write, path::Path};

const SIDE: usize = 16;
const N: usize = SIDE * SIDE;

type Mask = [bool; N];
struct Tile {
    mask: Mask,
    density: [i32; N],
}

fn metric(a: usize, b: usize) -> i32 {
    let torus = |x: usize| x.min(SIDE - x);
    let dx = torus((a % SIDE).abs_diff(b % SIDE));
    let dy = torus((a / SIDE).abs_diff(b / SIDE));
    let d2 = dx * dx + dy * dy;
    // Radially decaying integer kernel. No host float/libm dependency.
    if d2 > 40 {
        0
    } else {
        8192 >> (d2 / 3)
    }
}

impl Tile {
    fn new() -> Self {
        Self {
            mask: [false; N],
            density: [0; N],
        }
    }
    fn set(&mut self, at: usize, active: bool) {
        assert_ne!(self.mask[at], active);
        self.mask[at] = active;
        let sign = if active { 1 } else { -1 };
        for (i, d) in self.density.iter_mut().enumerate() {
            *d += sign * metric(at, i);
        }
    }
    fn cluster(&self) -> usize {
        (0..N)
            .filter(|&i| self.mask[i])
            .max_by_key(|&i| self.density[i])
            .unwrap()
    }
    fn void(&self) -> usize {
        (0..N)
            .filter(|&i| !self.mask[i])
            .min_by_key(|&i| self.density[i])
            .unwrap()
    }
}

pub fn emit(out: &Path) {
    let mut indices: Vec<_> = (0..N).collect();
    indices.sort_by_key(|&i| {
        let mut hash = i as u32 ^ 0x248ca42f;
        hash = (hash ^ (hash >> 16)).wrapping_mul(0x7feb352d);
        hash = (hash ^ (hash >> 15)).wrapping_mul(0x846ca68b);
        hash ^ (hash >> 16)
    });
    let mut tile = Tile::new();
    for i in indices.into_iter().take(N / 4) {
        tile.set(i, true);
    }
    // Strict decrease of pairwise energy prevents equal-density cycling.
    for _ in 0..(N * 16) {
        let dense = tile.cluster();
        tile.set(dense, false);
        let empty = tile.void();
        if tile.density[empty] >= tile.density[dense] {
            tile.set(dense, true);
            break;
        }
        tile.set(empty, true);
    }
    let start = tile.mask;
    let mut ranks = [0; N];
    for rank in (0..N / 4).rev() {
        let dense = tile.cluster();
        ranks[dense] = rank;
        tile.set(dense, false);
    }
    for (i, active) in start.into_iter().enumerate() {
        if active {
            tile.set(i, true);
        }
    }
    for rank in N / 4..N / 2 {
        let empty = tile.void();
        ranks[empty] = rank;
        tile.set(empty, true);
    }
    let filled = tile.mask;
    let mut holes = Tile::new();
    for (i, active) in filled.into_iter().enumerate() {
        if !active {
            holes.set(i, true);
        }
    }
    for rank in N / 2..N {
        let dense = holes.cluster();
        ranks[dense] = rank;
        holes.set(dense, false);
    }
    let mut wgsl = format!("const BLUE_RANK: array<u32,{N}> = array<u32,{N}>(\n");
    for row in ranks.chunks(SIDE) {
        for rank in row {
            write!(&mut wgsl, "{rank}u,").unwrap();
        }
        wgsl.push('\n');
    }
    wgsl.push_str(");\n");
    let mut ordered = ranks;
    ordered.sort_unstable();
    assert!(ordered.into_iter().eq(0..N));
    std::fs::write(out.join("lighter-blue-noise.wgsl"), wgsl).unwrap();
}
