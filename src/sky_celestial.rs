//! Native sun/moon billboard geometry and visibility rules (P5.03).
//!
//! Recovered from `elementclient.exe`:
//!
//! * `CECSunMoon` render 0x4b3e00..0x4b41ee submits two billboards. Each one
//!   is gated on its own elevation (`+0x4c` for the sun, `+0x50` for the moon)
//!   being greater than `0.0` (`fcomp ds:0x8d58a0`) and places the template's
//!   `y == 0` edge at `direction * ds:0x8d6a9c` (100.0 world units); the quad
//!   centre hangs `height / 2` below that point ([`template_hang`]). The quad
//!   sizes are the
//!   vertex templates `CECSunMoon::Init` fills (`0x4b2fde` sun, `0x4b3425`
//!   moon): 20 x 20 and 40 x 40 world units, so the discs subtend 11.42 and
//!   22.62 degrees. `0x8d6a94`/`0x8d6a98` are not read by that path.
//! * The direction is rebuilt every frame from `[+0x50]`/`[+0x4c]` and `[+0x60]`
//!   as `(cos(az)cos(el), sin(el), sin(az)cos(el))`.
//! * The vertex diffuse alpha is `fade_byte << 24` over an opaque RGB, taken
//!   from `+0x48`; the fade itself is the `0.0003 / ms` ramp in
//!   [`crate::client_sun::Visibility`] (`+0x7c` for the sun, `+0x80` for the
//!   moon).
//! * Textures are `textures/nature/sun.bmp` and `textures/nature/moon.tga`.
//!
//! `CECSunMoon::Init` also fills a 120 x 40 sun-shine strip over
//! `sunshine.bmp` (drawn by [`shine_ramp`]'s window) and the flare-chain streams
//! over `sunflare.bmp`/`flares.bmp`, whose per-sprite placement is not
//! recovered. States this module cannot reproduce are reported as
//! [`CelestialError`] instead of being drawn approximately.

use crate::sky_time::SunMoon;

/// Placement distance of both discs in world units (`ds:0x8d6a9c`).
pub const DISTANCE: f32 = 100.0;

// The quad extents are the *vertex data* `CECSunMoon::Init` fills into its
// streams, not a scale factor on an implicit unit quad. The templates are
// 28-byte vertices `{x, y, z, pad, diffuse, u, v}` in the disc plane, and the
// transform `0x788960` builds is orthonormal, so the template's numbers are
// world units at [`DISTANCE`]:
//
//   sun stream   (0x4b2fde): x in [-10, 10], y in [0, -20]  -> 20 x 20 units
//   sun shine    (0x4b3130): x in [-60, 60], y in [10, -30] -> 120 x 40 units
//   moon stream  (0x4b3425): x in [-20, 20], y in [0, -40]  -> 40 x 40 units
//
// `2 * atan(10/100)` = 11.42 degrees for the sun quad and `2 * atan(20/100)` =
// 22.62 degrees for the moon quad, before the sun's swell below. The templates
// hang from their anchor (`y` runs from 0 to `-height`), so the quad's centre
// sits `height/2` below the emitted position; [`template_hang`] applies it.
/// Native sun stream half extent, world units at [`DISTANCE`].
pub const SUN_HALF_EXTENT: f32 = 10.0;
/// Native moon stream half extent, world units at [`DISTANCE`].
pub const MOON_HALF_EXTENT: f32 = 20.0;
/// Native sun-shine stream half width (`x` in `-60..60`).
pub const SUNSHINE_HALF_WIDTH: f32 = 60.0;
/// Native sun-shine stream half height (`y` in `10..-30`).
pub const SUNSHINE_HALF_HEIGHT: f32 = 20.0;

/// Distance the quad centre hangs below the emitted point, in world units.
///
/// Every `CECSunMoon::Init` template starts at `y == 0` (the emitted point) and
/// runs to `-height`, so the anchor is the quad's **top edge** and the centre is
/// `height / 2` below it. For the sun and the moon that is exactly the half
/// extent ([`sun_half_extent`] includes the `1 + 2 n` swell, so the hang grows
/// with the quad); the sun-shine strip hangs
/// `SUNSHINE_HALF_HEIGHT / 2` below, which puts its centre on the disc's.
///
/// This is geometry, not a nudge: with it the recovered elevation curve is the
/// elevation of the *setting line*, so `+0x4c == 0` (the native `el > 0` gate at
/// `0x4b3e00`) means the sun has just fully set. Without it the emitted point
/// was used as the quad centre, which put the sun `atan(10 / 100)` = 5.71
/// degrees too high and made the gate hide it 5.71 degrees above the horizon.
pub fn template_hang(half_extent: f32) -> f32 {
    if half_extent.is_finite() {
        half_extent.max(0.0)
    } else {
        0.0
    }
}

/// The sun bills scale with the night strength as `1 + 2 * n`
/// (`0x4b3d1d` disc, `0x4b3fc6` shine); the moon block (`0x4b4161`) scales
/// nothing. The recovered night strength saturates at 1 while the sun is up.
pub const SUN_SWELL: f32 = 2.0;
/// Thresholds of the recovered sun-shine ramp (`0x4b3dc1`..`0x4b3e22`).
pub const SHINE_RAMP_START: f32 = 0.3;
pub const SHINE_RAMP_FULL: f32 = 0.6;
pub const SHINE_RAMP_FADE: f32 = 0.8;

/// Index list submitted per disc (six indices, two triangles).
pub const DISC_INDICES: [u16; 6] = [0, 1, 2, 0, 2, 3];

/// Sun stream half extent after the native `1 + 2 * n` swell.
pub fn sun_half_extent(night_strength: f32) -> f32 {
    let n = if night_strength.is_finite() {
        night_strength.clamp(0.0, 1.0)
    } else {
        0.0
    };
    SUN_HALF_EXTENT * (1.0 + SUN_SWELL * n)
}

/// Native half extent of one object's quad before any swell.
pub fn half_extent(kind: DiscKind) -> f32 {
    match kind {
        DiscKind::Sun => SUN_HALF_EXTENT,
        DiscKind::Moon => MOON_HALF_EXTENT,
    }
}

/// The sun's shine strength as the native render computes it.
///
/// `0x4b3d8a` skips the shine quad when the night strength is `0` or the night
/// set is selected; inside that window the strength ramps `0` -> `1` between
/// `0.3` and `0.6`, holds `1` to `0.8`, then falls back to `(1 - n) * 5` which
/// reaches `0` at `n = 1`. The shine is therefore a dusk/dawn glare, not a
/// midday glow.
pub fn shine_ramp(night_strength: f32, night_set: bool) -> f32 {
    if !night_strength.is_finite() || night_set {
        return 0.0;
    }
    let n = night_strength;
    if n <= SHINE_RAMP_START {
        0.0
    } else if n < SHINE_RAMP_FULL {
        (n - SHINE_RAMP_START) * (1.0 / (SHINE_RAMP_FULL - SHINE_RAMP_START))
    } else if n < SHINE_RAMP_FADE {
        1.0
    } else {
        ((GLOW_REFERENCE - n) * GLOW_GAIN).clamp(0.0, 1.0)
    }
}

/// Which of the two objects a disc belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscKind {
    Sun,
    Moon,
}

impl DiscKind {
    /// Config/asset name, also used in diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            DiscKind::Sun => "sun",
            DiscKind::Moon => "moon",
        }
    }
    /// Native texture path.
    pub fn texture(self) -> &'static str {
        match self {
            DiscKind::Sun => "textures/nature/sun.bmp",
            DiscKind::Moon => "textures/nature/moon.tga",
        }
    }
}

/// Why a disc cannot be drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum CelestialError {
    /// The supplied state was not finite.
    NotFinite(&'static str),
    /// Native gate: the object's elevation is not above the horizon.
    BelowHorizon {
        kind: &'static str,
        elevation_degrees: f32,
    },
    /// The regional flags switched the object off and its fade reached zero.
    Hidden { kind: &'static str, fade: f32 },
    /// The native texture is missing from the package.
    MissingAsset(&'static str),
}

impl std::fmt::Display for CelestialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CelestialError::NotFinite(what) => write!(f, "non-finite {what}"),
            CelestialError::BelowHorizon {
                kind,
                elevation_degrees,
            } => write!(
                f,
                "{kind} is below the horizon ({elevation_degrees:.3} deg)"
            ),
            CelestialError::Hidden { kind, fade } => {
                write!(f, "{kind} is switched off (fade {fade:.3})")
            }
            CelestialError::MissingAsset(path) => write!(f, "missing client asset {path}"),
        }
    }
}

/// One native billboard ready for submission.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Disc {
    pub kind: DiscKind,
    /// Quad centre, `direction * DISTANCE` minus the template hang
    /// ([`template_hang`]) along the quad's up axis, i.e. the centre of the
    /// recovered `y 0..-height` stream.
    pub center: [f32; 3],
    /// Four corners in native submission order.
    pub corners: [[f32; 3]; 4],
    /// Vertex diffuse alpha as the client uploads it (`fade * 255` truncated).
    pub alpha: u8,
    /// Half angle subtended by the quad, in radians.
    pub angular_radius: f32,
}

impl Disc {
    /// Native packed diffuse: opaque RGB with the fade in the alpha byte.
    #[allow(dead_code)]
    pub fn diffuse(&self) -> [u8; 4] {
        [255, 255, 255, self.alpha]
    }
}

/// Half angle subtended by a quad of `half_extent` world units at
/// [`DISTANCE`].
pub fn angular_radius(half_extent: f32) -> f32 {
    (half_extent / DISTANCE).atan()
}

/// Gain the native path multiplies the sun's `+0x44` light colour by
/// (`ds:0x8db0f8` = 5.0).
pub const GLOW_GAIN: f32 = 5.0;
/// Reference value the night strength `+0x70` is subtracted from
/// (`ds:0x8d5c28` = 1.0).
pub const GLOW_REFERENCE: f32 = 1.0;
/// Scale applied to the factor while the night flag `+0x74` selects the night
/// set (`ds:0x8d62dc` = 0.8).
pub const GLOW_NIGHT_SCALE: f32 = 0.8;

/// Texture factor the native path hands to the effect's stage-1 `TA_TFACTOR`
/// modulation (`0x4b3e00`–`0x4b3e39`).
///
/// The code tests the strength for unordered (NaN) first and falls back to the
/// literal `1.0` (`mov dword [esp+0x10], 0x3f800000`); otherwise it computes
/// `(1.0 - strength) * 5.0` and, when `[+0x74] == 1.0`, multiplies by `0.8`.
/// The client does not clamp the result: at the 1.75 midnight strength the
/// factor is negative and the hardware saturates it away when the colour is
/// modulated, which is why the glow is invisible while the sun is down.
#[allow(dead_code)]
pub fn glow_texture_factor(night_strength: f32, night_set: bool) -> f32 {
    if !night_strength.is_finite() {
        return 1.0;
    }
    let factor = (GLOW_REFERENCE - night_strength) * GLOW_GAIN;
    if night_set {
        factor * GLOW_NIGHT_SCALE
    } else {
        factor
    }
}

/// Unit direction of a disc from its elevation and the shared azimuth.
pub fn direction(elevation: f32, azimuth: f32) -> [f32; 3] {
    let (sin_e, cos_e) = elevation.sin_cos();
    let (sin_a, cos_a) = azimuth.sin_cos();
    [cos_a * cos_e, sin_e, sin_a * cos_e]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f32; 3]) -> Option<[f32; 3]> {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len.is_finite() && len > 1e-6 {
        let inv = 1.0 / len;
        Some([v[0] * inv, v[1] * inv, v[2] * inv])
    } else {
        None
    }
}

/// Build the native billboard for one object.
///
/// `fade` is the 0..1 ramp from [`crate::client_sun::Visibility`]; `asset_loaded`
/// says whether the native texture is present, so a missing asset fails here
/// instead of rendering an empty quad.
pub fn disc(
    kind: DiscKind,
    state: &SunMoon,
    fade: f32,
    asset_loaded: bool,
    night_strength: f32,
) -> Result<Disc, CelestialError> {
    if !fade.is_finite() || !state.azimuth.is_finite() {
        return Err(CelestialError::NotFinite("celestial fade or azimuth"));
    }
    if !asset_loaded {
        return Err(CelestialError::MissingAsset(kind.texture()));
    }
    let elevation = state.elevation(kind == DiscKind::Sun);
    if !elevation.is_finite() {
        return Err(CelestialError::NotFinite("celestial elevation"));
    }
    if elevation <= 0.0 {
        return Err(CelestialError::BelowHorizon {
            kind: kind.name(),
            elevation_degrees: elevation.to_degrees(),
        });
    }
    if fade <= 0.0 {
        return Err(CelestialError::Hidden {
            kind: kind.name(),
            fade,
        });
    }
    let dir = direction(elevation, state.azimuth);
    // Screen-aligned billboard: the native path builds the quad in view space,
    // which for a camera-anchored sky equals a basis perpendicular to the disc
    // direction. The fallback handles an exactly vertical disc.
    let right = normalize(cross([0.0, 1.0, 0.0], dir))
        .or_else(|| normalize(cross([1.0, 0.0, 0.0], dir)))
        .ok_or(CelestialError::NotFinite("celestial basis"))?;
    let up = cross(dir, right);
    let half = match kind {
        DiscKind::Sun => sun_half_extent(night_strength),
        DiscKind::Moon => MOON_HALF_EXTENT,
    };
    // The native streams hang from the emitted point, which is their top edge:
    // the centre is `template_hang` below it along the quad's own up axis. The
    // emitted point keeps the recovered direction, so the elevation curve stays
    // the setting line's elevation (see [`template_hang`]).
    let hang = template_hang(half);
    let center = [
        dir[0] * DISTANCE - up[0] * hang,
        dir[1] * DISTANCE - up[1] * hang,
        dir[2] * DISTANCE - up[2] * hang,
    ];
    let mut corners = [[0.0_f32; 3]; 4];
    for (index, (rs, us)) in [(-1.0_f32, 1.0_f32), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)]
        .iter()
        .enumerate()
    {
        for axis in 0..3 {
            corners[index][axis] = center[axis] + right[axis] * rs * half + up[axis] * us * half;
        }
    }
    Ok(Disc {
        kind,
        center,
        corners,
        alpha: (fade.clamp(0.0, 1.0) * 255.0).trunc() as u8,
        angular_radius: angular_radius(half),
    })
}

/// Both discs of a frame, each with its own explicit failure.
pub fn discs(
    state: &SunMoon,
    sun_fade: f32,
    moon_fade: f32,
    sun_asset: bool,
    moon_asset: bool,
    night_strength: f32,
) -> [Result<Disc, CelestialError>; 2] {
    [
        disc(DiscKind::Sun, state, sun_fade, sun_asset, night_strength),
        disc(DiscKind::Moon, state, moon_fade, moon_asset, night_strength),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sky_time::evaluate;

    fn state(hour: f32) -> SunMoon {
        evaluate(hour, [0.6, -0.4, 0.8]).expect("finite")
    }

    #[test]
    fn disc_size_matches_the_native_stream_templates_at_distance_100() {
        // 20 x 20 units at 100 -> 11.421 degrees; 40 x 40 -> 22.619 degrees.
        let sun = angular_radius(SUN_HALF_EXTENT);
        let moon = angular_radius(MOON_HALF_EXTENT);
        assert!((sun.to_degrees() - 5.710_593).abs() < 1e-5);
        assert!((moon.to_degrees() - 11.309_932).abs() < 1e-5);
        // tan(half angle) * distance is the half extent exactly; the arc length
        // is not, and at these sizes the difference is 0.07 units.
        assert!((sun.tan() * DISTANCE - SUN_HALF_EXTENT).abs() < 1e-3);
        assert!((moon.tan() * DISTANCE - MOON_HALF_EXTENT).abs() < 1e-3);
        // The templates say exactly 20, 40 and 120 x 40 units.
        assert_eq!(SUN_HALF_EXTENT * 2.0, 20.0);
        assert_eq!(MOON_HALF_EXTENT * 2.0, 40.0);
        assert_eq!(SUNSHINE_HALF_WIDTH * 2.0, 120.0);
        assert_eq!(SUNSHINE_HALF_HEIGHT * 2.0, 40.0);
        // The moon is twice the sun, and the sun swells to 3x at full strength.
        assert_eq!(MOON_HALF_EXTENT, 2.0 * SUN_HALF_EXTENT);
        assert_eq!(sun_half_extent(0.0), SUN_HALF_EXTENT);
        assert_eq!(sun_half_extent(1.0), SUN_HALF_EXTENT * 3.0);
        assert_eq!(sun_half_extent(0.5), SUN_HALF_EXTENT * 2.0);
        // A non-finite or out-of-range strength cannot grow the quad further.
        assert_eq!(sun_half_extent(f32::NAN), SUN_HALF_EXTENT);
        assert_eq!(sun_half_extent(1.75), SUN_HALF_EXTENT * 3.0);
    }

    #[test]
    fn sun_shine_ramp_is_the_recovered_dusk_window() {
        assert_eq!(shine_ramp(0.0, false), 0.0);
        assert_eq!(shine_ramp(0.29, false), 0.0);
        assert!((shine_ramp(0.45, false) - 0.5).abs() < 1e-5);
        assert_eq!(shine_ramp(0.6, false), 1.0);
        assert_eq!(shine_ramp(0.79, false), 1.0);
        assert!((shine_ramp(0.9, false) - 0.5).abs() < 1e-6);
        assert_eq!(shine_ramp(1.0, false), 0.0);
        // `0x4b3d9e` skips the shine quad while the night set is selected.
        assert_eq!(shine_ramp(0.45, true), 0.0);
        assert_eq!(shine_ramp(f32::NAN, false), 0.0);
    }

    #[test]
    fn sun_and_moon_are_placed_by_their_own_elevation() {
        let noon = state(12.0);
        let sun = disc(DiscKind::Sun, &noon, 1.0, true, 0.0).expect("noon sun");
        assert_eq!(sun.kind, DiscKind::Sun);
        // The emitted point (the top edge of the recovered `y 0..-20` template)
        // is still the recovered direction; only the quad centre hangs below it.
        let emitted = emitted_point(&sun);
        let up = emitted[1] / DISTANCE;
        assert!((up - 70_f32.to_radians().sin()).abs() < 1e-5);
        let horizontal = (emitted[0] * emitted[0] + emitted[2] * emitted[2]).sqrt() / DISTANCE;
        assert!((horizontal - 70_f32.to_radians().cos()).abs() < 1e-5);
        assert_eq!(
            disc(DiscKind::Moon, &noon, 1.0, true, 0.0),
            Err(CelestialError::BelowHorizon {
                kind: "moon",
                elevation_degrees: 0.0
            })
        );
        let midnight = state(0.0);
        assert!(disc(DiscKind::Moon, &midnight, 1.0, true, 0.0).is_ok());
        assert!(matches!(
            disc(DiscKind::Sun, &midnight, 1.0, true, 0.0),
            Err(CelestialError::BelowHorizon { .. })
        ));
    }

    /// Midpoint of the quad's top edge: the point the client's streams hang from.
    fn emitted_point(disc: &Disc) -> [f32; 3] {
        // `corners[0]` and `corners[1]` share `us = +1`, so their midpoint is
        // `center + up * half`, i.e. the template's `y == 0` anchor.
        let mut out = [0.0_f32; 3];
        for axis in 0..3 {
            out[axis] = (disc.corners[0][axis] + disc.corners[1][axis]) * 0.5;
        }
        out
    }

    #[test]
    fn the_disc_hangs_from_the_emitted_point_so_the_gate_is_a_real_sunset() {
        // The recovered curve's 0.0 elevation is the *setting line*: the sun's
        // top edge is on the horizon and the whole quad is below it, which is
        // why `el > 0` can hide the disc without popping it out mid-sky.
        let half = SUN_HALF_EXTENT;
        assert_eq!(template_hang(half), half);
        let mut just_before = state(20.99);
        // The curve's own end point is exactly 0 (21:00), which the native gate
        // rejects; evaluate the quad at a hair above it instead.
        just_before.sun_elevation = 1e-7;
        let sun = disc(DiscKind::Sun, &just_before, 1.0, true, 0.0).expect("setting sun");
        let edge = emitted_point(&sun);
        assert!(
            edge[1].abs() < 1e-3,
            "top edge {} not on the horizon",
            edge[1]
        );
        assert!(sun.center[1] < 0.0, "quad centre must be below the horizon");
        // The same holds for the swelled dusk sun and for the moon.
        let dusk = disc(DiscKind::Sun, &just_before, 1.0, true, 1.0).expect("swelled sun");
        let edge = emitted_point(&dusk);
        assert!(edge[1].abs() < 1e-2);
        assert!((dusk.center[1] + sun_half_extent(1.0)).abs() < 1e-2);
        let mut moonrise = state(21.01);
        moonrise.moon_elevation = 1e-7;
        let moon = disc(DiscKind::Moon, &moonrise, 1.0, true, 0.0).expect("rising moon");
        assert!(emitted_point(&moon)[1].abs() < 1e-3);
        assert!(moon.center[1] < 0.0);
    }

    #[test]
    fn quad_is_a_flat_two_triangle_billboard_of_the_recovered_extent() {
        let sun = disc(DiscKind::Sun, &state(12.0), 1.0, true, 0.0).expect("sun");
        assert_eq!(DISC_INDICES.len(), 6);
        let noon = state(12.0);
        let dir = direction(noon.sun_elevation, noon.azimuth);
        for corner in sun.corners {
            let mut offset = [0.0_f32; 3];
            for axis in 0..3 {
                offset[axis] = corner[axis] - sun.center[axis];
            }
            let distance =
                (offset[0] * offset[0] + offset[1] * offset[1] + offset[2] * offset[2]).sqrt();
            // Corner distance is half_extent * sqrt(2) for the 20-unit sun quad.
            assert!((distance - SUN_HALF_EXTENT * std::f32::consts::SQRT_2).abs() < 1e-3);
            // Every corner lies in the tangent plane of the disc direction.
            let dot = offset[0] * dir[0] + offset[1] * dir[1] + offset[2] * dir[2];
            assert!(dot.abs() < 1e-3);
        }
    }

    #[test]
    fn visibility_fade_is_uploaded_as_a_truncated_alpha_byte() {
        let sun = disc(DiscKind::Sun, &state(12.0), 0.925, true, 0.0).expect("sun");
        assert_eq!(sun.alpha, 235);
        assert_eq!(sun.diffuse(), [255, 255, 255, 235]);
        assert_eq!(
            disc(DiscKind::Sun, &state(12.0), 0.0, true, 0.0),
            Err(CelestialError::Hidden {
                kind: "sun",
                fade: 0.0
            })
        );
    }

    #[test]
    fn glow_texture_factor_follows_the_native_expression() {
        // Daytime: strength 0, day set -> the full 5x gain.
        assert_eq!(glow_texture_factor(0.0, false), 5.0);
        // Same clock with the night set selected -> 80 percent of it.
        assert!((glow_texture_factor(0.0, true) - 4.0).abs() < 1e-6);
        // 18:00: strength 0 -> 5.0; 21:00: strength 1 -> 0.
        assert!(glow_texture_factor(0.5, false) > 0.0);
        assert_eq!(glow_texture_factor(1.0, false), 0.0);
        // Midnight strength 1.75 overdrives it negative; the native code leaves
        // that to the modulator saturating, and so does this function.
        assert!((glow_texture_factor(1.75, false) + 3.75).abs() < 1e-6);
        // A non-finite strength takes the literal 1.0 branch.
        assert_eq!(glow_texture_factor(f32::NAN, true), 1.0);
        assert_eq!(glow_texture_factor(f32::INFINITY, false), 1.0);
    }

    #[test]
    fn unsupported_states_fail_explicitly() {
        assert_eq!(
            disc(DiscKind::Sun, &state(12.0), 1.0, false, 0.0),
            Err(CelestialError::MissingAsset("textures/nature/sun.bmp"))
        );
        let mut broken = state(12.0);
        broken.sun_elevation = f32::NAN;
        assert_eq!(
            disc(DiscKind::Sun, &broken, 1.0, true, 0.0),
            Err(CelestialError::NotFinite("celestial elevation"))
        );
        let errors = discs(&state(12.0), 1.0, 1.0, true, true, 0.0);
        assert!(errors[0].is_ok());
        assert!(errors[1].is_err());
        // The messages are what the viewer shows, so keep them readable.
        assert_eq!(
            errors[1].as_ref().unwrap_err().to_string(),
            "moon is below the horizon (0.000 deg)"
        );
    }
}
