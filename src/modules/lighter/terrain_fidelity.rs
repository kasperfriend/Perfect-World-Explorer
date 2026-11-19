//! Choose a bounded far mesh before packing the ray scene. The regular path
//! samples the native antidiagonal split; a small map that fails it can use
//! conforming, locally refined tiles without raising the index ceiling.
use std::collections::VecDeque;

pub(super) const ADAPTIVE_STEP: usize = 4;
const MAX_FAR_INDICES: usize = 600_000;
pub(super) const GRID_TOLERANCE: f64 = 0.5;

pub(super) struct FarGrid {
    pub step: usize,
    /// None for large maps that keep the stride-16 memory fallback. They have
    /// not passed the height tolerance; use the offline audit to measure them.
    pub max_sample_error: Option<f64>,
}

fn index_count(w: usize, h: usize, step: usize) -> usize {
    (w - 1)
        .div_ceil(step)
        .saturating_mul((h - 1).div_ceil(step))
        .saturating_mul(6)
}

fn choose_regular(
    width: usize,
    height: usize,
    mut at: impl FnMut(usize, usize) -> Result<f32, String>,
) -> Result<FarGrid, String> {
    assert!(width >= 2 && height >= 2);
    let mut plan = FarGrid {
        step: 16,
        max_sample_error: None,
    };
    for stride in [4, 2, 1] {
        if index_count(width, height, stride) > MAX_FAR_INDICES {
            break;
        }
        let error = if stride == 1 {
            0.
        } else {
            max_grid_error(width, height, stride, &mut at)?
        };
        if plan.max_sample_error.is_none_or(|earlier| error < earlier) {
            plan = FarGrid {
                step: stride,
                max_sample_error: Some(error),
            };
        }
        if error <= GRID_TOLERANCE {
            break;
        }
    }
    Ok(plan)
}

pub(super) enum Layout {
    Regular(FarGrid),
    Refined(AdaptiveGrid),
}

pub(super) fn choose(
    width: usize,
    height: usize,
    mut at: impl FnMut(usize, usize) -> Result<f32, String>,
) -> Result<Layout, String> {
    let regular = choose_regular(width, height, &mut at)?;
    if regular
        .max_sample_error
        .is_some_and(|error| error > GRID_TOLERANCE)
    {
        if let Some(plan) = refine(width, height, &mut at)? {
            return Ok(Layout::Refined(plan));
        }
    }
    Ok(Layout::Regular(regular))
}

fn max_grid_error(
    w: usize,
    h: usize,
    stride: usize,
    at: &mut impl FnMut(usize, usize) -> Result<f32, String>,
) -> Result<f64, String> {
    let mut peak: f64 = 0.;
    for y0 in (0..h - 1).step_by(stride) {
        let y1 = (y0 + stride).min(h - 1);
        for x0 in (0..w - 1).step_by(stride) {
            let x1 = (x0 + stride).min(w - 1);
            let [a, b, c, d] = [
                f64::from(at(x0, y0)?),
                f64::from(at(x1, y0)?),
                f64::from(at(x0, y1)?),
                f64::from(at(x1, y1)?),
            ];
            let fit = |x: f64, y: f64| {
                let u = (x - x0 as f64) / (x1 - x0) as f64;
                let v = (y - y0 as f64) / (y1 - y0) as f64;
                if u + v <= 1. {
                    (1. - u - v) * a + u * b + v * c
                } else {
                    (1. - v) * b + (u + v - 1.) * d + (1. - u) * c
                }
            };
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let error = (f64::from(at(x, y)?) - fit(x as f64, y as f64)).abs();
                    peak = peak.max(error);
                    if x < x1 && y < y1 {
                        // The native midpoint lies on the two triangles' shared edge.
                        let center = 0.5 * (f64::from(at(x + 1, y)?) + f64::from(at(x, y + 1)?));
                        peak = peak.max((center - fit(x as f64 + 0.5, y as f64 + 0.5)).abs());
                    }
                }
            }
        }
    }
    Ok(peak)
}

pub(super) type GridPoint = [usize; 2];
pub(super) type GridTriangle = [GridPoint; 3];

/// A tile either keeps its two coarse half-planes, with extra vertices on
/// edges shared with detailed tiles, or uses the source's native triangles.
/// Tile boundaries remain on the stride-four lattice used for near clipping.
pub(super) struct AdaptiveGrid {
    width: usize,
    height: usize,
    cols: usize,
    rows: usize,
    fine: Vec<bool>,
    pub index_count: usize,
    pub refined: usize,
    pub max_error: f64,
}

impl AdaptiveGrid {
    pub fn tiles(&self) -> [usize; 2] {
        [self.cols, self.rows]
    }

    fn sides(&self, x: usize, y: usize) -> [bool; 4] {
        let id = y * self.cols + x;
        [
            y > 0 && self.fine[id - self.cols],
            x > 0 && self.fine[id - 1],
            y + 1 < self.rows && self.fine[id + self.cols],
            x + 1 < self.cols && self.fine[id + 1],
        ]
    }

    fn extents(&self, x: usize, y: usize) -> [usize; 4] {
        let (x, y) = (x * ADAPTIVE_STEP, y * ADAPTIVE_STEP);
        [
            x,
            y,
            (x + ADAPTIVE_STEP).min(self.width - 1),
            (y + ADAPTIVE_STEP).min(self.height - 1),
        ]
    }

    fn indices_at(&self, x: usize, y: usize) -> usize {
        let [x0, y0, x1, y1] = self.extents(x, y);
        let (w, h) = (x1 - x0, y1 - y0);
        if self.fine[y * self.cols + x] {
            6 * w * h
        } else {
            let [top, left, bottom, right] = self.sides(x, y);
            6 + 3
                * ((usize::from(top) + usize::from(bottom)) * (w - 1)
                    + (usize::from(left) + usize::from(right)) * (h - 1))
        }
    }

    pub fn triangles(&self, x: usize, y: usize, mut emit: impl FnMut(GridTriangle)) {
        let [x0, y0, x1, y1] = self.extents(x, y);
        if self.fine[y * self.cols + x] {
            for y in y0..y1 {
                for x in x0..x1 {
                    emit([[x, y], [x + 1, y], [x, y + 1]]);
                    emit([[x + 1, y], [x + 1, y + 1], [x, y + 1]]);
                }
            }
            return;
        }
        let [top, left, bottom, right] = self.sides(x, y);
        let [a, b, c, d] = [[x0, y0], [x1, y0], [x0, y1], [x1, y1]];
        for (a, b, c, across, down) in [(a, b, c, top, left), (d, c, b, bottom, right)] {
            let nx = if across { x1 - x0 } else { 1 };
            let ny = if down { y1 - y0 } else { 1 };
            if nx == 1 && ny == 1 {
                emit([a, b, c]);
                continue;
            }
            // Preserve the antidiagonal and every shared edge segment without
            // a degenerate fan on a collinear boundary or a T-junction.
            let along = |i| [(a[0] * (nx - i) + b[0] * i) / nx, a[1]];
            let side = |j| [a[0], (a[1] * (ny - j) + c[1] * j) / ny];
            for i in 0..nx {
                emit([along(i), along(i + 1), side(1)]);
            }
            for j in 1..ny {
                emit([b, side(j + 1), side(j)]);
            }
        }
    }
}

/// Refine only coarse tiles that exceed the tolerance, including neighbours
/// whose newly inserted edge samples change their interior triangles. The
/// work list is bounded by the affordable stride-four grid. If the conforming
/// solution cannot fit, retain the old regular plan, not a truncated mesh.
fn refine(
    width: usize,
    height: usize,
    mut at: impl FnMut(usize, usize) -> Result<f32, String>,
) -> Result<Option<AdaptiveGrid>, String> {
    let initial_indices = index_count(width, height, ADAPTIVE_STEP);
    if initial_indices > MAX_FAR_INDICES {
        return Ok(None);
    }
    let cols = (width - 1).div_ceil(ADAPTIVE_STEP);
    let rows = (height - 1).div_ceil(ADAPTIVE_STEP);
    let mut heights = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let h = at(x, y)?;
            if !h.is_finite() {
                return Err("non-finite far terrain sample".into());
            }
            heights.push(h);
        }
    }
    let field = Field {
        width,
        height,
        heights,
    };
    let mut plan = AdaptiveGrid {
        width,
        height,
        cols,
        rows,
        fine: vec![false; cols * rows],
        index_count: initial_indices,
        refined: 0,
        max_error: 0.,
    };
    let mut errors = vec![0.; cols * rows];
    let mut pending = vec![true; cols * rows];
    let mut queue: VecDeque<_> = (0..cols * rows).collect();
    while let Some(id) = queue.pop_front() {
        pending[id] = false;
        if plan.fine[id] {
            continue;
        }
        let (x, y) = (id % cols, id / cols);
        let mut error: f64 = 0.;
        plan.triangles(x, y, |tri| error = error.max(field.triangle_error(tri)));
        if error <= GRID_TOLERANCE {
            errors[id] = error;
            continue;
        }
        let neighbours = [
            x.checked_sub(1).map(|i| (i, y)),
            (x + 1 < cols).then_some((x + 1, y)),
            y.checked_sub(1).map(|i| (x, i)),
            (y + 1 < rows).then_some((x, y + 1)),
        ];
        let changed = || std::iter::once((x, y)).chain(neighbours.into_iter().flatten());
        let old: usize = changed().map(|(x, y)| plan.indices_at(x, y)).sum();
        plan.fine[id] = true;
        plan.refined += 1;
        errors[id] = 0.;
        let new: usize = changed().map(|(x, y)| plan.indices_at(x, y)).sum();
        plan.index_count = plan.index_count - old + new;
        if plan.index_count > MAX_FAR_INDICES {
            return Ok(None);
        }
        for (x, y) in neighbours.into_iter().flatten() {
            let neighbour = y * cols + x;
            if !plan.fine[neighbour] && !pending[neighbour] {
                pending[neighbour] = true;
                queue.push_back(neighbour);
            }
        }
    }
    plan.max_error = errors.into_iter().fold(0., f64::max);
    Ok(Some(plan))
}

struct Field {
    width: usize,
    height: usize,
    heights: Vec<f32>,
}
impl Field {
    fn at(&self, [x, y]: GridPoint) -> f64 {
        f64::from(self.heights[y * self.width + x])
    }

    fn native(&self, [x, y]: [f64; 2]) -> f64 {
        let x = x.clamp(0., (self.width - 1) as f64);
        let y = y.clamp(0., (self.height - 1) as f64);
        let (ix, iy) = (
            (x as usize).min(self.width - 2),
            (y as usize).min(self.height - 2),
        );
        let (u, v) = (x - ix as f64, y - iy as f64);
        let b = self.at([ix + 1, iy]);
        let c = self.at([ix, iy + 1]);
        if u + v <= 1. {
            (1. - u - v) * self.at([ix, iy]) + u * b + v * c
        } else {
            (1. - v) * b + (u + v - 1.) * self.at([ix + 1, iy + 1]) + (1. - u) * c
        }
    }

    /// The difference of two piecewise-linear meshes is linear in each
    /// overlay polygon. Its extrema are at shared vertices, native vertices
    /// inside this triangle, or intersections of the two meshes' edges.
    fn triangle_error(&self, tri: GridTriangle) -> f64 {
        let [a, b, c] = tri.map(|p| p.map(|u| u as i64));
        let [za, zb, zc] = tri.map(|p| self.at(p));
        let cross = |p: [i64; 2], q: [i64; 2], r: [i64; 2]| {
            (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
        };
        let area = cross(a, b, c);
        debug_assert!(area > 0);
        let lo = a[0].min(b[0]).min(c[0]);
        let hi = a[0].max(b[0]).max(c[0]);
        let mut peak: f64 = 0.;
        for y in a[1].min(b[1]).min(c[1])..=a[1].max(b[1]).max(c[1]) {
            for x in lo..=hi {
                let p = [x, y];
                let [wa, wb, wc] = [cross(b, c, p), cross(c, a, p), cross(a, b, p)];
                if wa >= 0 && wb >= 0 && wc >= 0 {
                    let z = (wa as f64 * za + wb as f64 * zb + wc as f64 * zc) / area as f64;
                    peak = peak.max((z - self.at([x as usize, y as usize])).abs());
                }
            }
        }
        for ((p, zp), (q, zq)) in [((a, za), (b, zb)), ((b, zb), (c, zc)), ((c, zc), (a, za))] {
            // Native edge lines: vertical, horizontal and antidiagonal.
            for (start, end) in [(p[0], q[0]), (p[1], q[1]), (p[0] + p[1], q[0] + q[1])] {
                for k in start.min(end) + 1..start.max(end) {
                    let t = (k - start) as f64 / (end - start) as f64;
                    let xy = [
                        p[0] as f64 + t * (q[0] - p[0]) as f64,
                        p[1] as f64 + t * (q[1] - p[1]) as f64,
                    ];
                    peak = peak.max((zp + t * (zq - zp) - self.native(xy)).abs());
                }
            }
        }
        peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_grid_keeps_flat_maps_coarse_but_refines_rough_small_maps() {
        let flat = choose_regular(256, 256, |x, y| Ok((x + 3 * y) as f32)).unwrap();
        assert_eq!(flat.step, 4);
        assert!(flat.max_sample_error.unwrap() < 1.0e-9);
        let ridge = choose_regular(256, 256, |x, y| {
            Ok(if x == 137 && y == 223 { 66. } else { 0. })
        })
        .unwrap();
        assert_eq!(ridge.step, 1);
        assert_eq!(ridge.max_sample_error, Some(0.));
        assert!(index_count(256, 256, ridge.step) <= MAX_FAR_INDICES);
    }

    #[test]
    fn missing_samples_and_memory_limited_fallback_are_not_a_fidelity_pass() {
        assert!(choose_regular(256, 256, |_, _| Err("missing tile".into())).is_err());
        let large = choose_regular(4096, 5632, |_, _| {
            panic!("don't scan the mainland in startup's fidelity check")
        })
        .unwrap();
        assert_eq!(large.step, 16);
        assert_eq!(large.max_sample_error, None);
        // Native stride 4 fits this budget, but two strictly better candidates
        // do not. Keep it and publish the non-zero error rather than aborting.
        let capped = choose_regular(1200, 1200, |x, y| {
            Ok(if x == 11 && y == 13 { 90. } else { 0. })
        })
        .unwrap();
        assert_eq!(capped.step, 4);
        assert_eq!(capped.max_sample_error, Some(90.));
    }

    #[test]
    fn local_refinement_is_conforming_and_within_the_budget() {
        let (w, h) = (512, 510);
        let at = |x, y| Ok((x + 2 * y) as f32 + if x == 207 && y == 301 { 65. } else { 0. });
        let Layout::Refined(plan) = choose(w, h, at).unwrap() else {
            panic!("unaffordable stride-one map should refine the local ridge");
        };
        assert!(plan.index_count < MAX_FAR_INDICES);
        assert!(plan.refined > 0 && plan.refined <= 4);
        assert!(plan.max_error < 1e-9);
        let mut edges = std::collections::HashMap::new();
        let (mut count, mut area) = (0, 0);
        for y in 0..plan.rows {
            for x in 0..plan.cols {
                plan.triangles(x, y, |[a, b, c]| {
                    let [a1, b1, c1] = [a, b, c].map(|p| p.map(|v| v as isize));
                    let twice =
                        (b1[0] - a1[0]) * (c1[1] - a1[1]) - (b1[1] - a1[1]) * (c1[0] - a1[0]);
                    assert!(twice > 0);
                    area += twice as usize;
                    count += 3;
                    for mut edge in [[a, b], [b, c], [c, a]] {
                        edge.sort_unstable();
                        *edges.entry(edge).or_insert(0) += 1;
                    }
                });
            }
        }
        assert_eq!(count, plan.index_count);
        assert_eq!(area, 2 * (w - 1) * (h - 1));
        for ([[ax, ay], [bx, by]], count) in edges {
            let border =
                ax == bx && (ax == 0 || ax == w - 1) || ay == by && (ay == 0 || ay == h - 1);
            assert_eq!(count, if border { 1 } else { 2 });
        }
    }

    #[test]
    fn checkerboard_cannot_overrun_the_budget_or_truncate_the_mesh() {
        let Layout::Regular(plan) =
            choose(512, 512, |x, y| Ok(((x + y) % 2) as f32 * 10.)).unwrap()
        else {
            panic!("a full-detail checkerboard should keep the bounded regular fallback");
        };
        assert_eq!(plan.max_sample_error, Some(10.));
        assert!(index_count(512, 512, plan.step) <= MAX_FAR_INDICES);
    }
}
