//! Stable cross-fades between the three client-runtime geometry snapshots.
//! The 10% transition widths are an approximation, not a SpeedTreeRT fade port.
//!
//! `client_distance` supplies the renderer's 3D DLL-style distance estimate.
//! `client_level` and component selectors are experimental translations not
//! yet wired into snapshot selection: the existing cache lacks per-component
//! LODs and transition metadata required to choose the right geometry.

/// SpeedTreeRT 0x100282b0: calculate the continuous level from camera and
/// tree positions and client-provided near/far limits. The DLL computes a
/// 3-dimensional squared distance with x87 intermediates, rounds it to f32,
/// then uses the float-bit square-root approximation shown below. The input
/// limits must be those used by the client, not guessed from a mesh snapshot.
///
/// Returns None for unsupported nonfinite, invalid, or overflowed inputs. The
/// original DLL's handling of these edge cases is not reproduced here. f64
/// intermediates approximate x87 extended precision and are not guaranteed to
/// be bit-identical at rounding boundaries.
pub fn client_level(position: [f32; 3], camera: [f32; 3], near: f32, far: f32) -> Option<f32> {
    if !near.is_finite() || !far.is_finite() || near < 0.0 || near >= far {
        return None;
    }
    let estimate = client_distance(position, camera)?;
    let level = 1.0 - (f64::from(estimate) - f64::from(near)) / (f64::from(far) - f64::from(near));
    Some(level.clamp(0.0, 1.0) as f32)
}

/// SpeedTreeRT's 3D distance estimate before applying near/far limits.
/// Finite f32 values use f64 intermediates to approximate the DLL's x87
/// arithmetic; see `client_level` for the precision limitation.
pub fn client_distance(position: [f32; 3], camera: [f32; 3]) -> Option<f32> {
    if !position.into_iter().chain(camera).all(f32::is_finite) {
        return None;
    }
    let dx = f64::from(position[0]) - f64::from(camera[0]);
    let dy = f64::from(position[1]) - f64::from(camera[1]);
    let dz = f64::from(position[2]) - f64::from(camera[2]);
    let distance_squared = (dx * dx + dy * dy + dz * dz) as f32;
    if !distance_squared.is_finite() {
        return None;
    }
    Some(f32::from_bits(
        (distance_squared.to_bits() >> 1) + 0x1fc0_0000,
    ))
}
/// Translate the DLL's discrete LOD getters (0x10022260, 0x100222b0,
/// 0x10022300) for explicitly supplied levels in the range 0..=1. For leaf
/// geometry, `billboard_slot` must only be true when both the global drop flag
/// and the per-tree billboard prerequisite are set. If true, index `count`
/// denotes the billboard slot; otherwise `None` means no geometry levels.
///
/// Counts above i16::MAX and the DLL's negative-level sentinel are deliberately
/// unsupported. This helper is not wired to rendering: PWTREE07 does not carry
/// per-component geometry levels or enough data to apply the result faithfully.
pub fn client_discrete_level(level: f32, count: u16, billboard_slot: bool) -> Option<u16> {
    if !level.is_finite() || !(0.0..=1.0).contains(&level) || count > i16::MAX as u16 {
        return None;
    }
    let slots = u32::from(count) + u32::from(billboard_slot);
    if slots == 0 {
        return None;
    }
    let index = ((1.0 - f64::from(level)) * f64::from(slots)) as u32;
    Some(index.min(slots - 1) as u16)
}

/// Experimental translation of SpeedTreeRT's two-level alpha-reference
/// transition routine at 0x100254f0. Alpha is in the client's 0..255 units;
/// it is a cutout threshold, not opacity or ordered-dither coverage. This is
/// not connected to the old combined-snapshot renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClientTransition {
    pub primary: u16,
    pub secondary: Option<u16>,
    pub alpha_primary: f32,
    pub alpha_secondary: f32,
}

/// Unsupported input combinations return `None`; callers must provide the
/// original per-tree parameters and the actual number of component levels.
/// f64 intermediates approximate the x87 arithmetic and pow is not guaranteed
/// to match the client's imported CRT at every rounding boundary.
pub fn client_transition(
    level: f32,
    count: u16,
    half_width: f32,
    bias: f32,
    exponent: f32,
    alpha: f32,
) -> Option<ClientTransition> {
    if !level.is_finite()
        || !(0.0..=1.0).contains(&level)
        || count == 0
        || count > i16::MAX as u16
        || !half_width.is_finite()
        || half_width <= 0.0
        || !bias.is_finite()
        || !(0.0..1.0).contains(&bias)
        || !exponent.is_finite()
        || exponent <= 0.0
        || !alpha.is_finite()
        || !(0.0..=255.0).contains(&alpha)
    {
        return None;
    }
    let inverted = (1.0_f64 - f64::from(level)) as f32;
    let normalized = f64::from(inverted) * f64::from(count);
    let nearest = (normalized + 0.5).trunc() as u16;
    let delta = (f64::from(inverted) - f64::from(nearest) / f64::from(count)) as f32;
    if nearest == 0 || nearest == count || delta.abs() >= half_width {
        let primary = ((f64::from(inverted) * f64::from(count)) as u16).min(count - 1);
        return Some(ClientTransition {
            primary,
            secondary: None,
            alpha_primary: alpha,
            alpha_secondary: 255.0,
        });
    }
    let phase =
        (1.0 - (f64::from(half_width) - f64::from(delta)) / (2.0 * f64::from(half_width))) as f32;
    let remaining = (1.0_f64 - f64::from(bias)) as f32;
    let a = (1.0 - (f64::from(phase) - f64::from(bias)) / f64::from(remaining)).min(1.0) as f32;
    let b = (f64::from(phase) / f64::from(remaining)).min(1.0) as f32;
    let span = (255.0_f64 - f64::from(alpha)) as f32;
    let exp_f64 = f64::from(exponent);
    let curve = |x: f32| -> f64 {
        let base = 1.0 - f64::from(x);
        if base <= 0.0 && exp_f64 > 0.0 {
            0.0
        } else if exp_f64 == 1.0 {
            base
        } else {
            base.powf(exp_f64)
        }
    };
    let alpha_primary = (f64::from(alpha) + f64::from(span) * curve(a)) as f32;
    let alpha_secondary = (f64::from(alpha) + f64::from(span) * curve(b)) as f32;
    Some(ClientTransition {
        primary: nearest - 1,
        secondary: Some(nearest),
        alpha_primary,
        alpha_secondary,
    })
}

/// A per-component geometry slot and its alpha-test reference (0..255).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComponentLod {
    pub index: u16,
    pub alpha_reference: f32,
}

/// GetGeometry's LOD outputs with drop-to-billboard disabled. This is not yet
/// wired to the snapshot renderer: PWTREE07 does not contain independent LODs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClientComponents {
    pub branch: Option<ComponentLod>,
    pub frond: Option<ComponentLod>,
    pub leaf: [Option<ComponentLod>; 2],
}

/// Translate the component selection for the supported mode 0 and mode 1
/// leaf paths, for finite levels and an explicitly supplied material state.
/// Billboard blending and modes 2/3 are not represented: callers must not use
/// this for those states. This function does not select or synthesize geometry.
pub fn client_components(
    level: f32,
    counts: [u16; 3],
    leaf_mode: u32,
    half_width: f32,
    bias: f32,
    exponent: f32,
    alpha: u8,
) -> Option<ClientComponents> {
    if !level.is_finite()
        || !(0.0..=1.0).contains(&level)
        || counts.iter().any(|&count| count > i16::MAX as u16)
    {
        return None;
    }
    let reference = f32::from(alpha);
    let discrete = |count| {
        client_discrete_level(level, count, false).map(|index| ComponentLod {
            index,
            alpha_reference: reference,
        })
    };
    let leaf = match leaf_mode {
        0 => [discrete(counts[2]), None],
        1 if counts[2] == 0 => [None, None],
        1 => {
            let t = client_transition(level, counts[2], half_width, bias, exponent, reference)?;
            [
                Some(ComponentLod {
                    index: t.primary,
                    alpha_reference: t.alpha_primary,
                }),
                t.secondary.map(|index| ComponentLod {
                    index,
                    alpha_reference: t.alpha_secondary,
                }),
            ]
        }
        _ => return None,
    };
    Some(ClientComponents {
        branch: discrete(counts[0]),
        frond: discrete(counts[1]),
        leaf,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    pub primary: usize,
    pub secondary: Option<usize>,
    pub fraction: f32,
}
pub fn select(distance: f32, height: f32, scale: f32) -> Selection {
    let h = height.max(5.);
    let s = scale.clamp(0.25, 4.);
    let a = h * 5. * s;
    let b = h * 16. * s;
    for (i, boundary) in [(0, a), (1, b)] {
        let start = boundary * 0.9;
        let end = boundary * 1.1;
        if distance >= start && distance < end {
            return Selection {
                primary: i,
                secondary: Some(i + 1),
                fraction: ((distance - start) / (end - start)).clamp(0., 1.),
            };
        }
    }
    Selection {
        primary: if distance < a {
            0
        } else if distance < b {
            1
        } else {
            2
        },
        secondary: None,
        fraction: 0.,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transitions_are_bounded_and_continuous() {
        for (b, i) in [(50., 0), (160., 1)] {
            let before = select(b * 0.899, 10., 1.);
            let start = select(b * 0.9, 10., 1.);
            let half = select(b, 10., 1.);
            let end = select(b * 1.1, 10., 1.);
            assert_eq!(before.primary, i);
            assert_eq!(before.secondary, None);
            assert_eq!(start.secondary, Some(i + 1));
            assert!(start.fraction.abs() < 1e-5);
            assert!((half.fraction - 0.5).abs() < 1e-5);
            assert_eq!(end.primary, i + 1);
            assert_eq!(end.secondary, None);
        }
    }
    #[test]
    fn client_continuous_level_emulated_dll_fixtures() {
        // Generated by probe_lod_unicorn.py against the identified DLL, not a
        // native Windows/client capture. See SpeedTree-phase4-investigation.md.
        for (distance, expected) in [
            (0., 1.),
            (25., 1.),
            (49., 0.9931107759),
            (50., 0.9860795736),
            (55., 0.9487926364),
            (100., 0.5176136494),
            (150., 0.0737215877),
            (160., 0.),
            (170., 0.),
            (200., 0.),
        ] {
            let level = client_level([distance, 0., 0.], [0., 0., 0.], 50., 160.).unwrap();
            assert!(
                (level - expected).abs() < 1e-6,
                "distance {distance}: {level}"
            );
        }
        let level = client_level([0., 100., 0.], [0., 0., 0.], 50., 160.).unwrap();
        assert!((level - 0.5176136494).abs() < 1e-6);
        assert_eq!(
            client_level([110., 7., 2.], [10., 7., 2.], 50., 160.),
            Some(level)
        );
        assert_eq!(client_level([f32::NAN, 0., 0.], [0.; 3], 50., 160.), None);
        assert_eq!(client_level([f32::MAX, 0., 0.], [0.; 3], 50., 160.), None);
        assert_eq!(client_level([0.; 3], [0.; 3], 160., 50.), None);
    }
    #[test]
    fn component_selection_discrete_transition_and_unsupported_modes() {
        let discrete = client_components(0.5, [2, 4, 3], 0, 0.07, 0.5, 1.3, 84).unwrap();
        assert_eq!(discrete.branch.unwrap().index, 1);
        assert_eq!(discrete.frond.unwrap().index, 2);
        assert_eq!(discrete.leaf[0].unwrap().index, 1);
        assert_eq!(discrete.leaf[0].unwrap().alpha_reference, 84.0);
        assert_eq!(discrete.leaf[1], None);
        let transition = client_components(0.5, [5, 5, 6], 1, 0.07, 0.5, 1.3, 84).unwrap();
        assert_eq!(transition.leaf[0].unwrap().index, 2);
        assert_eq!(transition.leaf[1].unwrap().index, 3);
        assert_eq!(transition.leaf[0].unwrap().alpha_reference, 84.0);
        assert_eq!(transition.leaf[1].unwrap().alpha_reference, 84.0);
        assert_eq!(
            client_components(0.5, [0, 0, 0], 0, 0.0, 0.0, 0.0, 84)
                .unwrap()
                .leaf,
            [None, None]
        );
        assert_eq!(
            client_components(0.5, [2, 2, 2], 3, 0.07, 0.5, 1.3, 84),
            None
        );
        assert_eq!(
            client_components(f32::NAN, [2, 2, 2], 0, 0.07, 0.5, 1.3, 84),
            None
        );
        assert_eq!(
            client_components(0.5, [2, 2, 2], 1, 0.0, 0.5, 1.3, 84),
            None
        );
    }
    #[test]
    fn distance_estimate_includes_altitude_and_rejects_invalid_values() {
        let origin = [0.0, 0.0, 0.0];
        let horizontal = client_distance([100.0, 0.0, 0.0], origin).unwrap();
        let vertical = client_distance([0.0, 100.0, 0.0], origin).unwrap();
        assert_eq!(horizontal, vertical);
        // The DLL deliberately uses a bit approximation, not sqrt(10000).
        assert_ne!(horizontal, 100.0);
        assert_eq!(client_distance([f32::NAN, 0.0, 0.0], origin), None);
        assert_eq!(client_distance([f32::MAX, 0.0, 0.0], origin), None);
    }
    #[test]
    fn client_discrete_boundaries_and_empty_levels() {
        for (level, index) in [(1., 0), (0.7, 0), (0.6, 1), (0.5, 1), (0.3, 2), (0., 2)] {
            assert_eq!(client_discrete_level(level, 3, false), Some(index));
        }
        for (level, index) in [(1., 0), (0.8, 0), (0.75, 1), (0.5, 2), (0.25, 3), (0., 3)] {
            assert_eq!(client_discrete_level(level, 3, true), Some(index));
        }
        assert_eq!(client_discrete_level(0.5, 0, false), None);
        assert_eq!(client_discrete_level(0.5, 0, true), Some(0));
        assert_eq!(client_discrete_level(-1., 3, false), None);
        assert_eq!(client_discrete_level(f32::NAN, 3, false), None);
        assert_eq!(client_discrete_level(1., 32768, false), None);
    }
    #[test]
    fn client_two_level_transition_emulated_dll_fixtures() {
        for (level, primary, secondary, alpha_a, alpha_b) in [
            (0.738, 0, None, 84., 255.),
            (0.736, 0, Some(1), 84., 253.475494),
            (0.700, 0, Some(1), 87.680840, 178.330017),
            (0.600, 0, Some(1), 247.423080, 84.),
            (0.596, 1, None, 84., 255.),
            (0.404, 1, None, 84., 255.),
            (0.402, 1, Some(2), 84., 251.955597),
            (0.300, 1, Some(2), 178.329956, 87.680862),
            (0.264, 1, Some(2), 253.475449, 84.),
            (0.263, 2, None, 84., 255.),
        ] {
            let result = client_transition(level, 3, 0.07, 0.2, 1.5, 84.).unwrap();
            assert_eq!((result.primary, result.secondary), (primary, secondary));
            assert!(
                (result.alpha_primary - alpha_a).abs() < 0.001,
                "{level}: {result:?}"
            );
            assert!(
                (result.alpha_secondary - alpha_b).abs() < 0.001,
                "{level}: {result:?}"
            );
        }
        assert!(client_transition(0.5, 0, 0.07, 0.2, 1.5, 84.).is_none());
        assert!(client_transition(0.5, 3, 0.0, 0.2, 1.5, 84.).is_none());
        assert!(client_transition(0.5, 3, 0.07, 1., 1.5, 84.).is_none());
    }
    #[test]
    fn distance_and_scale_change_selection() {
        assert_eq!(select(40., 10., 1.).primary, 0);
        assert_eq!(select(100., 10., 1.).primary, 1);
        assert_eq!(select(200., 10., 1.).primary, 2);
        assert_eq!(select(200., 10., 2.).primary, 1);
    }
}
