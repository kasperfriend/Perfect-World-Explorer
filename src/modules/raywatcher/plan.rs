//! Raywatcher settings, and the frame plan derived from them.
//!
//! Everything in this file is plain data plus deterministic arithmetic: it says
//! *what* a frame should ray trace, at which budget, and with which
//! reconstruction provider tried first. Nothing here touches wgpu, a DLL or the
//! clock, so the whole decision surface is unit-testable on a headless runner —
//! which matters here, because the interesting failures (a disabled effect that
//! silently keeps its raster path, an upscaler that loses its jitter, a
//! reconstruction provider that is selected but cannot run) are decisions, not
//! pixels.

/// How the rays are traced.
///
/// `Software` walks the CPU BVH in a compute shader; `Hardware` binds the same
/// world geometry as a Vulkan ray-query acceleration structure and traces it on
/// the GPU's fixed-function ray traversal units. Both paths write the same
/// G-buffer and the same ray outputs, so the denoiser, the reconstruction and
/// the upscaler never know which one produced a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RayTracingMode {
    /// Raywatcher draws nothing and every original effect stays exactly as it
    /// was before the module existed.
    #[default]
    Off,
    /// CPU-side BVH traversal, in a compute shader, on any wgpu adapter.
    Software,
    /// `VK_KHR_ray_query` traversal of a scene BLAS/TLAS on supporting GPUs.
    Hardware,
}

impl RayTracingMode {
    pub const ALL: [RayTracingMode; 3] = [Self::Off, Self::Software, Self::Hardware];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Software => "Software (CPU)",
            Self::Hardware => "Hardware (GPU)",
        }
    }

    /// Persisted spelling. Unknown spellings keep the default rather than
    /// silently becoming a different mode.
    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Software => "software",
            Self::Hardware => "hardware",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "software" | "cpu" => Some(Self::Software),
            "hardware" | "gpu" => Some(Self::Hardware),
            _ => None,
        }
    }

    pub fn enabled(self) -> bool {
        self != Self::Off
    }
}

/// Which ray-reconstruction provider replaces the denoiser's output before the
/// upscaler runs.
///
/// Ray reconstruction is *not* a denoiser: it consumes the noisy ray signals,
/// the G-buffer and the motion vectors and produces a cleaner image with the
/// temporal guide data the vendor's network was trained on. It is therefore
/// mutually exclusive with the in-house denoiser — the selected provider
/// replaces it, exactly as the ray-traced effects replace the raster ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RayReconstruction {
    /// Raywatcher's own variance-guided à-trous denoiser. Runs on every backend
    /// and every platform.
    #[default]
    Off,
    /// NVIDIA DLSS Ray Reconstruction (`nvngx_dlssd.dll`, NGX feature
    /// `NVSDK_NGX_Feature_RayReconstruction`). RTX GPUs, Windows + DX12 — the
    /// same platform gate the DLSS upscaler already lives behind.
    DlssRayReconstruction,
    /// AMD FSR Ray Regeneration (`amd_fidelityfx_denoiser_dx12.dll`, FFX
    /// denoiser API). RDNA GPUs get the machine-learning path; any other DX12
    /// GPU gets the same API backed by the spatial fallback the SDK ships.
    FsrRayRegeneration,
}

impl RayReconstruction {
    pub const ALL: [RayReconstruction; 3] = [
        Self::Off,
        Self::DlssRayReconstruction,
        Self::FsrRayRegeneration,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Raywatcher denoiser",
            Self::DlssRayReconstruction => "DLSS Ray Reconstruction",
            Self::FsrRayRegeneration => "FSR Ray Regeneration",
        }
    }

    /// Short form for the on-screen badge.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Off => "denoise",
            Self::DlssRayReconstruction => "DLSS RR",
            Self::FsrRayRegeneration => "FSR RR",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::DlssRayReconstruction => "dlssrr",
            Self::FsrRayRegeneration => "fsrrr",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" | "denoiser" => Some(Self::Off),
            "dlssrr" | "dlss" => Some(Self::DlssRayReconstruction),
            "fsrrr" | "fsr" => Some(Self::FsrRayRegeneration),
            _ => None,
        }
    }

    /// The SDK file this provider needs, for the module's status line.
    pub fn sdk_file(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::DlssRayReconstruction => Some("nvngx_dlssd.dll"),
            Self::FsrRayRegeneration => Some("amd_fidelityfx_denoiser_dx12.dll"),
        }
    }
}

/// The effects Raywatcher can replace. Each one is an independent switch, and
/// the module's whole point is that turning one on *removes* the corresponding
/// raster effect: the world is rebound to the ray-traced lighting model, so a
/// disabled effect is absent rather than doubled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Effects {
    /// Sun and local-light shadow rays, with the penumbra the light radius
    /// implies.
    pub shadows: bool,
    /// Direct sun + sky lighting evaluated per pixel from the ray-traced
    /// visibility and the region's authored light record. Off leaves the
    /// surfaces with ambient only.
    pub lighting: bool,
    /// Specular reflection rays for water and glossy scenery.
    pub reflections: bool,
    /// Horizon-sampled ambient occlusion in the hemisphere above each pixel.
    pub ambient_occlusion: bool,
    /// One or more diffuse bounce rays, i.e. colour bleeding. The bounce count
    /// is [`Plan::bounces`]; this is the on/off gate the UI owns.
    pub global_illumination: bool,
    /// Water refraction/absorption on submerged pixels (Beer-Lambert).
    pub refraction: bool,
}

impl Default for Effects {
    /// Everything on: the module's default state is the full ray-traced world,
    /// because a partially-enabled replacement is the bug this module exists to
    /// remove.
    fn default() -> Self {
        Self {
            shadows: true,
            lighting: true,
            reflections: true,
            ambient_occlusion: true,
            global_illumination: true,
            refraction: true,
        }
    }
}

impl Effects {
    /// Number of enabled effects, for the plan's summary.
    pub fn count(&self) -> usize {
        [
            self.shadows,
            self.lighting,
            self.reflections,
            self.ambient_occlusion,
            self.global_illumination,
            self.refraction,
        ]
        .iter()
        .filter(|on| **on)
        .count()
    }
}

/// Everything the Raywatcher tab owns.
#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    pub mode: RayTracingMode,
    pub reconstruction: RayReconstruction,
    pub effects: Effects,
    /// Indirect ray bounces for diffuse transport. `0` means "no diffuse
    /// transport at all", which is a legal way to see direct light only.
    pub bounces: u32,
    /// Primary rays per pixel (1..=4). More samples trade frame time for noise.
    pub samples: u32,
    /// Denoiser iterations, 1..=5; more iterations are smoother and softer.
    pub denoise_iterations: u32,
    /// Shadow angular radius in degrees, the sun's own penumbra.
    pub sun_radius_degrees: f32,
    /// Maximum traced distance in world units, 0 = the camera's far plane.
    pub ray_length: f32,
    /// Strength of the ambient term, 0..=2.
    pub ambient_strength: f32,
    /// Multiplier on radiance gathered by indirect bounces, 0..=2. This is the
    /// only artistic knob on the transport, so sky fill can be dialled without
    /// changing the bounce count.
    pub bounce_strength: f32,
    /// Reflections' roughness cutoff: surfaces rougher than this use diffuse
    /// transport instead of a mirror ray (0..=1).
    pub reflection_roughness_cutoff: f32,
    /// Show the ray-traced image only through the raster frame's motion
    /// vectors, off by default: a debug switch that proves the reprojection is
    /// real.
    pub show_denoiser_input: bool,
    /// Explicit opt-in to zero/replace raster lighting terms instead of
    /// layering ray tracing over the full original world render. Defaults to
    /// `false` so textured terrain, buildings, trees, water, effects, sky and
    /// fog stay intact.
    pub replace_raster: bool,
    /// Folder the reconstruction SDKs are loaded from. Empty means
    /// `PW_UPSCALER_SDK_DIR`, then `<exe>/sdk`, the same rule the upscalers use.
    pub sdk_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: RayTracingMode::Off,
            reconstruction: RayReconstruction::Off,
            effects: Effects::default(),
            bounces: 2,
            samples: 1,
            denoise_iterations: 3,
            sun_radius_degrees: 0.6,
            ray_length: 0.,
            ambient_strength: 1.,
            bounce_strength: 1.,
            reflection_roughness_cutoff: 0.4,
            show_denoiser_input: false,
            replace_raster: false,
            sdk_dir: String::new(),
        }
    }
}

/// Bounce-count bounds. Beyond a handful, a 1-spp tracer's variance explodes
/// and the denoiser cannot recover the image.
pub const BOUNCES_RANGE: (u32, u32) = (0, 6);
/// Sample-count bounds.
pub const SAMPLES_RANGE: (u32, u32) = (1, 4);
/// Denoiser iteration bounds.
pub const DENOISE_RANGE: (u32, u32) = (1, 5);

impl Settings {
    /// Clamp every numeric field into its advertised range. A settings file is
    /// user-editable text, so nothing downstream may trust it.
    pub fn sanitise(&mut self) {
        self.bounces = self.bounces.clamp(BOUNCES_RANGE.0, BOUNCES_RANGE.1);
        self.samples = self.samples.clamp(SAMPLES_RANGE.0, SAMPLES_RANGE.1);
        self.denoise_iterations = self
            .denoise_iterations
            .clamp(DENOISE_RANGE.0, DENOISE_RANGE.1);
        self.sun_radius_degrees = finite_or(self.sun_radius_degrees, 0.6).clamp(0., 10.);
        self.ray_length = finite_or(self.ray_length, 0.).clamp(0., 64000.);
        self.ambient_strength = finite_or(self.ambient_strength, 1.).clamp(0., 2.);
        self.bounce_strength = finite_or(self.bounce_strength, 1.).clamp(0., 2.);
        self.reflection_roughness_cutoff =
            finite_or(self.reflection_roughness_cutoff, 0.4).clamp(0., 1.);
    }

    /// True when this configuration replaces at least one original effect, i.e.
    /// when the renderer must draw the ray-traced world instead of the raster
    /// one.
    pub fn replaces_effects(&self) -> bool {
        self.mode.enabled() && self.effects.count() > 0
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// What one frame's ray pass should do. Derived from [`Settings`] plus the
/// frame's resolution and the device's capabilities; the renderer consults only
/// this, never the settings, so a mid-frame settings change cannot make the
/// passes disagree about the extent or the sample count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    /// Trace rays at all.
    pub trace: bool,
    /// Half (or quarter) resolution ray grid, chosen so the ray pass fits the
    /// frame budget. The upscaler reconstructs to the render resolution, and the
    /// render resolution is already the upscaler's input extent.
    pub ray_scale: u32,
    pub samples: u32,
    pub bounces: u32,
    pub denoise_iterations: u32,
    pub effects: Effects,
    /// The reconstruction provider this frame will actually try. When the
    /// provider's SDK is missing the plan already says `Off`, so the renderer
    /// and the UI cannot disagree about which one produced the pixels.
    pub reconstruction: RayReconstruction,
    /// Ray grid size, in pixels.
    pub grid: (u32, u32),
    /// Render extent the rays are reconstructed into.
    pub render: (u32, u32),
    /// Explicit opt-in to replace raster lighting terms. `false` (default)
    /// keeps the full raster world render untouched and composites RT on top.
    pub replace_raster: bool,
}

impl Plan {
    /// The plan for a disabled module: no targets, no passes.
    pub const OFF: Plan = Plan {
        trace: false,
        ray_scale: 1,
        samples: 1,
        bounces: 0,
        denoise_iterations: 0,
        effects: Effects {
            shadows: false,
            lighting: false,
            reflections: false,
            ambient_occlusion: false,
            global_illumination: false,
            refraction: false,
        },
        reconstruction: RayReconstruction::Off,
        grid: (0, 0),
        render: (0, 0),
        replace_raster: false,
    };

    /// Largest ray grid edge a software trace can be asked for at 1 sample. A
    /// 1080p render extent lands on a half-resolution grid (960x540), which is
    /// the documented Software default.
    pub const SOFTWARE_GRID_BUDGET: u64 = 960 * 540;
    /// The same budget for a hardware trace, which is roughly an order of
    /// magnitude cheaper per ray.
    pub const HARDWARE_GRID_BUDGET: u64 = 1920 * 1080;

    /// Decide one frame.
    ///
    /// * `mode`/`settings` come from the module's settings.
    /// * `render` is the extent the world is drawn at — with any upscaler
    ///   running this is the upscaler's *input* extent, so the ray image is
    ///   reconstructed by that upscaler exactly like the raster image would be.
    /// * `hardware_rays` says whether the adapter really exposes ray queries.
    /// * `reconstruction_available` is the provider's own gate (SDK present and
    ///   platform supported); a provider that cannot run is never planned.
    pub fn decide(
        settings: &Settings,
        render: (u32, u32),
        hardware_rays: bool,
        reconstruction_available: impl Fn(RayReconstruction) -> bool,
    ) -> Plan {
        if !settings.replaces_effects() {
            return Plan::OFF;
        }
        if settings.mode == RayTracingMode::Hardware && !hardware_rays {
            // The caller falls back to Software before it gets here, but a plan
            // must never promise a path the adapter cannot run.
            return Plan::OFF;
        }
        let render = (render.0.max(1), render.1.max(1));
        let budget = match settings.mode {
            RayTracingMode::Hardware => Self::HARDWARE_GRID_BUDGET,
            _ => Self::SOFTWARE_GRID_BUDGET,
        };
        let ray_scale = grid_scale(render, budget, settings.samples);
        let mut reconstruction = settings.reconstruction;
        if reconstruction != RayReconstruction::Off && !reconstruction_available(reconstruction) {
            reconstruction = RayReconstruction::Off;
        }
        Plan {
            trace: true,
            ray_scale,
            samples: settings.samples,
            bounces: settings.bounces,
            denoise_iterations: settings.denoise_iterations,
            effects: settings.effects,
            reconstruction,
            grid: ((render.0 / ray_scale).max(1), (render.1 / ray_scale).max(1)),
            render,
            replace_raster: settings.replace_raster,
        }
    }
}

/// Smallest `1 / 2^k` downsample of `render` whose pixel count times `samples`
/// stays inside `budget`. Never upsamples: the result is `>= 1`.
///
/// The cost is non-increasing in `k`, so the first scale that fits is also the
/// finest one that fits. When even the coarsest grid is still over budget (a
/// pathological budget, never the shipped constants) the coarsest legal scale is
/// returned so the caller still allocates something.
pub fn grid_scale(render: (u32, u32), budget: u64, samples: u32) -> u32 {
    let samples = samples.max(1) as u64;
    let cost = |scale: u32| {
        let w = (render.0 / scale).max(1) as u64;
        let h = (render.1 / scale).max(1) as u64;
        w * h * samples
    };
    // Eight halvings cover any real extent; the loop is bounded so a zero
    // extent cannot spin.
    let mut scale = 1u32;
    for _ in 0..8 {
        if cost(scale) <= budget {
            return scale;
        }
        scale *= 2;
    }
    scale
}

/// Which reconstruction providers the build and platform can reach. Split out
/// from the plan so the UI can explain each disabled row, and so the tests can
/// pin the gate without a device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Availability {
    pub dlss_rr: bool,
    pub fsr_rr: bool,
}

impl Availability {
    pub const NONE: Availability = Availability {
        dlss_rr: false,
        fsr_rr: false,
    };

    pub fn get(self, provider: RayReconstruction) -> bool {
        match provider {
            RayReconstruction::Off => true,
            RayReconstruction::DlssRayReconstruction => self.dlss_rr,
            RayReconstruction::FsrRayRegeneration => self.fsr_rr,
        }
    }

    pub fn any(self) -> bool {
        self.dlss_rr || self.fsr_rr
    }

    /// Why a provider is unavailable, for the module's status line. `None` means
    /// it is available.
    pub fn reason(self, provider: RayReconstruction) -> Option<&'static str> {
        if self.get(provider) {
            return None;
        }
        match provider {
            RayReconstruction::Off => None,
            RayReconstruction::DlssRayReconstruction => {
                Some("needs Windows + DX12, an RTX GPU and nvngx_dlssd.dll in the SDK folder")
            }
            RayReconstruction::FsrRayRegeneration => {
                Some("needs Windows + DX12 and amd_fidelityfx_denoiser_dx12.dll in the SDK folder")
            }
        }
    }

    /// One-line summary for the tab.
    pub fn summary(self) -> String {
        if self.any() {
            let mut parts = Vec::new();
            if self.dlss_rr {
                parts.push("DLSS RR");
            }
            if self.fsr_rr {
                parts.push("FSR RR");
            }
            format!("Ray reconstruction available: {}", parts.join(", "))
        } else {
            "Ray reconstruction unavailable on this platform; the Raywatcher denoiser runs instead"
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::default()
    }

    #[test]
    fn off_is_the_shipped_default_and_means_nothing_changes() {
        let s = settings();
        assert_eq!(s.mode, RayTracingMode::Off);
        assert!(!s.replaces_effects());
        let plan = Plan::decide(&s, (1920, 1080), true, |_| true);
        assert_eq!(plan, Plan::OFF);
    }

    #[test]
    fn every_enabled_effect_replaces_its_raster_counterpart() {
        let mut s = settings();
        s.mode = RayTracingMode::Software;
        // With every effect off there is nothing to replace, so the renderer
        // keeps the raster world: that is the guard against a module that turns
        // the raster effects off and then draws nothing.
        s.effects = Effects {
            shadows: false,
            lighting: false,
            reflections: false,
            ambient_occlusion: false,
            global_illumination: false,
            refraction: false,
        };
        assert!(!s.replaces_effects());
        assert_eq!(
            Plan::decide(&s, (1280, 720), false, |_| true),
            Plan::OFF,
            "no enabled effect must not disable the raster path"
        );
        for enable in 0..6 {
            let mut e = Effects {
                shadows: false,
                lighting: false,
                reflections: false,
                ambient_occlusion: false,
                global_illumination: false,
                refraction: false,
            };
            match enable {
                0 => e.shadows = true,
                1 => e.lighting = true,
                2 => e.reflections = true,
                3 => e.ambient_occlusion = true,
                4 => e.global_illumination = true,
                _ => e.refraction = true,
            }
            s.effects = e;
            assert!(s.replaces_effects(), "effect {enable} must replace");
            let plan = Plan::decide(&s, (1280, 720), false, |_| true);
            assert!(plan.trace);
            assert_eq!(plan.effects.count(), 1);
        }
    }

    #[test]
    fn hardware_without_ray_queries_is_never_planned() {
        let mut s = settings();
        s.mode = RayTracingMode::Hardware;
        assert_eq!(Plan::decide(&s, (1920, 1080), false, |_| true), Plan::OFF);
        s.mode = RayTracingMode::Software;
        assert!(Plan::decide(&s, (1920, 1080), false, |_| true).trace);
    }

    #[test]
    fn software_defaults_to_half_resolution_and_hardware_to_full_resolution() {
        let mut s = settings();
        s.mode = RayTracingMode::Software;
        let plan = Plan::decide(&s, (1920, 1080), false, |_| true);
        assert_eq!(plan.ray_scale, 2, "Software traces 960x540 at 1080p");
        assert_eq!(plan.grid, (960, 540));
        s.mode = RayTracingMode::Hardware;
        let plan = Plan::decide(&s, (1920, 1080), true, |_| true);
        assert_eq!(plan.ray_scale, 1, "Hardware traces the render extent");
        assert_eq!(plan.grid, (1920, 1080));
    }

    #[test]
    fn more_samples_trade_resolution_for_noise() {
        let mut s = settings();
        s.mode = RayTracingMode::Software;
        s.samples = 4;
        let plan = Plan::decide(&s, (1920, 1080), false, |_| true);
        // The budget counts samples *and* pixels, so four samples buy the
        // quarter-rate grid instead of the half-rate one.
        assert_eq!(plan.ray_scale, 4);
        assert_eq!(plan.grid, (480, 270));
        // The same extent at one sample keeps the documented half rate.
        s.samples = 1;
        let plan = Plan::decide(&s, (1920, 1080), false, |_| true);
        assert_eq!(plan.ray_scale, 2);
        assert_eq!(plan.grid, (960, 540));
        // The scale only moves in one direction per axis: more samples never buy
        // a finer grid, a bigger budget never buys a coarser one.
        for budget in [64_000u64, 120_000, 300_000, 518_400, 2_073_600] {
            for samples in SAMPLES_RANGE.0..=SAMPLES_RANGE.1 {
                assert!(grid_scale((1920, 1080), budget, samples) >= 1);
                if samples > 1 {
                    assert!(
                        grid_scale((1920, 1080), budget, samples)
                            >= grid_scale((1920, 1080), budget, 1),
                        "budget {budget} samples {samples}"
                    );
                }
            }
        }
        assert!(
            grid_scale((1920, 1080), 2_073_600, 1) < grid_scale((1920, 1080), 64_000, 1),
            "a bigger budget must not halve more"
        );
    }

    #[test]
    fn the_ray_grid_never_upsamples_or_collapses() {
        // A budget that cannot be met still yields a legal, coarse scale rather
        // than zero — the caller then allocates the smallest grid it can.
        assert_eq!(grid_scale((1920, 1080), 0, 1), 256);
        assert_eq!(grid_scale((64, 32), Plan::HARDWARE_GRID_BUDGET, 1), 1);
        assert_eq!(grid_scale((1, 1), Plan::SOFTWARE_GRID_BUDGET, 4), 1);
        // Non-square and degenerate extents keep at least one pixel per axis.
        for extent in [(2560, 1), (1, 4096), (3, 3), (0, 0)] {
            let scale = grid_scale(extent, Plan::SOFTWARE_GRID_BUDGET, 1);
            assert!((extent.0 / scale).max(1) >= 1);
            assert!((extent.1 / scale).max(1) >= 1);
        }
    }

    #[test]
    fn a_provider_that_cannot_run_is_never_planned() {
        let mut s = settings();
        s.mode = RayTracingMode::Software;
        s.reconstruction = RayReconstruction::DlssRayReconstruction;
        let denied = Plan::decide(&s, (1920, 1080), false, |_| false);
        assert_eq!(denied.reconstruction, RayReconstruction::Off);
        let allowed = Plan::decide(&s, (1920, 1080), false, |p| {
            p == RayReconstruction::DlssRayReconstruction
        });
        assert_eq!(
            allowed.reconstruction,
            RayReconstruction::DlssRayReconstruction
        );
    }

    #[test]
    fn both_reconstruction_providers_are_selectable_and_have_their_own_sdk() {
        assert_eq!(RayReconstruction::ALL.len(), 3);
        assert_eq!(
            RayReconstruction::DlssRayReconstruction.sdk_file(),
            Some("nvngx_dlssd.dll")
        );
        assert_eq!(
            RayReconstruction::FsrRayRegeneration.sdk_file(),
            Some("amd_fidelityfx_denoiser_dx12.dll")
        );
        assert_eq!(RayReconstruction::Off.sdk_file(), None);
        // Both are distinct rows with distinct tokens, so the settings file
        // round-trips them.
        let tokens: Vec<_> = RayReconstruction::ALL.iter().map(|r| r.token()).collect();
        let mut unique = tokens.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(tokens.len(), unique.len());
        for row in RayReconstruction::ALL {
            assert_eq!(RayReconstruction::parse(row.token()), Some(row));
        }
    }

    #[test]
    fn modes_and_providers_round_trip_through_their_tokens() {
        for mode in RayTracingMode::ALL {
            assert_eq!(RayTracingMode::parse(mode.token()), Some(mode));
        }
        // Unknown spellings keep the default instead of guessing.
        assert_eq!(RayTracingMode::parse("nonsense"), None);
        assert_eq!(RayReconstruction::parse("nonsense"), None);
        assert!(RayTracingMode::Hardware.enabled());
        assert!(!RayTracingMode::Off.enabled());
    }

    #[test]
    fn settings_from_a_file_are_clamped_into_range() {
        let mut s = Settings::default();
        s.bounces = 99;
        s.samples = 0;
        s.denoise_iterations = 0;
        s.sun_radius_degrees = f32::NAN;
        s.ray_length = f32::INFINITY;
        s.ambient_strength = -4.;
        s.bounce_strength = 9.;
        s.reflection_roughness_cutoff = 7.;
        s.sanitise();
        assert_eq!(s.bounces, BOUNCES_RANGE.1);
        assert_eq!(s.samples, SAMPLES_RANGE.0);
        assert_eq!(s.denoise_iterations, DENOISE_RANGE.0);
        assert_eq!(s.sun_radius_degrees, 0.6);
        assert_eq!(s.ray_length, 0.);
        assert_eq!(s.ambient_strength, 0.);
        assert_eq!(s.bounce_strength, 2.);
        assert_eq!(s.reflection_roughness_cutoff, 1.);
    }

    #[test]
    fn availability_explains_every_disabled_row() {
        let none = Availability::NONE;
        assert!(!none.any());
        assert!(none.get(RayReconstruction::Off));
        assert!(none
            .reason(RayReconstruction::DlssRayReconstruction)
            .is_some());
        assert!(none.reason(RayReconstruction::FsrRayRegeneration).is_some());
        let all = Availability {
            dlss_rr: true,
            fsr_rr: true,
        };
        assert!(all.any());
        assert!(all
            .reason(RayReconstruction::DlssRayReconstruction)
            .is_none());
        assert!(all.reason(RayReconstruction::FsrRayRegeneration).is_none());
        assert!(all.summary().contains("DLSS RR"));
    }

    #[test]
    fn the_plan_carries_the_render_extent_the_upscaler_uses() {
        let mut s = settings();
        s.mode = RayTracingMode::Hardware;
        // The caller passes the upscaler's *input* extent here; the plan echoes
        // it unchanged so the ray image is reconstructed by that upscaler.
        let plan = Plan::decide(&s, (1280, 720), true, |_| true);
        assert_eq!(plan.render, (1280, 720));
        assert_eq!(plan.grid, (1280, 720));
        // A degenerate extent still produces a one-pixel plan instead of a
        // zero-sized texture.
        let plan = Plan::decide(&s, (0, 0), true, |_| true);
        assert_eq!(plan.render, (1, 1));
        assert_eq!(plan.grid, (1, 1));
    }
}
