//! Raywatcher: real ray-traced lighting for the world, in place of the raster
//! effects, with the upscaler still reconstructing the final image.
//!
//! # What it owns
//!
//! When the module is on, the ray pass *replaces* the effects it covers rather
//! than adding to them: the raster sun term, the sky ambient, the reflection
//! submissions and the fog occlusion term are switched off through
//! [`frame::RasterOverrides`], and the ray-traced versions are drawn instead.
//! Turning an effect off in the tab therefore removes both the rays and the
//! darkening — there is never a state where the world is half raster and half
//! ray traced.
//!
//! # The upscaler contract
//!
//! The ray image is produced at the *render* extent — the upscaler's input —
//! and fed to it exactly as the raster image would be, with the same motion
//! vectors and the same depth. So:
//!
//! * upscaling is never disabled while Raywatcher runs; the ray image is what
//!   gets upscaled,
//! * frame generation keeps working, because it consumes the upscaler's output
//!   and Raywatcher never touches that stage,
//! * and Ray Reconstruction, when selected, replaces the in-house denoiser's
//!   output on its way into that same upscaler.
//!
//! # Software or Hardware
//!
//! Both modes run the same shader and write the same G-buffer. `Software` builds
//! the BVH on the CPU (see [`bvh`]) and uploads it; `Hardware` is the same tree
//! in a Vulkan ray-query acceleration structure when the adapter exposes one.
//! A device without ray queries keeps the module on `Software` rather than
//! silently drawing nothing, and the tab says so.

pub mod bvh;
pub mod denoise;
pub mod frame;
pub mod harvest;
pub mod plan;
pub mod shade;

#[cfg(not(target_arch = "wasm32"))]
pub mod gpu;
#[cfg(not(target_arch = "wasm32"))]
pub mod reconstruct;

pub use plan::{
    Availability, Effects, Plan, RayReconstruction, RayTracingMode, Settings, BOUNCES_RANGE,
    DENOISE_RANGE, SAMPLES_RANGE,
};

use std::path::{Path, PathBuf};

/// The module's persisted section name in `settings.ini`.
pub const SECTION: &str = "raywatcher";

/// Everything the host needs to know about the module this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// Whether the ray pass replaces any raster effect this frame.
    pub active: bool,
    /// The plan the renderer must use.
    pub plan: Plan,
    /// What to switch off in the raster path.
    pub overrides: frame::RasterOverrides,
    /// One line for the tab: mode, grid, reconstruction and the effects that
    /// are being replaced.
    pub summary: String,
    /// Why the module is not doing what the settings ask, when it is not.
    pub note: Option<String>,
}

impl Status {
    pub fn inactive(note: Option<String>) -> Self {
        Self {
            active: false,
            plan: Plan::OFF,
            overrides: frame::RasterOverrides::NONE,
            summary: "off".to_string(),
            note,
        }
    }
}

/// The module's own state. Settings are the only thing that survives a restart;
/// everything else is derived.
pub struct Raywatcher {
    settings: Settings,
    availability: Availability,
    /// Sampled adapter capability. `None` until the device is queried.
    hardware_rays: bool,
    /// The SDK folder the reconstruction providers load from, resolved once.
    sdk_dir: PathBuf,
    /// Frame index, used for the temporal sequence and the history reset.
    frame: u32,
    /// Set when the previous frame was reset; the next `status` clears it.
    pending_reset: bool,
    /// Why hardware rays are unavailable, for the tab.
    hardware_note: Option<String>,
    /// The last frame's status, so the UI can show what the renderer did
    /// rather than what it was asked to do.
    last: Status,
}

impl Default for Raywatcher {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            availability: Availability::NONE,
            hardware_rays: false,
            sdk_dir: PathBuf::new(),
            frame: 0,
            pending_reset: true,
            hardware_note: None,
            last: Status::inactive(None),
        }
    }
}

impl Raywatcher {
    /// Create the module, resolving the SDK folder the same way the upscalers
    /// do: the setting if present, then `PW_UPSCALER_SDK_DIR`, then `<exe>/sdk`.
    pub fn new(exe_dir: &Path) -> Self {
        let mut this = Self::default();
        this.sdk_dir = resolve_sdk_dir(&this.settings.sdk_dir, exe_dir);
        this
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: Settings) {
        if settings.mode != self.settings.mode
            || settings.reconstruction != self.settings.reconstruction
            || settings.samples != self.settings.samples
        {
            self.pending_reset = true;
        }
        self.settings = settings;
        self.settings.sanitise();
    }

    pub fn availability(&self) -> Availability {
        self.availability
    }

    pub fn hardware_available(&self) -> bool {
        self.hardware_rays
    }

    pub fn hardware_note(&self) -> Option<&str> {
        self.hardware_note.as_deref()
    }

    /// Sample the adapter once the device exists.
    ///
    /// `ray_queries` is the feature bit the adapter advertises,
    /// `sdk_present` says which reconstruction DLLs were found in
    /// [`Self::sdk_dir`]. Keeping both as arguments (rather than reading them
    /// here) is what makes the whole decision surface testable without a GPU.
    pub fn configure(&mut self, ray_queries: bool, sdk_present: (bool, bool)) {
        self.hardware_rays = ray_queries;
        self.hardware_note = if ray_queries {
            None
        } else {
            Some("this adapter does not expose VK_KHR_ray_query; Software mode runs instead".into())
        };
        self.availability = Availability {
            dlss_rr: sdk_present.0,
            fsr_rr: sdk_present.1,
        };
        // A provider that is no longer available cannot stay selected: that
        // would leave the tab claiming DLSS RR while the denoiser ran.
        if !self.availability.get(self.settings.reconstruction) {
            self.settings.reconstruction = RayReconstruction::Off;
        }
        // Hardware mode without the feature is not a mode at all.
        if self.settings.mode == RayTracingMode::Hardware && !ray_queries {
            self.settings.mode = RayTracingMode::Software;
        }
        self.pending_reset = true;
    }

    /// The mode the renderer should actually use: `Hardware` degrades to
    /// `Software` when the adapter cannot do ray queries.
    pub fn effective_mode(&self) -> RayTracingMode {
        match self.settings.mode {
            RayTracingMode::Hardware if !self.hardware_rays => RayTracingMode::Software,
            other => other,
        }
    }

    /// Forget the history (the host calls this on a camera jump).
    pub fn reset_history(&mut self) {
        self.pending_reset = true;
    }

    /// Whether the renderer must clear the temporal history this frame.
    pub fn take_history_reset(&mut self) -> bool {
        let reset = self.pending_reset;
        self.pending_reset = false;
        reset
    }

    /// Advance the frame counter. Called once per rendered frame.
    pub fn next_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn frame_index(&self) -> u32 {
        self.frame
    }

    pub fn sdk_dir(&self) -> &Path {
        &self.sdk_dir
    }

    /// The plan for this frame, given the extent the world is rendered at.
    ///
    /// This is the only place a [`Plan`] is created, so the renderer, the
    /// pass and the tab cannot disagree about what is happening.
    pub fn plan(&self, render_extent: (u32, u32), history_valid: bool) -> Plan {
        let mode = self.effective_mode();
        let mut effective = self.settings.clone();
        effective.mode = mode;
        let availability = self.availability;
        // `history_valid` deliberately does not change the plan: a frame with
        // no history still traces at the same extent and the same sample count,
        // it just starts the accumulator over. That is expressed in the frame
        // parameters (`history_valid`), not by changing what the plan asks for,
        // so switching a provider on or off never also moves the render
        // extent underneath the upscaler.
        let _ = history_valid;
        Plan::decide(&effective, render_extent, self.hardware_rays, |provider| {
            availability.get(provider)
        })
    }

    /// Build this frame's status and remember it for the tab.
    pub fn status(&mut self, render_extent: (u32, u32), history_valid: bool) -> Status {
        let plan = self.plan(render_extent, history_valid);
        let overrides = frame::RasterOverrides::for_effects(plan.effects, plan.trace);
        let note = if !self.settings.mode.enabled() {
            None
        } else if !plan.trace {
            Some(
                "Raywatcher is on but no effect is enabled, so the raster world is drawn unchanged."
                    .to_string(),
            )
        } else if plan.effects.count() < self.settings.effects.count() {
            Some("Some effects could not be planned this frame.".to_string())
        } else {
            None
        };
        let summary = if plan.trace {
            format!(
                "{} | {}x{} rays | {} bounce{} | {}",
                self.effective_mode().label(),
                plan.grid.0,
                plan.grid.1,
                plan.bounces,
                if plan.bounces == 1 { "" } else { "s" },
                plan.reconstruction.label(),
            )
        } else if self.settings.mode.enabled() {
            "on, nothing replaced".to_string()
        } else {
            "off".to_string()
        };
        let status = Status {
            active: plan.trace,
            plan,
            overrides,
            summary,
            note,
        };
        self.last = status.clone();
        status
    }


    /// The module's settings tab, drawn in the Modules page beside Walker and
    /// Populator. It edits `self.settings` in place and reports what the last
    /// frame actually did, so a provider that silently fell back is visible
    /// here rather than inferred from the image.
    pub fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut settings = self.settings.clone();
        ui.horizontal(|ui| {
            ui.label("Mode");
            for mode in RayTracingMode::ALL {
                let enabled = mode != RayTracingMode::Hardware || self.hardware_rays;
                let response = ui.add_enabled(
                    enabled,
                    egui::SelectableLabel::new(settings.mode == mode, mode.label()),
                );
                if response.clicked() {
                    settings.mode = mode;
                }
                if mode == RayTracingMode::Hardware && !self.hardware_rays {
                    response.on_hover_text(
                        self.hardware_note
                            .as_deref()
                            .unwrap_or("this adapter does not expose ray queries"),
                    );
                }
            }
        });
        ui.label(
            egui::RichText::new(
                "Software builds the BVH on the CPU and walks it in a compute shader; Hardware uses the adapter's ray-query units. Both feed the same denoiser, reconstruction and upscaler.",
            )
            .small()
            .weak(),
        );
        ui.separator();
        ui.heading("Replaced effects");
        ui.label(
            egui::RichText::new(
                "Every effect ticked here has its raster version switched off and its ray-traced version drawn instead — there is no state where both run.",
            )
            .small()
            .weak(),
        );
        let mut effects = settings.effects;
        let mut changed_effects = false;
        for (label, slot) in [
            ("Shadows", &mut effects.shadows),
            ("Lighting", &mut effects.lighting),
            ("Reflections", &mut effects.reflections),
            ("Ambient occlusion", &mut effects.ambient_occlusion),
            ("Global illumination", &mut effects.global_illumination),
            ("Water refraction", &mut effects.refraction),
        ] {
            changed_effects |= ui.checkbox(slot, label).changed();
        }
        if changed_effects {
            settings.effects = effects;
        }
        ui.separator();
        ui.heading("Quality");
        ui.add(
            egui::Slider::new(
                &mut settings.bounces,
                BOUNCES_RANGE.0..=BOUNCES_RANGE.1,
            )
            .text("light bounces"),
        );
        ui.add(
            egui::Slider::new(
                &mut settings.samples,
                SAMPLES_RANGE.0..=SAMPLES_RANGE.1,
            )
            .text("rays per pixel"),
        );
        ui.add(
            egui::Slider::new(
                &mut settings.denoise_iterations,
                DENOISE_RANGE.0..=DENOISE_RANGE.1,
            )
            .text("denoiser iterations"),
        );
        ui.add(
            egui::Slider::new(&mut settings.sun_radius_degrees, 0.0..=10.0)
                .text("sun radius")
                .suffix("°"),
        );
        ui.add(
            egui::Slider::new(&mut settings.ray_length, 0.0..=64000.0)
                .text("ray length")
                .suffix(" u")
                .logarithmic(true),
        );
        ui.add(egui::Slider::new(&mut settings.ambient_strength, 0.0..=2.0).text("ambient"));
        ui.add(egui::Slider::new(&mut settings.bounce_strength, 0.0..=2.0).text("bounce strength"));
        ui.add(
            egui::Slider::new(&mut settings.reflection_roughness_cutoff, 0.0..=1.0)
                .text("reflection roughness cutoff"),
        );
        ui.checkbox(&mut settings.show_denoiser_input, "Show denoiser input")
            .on_hover_text(
                "Draws the raw one-sample-per-pixel ray image instead of the filtered one. A debug switch: it makes the denoiser's work visible.",
            );
        ui.separator();
        ui.heading("Ray reconstruction");
        ui.horizontal(|ui| {
            for provider in [
                RayReconstruction::Off,
                RayReconstruction::DlssRayReconstruction,
                RayReconstruction::FsrRayRegeneration,
            ] {
                let available = self.availability.get(provider);
                let response = ui.add_enabled(
                    available || provider == RayReconstruction::Off,
                    egui::SelectableLabel::new(settings.reconstruction == provider, provider.label()),
                );
                if response.clicked() {
                    settings.reconstruction = provider;
                }
                if !available && provider != RayReconstruction::Off {
                    let file = provider.sdk_file().unwrap_or("the vendor DLL");
                    response.on_hover_text(format!(
                        "{file} was not found in {}",
                        self.sdk_dir.display()
                    ));
                }
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "SDK folder: {}. DLLs present: DLSS RR {}, FSR RR {}.",
                self.sdk_dir.display(),
                if self.availability.dlss_rr { "yes" } else { "no" },
                if self.availability.fsr_rr { "yes" } else { "no" },
            ))
            .small()
            .weak(),
        );
        if let Some(note) = &self.hardware_note {
            ui.colored_label(egui::Color32::YELLOW, note);
        }
        ui.separator();
        let status = self.last.clone();
        ui.label(egui::RichText::new(format!("Last frame: {}", status.summary)).strong());
        ui.label(egui::RichText::new(status.overrides.summary()).small().weak());
        if let Some(note) = status.note {
            ui.colored_label(egui::Color32::YELLOW, note);
        }
        self.set_settings(settings);
    }

    /// The status of the frame that was last recorded.
    pub fn last_status(&self) -> &Status {
        &self.last
    }

    /// Persist the settings into a `settings.ini`-style section.
    pub fn encode(&self, out: &mut String) {
        use std::fmt::Write;
        let _ = writeln!(out, "[{SECTION}]");
        let _ = writeln!(out, "mode={}", self.settings.mode.token());
        let _ = writeln!(
            out,
            "reconstruction={}",
            self.settings.reconstruction.token()
        );
        let _ = writeln!(out, "bounces={}", self.settings.bounces);
        let _ = writeln!(out, "samples={}", self.settings.samples);
        let _ = writeln!(
            out,
            "denoise_iterations={}",
            self.settings.denoise_iterations
        );
        let _ = writeln!(out, "sun_radius={}", self.settings.sun_radius_degrees);
        let _ = writeln!(out, "ray_length={}", self.settings.ray_length);
        let _ = writeln!(out, "ambient_strength={}", self.settings.ambient_strength);
        let _ = writeln!(out, "bounce_strength={}", self.settings.bounce_strength);
        let _ = writeln!(
            out,
            "reflection_roughness={}",
            self.settings.reflection_roughness_cutoff
        );
        let _ = writeln!(
            out,
            "show_denoiser_input={}",
            u8::from(self.settings.show_denoiser_input)
        );
        let effects = self.settings.effects;
        for (name, on) in [
            ("shadows", effects.shadows),
            ("lighting", effects.lighting),
            ("reflections", effects.reflections),
            ("ambient_occlusion", effects.ambient_occlusion),
            ("global_illumination", effects.global_illumination),
            ("refraction", effects.refraction),
        ] {
            let _ = writeln!(out, "{name}={}", u8::from(on));
        }
        if !self.settings.sdk_dir.is_empty() {
            let _ = writeln!(out, "sdk_dir={}", self.settings.sdk_dir);
        }
    }

    /// Read the section back. Missing keys keep their defaults, and every
    /// numeric value is clamped: the file is user-editable text.
    pub fn decode(&mut self, get: impl Fn(&str) -> Option<String>) {
        let mut s = self.settings.clone();
        if let Some(mode) = get("mode").and_then(|v| RayTracingMode::parse(&v)) {
            s.mode = mode;
        }
        if let Some(provider) = get("reconstruction").and_then(|v| RayReconstruction::parse(&v)) {
            s.reconstruction = provider;
        }
        if let Some(v) = get("bounces").and_then(|v| v.parse().ok()) {
            s.bounces = v;
        }
        if let Some(v) = get("samples").and_then(|v| v.parse().ok()) {
            s.samples = v;
        }
        if let Some(v) = get("denoise_iterations").and_then(|v| v.parse().ok()) {
            s.denoise_iterations = v;
        }
        if let Some(v) = get("sun_radius").and_then(|v| v.parse().ok()) {
            s.sun_radius_degrees = v;
        }
        if let Some(v) = get("ray_length").and_then(|v| v.parse().ok()) {
            s.ray_length = v;
        }
        if let Some(v) = get("ambient_strength").and_then(|v| v.parse().ok()) {
            s.ambient_strength = v;
        }
        if let Some(v) = get("bounce_strength").and_then(|v| v.parse().ok()) {
            s.bounce_strength = v;
        }
        if let Some(v) = get("reflection_roughness").and_then(|v| v.parse().ok()) {
            s.reflection_roughness_cutoff = v;
        }
        if let Some(v) = get("show_denoiser_input").and_then(parse_flag) {
            s.show_denoiser_input = v;
        }
        let mut effects = s.effects;
        for (name, slot) in [
            ("shadows", &mut effects.shadows),
            ("lighting", &mut effects.lighting),
            ("reflections", &mut effects.reflections),
            ("ambient_occlusion", &mut effects.ambient_occlusion),
            ("global_illumination", &mut effects.global_illumination),
            ("refraction", &mut effects.refraction),
        ] {
            if let Some(v) = get(name).and_then(parse_flag) {
                *slot = v;
            }
        }
        s.effects = effects;
        if let Some(dir) = get("sdk_dir") {
            s.sdk_dir = dir.trim().to_string();
        }
        s.sanitise();
        let reset = s.mode != self.settings.mode
            || s.reconstruction != self.settings.reconstruction
            || s.samples != self.settings.samples;
        self.settings = s;
        if reset {
            self.pending_reset = true;
        }
        self.sdk_dir = resolve_sdk_dir(&self.settings.sdk_dir, &self.sdk_dir);
    }
}

fn parse_flag(value: String) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// The folder the reconstruction SDKs load from.
///
/// Same precedence as the upscalers: the explicit setting, then
/// `PW_UPSCALER_SDK_DIR`, then `<exe>/sdk`. A setting that points somewhere
/// without the DLLs is still honoured — the availability probe decides whether
/// the provider can actually run, so the tab can explain the mistake instead of
/// silently ignoring it.
pub fn resolve_sdk_dir(configured: &str, fallback: &Path) -> PathBuf {
    let configured = configured.trim();
    if !configured.is_empty() {
        return PathBuf::from(configured);
    }
    if let Ok(dir) = std::env::var("PW_UPSCALER_SDK_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    // `<exe dir>/sdk` when the fallback path is already a directory, else the
    // fallback's own `sdk` child; both spellings appear in the host's callers.
    if fallback.ends_with("sdk") {
        fallback.to_path_buf()
    } else {
        fallback.join("sdk")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module() -> Raywatcher {
        Raywatcher::default()
    }

    #[test]
    fn a_fresh_module_is_off_and_changes_nothing() {
        let mut m = module();
        m.configure(true, (true, true));
        assert_eq!(m.settings().mode, RayTracingMode::Off);
        let status = m.status((1920, 1080), false);
        assert!(!status.active);
        assert_eq!(status.overrides, frame::RasterOverrides::NONE);
        assert_eq!(status.summary, "off");
        assert!(status.note.is_none());
    }

    #[test]
    fn software_mode_replaces_lighting_and_keeps_a_plan_the_upscaler_can_use() {
        let mut m = module();
        m.configure(false, (false, false));
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Software;
        m.set_settings(s);
        let status = m.status((1920, 1080), false);
        assert!(status.active);
        // The raster sun term and ambient are gone, and the ray image is at the
        // render extent the upscaler consumes.
        assert!(status.overrides.directional_light);
        assert!(status.overrides.ambient);
        assert!(status.overrides.reflection_pass);
        assert_eq!(status.plan.render, (1920, 1080));
        assert_eq!(status.plan.grid, (960, 540));
        assert!(status.summary.contains("Software"));
        assert!(status.summary.contains("960x540"));
    }

    #[test]
    fn hardware_degrades_to_software_without_ray_queries_and_says_why() {
        let mut m = module();
        // The mode is only downgraded by `configure`, which runs when the
        // device exists; `set_settings` records what the user asked for.
        m.configure(false, (false, false));
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Hardware;
        m.set_settings(s);
        m.configure(false, (false, false));
        assert_eq!(m.settings().mode, RayTracingMode::Software);
        assert!(m.hardware_note().is_some());
        // ...and a later attempt through `effective_mode` still degrades.
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Hardware;
        m.set_settings(s);
        assert_eq!(m.effective_mode(), RayTracingMode::Software);
        assert!(m.status((1280, 720), false).active);
        // With the feature, the same settings trace at full rate.
        m.configure(true, (false, false));
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Hardware;
        m.set_settings(s);
        assert_eq!(m.effective_mode(), RayTracingMode::Hardware);
        assert_eq!(m.status((1280, 720), false).plan.grid, (1280, 720));
        assert!(m.hardware_note().is_none());
    }

    #[test]
    fn a_provider_that_is_not_installed_cannot_stay_selected() {
        let mut m = module();
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Software;
        s.reconstruction = RayReconstruction::DlssRayReconstruction;
        m.set_settings(s);
        // No SDKs: the selection falls back to the in-house denoiser.
        m.configure(false, (false, false));
        assert_eq!(m.settings().reconstruction, RayReconstruction::Off);
        // With the DLLs, the same choice survives.
        let mut s = m.settings().clone();
        s.reconstruction = RayReconstruction::FsrRayRegeneration;
        m.set_settings(s);
        m.configure(false, (true, true));
        assert_eq!(
            m.settings().reconstruction,
            RayReconstruction::FsrRayRegeneration
        );
        assert_eq!(
            m.status((1280, 720), false).plan.reconstruction,
            RayReconstruction::FsrRayRegeneration
        );
    }

    #[test]
    fn every_effect_switch_reaches_the_overrides() {
        let mut m = module();
        m.configure(false, (false, false));
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Software;
        m.set_settings(s);
        let all = m.status((640, 360), false);
        assert!(all.overrides.any());
        // Turning everything off leaves the raster world alone.
        let mut s = m.settings().clone();
        s.effects = Effects {
            shadows: false,
            lighting: false,
            reflections: false,
            ambient_occlusion: false,
            global_illumination: false,
            refraction: false,
        };
        m.set_settings(s);
        let none = m.status((640, 360), false);
        assert!(!none.active);
        assert_eq!(none.overrides, frame::RasterOverrides::NONE);
        assert!(none.note.is_some(), "the tab must explain the empty state");
    }

    #[test]
    fn settings_round_trip_through_the_file_format() {
        let mut m = module();
        m.configure(true, (true, true));
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Hardware;
        s.reconstruction = RayReconstruction::DlssRayReconstruction;
        s.bounces = 4;
        s.samples = 2;
        s.denoise_iterations = 5;
        s.effects.reflections = false;
        s.effects.refraction = false;
        s.sun_radius_degrees = 1.5;
        m.set_settings(s.clone());
        let mut text = String::new();
        m.encode(&mut text);
        assert!(text.contains("[raywatcher]"));
        let mut restored = module();
        restored.configure(true, (true, true));
        restored.decode(|key| {
            text.lines()
                .find_map(|line| line.strip_prefix(&format!("{key}=")))
                .map(str::to_string)
        });
        assert_eq!(restored.settings(), &s);
    }

    #[test]
    fn a_corrupt_settings_file_keeps_defaults_and_is_clamped() {
        let mut m = module();
        m.configure(true, (true, true));
        m.decode(|key| match key {
            "mode" => Some("nonsense".into()),
            "bounces" => Some("999".into()),
            "samples" => Some("0".into()),
            "denoise_iterations" => Some("99".into()),
            "sun_radius" => Some("NaN".into()),
            "shadows" => Some("maybe".into()),
            "ambient_strength" => Some("1e9".into()),
            _ => None,
        });
        let s = m.settings();
        assert_eq!(s.mode, RayTracingMode::Off);
        assert_eq!(s.bounces, BOUNCES_RANGE.1);
        assert_eq!(s.samples, SAMPLES_RANGE.0);
        assert_eq!(s.denoise_iterations, DENOISE_RANGE.1);
        assert_eq!(s.sun_radius_degrees, 0.6);
        assert!(s.effects.shadows, "an unparseable flag keeps the default");
        assert_eq!(s.ambient_strength, 2.);
    }

    #[test]
    fn the_history_reset_is_consumed_once() {
        let mut m = module();
        assert!(m.take_history_reset(), "the first frame starts clean");
        assert!(!m.take_history_reset());
        m.reset_history();
        assert!(m.take_history_reset());
        assert!(!m.take_history_reset());
        // Changing the mode or the provider also resets, because the image
        // being accumulated changes meaning.
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Software;
        m.set_settings(s);
        assert!(m.take_history_reset());
    }

    #[test]
    fn the_sdk_folder_follows_the_upscalers_precedence() {
        let exe = Path::new("/games/pwe");
        assert_eq!(resolve_sdk_dir("", exe), PathBuf::from("/games/pwe/sdk"));
        assert_eq!(
            resolve_sdk_dir("  /custom/sdk  ", exe),
            PathBuf::from("/custom/sdk")
        );
        // A caller that already passes the sdk folder does not get a nested one.
        assert_eq!(
            resolve_sdk_dir("", Path::new("/games/pwe/sdk")),
            PathBuf::from("/games/pwe/sdk")
        );
    }

    #[test]
    fn the_frame_counter_advances_and_wraps() {
        let mut m = module();
        assert_eq!(m.frame_index(), 0);
        m.next_frame();
        m.next_frame();
        assert_eq!(m.frame_index(), 2);
        m.frame = u32::MAX;
        m.next_frame();
        assert_eq!(m.frame_index(), 0);
    }
}
