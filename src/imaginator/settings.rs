//! The Imaginator's own settings block, persisted next to the module.
//!
//! Deliberately not part of `crate::view_settings`: the module owns its
//! configuration so the core settings file stays exactly as it was, and the
//! Imaginator can be deleted (or its folder replaced) without the viewer
//! noticing. The format is the same `key=value` text the rest of the project
//! uses, so it is inspectable by hand.

use std::path::{Path, PathBuf};

/// Which tracer produces the added light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracer {
    Off,
    /// Multithreaded CPU path tracer over the module's own BVH.
    Software,
    /// DXR on the D3D12 device wgpu itself runs on.
    Hardware,
}

impl Tracer {
    pub const ALL: [Tracer; 3] = [Tracer::Off, Tracer::Software, Tracer::Hardware];

    pub fn label(self) -> &'static str {
        match self {
            Tracer::Off => "Off",
            Tracer::Software => "Software (CPU)",
            Tracer::Hardware => "Hardware (DXR)",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Tracer::Off => "off",
            Tracer::Software => "software",
            Tracer::Hardware => "hardware",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "disabled" => Some(Tracer::Off),
            "software" | "cpu" => Some(Tracer::Software),
            "hardware" | "gpu" | "dxr" => Some(Tracer::Hardware),
            _ => None,
        }
    }
}

/// How the traced terms are mixed into the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Add the traced picture on top of the rendered one as extra light.
    Lighting,
    /// Show the traced picture alone, to see exactly what the tracer sees.
    TraceOnly,
}

impl Output {
    pub const ALL: [Output; 2] = [Output::Lighting, Output::TraceOnly];

    pub fn label(self) -> &'static str {
        match self {
            Output::Lighting => "Add to the rendered frame",
            Output::TraceOnly => "Traced image only (diagnostic)",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Output::Lighting => "lighting",
            Output::TraceOnly => "trace",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "lighting" | "add" => Some(Output::Lighting),
            "trace" | "only" => Some(Output::TraceOnly),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImaginatorSettings {
    pub tracer: Tracer,
    pub output: Output,
    /// How many indirect bounces the software tracer follows. 0 = only the
    /// directly visible surface, 1..=8 = that many extra bounces.
    pub light_bounces: u32,
    /// Mirror-like reflection term.
    pub reflections: bool,
    /// Diffuse sun + sky lighting and received-shadow term.
    pub lighting: bool,
    /// Cast shadows from the traced sun, attenuating `lighting`.
    pub shadows: bool,
    /// Multiplier on the traced contribution. 1.0 is "physically what the
    /// tracer computed"; higher values make the difference visible at a glance.
    pub strength: f32,
    /// Rays per pixel. More is smoother and slower; applies to both tracers.
    pub samples: u32,
    /// Percentage of the frame the traced image is produced at. A traced image
    /// is the *lighting*, not detail, so half resolution costs nothing visible
    /// on shadows and reflections and quarters the cost.
    pub trace_scale: u32,
    /// Reflection fill for the water pass: when on, the Imaginator's traced
    /// reflection image replaces the water planes' screen reflection once per
    /// frame. The core water path itself is untouched.
    pub water_reflection_fill: bool,
    /// Only used while `Output::TraceOnly` is selected.
    pub exposure: f32,
}

impl Default for ImaginatorSettings {
    fn default() -> Self {
        Self {
            tracer: Tracer::Off,
            output: Output::Lighting,
            light_bounces: 2,
            reflections: true,
            lighting: true,
            shadows: true,
            strength: 0.6,
            samples: 1,
            // A traced image is the *lighting*, not detail, so a fraction of the
            // frame costs nothing the eye can see on shadows and reflections.
            // 25% was chosen when the traversal visited most of the tree per
            // ray; it is front-to-back now, which is several times cheaper, so
            // half resolution is affordable and the blocky upscale that 25%
            // produced is gone.
            trace_scale: 50,
            water_reflection_fill: false,
            exposure: 1.0,
        }
    }
}

pub const MAX_BOUNCES: u32 = 8;
pub const MAX_SAMPLES: u32 = 4;
pub const MIN_TRACE_SCALE: u32 = 5;
pub const MAX_TRACE_SCALE: u32 = 100;

impl ImaginatorSettings {
    /// Minimum number of rays the renderer should ask for. Any enabled term
    /// means rays are traced at all; a tracer with every term off would only
    /// reproduce the raster picture, so it is treated as "nothing to do".
    pub fn active(&self) -> bool {
        self.tracer != Tracer::Off
            && (self.reflections || self.lighting || self.shadows)
            && self.strength > 0.
    }

    pub fn encode(&self) -> String {
        self.encode_with_tracer(self.tracer)
    }

    /// `encode` with the tracer line overridden; `save` uses it so the module
    /// is never remembered as running. See `save`.
    fn encode_with_tracer(&self, tracer: Tracer) -> String {
        format!(
            "tracer={}\noutput={}\nlight_bounces={}\nreflections={}\nlighting={}\nshadows={}\nstrength={}\nsamples={}\ntrace_scale={}\nwater_reflection_fill={}\nexposure={}\n",
            tracer.token(),
            self.output.token(),
            self.light_bounces,
            self.reflections as u8,
            self.lighting as u8,
            self.shadows as u8,
            self.strength,
            self.samples,
            self.trace_scale,
            self.water_reflection_fill as u8,
            self.exposure,
        )
    }

    pub fn decode(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "tracer" => {
                    if let Some(t) = Tracer::parse(value) {
                        s.tracer = t;
                    }
                }
                "output" => {
                    if let Some(o) = Output::parse(value) {
                        s.output = o;
                    }
                }
                "light_bounces" => {
                    if let Ok(v) = value.parse::<u32>() {
                        s.light_bounces = v.min(MAX_BOUNCES);
                    }
                }
                "reflections" => s.reflections = flag(value, s.reflections),
                "lighting" => s.lighting = flag(value, s.lighting),
                "shadows" => s.shadows = flag(value, s.shadows),
                "strength" => {
                    if let Ok(v) = value.parse::<f32>() {
                        if v.is_finite() {
                            s.strength = v.clamp(0., 4.);
                        }
                    }
                }
                "samples" => {
                    if let Ok(v) = value.parse::<u32>() {
                        s.samples = v.clamp(1, MAX_SAMPLES);
                    }
                }
                "trace_scale" => {
                    if let Ok(v) = value.parse::<u32>() {
                        s.trace_scale = v.clamp(MIN_TRACE_SCALE, MAX_TRACE_SCALE);
                    }
                }
                "water_reflection_fill" => {
                    s.water_reflection_fill = flag(value, s.water_reflection_fill)
                }
                "exposure" => {
                    if let Ok(v) = value.parse::<f32>() {
                        if v.is_finite() {
                            s.exposure = v.clamp(0.05, 8.);
                        }
                    }
                }
                _ => {}
            }
        }
        // A saved file from a build with a wider range must not disable the
        // module by accident.
        s.samples = s.samples.clamp(1, MAX_SAMPLES);
        s.light_bounces = s.light_bounces.min(MAX_BOUNCES);
        s.trace_scale = s.trace_scale.clamp(MIN_TRACE_SCALE, MAX_TRACE_SCALE);
        s
    }

    /// `<Modules>/Imaginator/settings.ini`, beside the module manifest. The
    /// module folder is the natural home: it travels with a portable install.
    pub fn file(directory: &Path) -> PathBuf {
        directory.join("settings.ini")
    }

    pub fn load(directory: &Path) -> Self {
        let path = Self::file(directory);
        let mut settings = std::fs::read(&path)
            .ok()
            .filter(|b| b.len() <= 16384)
            .and_then(|b| String::from_utf8(b).ok())
            .map(|text| Self::decode(&text))
            .unwrap_or_default();
        settings
    }

    /// Write the settings, with one deliberate omission.
    ///
    /// The tracer is stored as `off`. Every other module starts switched off,
    /// and this one is the most expensive: a remembered `cpu`/`gpu` used to
    /// begin tracing the instant the window appeared, before the user had asked
    /// for anything (and, while the tracer was broken, turned the terrain
    /// white on launch). Everything else — strength, bounces, terms, scale —
    /// is remembered, so the page comes back the way it was left and the
    /// tracer is one click from where it was.
    pub fn save(&self, directory: &Path) -> Result<(), String> {
        let path = Self::file(directory);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("tmp");
        // See this function's comment: the tracer is never persisted as on.
        std::fs::write(&tmp, self.encode_with_tracer(Tracer::Off)).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
}

fn flag(value: &str, previous: bool) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => true,
        "0" | "false" | "off" | "no" => false,
        _ => previous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module must start switched off, whatever the last session selected.
    #[test]
    fn a_saved_file_never_remembers_the_tracer_as_running() {
        let mut s = ImaginatorSettings::default();
        s.tracer = Tracer::Software;
        s.strength = 0.85;
        // `encode` round-trips the selection for tests and for on-page state…
        assert_eq!(
            ImaginatorSettings::decode(&s.encode()).tracer,
            Tracer::Software
        );
        // …but what reaches the disk never does.
        let persisted = ImaginatorSettings::decode(&s.encode_with_tracer(Tracer::Off));
        assert_eq!(
            persisted.tracer,
            Tracer::Off,
            "a launch must not begin tracing on its own"
        );
        assert_eq!(
            persisted.strength, 0.85,
            "everything else is still remembered"
        );
        assert!(!persisted.active());
    }

    #[test]
    fn defaults_are_off_and_round_trip() {
        let s = ImaginatorSettings::default();
        assert_eq!(s.tracer, Tracer::Off);
        assert!(!s.active());
        assert_eq!(ImaginatorSettings::decode(&s.encode()), s);
    }

    #[test]
    fn every_field_survives_a_round_trip() {
        let s = ImaginatorSettings {
            tracer: Tracer::Hardware,
            output: Output::TraceOnly,
            light_bounces: 5,
            reflections: false,
            lighting: true,
            shadows: false,
            strength: 1.75,
            samples: 3,
            trace_scale: 75,
            water_reflection_fill: true,
            exposure: 2.5,
        };
        assert_eq!(ImaginatorSettings::decode(&s.encode()), s);
    }

    #[test]
    fn parsing_is_forgiving_and_clamps() {
        let s = ImaginatorSettings::decode(
            "tracer=SOFTWARE\nlight_bounces=999\nsamples=0\nstrength=nan\njunk\n=3\nreflections=maybe",
        );
        assert_eq!(s.tracer, Tracer::Software);
        assert_eq!(s.light_bounces, MAX_BOUNCES);
        assert_eq!(s.samples, 1);
        assert_eq!(s.strength, ImaginatorSettings::default().strength);
        assert!(s.reflections, "an unreadable flag keeps the previous value");
    }

    #[test]
    fn unknown_tracer_spelling_keeps_the_module_off() {
        let s = ImaginatorSettings::decode("tracer=bogus\n");
        assert_eq!(s.tracer, Tracer::Off);
        assert!(!s.active());
    }

    #[test]
    fn active_requires_a_tracer_a_term_and_strength() {
        let mut s = ImaginatorSettings::default();
        s.tracer = Tracer::Software;
        assert!(s.active());
        s.reflections = false;
        s.lighting = false;
        s.shadows = false;
        assert!(!s.active(), "nothing to trace");
        s.lighting = true;
        s.strength = 0.;
        assert!(!s.active());
    }

    #[test]
    fn tokens_and_labels_stay_in_sync() {
        for t in Tracer::ALL {
            assert_eq!(Tracer::parse(t.token()), Some(t));
            assert!(!t.label().is_empty());
        }
        for o in Output::ALL {
            assert_eq!(Output::parse(o.token()), Some(o));
        }
    }
}
