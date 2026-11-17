//! The provider × quality-mode matrix: one table per provider, and the only
//! place that decides what a provider advertises.
//!
//! The three vendor APIs express "quality mode" in three different ways, and
//! they do **not** agree on the set of modes they offer. Before this module the
//! viewer kept one list for all providers plus a `match` per provider, so a
//! mode could be offered by the UI and then folded into something else at
//! `CreateFeature` time (XeSS "Native AA" became
//! `XESS_QUALITY_SETTING_ULTRA_QUALITY` at 1.0×, and DLSS "Ultra Quality" was
//! handed `PerfQualityValue 4` at 1.0×, which the NGX core rejects with
//! `FAIL_UnsupportedParameter`). The table below is now the single source of
//! truth for
//!
//! * which modes a provider offers ([`modes`], [`find`], [`quality_available`]),
//! * the per-dimension render ratio each mode asks for ([`Mode::factor`], used
//!   by `crate::provider_extent`),
//! * the vendor enum value that selects the mode in that provider's own API
//!   ([`Mode::vendor`]; NGX `NVSDK_NGX_PerfQuality_Value`, FFX
//!   `FFX_UPSCALE_QUALITY_MODE_*`, XeSS `XESS_QUALITY_SETTING_*`), and
//! * the name the provider actually gives the mode ([`Mode::name`] — "DLAA" for
//!   DLSS, "Native AA" for FSR 3.1/4 and XeSS, "Ultra Quality" for XeSS only).
//!
//! [`require`] is what the providers call: it answers the table row or a
//! reason, never a silent fallback.
//!
//! ## Sources
//!
//! * NGX: `NVSDK_NGX_PerfQuality_Value` in `nvsdk_ngx_defs.h` — `MaxPerf 0`,
//!   `Balanced 1`, `MaxQuality 2`, `UltraPerformance 3`, `UltraQuality 4`,
//!   `DLAA 5`. Measured against the installed core (`_nvngx.dll`
//!   32.0.16.2114) with `sdk/nvngx_dlss.dll` 310.9.16: values 0, 1, 2, 3 and 5
//!   create and evaluate; value 4 answers `0xbad00010`
//!   (`NVSDK_NGX_Result_FAIL_UnsupportedParameter`) from
//!   `NVSDK_NGX_D3D12_CreateFeature`. See `docs/dlss-fidelity.md` §3.
//! * FFX: `FFX_UPSCALE_QUALITY_MODE_*` in
//!   `sdk/include/upscalers/ffx_upscale.h` (FidelityFX SDK 2.4.0) —
//!   `NATIVEAA 0` (1.0×), `QUALITY 1` (1.5×), `BALANCED 2` (1.7×),
//!   `PERFORMANCE 3` (2.0×), `ULTRA_PERFORMANCE 4` (3.0×). There is no Ultra
//!   Quality preset: FSR 2/3/4 go straight from Native AA to Quality. The FFX
//!   upscaler is dispatched with explicit `renderSize`/`upscaleSize`, so the
//!   ratio in this table is what actually reaches it.
//! * XeSS: `xess_quality_settings_t` in `sdk/include/xess/xess.h` (XeSS SDK
//!   3.0.2) — `ULTRA_PERFORMANCE 100` (3.0×), `PERFORMANCE 101` (2.0×),
//!   `BALANCED 102` (1.7×), `QUALITY 103` (1.5×), `ULTRA_QUALITY 104` (1.3×),
//!   `ULTRA_QUALITY_PLUS 105` (1.15×), `AA 106` (1.0×, native resolution with
//!   XeSS's temporal antialiasing). XeSS 1.x had no Ultra Performance mode,
//!   which is why the viewer used to hide it; the vendored 3.0.2 runtime has
//!   it, so it is offered (and a `libxess.dll` older than 2.0 fails the create
//!   visibly instead of silently rendering at 2.0×).
//! * FSR 1: EASU + RCAS is `src/fsr1_gpu.rs`, a spatial pass with no temporal
//!   accumulation: it has no native-AA mode (a 1.0× dispatch would be a
//!   no-op), it takes a scale factor rather than a vendor enum, and it uses the
//!   same documented scale ladder as the FFX providers.

use crate::{UpscaleMethod, UpscaleQuality};

/// One advertised quality mode of one provider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mode {
    /// The viewer's (portable) mode name.
    pub quality: UpscaleQuality,
    /// The provider's own name for the mode — what the UI and the logs say.
    pub name: &'static str,
    /// Per-dimension render ratio: `render_extent = output_extent * factor`.
    pub factor: f64,
    /// The vendor enum value that selects the mode in the provider's API. For
    /// FSR 1 (EASU, scale-factor driven, no enum) this is `0` and unused.
    pub vendor: u32,
}

impl Mode {
    const fn new(quality: UpscaleQuality, name: &'static str, factor: f64, vendor: u32) -> Self {
        Self {
            quality,
            name,
            factor,
            vendor,
        }
    }
}

// `NVSDK_NGX_PerfQuality_Value` (nvsdk_ngx_defs.h). `ULTRA_QUALITY` (4) is
// deliberately absent: the core rejects it, see the module documentation.
const NGX_MAX_PERF: u32 = 0;
const NGX_BALANCED: u32 = 1;
const NGX_MAX_QUALITY: u32 = 2;
const NGX_ULTRA_PERFORMANCE: u32 = 3;
const NGX_DLAA: u32 = 5;

// `FFX_UPSCALE_QUALITY_MODE_*` (ffx_upscale.h, FidelityFX SDK 2.4.0).
const FFX_NATIVE_AA: u32 = 0;
const FFX_QUALITY: u32 = 1;
const FFX_BALANCED: u32 = 2;
const FFX_PERFORMANCE: u32 = 3;
const FFX_ULTRA_PERFORMANCE: u32 = 4;

// `XESS_QUALITY_SETTING_*` (xess.h, XeSS SDK 3.0.2).
const XESS_ULTRA_PERFORMANCE: u32 = 100;
const XESS_PERFORMANCE: u32 = 101;
const XESS_BALANCED: u32 = 102;
const XESS_QUALITY: u32 = 103;
const XESS_ULTRA_QUALITY: u32 = 104;
const XESS_AA: u32 = 106;

/// The documented scale ladder, independent of provider: the ratio a mode
/// *means*. Used to answer "what does this unadvertised mode degrade to".
pub fn nominal_factor(quality: UpscaleQuality) -> f64 {
    match quality {
        UpscaleQuality::NativeAa => 1.0,
        UpscaleQuality::UltraQuality => 1.0 / 1.3,
        UpscaleQuality::Quality => 1.0 / 1.5,
        UpscaleQuality::Balanced => 1.0 / 1.7,
        UpscaleQuality::Performance => 0.5,
        UpscaleQuality::UltraPerformance => 1.0 / 3.0,
    }
}

/// DLSS: DLAA (native), Quality, Balanced, Performance, Ultra Performance.
/// There is no Ultra Quality — DLSS's native-resolution mode *is* DLAA, and the
/// core rejects the Ultra Quality enum value outright.
const DLSS_MODES: [Mode; 5] = [
    Mode::new(UpscaleQuality::NativeAa, "DLAA", 1.0, NGX_DLAA),
    Mode::new(
        UpscaleQuality::Quality,
        "Quality",
        1.0 / 1.5,
        NGX_MAX_QUALITY,
    ),
    Mode::new(
        UpscaleQuality::Balanced,
        "Balanced",
        1.0 / 1.7,
        NGX_BALANCED,
    ),
    Mode::new(
        UpscaleQuality::Performance,
        "Performance",
        0.5,
        NGX_MAX_PERF,
    ),
    Mode::new(
        UpscaleQuality::UltraPerformance,
        "Ultra Performance",
        1.0 / 3.0,
        NGX_ULTRA_PERFORMANCE,
    ),
];

/// FSR 1 (EASU + RCAS): spatial only, so no native-AA mode, and no vendor
/// enum — EASU is driven by the scale factor (`vendor` is unused).
const FSR1_MODES: [Mode; 4] = [
    Mode::new(UpscaleQuality::Quality, "Quality", 1.0 / 1.5, 0),
    Mode::new(UpscaleQuality::Balanced, "Balanced", 1.0 / 1.7, 0),
    Mode::new(UpscaleQuality::Performance, "Performance", 0.5, 0),
    Mode::new(
        UpscaleQuality::UltraPerformance,
        "Ultra Performance",
        1.0 / 3.0,
        0,
    ),
];

/// FSR 2 (FFX temporal): the FFX ladder without Native AA — AMD introduced
/// `FFX_UPSCALE_QUALITY_MODE_NATIVEAA` with the 3.1 provider.
const FSR2_MODES: [Mode; 4] = [
    Mode::new(UpscaleQuality::Quality, "Quality", 1.0 / 1.5, FFX_QUALITY),
    Mode::new(
        UpscaleQuality::Balanced,
        "Balanced",
        1.0 / 1.7,
        FFX_BALANCED,
    ),
    Mode::new(
        UpscaleQuality::Performance,
        "Performance",
        0.5,
        FFX_PERFORMANCE,
    ),
    Mode::new(
        UpscaleQuality::UltraPerformance,
        "Ultra Performance",
        1.0 / 3.0,
        FFX_ULTRA_PERFORMANCE,
    ),
];

/// FSR 3.1 / FSR 4: the full FFX ladder, Native AA included.
const FSR3_MODES: [Mode; 5] = [
    Mode::new(UpscaleQuality::NativeAa, "Native AA", 1.0, FFX_NATIVE_AA),
    Mode::new(UpscaleQuality::Quality, "Quality", 1.0 / 1.5, FFX_QUALITY),
    Mode::new(
        UpscaleQuality::Balanced,
        "Balanced",
        1.0 / 1.7,
        FFX_BALANCED,
    ),
    Mode::new(
        UpscaleQuality::Performance,
        "Performance",
        0.5,
        FFX_PERFORMANCE,
    ),
    Mode::new(
        UpscaleQuality::UltraPerformance,
        "Ultra Performance",
        1.0 / 3.0,
        FFX_ULTRA_PERFORMANCE,
    ),
];

const FSR4_MODES: [Mode; 5] = FSR3_MODES;

/// XeSS: Native AA *and* Ultra Quality, plus Quality/Balanced/Performance/
/// Ultra Performance (SDK 3.0.2).
const XESS_MODES: [Mode; 6] = [
    Mode::new(UpscaleQuality::NativeAa, "Native AA", 1.0, XESS_AA),
    Mode::new(
        UpscaleQuality::UltraQuality,
        "Ultra Quality",
        1.0 / 1.3,
        XESS_ULTRA_QUALITY,
    ),
    Mode::new(UpscaleQuality::Quality, "Quality", 1.0 / 1.5, XESS_QUALITY),
    Mode::new(
        UpscaleQuality::Balanced,
        "Balanced",
        1.0 / 1.7,
        XESS_BALANCED,
    ),
    Mode::new(
        UpscaleQuality::Performance,
        "Performance",
        0.5,
        XESS_PERFORMANCE,
    ),
    Mode::new(
        UpscaleQuality::UltraPerformance,
        "Ultra Performance",
        1.0 / 3.0,
        XESS_ULTRA_PERFORMANCE,
    ),
];

/// The modes a provider advertises, in the order the UI lists them.
pub fn modes(method: UpscaleMethod) -> &'static [Mode] {
    match method {
        UpscaleMethod::Off => &[],
        UpscaleMethod::Dlss => &DLSS_MODES,
        UpscaleMethod::Fsr1 => &FSR1_MODES,
        UpscaleMethod::Fsr2 => &FSR2_MODES,
        UpscaleMethod::Fsr3 => &FSR3_MODES,
        UpscaleMethod::Fsr4 => &FSR4_MODES,
        UpscaleMethod::Xess => &XESS_MODES,
    }
}

/// The table row for `(method, quality)`, if the provider advertises it.
pub fn find(method: UpscaleMethod, quality: UpscaleQuality) -> Option<&'static Mode> {
    for mode in modes(method) {
        if mode.quality == quality {
            return Some(mode);
        }
    }
    None
}

/// Whether `method` advertises `quality`.
pub fn quality_available(method: UpscaleMethod, quality: UpscaleQuality) -> bool {
    find(method, quality).is_some()
}

/// Why a provider does not offer a mode, when the reason is a documented
/// restriction rather than "this provider has no such mode".
pub fn rejection(method: UpscaleMethod, quality: UpscaleQuality) -> Option<&'static str> {
    match (method, quality) {
        (UpscaleMethod::Dlss, UpscaleQuality::UltraQuality) => Some(
            "the NGX core rejects NVSDK_NGX_PerfQuality_Value 4 with 0xbad00010 \
             (FAIL_UnsupportedParameter) from CreateFeature; DLSS's native-resolution \
             mode is DLAA",
        ),
        (UpscaleMethod::Fsr1, UpscaleQuality::NativeAa) => Some(
            "FSR 1's EASU is a spatial pass with no temporal accumulation, so a 1.0x \
             dispatch would be a no-op",
        ),
        (UpscaleMethod::Fsr2, UpscaleQuality::NativeAa) => {
            Some("FFX_UPSCALE_QUALITY_MODE_NATIVEAA is a 3.1/4.0 provider mode")
        }
        (
            UpscaleMethod::Fsr1 | UpscaleMethod::Fsr2 | UpscaleMethod::Fsr3 | UpscaleMethod::Fsr4,
            UpscaleQuality::UltraQuality,
        ) => Some(
            "the FFX upscaler has no Ultra Quality preset: its ladder runs Native AA \
             1.0x, Quality 1.5x, Balanced 1.7x, Performance 2.0x, Ultra Performance 3.0x",
        ),
        _ => None,
    }
}

/// The table row for `(method, quality)`, or the reason there is none. This is
/// what the providers call at create time: a mode the table does not carry is
/// an error the session log reports, never a silently different render ratio.
pub fn require(method: UpscaleMethod, quality: UpscaleQuality) -> Result<&'static Mode, String> {
    let found = find(method, quality);
    if let Some(mode) = found {
        return Ok(mode);
    }
    if method == UpscaleMethod::Off {
        return Err(format!(
            "Upscaling is off, so there is no {} mode to create",
            quality.label()
        ));
    }
    let mut advertised: Vec<&str> = Vec::new();
    for mode in modes(method) {
        advertised.push(mode.name);
    }
    let reason = match rejection(method, quality) {
        Some(reason) => format!(": {reason}"),
        None => String::new(),
    };
    Err(format!(
        "{} does not offer {} ({}){}; it advertises {}",
        method.label(),
        quality.label(),
        quality.token(),
        reason,
        advertised.join(", ")
    ))
}

/// The render ratio for `(method, quality)`.
///
/// For an advertised pair this is the table's factor. For a pair the provider
/// does not advertise it is the advertised mode closest on the log scale, so a
/// settings value that was never sanitised (a config file edited by hand, a
/// provider switch mid-session) still produces a sane extent instead of a
/// divide-by-one — [`require`] is what refuses the pair at create time.
pub fn internal_factor(method: UpscaleMethod, quality: UpscaleQuality) -> f64 {
    if method == UpscaleMethod::Off {
        return 1.0;
    }
    if let Some(mode) = find(method, quality) {
        return mode.factor;
    }
    if let Some(mode) = find(method, nearest_available(method, quality)) {
        return mode.factor;
    }
    1.0
}

/// The advertised mode of `method` closest to `quality` on the log scale, so a
/// settings value that outlived its provider (or a mode that provider never
/// had) degrades to the nearest ratio it does offer instead of being dropped.
pub fn nearest_available(method: UpscaleMethod, quality: UpscaleQuality) -> UpscaleQuality {
    if quality_available(method, quality) {
        return quality;
    }
    let target = nominal_factor(quality).ln();
    let mut best = quality;
    let mut best_distance = f64::INFINITY;
    for mode in modes(method) {
        let distance = (mode.factor.ln() - target).abs();
        if distance < best_distance {
            best = mode.quality;
            best_distance = distance;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UpscaleMethod, UpscaleQuality};

    /// The providers' mode lists, one per provider.
    fn list(method: UpscaleMethod) -> Vec<UpscaleQuality> {
        let mut qualities = Vec::new();
        for mode in modes(method) {
            qualities.push(mode.quality);
        }
        qualities
    }

    /// The matrix the viewer ships, provider by provider.
    #[test]
    fn matrix_matches_the_provider_lists() {
        assert_eq!(
            list(UpscaleMethod::Dlss),
            vec![
                UpscaleQuality::NativeAa,
                UpscaleQuality::Quality,
                UpscaleQuality::Balanced,
                UpscaleQuality::Performance,
                UpscaleQuality::UltraPerformance,
            ]
        );
        let no_ultra = !quality_available(UpscaleMethod::Dlss, UpscaleQuality::UltraQuality);
        assert!(no_ultra, "DLSS must not offer Ultra Quality");
        for fsr in [
            UpscaleMethod::Fsr1,
            UpscaleMethod::Fsr2,
            UpscaleMethod::Fsr3,
            UpscaleMethod::Fsr4,
        ] {
            let no_ultra = !quality_available(fsr, UpscaleQuality::UltraQuality);
            assert!(no_ultra, "{fsr:?} must not offer Ultra Quality");
            for quality in [
                UpscaleQuality::Quality,
                UpscaleQuality::Balanced,
                UpscaleQuality::Performance,
                UpscaleQuality::UltraPerformance,
            ] {
                assert!(
                    quality_available(fsr, quality),
                    "{fsr:?} {}",
                    quality.token()
                );
            }
        }
        // FSR 3.1 and FSR 4 carry AMD's Native AA mode; FSR 1 is spatial and
        // FSR 2 predates the enum value.
        assert!(quality_available(
            UpscaleMethod::Fsr3,
            UpscaleQuality::NativeAa
        ));
        assert!(quality_available(
            UpscaleMethod::Fsr4,
            UpscaleQuality::NativeAa
        ));
        assert!(!quality_available(
            UpscaleMethod::Fsr1,
            UpscaleQuality::NativeAa
        ));
        assert!(!quality_available(
            UpscaleMethod::Fsr2,
            UpscaleQuality::NativeAa
        ));
        // XeSS: Native AA *and* Ultra Quality.
        assert!(quality_available(
            UpscaleMethod::Xess,
            UpscaleQuality::NativeAa
        ));
        assert!(quality_available(
            UpscaleMethod::Xess,
            UpscaleQuality::UltraQuality
        ));
        // Nothing is advertised for "Upscaling Off".
        assert!(modes(UpscaleMethod::Off).is_empty());
        for quality in UpscaleQuality::ALL {
            assert!(!quality_available(UpscaleMethod::Off, quality));
        }
    }

    /// Every advertised mode carries the vendor value its SDK documents, and
    /// no provider hands two modes the same value.
    #[test]
    fn every_mode_maps_to_its_documented_vendor_value() {
        let dlss = find(UpscaleMethod::Dlss, UpscaleQuality::NativeAa).unwrap();
        assert_eq!(dlss.vendor, 5); // NVSDK_NGX_PerfQuality_DLAA
        let dlss = find(UpscaleMethod::Dlss, UpscaleQuality::Quality).unwrap();
        assert_eq!(dlss.vendor, 2); // MaxQuality
        let dlss = find(UpscaleMethod::Dlss, UpscaleQuality::Balanced).unwrap();
        assert_eq!(dlss.vendor, 1);
        let dlss = find(UpscaleMethod::Dlss, UpscaleQuality::Performance).unwrap();
        assert_eq!(dlss.vendor, 0); // MaxPerf
        let dlss = find(UpscaleMethod::Dlss, UpscaleQuality::UltraPerformance).unwrap();
        assert_eq!(dlss.vendor, 3);
        let ffx = find(UpscaleMethod::Fsr3, UpscaleQuality::NativeAa).unwrap();
        assert_eq!(ffx.vendor, 0); // FFX_UPSCALE_QUALITY_MODE_NATIVEAA
        let ffx = find(UpscaleMethod::Fsr3, UpscaleQuality::Quality).unwrap();
        assert_eq!(ffx.vendor, 1);
        let ffx = find(UpscaleMethod::Fsr3, UpscaleQuality::Balanced).unwrap();
        assert_eq!(ffx.vendor, 2);
        let ffx = find(UpscaleMethod::Fsr3, UpscaleQuality::Performance).unwrap();
        assert_eq!(ffx.vendor, 3);
        let ffx = find(UpscaleMethod::Fsr3, UpscaleQuality::UltraPerformance).unwrap();
        assert_eq!(ffx.vendor, 4);
        let xess = find(UpscaleMethod::Xess, UpscaleQuality::UltraPerformance).unwrap();
        assert_eq!(xess.vendor, 100);
        let xess = find(UpscaleMethod::Xess, UpscaleQuality::Performance).unwrap();
        assert_eq!(xess.vendor, 101);
        let xess = find(UpscaleMethod::Xess, UpscaleQuality::Balanced).unwrap();
        assert_eq!(xess.vendor, 102);
        let xess = find(UpscaleMethod::Xess, UpscaleQuality::Quality).unwrap();
        assert_eq!(xess.vendor, 103);
        let xess = find(UpscaleMethod::Xess, UpscaleQuality::UltraQuality).unwrap();
        assert_eq!(xess.vendor, 104);
        let xess = find(UpscaleMethod::Xess, UpscaleQuality::NativeAa).unwrap();
        assert_eq!(xess.vendor, 106); // XESS_QUALITY_SETTING_AA
        for method in [
            UpscaleMethod::Dlss,
            UpscaleMethod::Fsr3,
            UpscaleMethod::Fsr4,
            UpscaleMethod::Xess,
        ] {
            let mut vendors = Vec::new();
            for mode in modes(method) {
                vendors.push(mode.vendor);
            }
            let count = vendors.len();
            vendors.sort_unstable();
            vendors.dedup();
            assert_eq!(vendors.len(), count, "{method:?} reuses a vendor value");
        }
    }

    /// Factors also encode the ratio the SDK documents for the vendor value.
    #[test]
    fn factors_match_the_documented_ratios() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        for mode in modes(UpscaleMethod::Dlss) {
            let expected = match mode.quality {
                UpscaleQuality::NativeAa => 1.0,
                UpscaleQuality::Quality => 1.0 / 1.5,
                UpscaleQuality::Balanced => 1.0 / 1.7,
                UpscaleQuality::Performance => 0.5,
                UpscaleQuality::UltraPerformance => 1.0 / 3.0,
                UpscaleQuality::UltraQuality => f64::NAN,
            };
            assert!(close(mode.factor, expected), "{:?}", mode.quality);
        }
        // XeSS: 3.0 / 2.0 / 1.7 / 1.5 / 1.3 / 1.0, exactly as xess.h documents.
        for mode in modes(UpscaleMethod::Xess) {
            let expected = match mode.quality {
                UpscaleQuality::NativeAa => 1.0,
                UpscaleQuality::UltraQuality => 1.0 / 1.3,
                UpscaleQuality::Quality => 1.0 / 1.5,
                UpscaleQuality::Balanced => 1.0 / 1.7,
                UpscaleQuality::Performance => 0.5,
                UpscaleQuality::UltraPerformance => 1.0 / 3.0,
            };
            assert!(close(mode.factor, expected), "{:?}", mode.quality);
        }
        // The advertised ladder never grows as quality drops.
        for method in UpscaleMethod::ALL {
            let mut previous = 1.0;
            for mode in modes(method) {
                assert!(mode.factor > 0.0 && mode.factor <= 1.0, "{method:?}");
                assert!(mode.factor <= previous, "{method:?} is out of order");
                previous = mode.factor;
            }
        }
    }

    /// [`require`] is the guard the providers use: no silent folding.
    #[test]
    fn require_refuses_unadvertised_modes_with_a_reason() {
        let error = require(UpscaleMethod::Dlss, UpscaleQuality::UltraQuality).unwrap_err();
        assert!(error.contains("0xbad00010"), "{error}");
        assert!(error.contains("DLAA"), "{error}");
        let error = require(UpscaleMethod::Fsr3, UpscaleQuality::UltraQuality).unwrap_err();
        assert!(error.contains("no Ultra Quality preset"), "{error}");
        let xess = require(UpscaleMethod::Xess, UpscaleQuality::NativeAa);
        assert_eq!(xess.unwrap().vendor, XESS_AA);
        assert!(require(UpscaleMethod::Off, UpscaleQuality::Quality).is_err());
        let dlss = require(UpscaleMethod::Dlss, UpscaleQuality::NativeAa);
        assert_eq!(dlss.unwrap().name, "DLAA");
    }

    /// A settings value that outlives its provider degrades to the nearest
    /// ratio the new provider offers; it never renders at a ratio nobody asked
    /// for.
    #[test]
    fn unavailable_modes_degrade_to_the_nearest_advertised_ratio() {
        let dlss = nearest_available(UpscaleMethod::Dlss, UpscaleQuality::UltraQuality);
        assert_eq!(dlss, UpscaleQuality::Quality);
        let fsr2 = nearest_available(UpscaleMethod::Fsr2, UpscaleQuality::NativeAa);
        assert_eq!(fsr2, UpscaleQuality::Quality);
        let fsr3 = nearest_available(UpscaleMethod::Fsr3, UpscaleQuality::UltraQuality);
        assert_eq!(fsr3, UpscaleQuality::Quality);
        // XeSS offers both, so nothing moves, and the factor is the nominal one.
        for quality in [
            UpscaleQuality::NativeAa,
            UpscaleQuality::UltraQuality,
            UpscaleQuality::UltraPerformance,
        ] {
            let chosen = nearest_available(UpscaleMethod::Xess, quality);
            assert_eq!(chosen, quality);
            let factor = internal_factor(UpscaleMethod::Xess, quality);
            assert!((factor - nominal_factor(quality)).abs() < 1e-9);
        }
        // Never a factor the provider does not advertise.
        for method in UpscaleMethod::ALL {
            for quality in UpscaleQuality::ALL {
                let chosen = nearest_available(method, quality);
                if method == UpscaleMethod::Off {
                    continue;
                }
                assert!(quality_available(method, chosen), "{method:?}/{quality:?}");
                let advertised = find(method, chosen).unwrap();
                assert_eq!(internal_factor(method, quality), advertised.factor);
            }
        }
        let off = internal_factor(UpscaleMethod::Off, UpscaleQuality::Quality);
        assert_eq!(off, 1.0);
    }
}
