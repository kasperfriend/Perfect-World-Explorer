//! Terrain block window, resolution levels and block-edge rule.
//!
//! Transcribed from `elementclient.exe` 1.4.2 (PE i386, image base 0x400000,
//! md5 5f02895688fa580506e869156340a664). Every rule below names the native
//! code it came from; nothing here is a viewer approximation.
//!
//! * Block grid: `world.trn2` header dword @20 is the cell count of one T2BK
//!   block quadrant (`grid`, 32 for the shipped world). A block therefore spans
//!   `grid * grid_spacing` world units (32 * 2.0 = 64) on both axes.
//! * Window: `A3DTerrain2` camera block window, `0x77ecf0`
//!   (`1/zoom` = 1/block-size, `ceil(range * 1/zoom)` per side, `(2r+1)` square
//!   clamped to the block array) driven from the frame update `0x77e1b0`.
//! * Levels: `0x77e360` per block; distance from the camera to the block centre
//!   in XZ, minus the block's half diagonal (`0x8f0a5c` = 0.7071067), compared
//!   against two thresholds (`terrain+0x26c`/`+0x270`, 180/320 world units for
//!   the shipped quality preset, set by `0x77f450` from `[CECWorld+0x18]+0x60/
//!   +0x64`) scaled per block by `block+0x7c`.
//! * Level geometry: `0x7e5f70`/`0x7e6070` build one index grid per level with
//!   `step = 1 << (3 - level)` and `q = (cells - 2*step)/step` interior cells,
//!   i.e. the interior mesh spans grid coordinates `step ..= cells-step`.
//! * Edges: `0x7c40e6` compares a block level with each of its four neighbour
//!   block levels (neighbour pointers filled by the same `0x77e360` sweep at
//!   `block+0xc4..0xd0`). Only a *coarser* neighbour coarsens the edge, by
//!   `own - neighbour` doubling steps (never more than the level span), and a
//!   missing/unloaded neighbour leaves the edge at the block's own step.

/// Cells per block edge. `world.trn2` @20; 32 for the shipped world.
pub const CELLS: usize = 32;
/// Highest resolution level. Levels are 1 (coarsest) ..= 3 (full resolution).
pub const MAX_LEVEL: u8 = 3;
/// `0x8f0a5c`: half the diagonal of a unit square, used to shrink the
/// camera-to-block-centre distance so that the level depends on the distance to
/// the block, not to its middle.
pub const HALF_DIAGONAL: f32 = 0.707_106_78;

/// Mesh stride of a level in cells: 1, 2 or 4 (`0x7e5f70`, `1 << (3 - level)`).
pub fn step(level: u8) -> usize {
    1 << (MAX_LEVEL - level.clamp(1, MAX_LEVEL))
}

/// Interior cell count per edge: `(cells - 2*step)/step` (`0x7e5f70`).
/// Level 3 -> 30, level 2 -> 14, level 1 -> 6; the remainder is the edge ring.
pub fn interior_cells(level: u8) -> usize {
    let s = step(level);
    (CELLS - 2 * s) / s
}

/// The level ladder. `near`/`far` are `A3DTerrain2+0x26c` / `+0x270`, both in
/// world units, written by `0x77f450` from the render settings object
/// (`[CECWorld+0x18]+0x60/+0x64`, quality preset `0x40e3e0`): 40/80, 120/240 or
/// 180/320 for quality 0, 1 and >=2. `0x77f450` stores the first value into
/// `+0x26c` and the larger of the two into `+0x270`. `0x77e360` multiplies each
/// threshold by the per-block scale `block+0x7c` (1.0 for shipped blocks) and
/// compares it against the camera distance with the block's half diagonal
/// removed (`world+0x1c8 = block_size * 0.7071067`, `0x8f0a5c`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodConfig {
    /// World size of one block (`world+0x1c4`, `TRN2 grid * grid_spacing`).
    pub block_size: f32,
    /// Level 3 distance (`world+0x26c`), world units.
    pub near: f32,
    /// Level 2 distance (`world+0x270`), world units.
    pub far: f32,
    /// Per-block scale `block+0x7c`; 1.0 for the shipped blocks.
    pub block_scale: f32,
}

impl Default for LodConfig {
    fn default() -> Self {
        // Shipped quality preset >= 2: 180 / 320 world units.
        Self {
            block_size: 64.0,
            near: 180.0,
            far: 320.0,
            block_scale: 1.0,
        }
    }
}

impl LodConfig {
    /// Distance from the camera to a block whose centre is `centre` in XZ,
    /// as the client measures it (`0x77e443..0x77e461`).
    pub fn block_distance(&self, cam: [f32; 2], centre: [f32; 2]) -> f32 {
        let dx = centre[0] - cam[0];
        let dz = centre[1] - cam[1];
        (dx * dx + dz * dz).sqrt() - self.block_size * HALF_DIAGONAL
    }
    /// Level for a block at that distance. There is no hysteresis band: the
    /// client re-evaluates the two comparisons every frame (`0x77e467..0x77e4a3`)
    /// and switches exactly at the threshold.
    pub fn level(&self, distance: f32) -> u8 {
        if distance < self.near * self.block_scale {
            3
        } else if distance < self.far * self.block_scale {
            2
        } else {
            1
        }
    }
    /// Level for a block centred at `centre` in world XZ. Block availability
    /// (`block+0x74`) is the caller's business; see `window_lod`.
    pub fn level_at(&self, cam: [f32; 2], centre: [f32; 2]) -> u8 {
        let dx = centre[0] - cam[0];
        let dz = centre[1] - cam[1];
        let dist_sq = dx * dx + dz * dz;
        let offset = self.block_size * HALF_DIAGONAL;
        let near_lim = self.near * self.block_scale + offset;
        if near_lim > 0.0 && dist_sq < near_lim * near_lim {
            3
        } else {
            let far_lim = self.far * self.block_scale + offset;
            if far_lim > 0.0 && dist_sq < far_lim * far_lim {
                2
            } else {
                1
            }
        }
    }
}

/// Half-open block window `[x0, x1) x [y0, y1)` recovered from `0x77ecf0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Window {
    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
    pub fn blocks(&self) -> usize {
        if self.is_empty() {
            0
        } else {
            (self.x1 - self.x0) * (self.y1 - self.y0)
        }
    }
    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

/// Camera block window. `cell` is the camera position in block units on each
/// axis (`(world - origin) / block_size`), `range` the draw range in world
/// units (`world+0x1bc`, clamped to at least `world+0x1c0` by `0x77d720`).
///
/// Recovered shape: `r = ceil(range / block_size)`, a `(2r+1)` square centred on
/// the camera's block. At a map edge the square is *shifted*, not shrunk: the
/// client keeps `2r+1` blocks and clamps the far side (`0x77ed80..0x77edc2`,
/// first axis limited by `world+0x188`, second by `world+0x184`).
pub fn window(cell: [f32; 2], dims: [usize; 2], range: f32, block_size: f32) -> Window {
    let r = (range / block_size).ceil().max(0.0) as isize;
    let width = 2 * r + 1;
    let centre = [cell[0].floor() as isize, cell[1].floor() as isize];
    let axis = |c: isize, dim: usize| {
        let mut lo = c - r;
        let mut hi = c + r + 1;
        if lo < 0 {
            lo = 0;
            hi = width.min(dim as isize);
        }
        if hi > dim as isize {
            hi = dim as isize;
            lo = (dim as isize - width).max(0);
        }
        (lo.max(0) as usize, hi.max(0) as usize)
    };
    let (x0, x1) = axis(centre[0], dims[0]);
    let (y0, y1) = axis(centre[1], dims[1]);
    Window { x0, y0, x1, y1 }
}

/// Extra coarsening of one block edge, in doublings of the block's own step.
/// `0x7c40e6`: only a strictly coarser neighbour (lower level) coarsens the
/// edge, by `own - neighbour`; a missing/unloaded neighbour (`+0xc4..0xd0 == 0`
/// or `neighbour+0x70 == 0`) leaves the edge untouched.
pub fn edge_case(own: u8, neighbour: Option<u8>) -> u8 {
    match neighbour {
        None => 0,
        Some(other) if other == 0 || other >= own => 0,
        Some(other) => (own - other).min(2),
    }
}

/// Mesh stride along one block edge: own step doubled `case` times. It always
/// equals the coarser of the two neighbouring blocks' steps, which is what keeps
/// the shared edge crack-free.
pub fn edge_stride(level: u8, case: u8) -> usize {
    step(level) << case
}

/// Edge coarsening for the four sides in `N, S, W, E` order (the client runs the
/// same comparison over `block+0xc4..0xd0`, which `0x77e360` fills with the four
/// orthogonal neighbours).
pub fn edge_cases(own: u8, neighbours: [Option<u8>; 4]) -> [u8; 4] {
    std::array::from_fn(|i| edge_case(own, neighbours[i]))
}

/// One block surface: vertices in block-cell coordinates and triangle indices.
#[derive(Debug, Default, Clone)]
pub struct BlockSurface {
    /// Grid coordinates (0..=CELLS) of each vertex.
    pub grid: Vec<[u32; 2]>,
    pub indices: Vec<u32>,
    /// Interior cells (as the native per-level index grid) and edge ring cells.
    pub interior_cells: usize,
    pub ring_cells: usize,
}

impl BlockSurface {
    pub fn triangles(&self) -> impl Iterator<Item = [[u32; 2]; 3]> + '_ {
        self.indices.chunks_exact(3).map(move |t| {
            [
                self.grid[t[0] as usize],
                self.grid[t[1] as usize],
                self.grid[t[2] as usize],
            ]
        })
    }
}

/// Build one block's surface for `level` with per-edge `cases` (`N, S, W, E`).
///
/// The native builder keeps one vertex grid of `(cells+1)²` entries and only
/// changes the index grid (a 16-bit index per cell, `0x7e5f70`), so the coarse
/// levels read the same height grid as the fine ones. The interior covers
/// `step ..= cells-step` at `step` resolution; the four edge rings join the
/// block boundary, whose nodes sit at `edge_stride` spacing, to that interior.
/// The two adjacent rings together fill each corner cell exactly once.
/// Return the immutable, lazily-built surface for a terrain LOD/edge variant.
///
/// There are only 243 possible combinations (three levels and 3^4 edge
/// stitch patterns). Terrain-window rebuilds revisit these combinations for
/// thousands of blocks, so retain each triangulation instead of rebuilding its
/// vertex/index topology on every camera crossing. The cache is process-wide,
/// thread-safe, and initializes each variant only when first requested.
pub fn block_surface_cached(level: u8, cases: [u8; 4]) -> &'static BlockSurface {
    static SURFACES: std::sync::OnceLock<Vec<std::sync::OnceLock<BlockSurface>>> =
        std::sync::OnceLock::new();
    let level = level.clamp(1, MAX_LEVEL);
    let cases = cases.map(|case| case.min(2));
    let slot = (level as usize - 1) * 81
        + cases[0] as usize * 27
        + cases[1] as usize * 9
        + cases[2] as usize * 3
        + cases[3] as usize;
    SURFACES
        .get_or_init(|| (0..243).map(|_| std::sync::OnceLock::new()).collect())[slot]
        .get_or_init(|| build_block_surface(level, cases))
}

/// Owned compatibility wrapper for callers that need to modify or consume a
/// surface. Hot terrain residency code should use block_surface_cached.
pub fn block_surface(level: u8, cases: [u8; 4]) -> BlockSurface {
    block_surface_cached(level, cases).clone()
}

fn build_block_surface(level: u8, cases: [u8; 4]) -> BlockSurface {
    let level = level.clamp(1, MAX_LEVEL);
    let s = step(level);
    let u = CELLS / s; // block edge in step units
    let mut b = SurfaceBuilder {
        s,
        grid: Vec::with_capacity((u + 1) * (u + 1)),
        // The surface's integer grid is dense and bounded (at most 33x33).
        // A direct index table avoids allocating and hashing a HashMap for
        // every block/LOD combination generated during terrain rebuilds.
        index_of: vec![u32::MAX; (u + 1) * (u + 1)],
        indices: Vec::with_capacity(u * u * 6),
    };
    // Interior: `q x q` cells at `step`, spanning 1 ..= u-1 in step units.
    let q = interior_cells(level);
    debug_assert_eq!(q, u - 2);
    for j in 1..u - 1 {
        for i in 1..u - 1 {
            let a = b.v(i, j);
            let c = b.v(i + 1, j);
            let d = b.v(i, j + 1);
            let e = b.v(i + 1, j + 1);
            b.tri(a, c, d);
            b.tri(c, e, d);
        }
    }
    // Edge rings. Each ring joins its boundary line (outer nodes every
    // `stride` steps) to the interior line one step inside, fanning from the
    // boundary node that starts each segment. The wedge at either end of a ring
    // is not emitted: the perpendicular ring of the same block covers exactly
    // that half of the corner cell, which is what keeps the corners gap- and
    // overlap-free (see the coverage tests).
    for side in 0..4usize {
        let m = 1usize << cases[side].min(2);
        let (outer_line, inner_line) = match side {
            0 => (0usize, 1usize), // north: j = 0 / 1
            1 => (u, u - 1),       // south: j = u / u-1
            2 => (0, 1),           // west:  i = 0 / 1
            _ => (u, u - 1),       // east:  i = u / u-1
        };
        let horizontal = side < 2;
        // Outer and inner node accessors in block step units.
        let outer = |t: usize, b: &mut SurfaceBuilder| -> u32 {
            if horizontal {
                b.v(t, outer_line)
            } else {
                b.v(outer_line, t)
            }
        };
        let inner = |t: usize, b: &mut SurfaceBuilder| -> u32 {
            if horizontal {
                b.v(t, inner_line)
            } else {
                b.v(inner_line, t)
            }
        };
        let mut a = 0usize;
        while a < u {
            let e = (a + m).min(u);
            let oa = outer(a, &mut b);
            // Inner nodes shared with the neighbouring segment: both ends are
            // included, except the two corners of the ring (t = 0 and t = u,
            // which belong to the perpendicular rings). Stream the tiny chain
            // directly into the fan instead of allocating a Vec for every ring
            // segment generated during LOD/edge rebuilds.
            let mut previous = None;
            for t in (a..=e).filter(|&t| t >= 1 && t <= u - 1) {
                let current = inner(t, &mut b);
                if let Some(last) = previous {
                    b.tri(oa, last, current);
                }
                previous = Some(current);
            }
            let oe = outer(e, &mut b);
            // At least one interior node remains in every segment.
            let last = previous.expect("ring segment must contain an inner node");
            b.tri(oa, last, oe);
            a = e;
        }
    }
    BlockSurface {
        grid: b.grid,
        indices: b.indices,
        interior_cells: q * q,
        ring_cells: u * u - q * q,
    }
}

struct SurfaceBuilder {
    s: usize,
    grid: Vec<[u32; 2]>,
    index_of: Vec<u32>,
    indices: Vec<u32>,
}

impl SurfaceBuilder {
    /// Vertex for the block-step coordinate `(i, j)`.
    fn v(&mut self, i: usize, j: usize) -> u32 {
        let width = CELLS / self.s + 1;
        let slot = j * width + i;
        let existing = self.index_of[slot];
        if existing != u32::MAX {
            return existing;
        }
        let index = self.grid.len() as u32;
        self.grid.push([(i * self.s) as u32, (j * self.s) as u32]);
        self.index_of[slot] = index;
        index
    }
    fn tri(&mut self, a: u32, b: u32, c: u32) {
        if a != b && b != c && a != c {
            self.indices.extend_from_slice(&[a, b, c]);
        }
    }
}

/// Levels and edge cases for one block window.
#[derive(Debug, Clone)]
pub struct WindowLod {
    pub window: Window,
    /// Level per block in row-major `window` order; `0` means "not drawn".
    pub levels: Vec<u8>,
    /// Edge cases per block in the same order, `N, S, W, E`.
    pub cases: Vec<[u8; 4]>,
}

impl WindowLod {
    /// Viewer-only join to the persistent four-cell far grid. Internal edges
    /// still use the recovered neighbor rule; only the perimeter is matched.
    pub fn stitch_far_boundary(&mut self) {
        let win = self.window;
        for y in win.y0..win.y1 {
            for x in win.x0..win.x1 {
                let i = (y - win.y0) * (win.x1 - win.x0) + x - win.x0;
                let case = self.level(x, y).saturating_sub(1);
                for (side, boundary) in [y == win.y0, y + 1 == win.y1, x == win.x0, x + 1 == win.x1]
                    .into_iter()
                    .enumerate()
                {
                    if boundary {
                        self.cases[i][side] = case;
                    }
                }
            }
        }
    }
    pub fn level(&self, x: usize, y: usize) -> u8 {
        if !self.window.contains(x, y) {
            return 0;
        }
        self.levels[(y - self.window.y0) * (self.window.x1 - self.window.x0) + (x - self.window.x0)]
    }
}

/// Assign levels and edge cases for the blocks of `win`.
///
/// `centre` returns the block centre in world XZ; `visible` mirrors the client's
/// `block+0x74` availability flag. Neighbours outside the window (or invisible)
/// produce no coarsening, exactly like the null neighbour pointers in
/// `0x7c40e6`.
/// The level grid alone: what a caller needs to know whether the drawn mesh
/// would differ. `window_lod` also walks every cell's neighbours to stitch the
/// edge cases, which only matter when the mesh is actually rebuilt.
pub fn window_levels(
    win: Window,
    cfg: &LodConfig,
    cam: [f32; 2],
    centre: impl Fn(usize, usize) -> [f32; 2],
    visible: impl Fn(usize, usize) -> bool,
) -> Vec<u8> {
    let w = win.x1.saturating_sub(win.x0);
    let h = win.y1.saturating_sub(win.y0);
    let mut levels = vec![0u8; w * h];
    for y in win.y0..win.y1 {
        for x in win.x0..win.x1 {
            let i = (y - win.y0) * w + (x - win.x0);
            levels[i] = if visible(x, y) {
                cfg.level_at(cam, centre(x, y))
            } else {
                0
            };
        }
    }
    levels
}
pub fn window_lod(
    win: Window,
    cfg: &LodConfig,
    cam: [f32; 2],
    centre: impl Fn(usize, usize) -> [f32; 2],
    visible: impl Fn(usize, usize) -> bool,
) -> WindowLod {
    let w = win.x1.saturating_sub(win.x0);
    let h = win.y1.saturating_sub(win.y0);
    let levels = window_levels(win, cfg, cam, centre, visible);
    let get = |x: isize, y: isize| -> Option<u8> {
        if x < win.x0 as isize
            || y < win.y0 as isize
            || x >= win.x1 as isize
            || y >= win.y1 as isize
        {
            return None;
        }
        let l = levels[(y as usize - win.y0) * w + (x as usize - win.x0)];
        if l == 0 {
            None
        } else {
            Some(l)
        }
    };
    let mut cases = vec![[0u8; 4]; w * h];
    for y in win.y0..win.y1 {
        for x in win.x0..win.x1 {
            let i = (y - win.y0) * w + (x - win.x0);
            let own = levels[i];
            if own == 0 {
                continue;
            }
            let neighbours = [
                get(x as isize, y as isize - 1),
                get(x as isize, y as isize + 1),
                get(x as isize - 1, y as isize),
                get(x as isize + 1, y as isize),
            ];
            cases[i] = edge_cases(own, neighbours);
        }
    }
    WindowLod {
        window: win,
        levels,
        cases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_grid_is_the_grid_the_full_window_carries() {
        let cfg = LodConfig::default();
        let win = Window {
            x0: 0,
            y0: 0,
            x1: 3,
            y1: 2,
        };
        let cam = [10., 10.];
        let centre = |x: usize, y: usize| [x as f32, y as f32];
        let visible = |x: usize, _y: usize| x != 1;
        let full = window_lod(win, &cfg, cam, centre, visible);
        assert_eq!(window_levels(win, &cfg, cam, centre, visible), full.levels);
        // The skipped column really is skipped, so the grid is not all zeroes.
        assert!(full.levels.iter().any(|level| *level != 0));
        assert_eq!(full.levels[1], 0);
    }

    #[test]
    fn far_boundary_matches_four_cell_grid_at_every_level() {
        for level in 1..=3 {
            let mut lod = WindowLod {
                window: Window {
                    x0: 4,
                    y0: 7,
                    x1: 7,
                    y1: 10,
                },
                levels: vec![level; 9],
                cases: vec![[0; 4]; 9],
            };
            lod.stitch_far_boundary();
            for y in 7..10 {
                for x in 4..7 {
                    let i = (y - 7) * 3 + x - 4;
                    for (side, boundary) in [y == 7, y == 9, x == 4, x == 6].into_iter().enumerate()
                    {
                        assert_eq!(
                            edge_stride(level, lod.cases[i][side]),
                            if boundary { 4 } else { step(level) }
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn window_is_the_recovered_2r_plus_1_square() {
        // range 420 / block 64 -> r = ceil(6.5625) = 7 -> 15x15 blocks.
        let w = window([50.0, 40.5], [128, 176], 420.0, 64.0);
        assert_eq!((w.x1 - w.x0, w.y1 - w.y0), (15, 15));
        assert!(w.contains(50, 40) && w.contains(43, 33) && w.contains(57, 47));
        assert!(!w.contains(42, 40) && !w.contains(58, 40));
        // Edge of the array: the square is shifted, not shrunk (0x77ed80).
        let w = window([1.0, 2.0], [128, 176], 420.0, 64.0);
        assert_eq!((w.x0, w.y0), (0, 0));
        assert_eq!((w.x1, w.y1), (15, 15));
        // A window that would start past the far edge is pulled back to keep
        // its size.
        let w = window([127.0, 175.0], [128, 176], 420.0, 64.0);
        assert_eq!((w.x0, w.x1), (113, 128));
        assert_eq!((w.y0, w.y1), (161, 176));
        // A world smaller than the window is covered completely.
        let w = window([0.0, 0.0], [4, 4], 420.0, 64.0);
        assert_eq!(
            w,
            Window {
                x0: 0,
                y0: 0,
                x1: 4,
                y1: 4
            }
        );
    }

    #[test]
    fn levels_use_the_two_recovered_thresholds_and_no_hysteresis() {
        let cfg = LodConfig::default();
        let block = cfg.block_size;
        // A block under the camera: distance = -half diagonal.
        assert_eq!(cfg.level_at([32.0, 32.0], [32.0, 32.0]), 3);
        // Level 3 boundary: camera distance where centre distance - bias == 180.
        let edge = cfg.near + block * HALF_DIAGONAL;
        assert_eq!(cfg.level_at([0.0, 0.0], [edge - 0.01, 0.0]), 3);
        assert_eq!(cfg.level_at([0.0, 0.0], [edge + 0.01, 0.0]), 2);
        let edge = cfg.far + block * HALF_DIAGONAL;
        assert_eq!(cfg.level_at([0.0, 0.0], [edge - 0.01, 0.0]), 2);
        assert_eq!(cfg.level_at([0.0, 0.0], [edge + 0.01, 0.0]), 1);
        // Shipped preset distances (0x40e41f): 180 / 320 world units.
        assert_eq!(cfg.near, 180.0);
        assert_eq!(cfg.far, 320.0);
        // A block scale other than 1.0 stretches both thresholds.
        let coarse = LodConfig {
            block_scale: 0.5,
            ..cfg
        };
        assert_eq!(coarse.level(89.9), 3);
        assert_eq!(coarse.level(90.1), 2);
        // No hysteresis band: the level is a pure function of the distance, so
        // the same distance always yields the same level and there is no state
        // that could hold a previous result.
        for &d in &[0.0, cfg.near - 0.01, cfg.near, cfg.far - 0.01, cfg.far, 1e6] {
            assert_eq!(cfg.level(d), cfg.level(d));
        }
        assert_eq!(cfg.level(cfg.near - 0.01), 3);
        assert_eq!(cfg.level(cfg.near), 2);
        assert_eq!(cfg.level(cfg.far - 0.01), 2);
        assert_eq!(cfg.level(cfg.far), 1);
    }

    #[test]
    fn edge_cases_only_coarsen_towards_a_coarser_neighbour() {
        assert_eq!(edge_case(3, None), 0);
        assert_eq!(edge_case(3, Some(0)), 0);
        assert_eq!(edge_case(3, Some(3)), 0);
        assert_eq!(edge_case(3, Some(2)), 1);
        assert_eq!(edge_case(3, Some(1)), 2);
        assert_eq!(edge_case(2, Some(1)), 1);
        assert_eq!(edge_case(1, Some(3)), 0);
        // Both sides of a seam agree on the stride along the shared edge.
        for own in 1..=3u8 {
            for other in 1..=3u8 {
                if own == other {
                    continue;
                }
                let a = edge_stride(own, edge_case(own, Some(other)));
                let b = edge_stride(other, edge_case(other, Some(own)));
                assert_eq!(a, b, "levels {own}/{other} disagree on the seam stride");
            }
        }
    }

    #[test]
    fn interior_index_counts_match_the_native_level_grid() {
        // 0x7e5f70 writes 6*q^2 u16 indices per level record, so the interior
        // grid covers q x q cells: 5400 / 1176 / 216 indices for levels 3/2/1.
        assert_eq!(interior_cells(3), 30);
        assert_eq!(interior_cells(2), 14);
        assert_eq!(interior_cells(1), 6);
        for (level, indices) in [(3u8, 5400usize), (2, 1176), (1, 216)] {
            let q = interior_cells(level);
            assert_eq!(6 * q * q, indices);
            assert_eq!(q * step(level), CELLS - 2 * step(level));
            // The emitted interior triangles are two per covered cell.
            let surface = block_surface(level, [0; 4]);
            assert!(surface.indices.len() / 3 >= 2 * q * q);
        }
    }

    /// Rasterised coverage: every point of a block must be covered by exactly
    /// one triangle, for every level and every edge-case combination. Samples
    /// use an offset lattice so none of them lands exactly on a shared edge.
    fn coverage(
        surfaces: &[([f32; 2], BlockSurface)],
        lo: f32,
        hi: f32,
        step: f32,
    ) -> (usize, usize) {
        let nx = ((hi - lo) / step) as usize + 2;
        let lattice = |offset: f32| -> Vec<f32> {
            (0..nx)
                .map(|k| lo + step * offset + k as f32 * step)
                .collect()
        };
        let xs = lattice(0.37);
        let ys = lattice(0.61);
        let mut counts = vec![0u32; nx * nx];
        for (origin, surface) in surfaces {
            for t in surface.triangles() {
                let p = t.map(|v| [origin[0] + v[0] as f32, origin[1] + v[1] as f32]);
                let (x0, x1) = (
                    p[0][0].min(p[1][0]).min(p[2][0]),
                    p[0][0].max(p[1][0]).max(p[2][0]),
                );
                let (y0, y1) = (
                    p[0][1].min(p[1][1]).min(p[2][1]),
                    p[0][1].max(p[1][1]).max(p[2][1]),
                );
                let i0 = ((x0 - lo) / step).floor().max(0.) as usize;
                let i1 = (((x1 - lo) / step).ceil() as usize + 1).min(nx - 1);
                let j0 = ((y0 - lo) / step).floor().max(0.) as usize;
                let j1 = (((y1 - lo) / step).ceil() as usize + 1).min(nx - 1);
                for (j, &y) in ys.iter().enumerate().take(j1 + 1).skip(j0) {
                    if y < y0 || y > y1 || y >= hi {
                        continue;
                    }
                    for (i, &x) in xs.iter().enumerate().take(i1 + 1).skip(i0) {
                        if x < x0 || x > x1 || x >= hi {
                            continue;
                        }
                        if point_in_triangle([x, y], p) {
                            counts[j * nx + i] += 1;
                        }
                    }
                }
            }
        }
        let mut inside = 0usize;
        let mut bad = 0usize;
        for (j, &y) in ys.iter().enumerate() {
            if y >= hi {
                break;
            }
            for (i, &x) in xs.iter().enumerate() {
                if x >= hi {
                    break;
                }
                inside += 1;
                if counts[j * nx + i] != 1 {
                    bad += 1;
                }
            }
        }
        (inside, bad)
    }

    fn point_in_triangle(p: [f32; 2], t: [[f32; 2]; 3]) -> bool {
        let sign = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
            (a[0] - c[0]) * (b[1] - c[1]) - (b[0] - c[0]) * (a[1] - c[1])
        };
        let d1 = sign(p, t[0], t[1]);
        let d2 = sign(p, t[1], t[2]);
        let d3 = sign(p, t[2], t[0]);
        let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(neg && pos)
    }

    #[test]
    fn cached_surface_variants_are_stable_and_match_owned_surfaces() {
        for level in 1..=MAX_LEVEL {
            for encoded in 0..81 {
                let cases = [
                    encoded / 27 % 3,
                    encoded / 9 % 3,
                    encoded / 3 % 3,
                    encoded % 3,
                ];
                let cached = block_surface_cached(level, cases);
                assert!(std::ptr::eq(cached, block_surface_cached(level, cases)));
                assert_eq!(cached.grid, block_surface(level, cases).grid);
                assert_eq!(cached.indices, block_surface(level, cases).indices);
            }
        }
        // Invalid edge values are clamped to the same canonical cache entry.
        assert!(std::ptr::eq(
            block_surface_cached(3, [9, 0, 0, 0]),
            block_surface_cached(3, [2, 0, 0, 0]),
        ));
    }

    #[test]
    fn every_block_surface_covers_its_block_exactly_once() {
        for level in 1..=MAX_LEVEL {
            for encoded in 0..81 {
                let cases = [
                    encoded / 27 % 3,
                    encoded / 9 % 3,
                    encoded / 3 % 3,
                    encoded % 3,
                ];
                let s = block_surface(level, cases);
                assert!(!s.indices.is_empty(), "level {level} {cases:?} empty");
                let cell = step(level) as f32;
                let (n, bad) = coverage(&[([0.0, 0.0], s)], 0.0, CELLS as f32, cell * 0.25);
                assert!(n > 0);
                assert_eq!(
                    bad, 0,
                    "level {level} cases {cases:?}: {bad}/{n} samples uncovered or doubled"
                );
            }
        }
    }

    #[test]
    fn neighbouring_blocks_at_different_levels_leave_no_crack() {
        // 2x2 blocks: levels 3, 2 next to each other, cases derived from the
        // neighbour levels exactly as `0x7c40e6` does.
        let levels = [[3u8, 2u8], [1u8, 3u8]];
        let at = |x: isize, y: isize| -> Option<u8> {
            if !(0..2).contains(&x) || !(0..2).contains(&y) {
                None
            } else {
                Some(levels[y as usize][x as usize])
            }
        };
        let mut surfaces = Vec::new();
        for y in 0..2usize {
            for x in 0..2usize {
                let own = levels[y][x];
                let cases = edge_cases(
                    own,
                    [
                        at(x as isize, y as isize - 1),
                        at(x as isize, y as isize + 1),
                        at(x as isize - 1, y as isize),
                        at(x as isize + 1, y as isize),
                    ],
                );
                surfaces.push((
                    [(x * CELLS) as f32, (y * CELLS) as f32],
                    block_surface(own, cases),
                ));
            }
        }
        let (n, bad) = coverage(&surfaces, 0.0, (2 * CELLS) as f32, 0.25);
        assert!(n > 0);
        assert_eq!(
            bad, 0,
            "{bad}/{n} samples uncovered or doubled across levels"
        );
    }
}
