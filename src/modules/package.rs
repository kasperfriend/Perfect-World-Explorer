//! Discovery of installed, executable-adjacent module packages.
//!
//! A package supplies its identity and user-facing metadata through
//! `module.ini`. The current executable provides the implementation for known
//! package IDs; an unknown package remains visible but cannot be enabled.

use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub const MODULES_DIRECTORY: &str = "Modules";
pub const MODULE_MANIFEST: &str = "module.ini";
const MAX_MANIFEST_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub directory: PathBuf,
}

impl Package {
    fn load(directory: PathBuf) -> Result<Self, String> {
        let manifest = directory.join(MODULE_MANIFEST);
        let metadata =
            fs::metadata(&manifest).map_err(|error| format!("{}: {error}", manifest.display()))?;
        if metadata.len() > MAX_MANIFEST_BYTES as u64 {
            return Err(format!(
                "{} exceeds the {} byte manifest limit",
                manifest.display(),
                MAX_MANIFEST_BYTES
            ));
        }
        let text = fs::read_to_string(&manifest)
            .map_err(|error| format!("{}: {error}", manifest.display()))?;
        let mut fields = HashMap::new();
        for (index, raw_line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("manifest line {} is not key=value", index + 1))?;
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                return Err(format!(
                    "manifest line {} has an empty key or value",
                    index + 1
                ));
            }
            if fields.insert(key.clone(), value.to_owned()).is_some() {
                return Err(format!("manifest key '{key}' is repeated"));
            }
        }
        let mut required = |key: &str| {
            fields
                .remove(key)
                .ok_or_else(|| format!("manifest key '{key}' is required"))
        };
        let format = required("format")?;
        if format != "1" {
            return Err(format!("unsupported module manifest format '{format}'"));
        }
        let id = required("id")?.to_ascii_lowercase();
        if id.is_empty()
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(
                "module id must contain only lowercase ASCII letters, digits or '-'".into(),
            );
        }
        Ok(Self {
            id,
            name: required("name")?,
            version: required("version")?,
            description: required("description")?,
            directory,
        })
    }
}

#[derive(Debug)]
pub struct Registry {
    pub root: PathBuf,
    pub packages: Vec<Package>,
    pub errors: Vec<String>,
}

impl Registry {
    pub fn discover(root: PathBuf) -> Self {
        let mut registry = Self {
            root: root.clone(),
            packages: Vec::new(),
            errors: Vec::new(),
        };
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return registry,
            Err(error) => {
                registry.errors.push(format!("{}: {error}", root.display()));
                return registry;
            }
        };
        let mut directories = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => match entry.file_type() {
                    Ok(kind) if kind.is_dir() => directories.push(entry.path()),
                    Ok(_) => {}
                    Err(error) => registry
                        .errors
                        .push(format!("{}: {error}", entry.path().display())),
                },
                Err(error) => registry.errors.push(format!("{}: {error}", root.display())),
            }
        }
        directories.sort_by_key(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase()
        });
        let mut ids = HashSet::new();
        for directory in directories {
            match Package::load(directory.clone()) {
                Ok(package) => {
                    if !ids.insert(package.id.clone()) {
                        registry.errors.push(format!(
                            "{}: duplicate module id '{}'",
                            directory.display(),
                            package.id
                        ));
                    } else {
                        registry.packages.push(package);
                    }
                }
                Err(error) => registry.errors.push(error),
            }
        }
        registry
    }
}

pub fn root_next_to(executable: &Path) -> PathBuf {
    executable
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(MODULES_DIRECTORY)
}

pub fn default_root() -> PathBuf {
    if let Some(root) = env::var_os("PW_MODULES_DIR").filter(|root| !root.is_empty()) {
        return PathBuf::from(root);
    }
    match env::current_exe() {
        Ok(executable) => root_next_to(&executable),
        Err(_) => PathBuf::from(MODULES_DIRECTORY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "pwe-module-test-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_package(root: &Path, folder: &str, id: &str) {
        let directory = root.join(folder);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(MODULE_MANIFEST),
            format!(
                "format=1\nid={id}\nname={folder}\nversion=1.2.3\ndescription=Installed test module\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn executable_location_defines_the_distribution_module_root() {
        let executable = Path::new("/opt/Perfect World Explorer/Perfect-World-Explorer.exe");
        assert_eq!(
            root_next_to(executable),
            Path::new("/opt/Perfect World Explorer/Modules")
        );
    }

    #[test]
    fn discovery_reads_individual_module_folders() {
        let temp = TempDirectory::new();
        write_package(&temp.0, "Walker", "walker");
        write_package(&temp.0, "Example", "example");
        fs::write(temp.0.join("not-a-module.txt"), "ignored").unwrap();
        let registry = Registry::discover(temp.0.clone());
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        assert_eq!(registry.packages.len(), 2);
        assert_eq!(registry.packages[0].id, "example");
        assert_eq!(registry.packages[1].id, "walker");
        assert_eq!(registry.packages[1].name, "Walker");
        assert_eq!(registry.packages[1].version, "1.2.3");
        assert_eq!(registry.packages[1].description, "Installed test module");
        assert_eq!(registry.packages[1].directory, temp.0.join("Walker"));
    }

    #[test]
    fn absent_modules_are_an_empty_optional_installation() {
        let temp = TempDirectory::new();
        let registry = Registry::discover(temp.0.join("missing"));
        assert!(registry.packages.is_empty());
        assert!(registry.errors.is_empty());
    }

    #[test]
    fn malformed_and_duplicate_packages_do_not_register() {
        let temp = TempDirectory::new();
        let malformed = temp.0.join("Broken");
        fs::create_dir_all(&malformed).unwrap();
        fs::write(malformed.join(MODULE_MANIFEST), "format=2\nid=broken").unwrap();
        write_package(&temp.0, "First", "same");
        write_package(&temp.0, "Second", "same");
        let registry = Registry::discover(temp.0.clone());
        assert_eq!(registry.packages.len(), 1);
        assert_eq!(registry.errors.len(), 2);
        assert!(registry.errors.iter().any(|error| error.contains("format")));
        assert!(registry
            .errors
            .iter()
            .any(|error| error.contains("duplicate module id 'same'")));
    }

    #[test]
    fn shipped_populator_manifest_is_discoverable() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(MODULES_DIRECTORY);
        let registry = Registry::discover(root);
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let populator = registry
            .packages
            .iter()
            .find(|package| package.id == "populator")
            .expect("Modules/Populator must be shipped");
        assert_eq!(populator.name, "Populator");
        assert!(!populator.description.is_empty());
    }

    #[test]
    fn shipped_lighter_manifest_is_discoverable() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(MODULES_DIRECTORY);
        let registry = Registry::discover(root);
        let lighter = registry
            .packages
            .iter()
            .find(|p| p.id == "lighter")
            .expect("Modules/Lighter must be shipped");
        assert_eq!(lighter.name, "Lighter");
    }

    #[test]
    fn shipped_walker_manifest_is_discoverable() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(MODULES_DIRECTORY);
        let registry = Registry::discover(root);
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        let walker = registry
            .packages
            .iter()
            .find(|package| package.id == "walker")
            .expect("Modules/Walker must be shipped");
        assert_eq!(walker.name, "Walker");
        assert!(!walker.description.is_empty());
    }
}
