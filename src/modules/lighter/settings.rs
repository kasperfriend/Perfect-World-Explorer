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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    pub shadows: bool,
    pub soft_sun: bool,
    pub lighting: bool,
    pub reflections: bool,
    pub denoiser: bool,
    pub progressive: bool,
    pub bounces: u32,
    pub samples: u32,
    pub resolution: u32,
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
            reflections: true,
            denoiser: true,
            progressive: true,
            bounces: 1,
            samples: 1,
            resolution: 50,
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
            relight_baked: true,
            progressive: false,
            denoiser: false,
            bounces: 8,
            samples: 32,
            resolution: 100,
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
                "reflections" => settings.reflections = toggle(settings.reflections),
                "denoiser" => settings.denoiser = toggle(settings.denoiser),
                "progressive" => settings.progressive = toggle(settings.progressive),
                "bounces" => settings.bounces = number(settings.bounces, 0, 4),
                "samples" => settings.samples = number(settings.samples, 1, 4),
                "resolution" => settings.resolution = number(settings.resolution, 25, 100),
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
            "# Lighter settings\nmode={}\nlighting={}\nshadows={}\nsoft_sun={}\nreflections={}\ndenoiser={}\nprogressive={}\nbounces={}\nsamples={}\nresolution={}\nanisotropy={}\nrelight_baked={}\nphysical_bsdf={}\nreference={}\n",
            self.mode.token(), self.lighting, self.shadows, self.soft_sun, self.reflections, self.denoiser, self.progressive, self.bounces,
            self.samples, self.resolution, self.anisotropy, self.relight_baked, self.physical_bsdf, self.reference,
        )
    }

    fn path() -> Option<PathBuf> {
        #[cfg(windows)]
        let directory = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        #[cfg(not(windows))]
        let directory = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")));
        Some(directory?.join("PerfectWorldViewer").join("lighter.ini"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .filter(|s| s.len() <= 4096)
            .map_or_else(Self::default, |text| Self::decode(&text))
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
        assert!(defaults.progressive && defaults.denoiser);
        let s = Settings::decode(
            "mode=hardware\nbounces=99\nshadows=false\nsoft_sun=false\ndenoiser=false\nprogressive=false\nsamples=0\nresolution=bad\nanisotropy=16\nphysical_bsdf=true",
        );
        assert_eq!(s.mode, Mode::Hardware);
        assert_eq!(s.bounces, 4);
        assert_eq!(s.samples, 1);
        assert_eq!(s.resolution, 50);
        assert_eq!(s.anisotropy, 16);
        assert_eq!(Settings::decode("anisotropy=12").anisotropy, 8);
        assert!(!s.shadows && s.lighting && s.reflections);
        assert!(!s.denoiser && !s.progressive && !s.soft_sun);
        assert!(s.physical_bsdf);
        assert_eq!(Settings::decode(&s.encode()), s);
        assert_eq!(Settings::decode("mode=unknown"), Settings::default());
        let reference = Settings::decode("reference=true");
        assert_eq!(Settings::decode(&reference.encode()), reference);
        let e = reference.effective();
        assert_eq!([e.samples, e.bounces, e.resolution], [32, 8, 100]);
        assert!(e.physical_bsdf && e.shadows && e.relight_baked);
        assert!(!e.progressive && !e.denoiser && reference.progressive);
    }
}
