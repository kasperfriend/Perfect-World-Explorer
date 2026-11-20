use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Off,
    Software,
    Hardware,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::Off, Self::Software, Self::Hardware];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off (native renderer)",
            Self::Software => "Software RT (compute BVH)",
            Self::Hardware => "Hardware RT (Vulkan ray queries)",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.token() == token)
    }

    fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Software => "software",
            Self::Hardware => "hardware",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Denoiser {
    #[default]
    Svgf,
    None,
    DlssRr,
    FsrRayRegen,
}

impl Denoiser {
    pub const ALL: [Self; 4] = [Self::Svgf, Self::None, Self::DlssRr, Self::FsrRayRegen];

    pub fn label(self) -> &'static str {
        match self {
            Self::Svgf => "SVGF (portable)",
            Self::None => "None",
            Self::DlssRr => "DLSS Ray Reconstruction (RTX)",
            Self::FsrRayRegen => "FSR Ray Regeneration",
        }
    }

    pub fn unavailable_reason(self) -> Option<&'static str> {
        match self {
            Self::Svgf => None,
            Self::None => None,
            Self::DlssRr => Some("requires RTX GPU"),
            Self::FsrRayRegen => Some("DirectX 12 only; not available on Vulkan"),
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "svgf" => Some(Self::Svgf),
            "none" => Some(Self::None),
            "rr" => Some(Self::DlssRr),
            "fsr-ray-regen" => Some(Self::FsrRayRegen),
            _ => None,
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Svgf => "svgf",
            Self::None => "none",
            Self::DlssRr => "rr",
            Self::FsrRayRegen => "fsr-ray-regen",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    pub shadows: bool,
    pub soft_sun: bool,
    pub lighting: bool,
    pub regional_lighting: bool,
    pub reflections: bool,
    pub single_path: bool,
    pub hybrid_primary: bool,
    pub denoiser: Denoiser,
    pub guided: bool,
    pub ray_lighting: bool,
    pub progressive: bool,
    pub bounces: u32,
    pub diffuse_distance: u32,
    pub samples: u32,
    pub resolution: u32,
    pub dynamic_resolution: bool,
    pub gpu_target_ms: u32,
    pub anisotropy: u32,
    pub relight_baked: bool,
    pub physical_bsdf: bool,
    pub reference: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            shadows: true,
            soft_sun: true,
            lighting: true,
            regional_lighting: false,
            reflections: true,
            single_path: true,
            hybrid_primary: false,
            denoiser: Denoiser::Svgf,
            guided: true,
            ray_lighting: true,
            progressive: true,
            bounces: 1,
            diffuse_distance: 256,
            samples: 1,
            resolution: 50,
            dynamic_resolution: false,
            gpu_target_ms: 12,
            anisotropy: 8,
            relight_baked: false,
            physical_bsdf: false,
            reference: false,
        }
    }
}

impl Settings {
    /// Reference transports only the current samples; interactive controls
    /// remain persisted so disabling this preset restores them unchanged.
    pub fn effective(self) -> Self {
        if !self.reference {
            return self;
        }
        Self {
            lighting: true,
            shadows: true,
            reflections: true,
            physical_bsdf: true,
            single_path: false,
            hybrid_primary: false,
            relight_baked: true,
            progressive: false,
            denoiser: Denoiser::Svgf,
            guided: true,
            bounces: 8,
            diffuse_distance: 0,
            samples: 32,
            resolution: 100,
            dynamic_resolution: false,
            ..self
        }
    }

    pub(crate) fn decode(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let number = |old: u32, min, max| value.parse::<u32>().unwrap_or(old).clamp(min, max);
            let toggle = |old| match value {
                "true" => true,
                "false" => false,
                _ => old,
            };
            match key.trim() {
                "mode" => {
                    if let Some(mode) = Mode::parse(value) {
                        settings.mode = mode;
                    }
                }
                "shadows" => settings.shadows = toggle(settings.shadows),
                "soft_sun" => settings.soft_sun = toggle(settings.soft_sun),
                "lighting" => settings.lighting = toggle(settings.lighting),
                "regional_lighting" => {
                    settings.regional_lighting = toggle(settings.regional_lighting)
                }
                "reflections" => settings.reflections = toggle(settings.reflections),
                "single_path" => settings.single_path = toggle(settings.single_path),
                "hybrid_primary" => settings.hybrid_primary = toggle(settings.hybrid_primary),
                "denoiser" => {
                    if let Some(d) = Denoiser::parse(value) {
                        settings.denoiser = d;
                    }
                }
                "guided" => settings.guided = toggle(settings.guided),
                "ray_lighting" => settings.ray_lighting = toggle(settings.ray_lighting),
                "progressive" => settings.progressive = toggle(settings.progressive),
                "bounces" => settings.bounces = number(settings.bounces, 0, 4),
                "diffuse_distance" => {
                    settings.diffuse_distance = number(settings.diffuse_distance, 0, 24000)
                }
                "samples" => settings.samples = number(settings.samples, 1, 4),
                "resolution" => settings.resolution = number(settings.resolution, 25, 100),
                "dynamic_resolution" => {
                    settings.dynamic_resolution = toggle(settings.dynamic_resolution)
                }
                "gpu_target_ms" => settings.gpu_target_ms = number(settings.gpu_target_ms, 4, 50),
                "reference" => settings.reference = toggle(settings.reference),
                "physical_bsdf" => settings.physical_bsdf = toggle(settings.physical_bsdf),
                "relight_baked" => settings.relight_baked = toggle(settings.relight_baked),
                "anisotropy" => {
                    if let Ok(n @ (1 | 8 | 16)) = value.parse::<u32>() {
                        settings.anisotropy = n;
                    }
                }
                _ => {}
            }
        }
        settings
    }

    pub(crate) fn encode(self) -> String {
        format!(
            "# Lighter settings\nmode={}\nlighting={}\nregional_lighting={}\nshadows={}\nsoft_sun={}\nreflections={}\nsingle_path={}\nhybrid_primary={}\ndenoiser={}\nguided={}\nray_lighting={}\nprogressive={}\nbounces={}\ndiffuse_distance={}\nsamples={}\nresolution={}\ndynamic_resolution={}\ngpu_target_ms={}\nanisotropy={}\nrelight_baked={}\nphysical_bsdf={}\nreference={}\n",
            self.mode.token(), self.lighting, self.regional_lighting, self.shadows, self.soft_sun, self.reflections, self.single_path, self.hybrid_primary, self.denoiser.token(), self.guided, self.ray_lighting, self.progressive, self.bounces,
            self.diffuse_distance, self.samples, self.resolution, self.dynamic_resolution, self.gpu_target_ms, self.anisotropy, self.relight_baked, self.physical_bsdf, self.reference,
        )
    }

    pub(super) fn path() -> Option<PathBuf> {
        #[cfg(windows)]
        let directory = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        #[cfg(not(windows))]
        let directory = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")));
        Some(directory?.join("PerfectWorldViewer").join("lighter.ini"))
    }

    pub fn load() -> Self {
        let mut settings = Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .filter(|s| s.len() <= 4096)
            .map_or_else(Self::default, |text| Self::decode(&text));
        // PW_RAY_RECONSTRUCTION overrides the denoiser dropdown:
        // fsr → FSR Ray Regeneration, rr → DLSS Ray Reconstruction,
        // off → SVGF (portable default).
        if let Ok(val) = std::env::var("PW_RAY_RECONSTRUCTION") {
            match val.as_str() {
                "fsr" => settings.denoiser = Denoiser::FsrRayRegen,
                "rr" => settings.denoiser = Denoiser::DlssRr,
                "off" | "" => settings.denoiser = Denoiser::Svgf,
                _ => {}
            }
        }
        settings
    }

    pub fn save(self) -> Result<(), String> {
        let path = Self::path().ok_or("Lighter settings directory unavailable")?;
        let parent = path
            .parent()
            .ok_or("Lighter settings directory unavailable")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        std::fs::write(path, self.encode()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_off_and_validates_persisted_controls() {
        let defaults = Settings::default();
        assert_eq!(defaults.mode, Mode::Off);
        assert_eq!(
            [defaults.samples, defaults.bounces, defaults.resolution],
            [1, 1, 50]
        );
        assert!(defaults.progressive && defaults.denoiser == Denoiser::Svgf);
        assert_eq!(defaults.diffuse_distance, 256);
        assert!(!defaults.regional_lighting);
        let s = Settings::decode(
            "mode=hardware\nbounces=99\nshadows=false\nsoft_sun=false\nprogressive=false\nsamples=0\nresolution=bad\nanisotropy=16\nphysical_bsdf=true\ndiffuse_distance=512\nregional_lighting=true",
        );
        assert_eq!(s.diffuse_distance, 512);
        assert!(s.regional_lighting);
        assert_eq!(s.mode, Mode::Hardware);
        assert_eq!(s.bounces, 4);
        assert_eq!(s.samples, 1);
        assert_eq!(s.resolution, 50);
        assert_eq!(s.anisotropy, 16);
        assert_eq!(Settings::decode("anisotropy=12").anisotropy, 8);
        assert!(!s.shadows && s.lighting && s.reflections);
        assert!(s.denoiser == Denoiser::Svgf && !s.progressive && !s.soft_sun);
        assert!(s.physical_bsdf);
        assert_eq!(Settings::decode(&s.encode()), s);
        assert_eq!(Settings::decode("mode=unknown"), Settings::default());
        let reference =
            Settings::decode("reference=true\nhybrid_primary=true\ndynamic_resolution=true");
        assert_eq!(Settings::decode(&reference.encode()), reference);
        let e = reference.effective();
        assert_eq!([e.samples, e.bounces, e.resolution], [32, 8, 100]);
        assert!(e.physical_bsdf && e.shadows && e.relight_baked);
        assert!(!e.progressive && reference.progressive);
        assert_eq!(e.diffuse_distance, 0);
        assert!(!e.hybrid_primary && !e.single_path && !e.dynamic_resolution);
        assert!(reference.hybrid_primary && reference.single_path && reference.dynamic_resolution);
    }
}
