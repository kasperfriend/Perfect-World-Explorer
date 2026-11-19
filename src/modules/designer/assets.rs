//! Designer-owned archive and loose-file access. No game asset is cached in or
//! written back to the original PCK files.

use super::{
    catalog::{Catalog, Class, Gender},
    customize::{CharacterDesign, SliderBounds},
};
use crate::{ecmodel, modules::walker::archive::Archive};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub struct Assets {
    pub element: PathBuf,
    pub models: Archive,
    pub facedata: Archive,
    pub configs: Archive,
    pub textures: Archive,
    pub gfx: Archive,
    pub catalog: Catalog,
    pub bounds: SliderBounds,
    pub(crate) skin_omissions: crate::skin::UnsupportedSkins,
}
impl Assets {
    pub fn open(element: &Path) -> Result<Self, String> {
        let models = Archive::open(element)?;
        let facedata = Archive::open_archive(&element.join("facedata.pck"))?;
        let configs = Archive::open_archive(&element.join("configs.pck"))?;
        let textures = Archive::open_archive(&element.join("textures.pck"))?;
        let gfx = Archive::open_archive(&element.join("gfx.pck"))?;
        let elements_path = element.join("data/elements.data");
        let elements = fs::read(&elements_path)
            .map_err(|error| format!("{}: {error}", elements_path.display()))?;
        let catalog = Catalog::parse_v27(&elements)
            .map_err(|error| format!("{}: {error}", elements_path.display()))?;
        let mut facedata = facedata;
        let bounds_path = "facedata/整容卷/普通整容卷限制参数.ini";
        let bounds = SliderBounds::parse(&facedata.read(bounds_path)?)
            .map_err(|error| format!("{bounds_path}: {error}"))?;
        Ok(Self {
            element: element.to_owned(),
            models,
            facedata,
            configs,
            textures,
            gfx,
            catalog,
            bounds,
            skin_omissions: Default::default(),
        })
    }

    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let bytes = self.read_raw(path)?;
        self.skin_omissions.observe(path, &bytes);
        Ok(bytes)
    }

    fn read_raw(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let normalized = ecmodel::normalize(path);
        if normalized.starts_with("gfx/") {
            return self.gfx.read(&normalized);
        }
        if normalized.starts_with("facedata/") {
            return self.facedata.read(&normalized);
        }
        if normalized.starts_with("configs/") {
            return self.configs.read(&normalized);
        }
        if normalized.starts_with("models/") {
            return self.models.read(&normalized);
        }
        if normalized.starts_with("textures/") {
            return self.textures.read(&normalized);
        }
        let relative = safe_relative(path)?;
        let path = self.element.join(relative);
        fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn contains(&self, path: &str) -> bool {
        let normalized = ecmodel::normalize(path);
        if normalized.starts_with("gfx/") {
            self.gfx.contains(&normalized)
        } else if normalized.starts_with("facedata/") {
            self.facedata.contains(&normalized)
        } else if normalized.starts_with("configs/") {
            self.configs.contains(&normalized)
        } else if normalized.starts_with("models/") {
            self.models.contains(&normalized)
        } else if normalized.starts_with("textures/") {
            self.textures.contains(&normalized)
        } else {
            safe_relative(path)
                .map(|relative| self.element.join(relative).is_file())
                .unwrap_or(false)
        }
    }

    pub fn default_design(
        &mut self,
        class: Class,
        gender: Gender,
    ) -> Result<CharacterDesign, String> {
        if !class.allows(gender) {
            return Err(format!(
                "{} does not support {} characters",
                class.name(),
                gender.name()
            ));
        }
        let path = class.default_ini(gender);
        let bytes = self.read(&path)?;
        let mut design = CharacterDesign::from_default_ini(class, gender, &bytes)
            .map_err(|error| format!("{path}: {error}"))?;
        design.clamp(&self.bounds);
        if self.body_variants(class, gender).is_empty() {
            return Err(format!(
                "no complete body/head/face variant for {} {}",
                class.name(),
                gender.name()
            ));
        }
        if !self
            .body_variants(class, gender)
            .contains(&design.customize.body_id)
        {
            return Err(format!(
                "{} {} default body ID {} has incomplete authentic assets",
                class.name(),
                gender.name(),
                design.customize.body_id
            ));
        }
        Ok(design)
    }

    /// Only variants with the complete native body, head, BON and SKI set are
    /// offered. In particular, numbered assets use bodyID + 1 and are never
    /// substituted with a different race's model.
    pub fn body_variants(&self, class: Class, gender: Gender) -> Vec<u16> {
        let count = if class.numbered_body() { 16 } else { 1 };
        (0..count)
            .filter(|&body_id| {
                self.contains(&class.body_skin(gender, body_id))
                    && self.contains(&class.head_skin(gender, body_id))
                    && self.contains(&class.face_asset(gender, body_id, "bon"))
                    && self.contains(&class.face_asset(gender, body_id, "ski"))
            })
            .collect()
    }

    pub fn apply_preset(&mut self, design: &mut CharacterDesign, path: &str) -> Result<(), String> {
        // The caller supplies a catalog entry or a user preset from the scanned
        // directory. Read through the same archive/loose-file rules as defaults.
        let bytes = self.read(path)?;
        design.apply_preset_ini(&bytes)?;
        design.clamp(&self.bounds);
        Ok(())
    }

    pub fn save_preset(&self, design: &CharacterDesign, name: &str) -> Result<PathBuf, String> {
        let name = valid_preset_name(name)?;
        let directory = self
            .element
            .join("userdata/character/Customize")
            .join(format!("{}{}", design.class.index(), design.gender.index()));
        fs::create_dir_all(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        let path = directory.join(format!("{name}.ini"));
        let bytes = design.to_ini_gbk()?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(path)
    }

    pub fn user_presets(&self, class: Class, gender: Gender) -> Result<Vec<UserPreset>, String> {
        let directory = self
            .element
            .join("userdata/character/Customize")
            .join(format!("{}{}", class.index(), gender.index()));
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("{}: {error}", directory.display())),
        };
        let mut presets = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("ini"))
                != Some(true)
            {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|x| x.to_str()) else {
                continue;
            };
            presets.push(UserPreset {
                name: name.to_owned(),
                path,
            });
        }
        presets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(presets)
    }

    pub fn apply_user_preset(
        &self,
        design: &mut CharacterDesign,
        path: &Path,
    ) -> Result<(), String> {
        let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        design.apply_preset_ini(&bytes)?;
        design.clamp(&self.bounds);
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct UserPreset {
    pub name: String,
    pub path: PathBuf,
}

fn safe_relative(path: &str) -> Result<PathBuf, String> {
    let mut result = PathBuf::new();
    for component in path.replace('\\', "/").split('/') {
        if component.is_empty() || component == "." || component == ".." || component.contains(':')
        {
            return Err(format!("unsafe asset path: {path}"));
        }
        result.push(component);
    }
    if result.as_os_str().is_empty() {
        Err("empty asset path".into())
    } else {
        Ok(result)
    }
}

fn valid_preset_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 96
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':', '\0'])
        || name.chars().any(char::is_control)
    {
        return Err("Preset name must be 1–96 plain characters without path separators".into());
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loose_paths_cannot_escape_element_directory() {
        assert!(safe_relative("../outside.ini").is_err());
        assert!(safe_relative("C:/outside.ini").is_err());
        assert_eq!(
            safe_relative("userdata\\character\\a.ini").unwrap(),
            PathBuf::from("userdata/character/a.ini")
        );
        assert!(valid_preset_name("../bad").is_err());
        assert!(valid_preset_name("My preset 1").is_ok());
    }
}
