//! Compact material residency for the native half-open terrain block window.
//! Array entries keep global area indices; only mask payloads are windowed.
/// One area of safety covers boundary vertices, rounding and filtering without
/// ever asking a cold area to borrow another area's layer records.
pub fn area_window(
    [x0, y0, x1, y1]: [usize; 4],
    block_cells: usize,
    mask_grid: usize,
    cols: usize,
    rows: usize,
) -> [usize; 4] {
    assert!(mask_grid > 0);
    [
        (x0 * block_cells / mask_grid).saturating_sub(1).min(cols),
        (y0 * block_cells / mask_grid).saturating_sub(1).min(rows),
        ((x1 * block_cells).div_ceil(mask_grid) + 1).min(cols),
        ((y1 * block_cells).div_ceil(mask_grid) + 1).min(rows),
    ]
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn border_halo_is_clamped_to_world() {
        assert_eq!(area_window([0, 0, 15, 15], 32, 128, 32, 44), [0, 0, 5, 5]);
        assert_eq!(
            area_window([113, 161, 128, 176], 32, 128, 32, 44),
            [27, 39, 32, 44]
        );
    }
    #[test]
    fn every_resident_cell_has_its_original_area() {
        for x0 in 0..128 {
            let x1 = (x0 + 15).min(128);
            let [lo, _, hi, _] = area_window([x0, 4, x1, 19], 32, 128, 32, 44);
            for cell in x0 * 32..x1 * 32 {
                assert!((lo..hi).contains(&(cell / 128)));
            }
        }
    }
    #[test]
    fn native_window_does_not_expand_to_whole_world() {
        let [x0, y0, x1, y1] = area_window([48, 64, 63, 79], 32, 128, 32, 44);
        assert!((x1 - x0) * (y1 - y0) <= 49);
        assert!((x1 - x0) * (y1 - y0) < 32 * 44 / 20);
    }
}

/// Resident world XZ extent, shared by terrain, vegetation and scenery.
pub fn world_bounds(
    blocks: [usize; 4],
    block_cells: usize,
    width: usize,
    height: usize,
    spacing: f32,
) -> [f32; 4] {
    let [x0, y0, x1, y1] = blocks;
    [
        ((x0 * block_cells).min(width - 1) as f32 - width as f32 * 0.5) * spacing,
        ((x1 * block_cells).min(width - 1) as f32 - width as f32 * 0.5) * spacing,
        (height as f32 * 0.5 - (y1 * block_cells).min(height - 1) as f32) * spacing,
        (height as f32 * 0.5 - (y0 * block_cells).min(height - 1) as f32) * spacing,
    ]
}
pub fn contains(bounds: Option<[f32; 4]>, p: [f32; 3]) -> bool {
    bounds.map_or(true, |[x0, x1, z0, z1]| {
        p[0] >= x0 && p[0] < x1 && p[2] >= z0 && p[2] < z1
    })
}
#[cfg(test)]
mod residency_tests {
    use super::*;
    #[test]
    fn half_open_residency_and_unrestricted_mode() {
        let b = Some([-100., 100., -80., 80.]);
        assert!(contains(b, [-100., 1., -80.]));
        assert!(!contains(b, [100., 1., 0.]));
        assert!(!contains(b, [0., 1., 80.]));
        assert!(contains(None, [10000., 1., 0.]));
    }

    #[test]
    fn fog_cap_covers_wide_screen_corners_and_fit_moves_start_inside_window() {
        for aspect in [1., 16. / 9., 32. / 9.] {
            let cap = fog_cap([-448., 512., -448., 512.], [0., 300., 0.], aspect).unwrap();
            let diagonal_cos =
                1. / (1. + 35f32.to_radians().tan().powi(2) * (1. + aspect * aspect)).sqrt();
            assert!(cap < 448. * diagonal_cos);
            let [start, end] = fit_fog(300., 500., cap);
            assert!(start < end && end <= cap);
            assert!(start <= end * 0.75);
        }
        assert_eq!(fog_cap([-1., 1., -1., 1.], [5., 0., 0.], 1.), None);
    }
}


/// Conservative view-space fog limit for the resident XZ rectangle. Account
/// for the screen-corner ray, not just the centre ray, so the entire visible
/// boundary is covered by the fitted fog interval. This is a viewer adaptation,
/// not a recovered native-client fog equation.
pub fn fog_cap(bounds: [f32; 4], eye: [f32; 3], aspect: f32) -> Option<f32> {
    if !contains(Some(bounds), eye) {
        return None;
    }
    let distance_to_edge = (eye[0] - bounds[0])
        .min(bounds[1] - eye[0])
        .min(eye[2] - bounds[2])
        .min(bounds[3] - eye[2]);
    let tangent = 35f32.to_radians().tan();
    let diagonal = (1. + tangent * tangent * (1. + aspect.max(0.01).powi(2))).sqrt();
    Some((distance_to_edge / diagonal * 0.95).max(0.01))
}

/// Move the fog start inward with the fitted end. An authored start beyond the
/// resident window would leave local terrain clear until its far edge.
pub fn fit_fog(start: f32, end: f32, cap: f32) -> [f32; 2] {
    let end = end.min(cap).max(0.01);
    [start.min(end * 0.75), end]
}

/// Visit only cells intersecting residency, instead of scanning a whole ocean
/// on every near-mesh rebuild. Origin is the area's left/top corner in XZ.
pub fn cell_window(
    bounds: Option<[f32; 4]>,
    origin: [f32; 2],
    size: f32,
    cols: usize,
    rows: usize,
) -> [usize; 4] {
    bounds.map_or([0, 0, cols, rows], |b| {
        [
            (((b[0] - origin[0]) / size).floor().max(0.) as usize).min(cols),
            (((origin[1] - b[3]) / size).floor().max(0.) as usize).min(rows),
            (((b[1] - origin[0]) / size).ceil().max(0.) as usize).min(cols),
            (((origin[1] - b[2]) / size).ceil().max(0.) as usize).min(rows),
        ]
    })
}
#[cfg(test)]
mod water_tests {
    use super::*;
    #[test]
    fn ocean_scan_is_windowed_and_partial_cells_are_preserved() {
        assert_eq!(
            cell_window(None, [0., 100.], 8., 1024, 1024),
            [0, 0, 1024, 1024]
        );
        assert_eq!(
            cell_window(Some([1., 17., 82., 99.]), [0., 100.], 8., 1024, 1024),
            [0, 0, 3, 3]
        );
        assert_eq!(
            cell_window(Some([-20., -10., 110., 120.]), [0., 100.], 8., 1024, 1024),
            [0, 0, 0, 0]
        );
        assert_eq!(
            cell_window(
                Some([9000., 10000., -10000., -9000.]),
                [0., 100.],
                8.,
                1024,
                1024
            ),
            [1024, 1024, 1024, 1024]
        );
    }
}
