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
pub mod dxr;
pub mod frame;
pub mod harvest;
pub mod plan;
pub mod shade;

#[cfg(not(target_arch = "wasm32"))]
pub mod gpu;
#[cfg(not(target_arch = "wasm32"))]
pub mod reconstruct;

pub use plan::{
    Availability, DebugView, Effects, Plan, RayReconstruction, RayTracingMode, Settings,
    BOUNCES_RANGE, DENOISE_RANGE, SAMPLES_RANGE,
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
    /// True while the background worker thread is rebuilding the BVH.
    scene_building: bool,
    /// Optional note when `PW_UPSCALER_SDK_DIR` lacked `nvngx_dlssd.dll` and fell back to `<exe>/sdk`.
    sdk_fallback_note: Option<String>,
    /// Last runtime NGX DLSS Ray Reconstruction error reported by the GPU runner.
    dlss_rr_error: Option<String>,
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
            scene_building: false,
            sdk_fallback_note: None,
            dlss_rr_error: None,
            last: Status::inactive(None),
        }
    }
}

impl Raywatcher {
    /// Create the module, resolving the SDK folder the same way the upscalers
    /// do: the setting if present, then `PW_UPSCALER_SDK_DIR`, then `<exe>/sdk`.
    pub fn new(exe_dir: &Path) -> Self {
        let mut this = Self::default();
        let (sdk_dir, fallback_note) = resolve_sdk_dir_with_note(&this.settings.sdk_dir, exe_dir);
        this.sdk_dir = sdk_dir;
        this.sdk_fallback_note = fallback_note;
        let dlss_present = this.sdk_dir.join("nvngx_dlssd.dll").is_file();
        eprintln!(
            "[Raywatcher] resolved SDK folder: {} (nvngx_dlssd.dll: {})",
            this.sdk_dir.display(),
            if dlss_present { "present" } else { "missing" },
        );
        if let Some(note) = &this.sdk_fallback_note {
            eprintln!("[Raywatcher] {note}");
        }
        this
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: Settings) {
        if settings.mode != self.settings.mode
            || settings.reconstruction != self.settings.reconstruction
            || settings.samples != self.settings.samples
            || settings.replace_raster != self.settings.replace_raster
            || settings.point_light_shadows != self.settings.point_light_shadows
            || settings.debug_view != self.settings.debug_view
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

    /// Update whether the background ray-scene worker is currently rebuilding.
    pub fn set_scene_building(&mut self, building: bool) {
        self.scene_building = building;
    }

    /// Record or clear the runtime DLSS Ray Reconstruction failure diagnostic
    /// so the tab can display it in red under the reconstruction selector.
    pub fn set_dlss_rr_error(&mut self, error: Option<String>) {
        self.dlss_rr_error = error;
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
    pub fn set_hardware_note(&mut self, note: Option<String>) {
        self.hardware_note = note;
    }

    pub fn configure(&mut self, ray_queries: bool, sdk_present: (bool, bool)) {
        let next_avail = Availability {
            dlss_rr: sdk_present.0,
            fsr_rr: sdk_present.1,
        };
        let changed = self.hardware_rays != ray_queries || self.availability != next_avail;
        self.hardware_rays = ray_queries;
        self.hardware_note = if ray_queries {
            None
        } else {
            Some("this adapter does not expose DXR 1.1 hardware ray tracing; Software mode runs instead".into())
        };
        self.availability = next_avail;
        // A provider that is no longer available cannot stay selected: that
        // would leave the tab claiming DLSS RR while the denoiser ran.
        if !self.availability.get(self.settings.reconstruction) {
            self.settings.reconstruction = RayReconstruction::Off;
        }
        // Hardware mode without the feature is not a mode at all.
        if self.settings.mode == RayTracingMode::Hardware && !ray_queries {
            self.settings.mode = RayTracingMode::Software;
        }
        if changed {
            self.pending_reset = true;
        }
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
    /// Toggle the dynamic RT Camera Light on/off (bound to the `L` hotkey).
    pub fn toggle_camera_light(&mut self) -> bool {
        self.settings.camera_light_enabled = !self.settings.camera_light_enabled;
        self.settings.camera_light_enabled
    }

    pub fn next_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn frame_index(&self) -> u32 {
        self.frame
    }

    pub fn sdk_dir(&self) -> &Path {
        &self.sdk_dir
    }

    /// Sync the host's configured upscaler SDK directory (same folder DLSS/FSR/XeSS
    /// use) into Raywatcher so `nvngx_dlssd.dll` is found whenever it sits in the
    /// upscaler SDK folder, `PW_UPSCALER_SDK_DIR`, `<exe>/sdk`, or beside the exe.
    pub fn sync_sdk_dir(&mut self, upscale_sdk_dir: impl AsRef<Path>) {
        let upscale_sdk_dir = upscale_sdk_dir.as_ref();
        if !self.settings.sdk_dir.trim().is_empty() {
            return;
        }
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| upscale_sdk_dir.to_path_buf());
        let (resolved, note) = resolve_sdk_dir_candidates(upscale_sdk_dir, &exe_dir);
        if resolved != self.sdk_dir {
            let dlss_present = resolved.join("nvngx_dlssd.dll").is_file();
            eprintln!(
                "[Raywatcher] synced SDK folder: {} (nvngx_dlssd.dll: {})",
                resolved.display(),
                if dlss_present { "present" } else { "missing" },
            );
            self.sdk_dir = resolved;
            self.sdk_fallback_note = note;
        }
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
        let overrides = frame::RasterOverrides::for_plan(&plan);
        let note = if !self.settings.mode.enabled() {
            None
        } else if !plan.trace {
            Some(
                "Raywatcher is on but no effect is enabled, so the raster world is drawn unchanged."
                    .to_string(),
            )
        } else if self.scene_building {
            Some("updating ray scene in background...".to_string())
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
        ui.heading("Layered RT effects");
        ui.label(
            egui::RichText::new(
                "By default, Raywatcher keeps the full original raster world and layers the ticked ray-traced signals on top.",
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
            ("Water caustics", &mut effects.caustics),
        ] {
            changed_effects |= ui.checkbox(slot, label).changed();
        }
        if changed_effects {
            settings.effects = effects;
        }
        ui.separator();
        // Mini-tab / collapsible section for the dynamic RT Camera Light (Hotkey: L).
        egui::CollapsingHeader::new("Camera light [Hotkey: L]")
            .default_open(true)
            .show(ui, |ui| {
                ui.checkbox(
                    &mut settings.camera_light_enabled,
                    "Enable camera light (L)",
                )
                .on_hover_text(
                    "Attaches a real ray-traced shadow-casting point light to the camera for exploring dark interiors and night scenes.",
                );
                ui.add(
                    egui::Slider::new(&mut settings.camera_light_intensity, 0.5..=20.0)
                        .text("intensity"),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Slider::new(&mut settings.camera_light_warmness, -1.0..=1.0)
                            .text("warmness (red -> blue)"),
                    );
                    let rgb = plan::camera_light_rgb(settings.camera_light_warmness);
                    let swatch = egui::Color32::from_rgb(
                        (rgb.x.clamp(0.0, 1.0) * 255.0) as u8,
                        (rgb.y.clamp(0.0, 1.0) * 255.0) as u8,
                        (rgb.z.clamp(0.0, 1.0) * 255.0) as u8,
                    );
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(18.0, 14.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, swatch);
                });
            });
        ui.separator();
        ui.heading("Quality");
        ui.add(
            egui::Slider::new(&mut settings.bounces, BOUNCES_RANGE.0..=BOUNCES_RANGE.1)
                .text("light bounces"),
        );
        ui.add(
            egui::Slider::new(&mut settings.samples, SAMPLES_RANGE.0..=SAMPLES_RANGE.1)
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
            egui::Slider::new(&mut settings.ray_length, 0.0..=plan::MAX_RAY_LENGTH)
                .text("ray length")
                .suffix(" u")
                .logarithmic(true),
        )
        .on_hover_text(
            "Maximum RT trace & scene harvest distance (0 = default 1024 u, max 4096 u).",
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
        ui.checkbox(
            &mut settings.point_light_shadows,
            "RT point-light shadows (opt-in)",
        )
        .on_hover_text(
            "Shadows the game's own SceneLights without adding extra light. Off by default.",
        );
        ui.checkbox(&mut settings.replace_raster, "Replace raster lighting (debug opt-in)")
            .on_hover_text(
                "Off by default: Raywatcher layers ray-traced shadows, reflections, AO, point lights and GI over the full textured raster world.",
            );
        egui::ComboBox::from_label("Debug view")
            .selected_text(settings.debug_view.label())
            .show_ui(ui, |ui| {
                for view in DebugView::ALL {
                    ui.selectable_value(&mut settings.debug_view, view, view.label());
                }
            });
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
                    egui::SelectableLabel::new(
                        settings.reconstruction == provider,
                        provider.label(),
                    ),
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
        if let Some(err) = &self.dlss_rr_error {
            ui.colored_label(
                egui::Color32::RED,
                format!("DLSS Ray Reconstruction failed: {err}"),
            );
        }
        let sdk_line = ui.label(
            egui::RichText::new(format!(
                "SDK folder: {} | nvngx_dlssd.dll: {} | FSR RR: {}",
                self.sdk_dir.display(),
                if self.availability.dlss_rr {
                    "present"
                } else {
                    "missing"
                },
                if self.availability.fsr_rr {
                    "present"
                } else {
                    "missing"
                },
            ))
            .small()
            .weak(),
        );
        sdk_line.on_hover_text(format!("Resolved SDK folder: {}", self.sdk_dir.display()));
        if let Some(note) = &self.sdk_fallback_note {
            ui.label(egui::RichText::new(note).small().weak());
        }
        if let Some(note) = &self.hardware_note {
            ui.colored_label(egui::Color32::YELLOW, note);
        }
        ui.separator();
        let status = self.last.clone();
        ui.label(egui::RichText::new(format!("Last frame: {}", status.summary)).strong());
        ui.label(
            egui::RichText::new(status.overrides.summary())
                .small()
                .weak(),
        );
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
        let _ = writeln!(
            out,
            "replace_raster={}",
            u8::from(self.settings.replace_raster)
        );
        let _ = writeln!(
            out,
            "point_light_shadows={}",
            u8::from(self.settings.point_light_shadows)
        );
        let _ = writeln!(out, "debug_view={}", self.settings.debug_view.token());
        let effects = self.settings.effects;
        for (name, on) in [
            ("shadows", effects.shadows),
            ("lighting", effects.lighting),
            ("reflections", effects.reflections),
            ("ambient_occlusion", effects.ambient_occlusion),
            ("global_illumination", effects.global_illumination),
            ("refraction", effects.refraction),
            ("caustics", effects.caustics),
        ] {
            let _ = writeln!(out, "{name}={}", u8::from(on));
        }
        let _ = writeln!(
            out,
            "camera_light_enabled={}",
            u8::from(self.settings.camera_light_enabled)
        );
        let _ = writeln!(
            out,
            "camera_light_intensity={}",
            self.settings.camera_light_intensity
        );
        let _ = writeln!(
            out,
            "camera_light_warmness={}",
            self.settings.camera_light_warmness
        );
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
        if let Some(v) = get("replace_raster").and_then(parse_flag) {
            s.replace_raster = v;
        }
        if let Some(v) = get("point_light_shadows").and_then(parse_flag) {
            s.point_light_shadows = v;
        }
        if let Some(v) = get("debug_view").and_then(|v| DebugView::parse(&v)) {
            s.debug_view = v;
        }
        let mut effects = s.effects;
        for (name, slot) in [
            ("shadows", &mut effects.shadows),
            ("lighting", &mut effects.lighting),
            ("reflections", &mut effects.reflections),
            ("ambient_occlusion", &mut effects.ambient_occlusion),
            ("global_illumination", &mut effects.global_illumination),
            ("refraction", &mut effects.refraction),
            ("caustics", &mut effects.caustics),
        ] {
            if let Some(v) = get(name).and_then(parse_flag) {
                *slot = v;
            }
        }
        s.effects = effects;
        if let Some(v) = get("camera_light_enabled").and_then(parse_flag) {
            s.camera_light_enabled = v;
        }
        if let Some(v) = get("camera_light_intensity").and_then(|v| v.parse().ok()) {
            s.camera_light_intensity = v;
        }
        if let Some(v) = get("camera_light_warmness").and_then(|v| v.parse().ok()) {
            s.camera_light_warmness = v;
        }
        if let Some(dir) = get("sdk_dir") {
            s.sdk_dir = dir.trim().to_string();
        }
        s.sanitise();
        let reset = s.mode != self.settings.mode
            || s.reconstruction != self.settings.reconstruction
            || s.samples != self.settings.samples
            || s.replace_raster != self.settings.replace_raster
            || s.point_light_shadows != self.settings.point_light_shadows
            || s.debug_view != self.settings.debug_view;
        self.settings = s;
        if reset {
            self.pending_reset = true;
        }
        let (sdk_dir, fallback_note) =
            resolve_sdk_dir_with_note(&self.settings.sdk_dir, &self.sdk_dir);
        self.sdk_dir = sdk_dir;
        self.sdk_fallback_note = fallback_note;
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
    resolve_sdk_dir_with_note(configured, fallback).0
}

/// Resolve the reconstruction SDK folder, returning an optional diagnostic note
/// when `PW_UPSCALER_SDK_DIR` does not contain `nvngx_dlssd.dll` and `<exe>/sdk`
/// does, so the status line can explain the fallback.
pub fn resolve_sdk_dir_with_note(configured: &str, fallback: &Path) -> (PathBuf, Option<String>) {
    let env_dir = std::env::var("PW_UPSCALER_SDK_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    resolve_sdk_dir_with_env(configured, fallback, env_dir.as_deref())
}

fn resolve_sdk_dir_with_env(
    configured: &str,
    fallback: &Path,
    env_dir: Option<&Path>,
) -> (PathBuf, Option<String>) {
    let configured = configured.trim();
    if !configured.is_empty() {
        return (PathBuf::from(configured), None);
    }
    // `<exe dir>/sdk` when the fallback path is already a directory, else the
    // fallback's own `sdk` child; both spellings appear in the host's callers.
    let exe_sdk = if fallback.ends_with("sdk") {
        fallback.to_path_buf()
    } else {
        fallback.join("sdk")
    };
    if let Some(dir) = env_dir {
        let env_has_dlssd = dir.join("nvngx_dlssd.dll").is_file();
        let exe_has_dlssd = exe_sdk.join("nvngx_dlssd.dll").is_file();
        if !env_has_dlssd && exe_has_dlssd {
            let note = format!(
                "PW_UPSCALER_SDK_DIR ({}) lacked nvngx_dlssd.dll; fell back to {}",
                dir.display(),
                exe_sdk.display(),
            );
            return (exe_sdk, Some(note));
        }
        if !env_has_dlssd && fallback.join("nvngx_dlssd.dll").is_file() {
            let note = format!(
                "PW_UPSCALER_SDK_DIR ({}) lacked nvngx_dlssd.dll; fell back to {}",
                dir.display(),
                fallback.display(),
            );
            return (fallback.to_path_buf(), Some(note));
        }
        return (dir.to_path_buf(), None);
    }
    if !exe_sdk.join("nvngx_dlssd.dll").is_file() && fallback.join("nvngx_dlssd.dll").is_file() {
        return (fallback.to_path_buf(), None);
    }
    (exe_sdk, None)
}

fn resolve_sdk_dir_candidates(upscale_sdk_dir: &Path, exe_dir: &Path) -> (PathBuf, Option<String>) {
    let env_dir = std::env::var("PW_UPSCALER_SDK_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let exe_sdk = if exe_dir.ends_with("sdk") {
        exe_dir.to_path_buf()
    } else {
        exe_dir.join("sdk")
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if !upscale_sdk_dir.as_os_str().is_empty() {
        candidates.push(upscale_sdk_dir.to_path_buf());
    }
    if let Some(ref d) = env_dir {
        if !candidates.contains(d) {
            candidates.push(d.clone());
        }
    }
    if !candidates.contains(&exe_sdk) {
        candidates.push(exe_sdk.clone());
    }
    if !candidates.contains(&exe_dir.to_path_buf()) {
        candidates.push(exe_dir.to_path_buf());
    }
    for dir in &candidates {
        if dir.join("nvngx_dlssd.dll").is_file() {
            let note = if let Some(first) = candidates.first() {
                if first != dir {
                    Some(format!(
                        "{} lacked nvngx_dlssd.dll; using {}",
                        first.display(),
                        dir.display()
                    ))
                } else {
                    None
                }
            } else {
                None
            };
            return (dir.clone(), note);
        }
    }
    if !upscale_sdk_dir.as_os_str().is_empty() {
        (upscale_sdk_dir.to_path_buf(), None)
    } else {
        resolve_sdk_dir_with_env("", exe_dir, env_dir.as_deref())
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
    fn default_enabled_frame_preserves_raster_terms_identical_to_disabled() {
        let mut off_mod = module();
        off_mod.configure(true, (true, true));
        let disabled = off_mod.status((1920, 1080), false);

        let mut on_mod = module();
        on_mod.configure(true, (true, true));
        let mut s = on_mod.settings().clone();
        s.mode = RayTracingMode::Hardware;
        assert!(!s.replace_raster, "layered mode must be the default");
        on_mod.set_settings(s);
        let enabled = on_mod.status((1920, 1080), false);

        assert!(enabled.active);
        assert_eq!(
            enabled.overrides.directional_light,
            disabled.overrides.directional_light
        );
        assert_eq!(enabled.overrides.ambient, disabled.overrides.ambient);
        assert!(enabled.overrides.reflection_pass);
        assert!(!disabled.overrides.reflection_pass);
        assert_eq!(enabled.overrides.occlusion, disabled.overrides.occlusion);
        assert_eq!(
            enabled.overrides.atmosphere_fog,
            disabled.overrides.atmosphere_fog
        );
        assert!(
            enabled.overrides.suppress_baked_shadows,
            "Phase 5 single shadow source suppresses baked lightmaps when RT shadows are active"
        );
        assert_eq!(enabled.plan.render, (1920, 1080));
        assert_eq!(enabled.plan.grid, (1920, 1080));
    }

    #[test]
    fn software_mode_layers_by_default_and_only_replaces_when_opted_in() {
        let mut m = module();
        m.configure(false, (false, false));
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Software;
        m.set_settings(s);
        let layered = m.status((1920, 1080), false);
        assert!(layered.active);
        assert!(!layered.overrides.directional_light);
        assert!(!layered.overrides.ambient);
        assert!(layered.overrides.reflection_pass);
        assert!(!layered.overrides.occlusion);
        assert!(!layered.overrides.atmosphere_fog);
        assert!(layered.overrides.suppress_baked_shadows);
        assert_eq!(layered.plan.render, (1920, 1080));
        assert_eq!(layered.plan.grid, (960, 540));
        assert!(layered.summary.contains("Software"));
        assert!(layered.summary.contains("960x540"));

        // Opting into `replace_raster` removes the raster sun/ambient/reflection terms.
        let mut s = m.settings().clone();
        s.replace_raster = true;
        m.set_settings(s);
        let replaced = m.status((1920, 1080), false);
        assert!(replaced.active);
        assert!(replaced.overrides.directional_light);
        assert!(replaced.overrides.ambient);
        assert!(replaced.overrides.reflection_pass);
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
    fn every_effect_switch_reaches_the_overrides_when_replace_opted_in() {
        let mut m = module();
        m.configure(false, (false, false));
        let mut s = m.settings().clone();
        s.mode = RayTracingMode::Software;
        s.replace_raster = true;
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
            caustics: false,
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

    #[test]
    fn pw_upscaler_sdk_dir_without_dlssd_falls_back_to_exe_sdk() {
        let root = std::env::temp_dir().join(format!("pwe_rw_sdk_fallback_{}", std::process::id()));
        let env_dir = root.join("env_empty");
        let exe_dir = root.join("exe_bin");
        let exe_sdk = exe_dir.join("sdk");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&env_dir).unwrap();
        std::fs::create_dir_all(&exe_sdk).unwrap();
        std::fs::write(exe_sdk.join("nvngx_dlssd.dll"), b"stub").unwrap();

        let (resolved, note) = resolve_sdk_dir_with_env("", &exe_dir, Some(&env_dir));
        assert_eq!(resolved, exe_sdk);
        let note = note.expect("fallback note must be populated when falling back to <exe>/sdk");
        assert!(
            note.contains("nvngx_dlssd.dll"),
            "fallback note should mention nvngx_dlssd.dll: {note}"
        );

        // When env_dir DOES contain nvngx_dlssd.dll, keep env_dir with no fallback note.
        std::fs::write(env_dir.join("nvngx_dlssd.dll"), b"stub").unwrap();
        let (resolved2, note2) = resolve_sdk_dir_with_env("", &exe_dir, Some(&env_dir));
        assert_eq!(resolved2, env_dir);
        assert!(note2.is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_sdk_dir_finds_nvngx_dlssd_in_upscale_sdk_dir() {
        let base = std::env::temp_dir().join(format!("pwe_rw_sync_sdk_{}", std::process::id()));
        let wrong_dir = base.join("game_root");
        let upscale_dir = base.join("upscaler_sdk");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&wrong_dir).unwrap();
        std::fs::create_dir_all(&upscale_dir).unwrap();
        std::fs::write(upscale_dir.join("nvngx_dlssd.dll"), b"stub").unwrap();

        let mut m = Raywatcher::new(&wrong_dir);
        m.sync_sdk_dir(&upscale_dir);
        assert_eq!(m.sdk_dir(), upscale_dir.as_path());
        let (dlss, _) = super::reconstruct::probe_offline(m.sdk_dir(), true);
        assert!(
            dlss.available,
            "DLSS RR should be available once synced to upscale_sdk_dir"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn camera_light_warmness_and_toggle_work() {
        let mut m = module();
        assert!(!m.settings().camera_light_enabled);
        assert!(m.toggle_camera_light());
        assert!(m.settings().camera_light_enabled);
        assert!(!m.toggle_camera_light());
        assert!(!m.settings().camera_light_enabled);

        let mut s = m.settings().clone();
        s.camera_light_warmness = -1.0;
        let hot = plan::camera_light_rgb(s.camera_light_warmness);
        assert!(
            hot[0] > hot[2] * 3.0,
            "warmness=-1 should be hot red: {hot:?}"
        );

        s.camera_light_warmness = 0.0;
        let neutral = plan::camera_light_rgb(s.camera_light_warmness);
        assert!(
            (neutral[0] - 1.0).abs() < 1e-4
                && (neutral[1] - 0.97).abs() < 1e-4
                && (neutral[2] - 0.94).abs() < 1e-4
        );

        s.camera_light_warmness = 1.0;
        let cold = plan::camera_light_rgb(s.camera_light_warmness);
        assert!(
            cold[2] > cold[0] * 3.0,
            "warmness=+1 should be cold blue: {cold:?}"
        );
    }
}
