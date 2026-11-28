//! Module-owned client archive access for Librarian.
//!
//! Librarian owns its own `.pck` directory indices and file handles so that
//! enabling, browsing, or disabling the module never shares mutable state with
//! core world streaming or other optional modules.

use crate::{ecmodel, pck};
use std::{
    collections::HashMap,
    fs::{self, File},
    path::{Component, Path, PathBuf},
};

pub struct PckArchive {
    file: File,
    entries: Vec<pck::Entry>,
    by_path: HashMap<String, usize>,
}

impl PckArchive {
    pub fn open(element_root: &Path, archive_name: &str) -> Result<Self, String> {
        let path = element_root.join(archive_name);
        let mut file = File::open(&path)
            .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        let entries = pck::directory(&mut file)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let mut by_path = HashMap::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let path = normalize_archive_path(&entry.name);
            let path = if archive_name == "gfx.pck" && !path.starts_with("gfx/") {
                format!("gfx/{path}")
            } else {
                path
            };
            by_path.insert(path, index);
        }
        Ok(Self {
            file,
            entries,
            by_path,
        })
    }

    pub fn contains(&self, path: &str) -> bool {
        let normalized = normalize_archive_path(path);
        self.by_path.contains_key(&normalized)
    }

    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let normalized = normalize_archive_path(path);
        let index = *self
            .by_path
            .get(&normalized)
            .ok_or_else(|| format!("archive entry '{normalized}' was not found"))?;
        let entry = &self.entries[index];
        pck::read_entry(&mut self.file, entry)
            .map_err(|error| format!("cannot read '{normalized}': {error}"))
    }
}

pub fn normalize_archive_path(path: &str) -> String {
    ecmodel::normalize(path)
        .trim_start_matches('/')
        .to_lowercase()
}

fn safe_join(root: &Path, path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(root.join(path))
}

pub struct LibrarianAssets {
    element_root: PathBuf,
    configs: Option<PckArchive>,
    surfaces: Option<PckArchive>,
    models: Option<Result<PckArchive, String>>,
    litmodels: Option<Result<PckArchive, String>>,
    gfx: Option<Result<PckArchive, String>>,
    loose_gfx: HashMap<String, PathBuf>,
    textures: Option<Result<PckArchive, String>>,
    facedata: Option<Result<PckArchive, String>>,
}

impl LibrarianAssets {
    pub fn open(element_root: &Path) -> Self {
        let configs = PckArchive::open(element_root, "configs.pck").ok();
        let surfaces = PckArchive::open(element_root, "surfaces.pck").ok();
        Self {
            element_root: element_root.to_path_buf(),
            configs,
            surfaces,
            models: None,
            litmodels: None,
            gfx: None,
            loose_gfx: HashMap::new(),
            textures: None,
            facedata: None,
        }
    }

    /// Index names only. Member payloads (including dependencies) are read on selection.
    pub fn effect_catalog(&mut self) -> Result<crate::gfx_catalog::Catalog, String> {
        let archive = self
            .gfx
            .get_or_insert_with(|| PckArchive::open(&self.element_root, "gfx.pck"));
        let mut names: Vec<(String, u32)> = Vec::new();
        if let Ok(pack) = archive {
            names.extend(
                pack.entries
                    .iter()
                    .filter(|e| e.name.to_ascii_lowercase().ends_with(".gfx"))
                    .map(|e| (e.name.clone(), e.size)),
            );
        }
        // GFX dependencies are normalized by the player, but an extracted
        // Windows client's file names retain their case on Linux.
        self.loose_gfx.clear();
        let loose = self.element_root.join("gfx");
        let loose = if loose.is_dir() {
            loose
        } else {
            fs::read_dir(&self.element_root)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .find(|e| {
                    e.file_name().eq_ignore_ascii_case("gfx")
                        && e.file_type().is_ok_and(|t| t.is_dir())
                })
                .map_or(loose, |e| e.path())
        };
        if loose.is_dir() {
            let mut dirs = vec![loose];
            while let Some(dir) = dirs.pop() {
                for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let ty = entry.file_type().map_err(|e| e.to_string())?;
                    let path = entry.path();
                    if ty.is_dir() {
                        dirs.push(path);
                    } else if ty.is_file() {
                        let name = path
                            .strip_prefix(&self.element_root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        self.loose_gfx
                            .insert(normalize_archive_path(&name), path.clone());
                        if !path
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("gfx"))
                        {
                            continue;
                        }
                        // The reader gives a packed member precedence. Do not
                        // advertise its loose shadow with a different byte size.
                        if archive.as_ref().is_ok_and(|p| p.contains(&name)) {
                            continue;
                        }
                        names.push((
                            name,
                            entry.metadata().map_err(|e| e.to_string())?.len() as u32,
                        ));
                    }
                }
            }
        } else if let Err(error) = archive {
            return Err(error.clone());
        }
        Ok(crate::gfx_catalog::Catalog::from_directory(
            names
                .iter()
                .map(|(path, size)| (path.as_str(), *size))
                .collect(),
        ))
    }

    pub fn read_elements_data(&self) -> Result<Vec<u8>, String> {
        let primary = self.element_root.join("data/elements.data");
        if primary.is_file() {
            return fs::read(&primary)
                .map_err(|error| format!("cannot read {}: {error}", primary.display()));
        }
        let fallback = self.element_root.join("elements.data");
        fs::read(&fallback).map_err(|error| format!("cannot read {}: {error}", primary.display()))
    }

    pub fn read_config(&mut self, path: &str) -> Option<Vec<u8>> {
        if let Some(configs) = self.configs.as_mut() {
            if let Ok(bytes) = configs.read(path) {
                return Some(bytes);
            }
        }
        let normalized = normalize_archive_path(path);
        fs::read(safe_join(&self.element_root, &normalized)?).ok()
    }

    pub fn read_surface(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let normalized = normalize_archive_path(path);
        let full = if normalized.starts_with("surfaces/") {
            normalized
        } else {
            format!("surfaces/{normalized}")
        };
        if let Some(surfaces) = self.surfaces.as_mut() {
            if let Ok(bytes) = surfaces.read(&full) {
                return Ok(bytes);
            }
        }
        let disk = safe_join(&self.element_root, &full).ok_or("invalid surface asset path")?;
        if disk.is_file() {
            return fs::read(&disk)
                .map_err(|error| format!("cannot read {}: {error}", disk.display()));
        }
        Err(format!("surface asset '{full}' not found"))
    }

    fn ensure_models(&mut self) -> Result<&mut PckArchive, String> {
        if self.models.is_none() {
            self.models = Some(PckArchive::open(&self.element_root, "models.pck"));
        }
        match self.models.as_mut().unwrap() {
            Ok(archive) => Ok(archive),
            Err(error) => Err(error.clone()),
        }
    }

    fn ensure_textures(&mut self) -> Result<&mut PckArchive, String> {
        if self.textures.is_none() {
            self.textures = Some(PckArchive::open(&self.element_root, "textures.pck"));
        }
        match self.textures.as_mut().unwrap() {
            Ok(archive) => Ok(archive),
            Err(error) => Err(error.clone()),
        }
    }

    fn ensure_facedata(&mut self) -> Result<&mut PckArchive, String> {
        if self.facedata.is_none() {
            self.facedata = Some(PckArchive::open(&self.element_root, "facedata.pck"));
        }
        match self.facedata.as_mut().unwrap() {
            Ok(archive) => Ok(archive),
            Err(error) => Err(error.clone()),
        }
    }

    fn loose_resource(&self, path: &str) -> Option<Vec<u8>> {
        let path = path.replace('\\', "/");
        let disk = safe_join(&self.element_root, &path)?;
        std::fs::read(&disk).ok().or_else(|| {
            self.loose_gfx
                .get(&normalize_archive_path(&path))
                .and_then(|p| fs::read(p).ok())
        })
    }

    pub fn model_exists(&mut self, path: &str) -> bool {
        let normalized = normalize_archive_path(path);
        let in_archive = if normalized.starts_with("facedata/") {
            self.ensure_facedata()
                .map(|archive| archive.contains(&normalized))
                .unwrap_or(false)
        } else if normalized.starts_with("models/") {
            self.ensure_models()
                .map(|archive| archive.contains(&normalized))
                .unwrap_or(false)
        } else {
            false
        };
        in_archive || self.loose_resource(path).is_some()
    }

    /// Read one map-partitioned static BMD from litmodels.pck. The caller
    /// supplies the canonical `litmodels/<map-id>/...` key produced by the
    /// Architect resolver; archive indexing remains lazy like other assets.
    pub fn read_litmodel_resource(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let normalized = normalize_archive_path(path);
        if !normalized.starts_with("litmodels/")
            || safe_join(&self.element_root, &path.replace('\\', "/")).is_none()
        {
            return Err(format!("invalid litmodel asset path '{path}'"));
        }
        if self.litmodels.is_none() {
            self.litmodels = Some(PckArchive::open(&self.element_root, "litmodels.pck"));
        }
        let result = match self.litmodels.as_mut().unwrap() {
            Ok(archive) => archive.read(&normalized),
            Err(error) => Err(error.clone()),
        };
        result.or_else(|error| {
            self.loose_resource(path).ok_or_else(|| {
                format!("{error}; check that litmodels.pck or the matching loose BMD is installed")
            })
        })
    }

    /// Unified package reader compatible with `ModelAsset` and `model_draw_gpu::Draw`.
    pub fn read_model_resource(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let normalized = normalize_archive_path(path);
        if safe_join(&self.element_root, &path.replace('\\', "/")).is_none() {
            return Err(format!("invalid asset path '{path}'"));
        }
        let result = if normalized.starts_with("gfx/") {
            if self.gfx.is_none() {
                self.gfx = Some(PckArchive::open(&self.element_root, "gfx.pck"));
            }
            match self.gfx.as_mut().unwrap() {
                Ok(archive) => archive.read(&normalized),
                Err(error) => Err(error.clone()),
            }
        } else if normalized.starts_with("models/") {
            self.ensure_models()
                .and_then(|archive| archive.read(&normalized))
        } else if normalized.starts_with("facedata/") {
            self.ensure_facedata()
                .and_then(|archive| archive.read(&normalized))
        } else if normalized.starts_with("textures/") {
            self.ensure_textures()
                .and_then(|archive| archive.read(&normalized))
        } else if normalized.starts_with("surfaces/") {
            self.read_surface(&normalized)
        } else if normalized.starts_with("configs/") {
            self.configs
                .as_mut()
                .ok_or_else(|| "configs.pck unavailable".to_owned())
                .and_then(|archive| archive.read(&normalized))
        } else {
            Err(format!("unsupported Librarian asset path '{normalized}'"))
        };
        result.or_else(|error| {
            self.loose_resource(path).ok_or_else(|| {
                format!(
                    "{error}; check that the matching client archive or loose asset is installed"
                )
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_archives_can_use_loose_model_files() {
        let root = std::env::temp_dir().join(format!("librarian-loose-{}", std::process::id()));
        std::fs::create_dir_all(root.join("models/test")).unwrap();
        std::fs::write(root.join("models/test/item.ecm"), b"model").unwrap();
        let mut assets = LibrarianAssets::open(&root);
        assert!(assets.model_exists("models/test/item.ecm"));
        assert_eq!(
            assets.read_model_resource("models/test/item.ecm").unwrap(),
            b"model"
        );
        assert!(assets.read_model_resource("models/../../outside").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn loose_asset_paths_cannot_escape_client_root() {
        let root = Path::new("element");
        assert!(safe_join(root, "surfaces/icon.dds").is_some());
        assert!(safe_join(root, "surfaces/../../secret").is_none());
        assert!(safe_join(root, "/etc/passwd").is_none());
    }
}
