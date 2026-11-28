//! Recovered sky-object direction curve from elementclient 0x4b60b0/0x4b60e0.
//! Wired only to experimental sky discs, never substituted for authored model
//! light direction. Disc materials/size/halo remain separate unfinished work.
#[allow(dead_code)]
pub fn sky_object_direction(hour: f32, authored_direction: [f32; 3]) -> Option<[f32; 3]> {
    if !hour.is_finite() || !authored_direction.iter().all(|x| x.is_finite()) {
        return None;
    }
    let x = authored_direction[0];
    let z = authored_direction[2];
    if x * x + z * z < 1e-12 {
        return None;
    }
    let azimuth = (-z).atan2(-x);
    let t = hour.rem_euclid(24.) / 24.;
    let elevation = if t < f32::from_bits(0x3e155555) {
        (1. - t * f32::from_bits(0x40db6db7)) * f32::from_bits(0x3f860a92)
    } else if t < 0.5 {
        (t - f32::from_bits(0x3e155555)) * f32::from_bits(0x405cc61e)
    } else if t < 0.875 {
        (1. - (t - 0.5) * f32::from_bits(0x402aaaab)) * f32::from_bits(0x3f9c61aa)
    } else {
        (t - 0.875) * f32::from_bits(0x41060a92)
    };
    let (s, c) = elevation.sin_cos();
    let (az_s, az_c) = azimuth.sin_cos();
    Some([-az_c * c, -s, -az_s * c])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_elevation_curve_and_direction_are_periodic() {
        let base = [0.6, -0.4, 0.8];
        for (h, angle) in [(0., 60_f32), (3.5, 0.), (12., 70.), (21., 0.), (24., 60.)] {
            let v = sky_object_direction(h, base).unwrap();
            assert!(
                (v[1] + angle.to_radians().sin()).abs() < 0.00001,
                "hour {h}: {v:?}"
            );
            assert!(((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]) - 1.).abs() < 0.00001);
            assert!(v[0] >= -1e-6 && v[2] >= -1e-6);
        }
        assert!(sky_object_direction(1., [0., 1., 0.]).is_none());
        assert!(sky_object_direction(f32::NAN, base).is_none());
    }
}

/// Sun/moon display visibility is independent of directional material colors.
/// Constructor 0x4b2d0c; Tick 0x4b5f63; packed render alpha 0x4b3b80.
pub struct Visibility {
    values: [f32; 2],
    flags: u32,
}
impl Default for Visibility {
    fn default() -> Self {
        Self {
            values: [1.; 2],
            flags: 0x18,
        }
    }
}
impl Visibility {
    /// World tick precedes AdjustEnvironment/render: consume last render's
    /// flags, then latch this render's independently selected default-area flags.
    /// Call once per root frame, NOT once per reflected sky draw.
    pub fn update(&mut self, ms: u32, flags: u32) {
        let step = (ms as f64 * f32::from_bits(0x399d4952) as f64) as f32;
        for (i, v) in self.values.iter_mut().enumerate() {
            *v = (if self.flags & (8 << i) != 0 {
                *v + step
            } else {
                *v - step
            })
            .clamp(0., 1.);
        }
        // Latched exactly as the client latches it. A word without either disc
        // bit is passed in as `0x18` by the viewer when the `+0x80` source has
        // not been pinned (`0x421f50` is not the EC-BSD box constructor), so
        // the recovered step below stays untouched.
        self.flags = flags;
    }
    /// The flag word the last `update` latched (`+0x84`/`+0x85` sources): the
    /// default area's `+0x80` word, bit 3 sun and bit 4 moon.
    pub fn flags(&self) -> u32 {
        self.flags
    }
    /// Per-object fade: `+0x7c` for the sun, `+0x80` for the moon.
    pub fn fade(&self, sun: bool) -> f32 {
        self.values[if sun { 0 } else { 1 }]
    }
    /// Legacy hour-selected alpha, still reported in the frame snapshot; the
    /// sky shader consumes [`Self::fade`] per object instead.
    pub fn alpha(&self, hour: f32) -> f32 {
        let t = hour.rem_euclid(24.) / 24.;
        let sun = t > f32::from_bits(0x3e155555) && t < 0.875;
        // Native _ftol then BYTE before uploading diffuse vertex alpha.
        (self.values[if sun { 0 } else { 1 }] * 255.).trunc() / 255.
    }
}
#[cfg(test)]
mod visibility_tests {
    use super::*;
    #[test]
    fn flags_latch_after_tick_and_do_not_crossfade_one_object_into_the_other() {
        let mut v = Visibility::default();
        v.update(250, 8); // moon target changes AFTER this tick
        assert_eq!(v.values, [1., 1.]);
        v.update(250, 8);
        assert_eq!(v.values[0], 1.);
        assert_eq!(v.values[1], 0.925);
        assert_eq!(v.alpha(12.), 1.);
        assert_eq!(v.alpha(0.), 235. / 255.);
        for _ in 0..20 {
            v.update(250, 8);
        }
        assert_eq!(v.values, [1., 0.]);
        assert_eq!(v.alpha(24.), 0.);
        v.update(250, 0x18);
        assert_eq!(v.values[1], 0.);
        v.update(250, 0x18);
        assert!(v.values[1] > 0.);
    }
    #[test]
    fn zero_tick_latches_flags_without_changing_visibility() {
        let mut v = Visibility::default();
        v.update(0, 0);
        for _ in 0..100 {
            v.update(16, 0);
        }
        assert_eq!(v.values[0].to_bits(), 0x3f051e94);
        assert_eq!(v.values[0], v.values[1]);
    }
}
