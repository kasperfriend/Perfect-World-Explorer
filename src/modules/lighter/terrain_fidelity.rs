//! Choose a bounded regular far grid before packing the ray scene. Sample
//! the same antidiagonal triangles as the native terrain index builder.
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

pub(super) fn choose(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_grid_keeps_flat_maps_coarse_but_refines_rough_small_maps() {
        let flat = choose(256, 256, |x, y| Ok((x + 3 * y) as f32)).unwrap();
        assert_eq!(flat.step, 4);
        assert!(flat.max_sample_error.unwrap() < 1.0e-9);
        let ridge = choose(256, 256, |x, y| {
            Ok(if x == 137 && y == 223 { 66. } else { 0. })
        })
        .unwrap();
        assert_eq!(ridge.step, 1);
        assert_eq!(ridge.max_sample_error, Some(0.));
        assert!(index_count(256, 256, ridge.step) <= MAX_FAR_INDICES);
    }

    #[test]
    fn missing_samples_and_memory_limited_fallback_are_not_a_fidelity_pass() {
        assert!(choose(256, 256, |_, _| Err("missing tile".into())).is_err());
        let large = choose(4096, 5632, |_, _| {
            panic!("don't scan the mainland in startup's fidelity check")
        })
        .unwrap();
        assert_eq!(large.step, 16);
        assert_eq!(large.max_sample_error, None);
        // Native stride 4 fits this budget, but two strictly better candidates
        // do not. Keep it and publish the non-zero error rather than aborting.
        let capped = choose(1200, 1200, |x, y| {
            Ok(if x == 11 && y == 13 { 90. } else { 0. })
        })
        .unwrap();
        assert_eq!(capped.step, 4);
        assert_eq!(capped.max_sample_error, Some(90.));
    }
}
