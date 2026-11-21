//! Vulkan's opaque cache, admitted only for the same driver/device UUID.
//! A miss or unwritable cache must not disable ray tracing.
use ash::vk;
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
};

const LIMIT: u64 = 16 << 20;
const HEADER: u32 = 32;

fn compatible(bytes: &[u8], vendor: u32, device: u32, uuid: [u8; 16]) -> bool {
    if bytes.len() < HEADER as usize || bytes.len() as u64 > LIMIT {
        return false;
    }
    let word = |i| u32::from_ne_bytes(bytes[i..i + 4].try_into().unwrap());
    word(0) == HEADER
        && word(4) == vk::PipelineCacheHeaderVersion::ONE.as_raw() as u32
        && word(8) == vendor
        && word(12) == device
        && bytes[16..32] == uuid
}

/// Resolve the persistent pipeline cache directory from environment values.
/// Precedence:
/// 1. `PW_CACHE_DIR` (if set and non-empty)
/// 2. Windows: `%LOCALAPPDATA%\PerfectWorldViewer\cache`
///    Unix: `$XDG_CACHE_HOME/PerfectWorldViewer/cache` or `$HOME/.cache/PerfectWorldViewer/cache`
pub(crate) fn resolve_cache_dir_from(
    pw_cache_dir: Option<OsString>,
    local_app_data: Option<OsString>,
    xdg_cache_home: Option<OsString>,
    home: Option<OsString>,
    windows_target: bool,
) -> Option<PathBuf> {
    if let Some(dir) = pw_cache_dir.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if windows_target {
        if let Some(local) = local_app_data.filter(|v| !v.is_empty()) {
            return Some(
                PathBuf::from(local)
                    .join("PerfectWorldViewer")
                    .join("cache"),
            );
        }
    } else {
        if let Some(xdg) = xdg_cache_home.filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(xdg).join("PerfectWorldViewer").join("cache"));
        }
        if let Some(home) = home.filter(|v| !v.is_empty()) {
            return Some(
                PathBuf::from(home)
                    .join(".cache")
                    .join("PerfectWorldViewer")
                    .join("cache"),
            );
        }
    }
    None
}

pub(crate) fn default_cache_dir() -> Option<PathBuf> {
    resolve_cache_dir_from(
        std::env::var_os("PW_CACHE_DIR"),
        std::env::var_os("LOCALAPPDATA"),
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
        cfg!(windows),
    )
    .or_else(|| super::super::Settings::path().and_then(|p| Some(p.parent()?.to_owned())))
}

/// Read and validate an on-disk pipeline cache file for `(vendor, device, uuid)`.
/// Returns `(bytes, hit)` where `hit` is true iff a compatible non-empty cache
/// file was loaded.
pub(crate) fn read_cached_bytes(
    path: Option<&Path>,
    vendor: u32,
    device: u32,
    uuid: [u8; 16],
) -> (Vec<u8>, bool) {
    let mut data = Vec::new();
    if let Some(file) = path.and_then(|p| std::fs::File::open(p).ok()) {
        if file.take(LIMIT + 1).read_to_end(&mut data).is_ok()
            && compatible(&data, vendor, device, uuid)
        {
            return (data, true);
        }
        data.clear();
    }
    (data, false)
}

pub(super) struct PipelineCache {
    pub(super) handle: vk::PipelineCache,
    pub(super) hit: bool,
    pub(super) loaded_bytes: usize,
    path: Option<PathBuf>,
    device: ash::Device,
}

impl PipelineCache {
    pub(super) fn new(
        device: &ash::Device,
        vendor: u32,
        id: u32,
        uuid: [u8; 16],
        progress: &(impl Fn(&str) + Sync),
    ) -> Self {
        let path = if cfg!(test) {
            None
        } else {
            default_cache_dir().map(|root| {
                let key = uuid.iter().map(|b| format!("{b:02x}")).collect::<String>();
                root.join("vulkan-cache")
                    .join(format!("{vendor:04x}-{id:04x}-{key}.bin"))
            })
        };
        let (cache, loaded) = Self::open(device, vendor, id, uuid, path);
        let name = cache
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("<in-memory>");
        if loaded {
            progress(&format!(
                "Vulkan RT pipeline cache: lookup {name} -> HIT ({} bytes loaded)",
                cache.loaded_bytes
            ));
        } else {
            progress(&format!(
                "Vulkan RT pipeline cache: lookup {name} -> MISS (cold compile)"
            ));
        }
        cache
    }

    fn open(
        device: &ash::Device,
        vendor: u32,
        id: u32,
        uuid: [u8; 16],
        path: Option<PathBuf>,
    ) -> (Self, bool) {
        let (data, _) = read_cached_bytes(path.as_deref(), vendor, id, uuid);
        let mut loaded = false;
        let mut loaded_bytes = 0usize;
        unsafe {
            let handle = device
                .create_pipeline_cache(
                    &vk::PipelineCacheCreateInfo::builder().initial_data(&data),
                    None,
                )
                .map(|handle| {
                    if !data.is_empty() {
                        eprintln!("Lighter Vulkan pipeline cache: loaded {} bytes", data.len());
                        loaded = true;
                        loaded_bytes = data.len();
                    }
                    handle
                })
                .or_else(|_| {
                    device.create_pipeline_cache(&vk::PipelineCacheCreateInfo::default(), None)
                })
                .unwrap_or_default();
            (
                Self {
                    handle,
                    hit: loaded,
                    loaded_bytes,
                    path,
                    device: device.clone(),
                },
                loaded,
            )
        }
    }

    pub(super) fn save_with_progress(&self, progress: &(impl Fn(&str) + Sync)) -> Option<usize> {
        let Some(path) = &self.path else {
            return None;
        };
        if self.handle == vk::PipelineCache::null() {
            return None;
        }
        let Ok(data) = (unsafe { self.device.get_pipeline_cache_data(self.handle) }) else {
            return None;
        };
        if data.len() as u64 > LIMIT || std::fs::create_dir_all(path.parent().unwrap()).is_err() {
            return None;
        }
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&tmp, &data)
            .and_then(|_| std::fs::rename(&tmp, path))
            .is_err()
        {
            let _ = std::fs::remove_file(tmp);
            return None;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cache.bin");
        progress(&format!(
            "Vulkan RT pipeline cache: wrote {} bytes to {name}",
            data.len()
        ));
        Some(data.len())
    }

    pub(super) fn save(&self) {
        let _ = self.save_with_progress(&|_| {});
    }
}
impl Drop for PipelineCache {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_pipeline_cache(self.handle, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_incorrect_device_and_truncated_driver_data() {
        let uuid = [24u8; 16];
        let mut bytes: Vec<_> = [32u32, 1, 0x10de, 123]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        bytes.extend(uuid);
        assert!(compatible(&bytes, 0x10de, 123, uuid));
        assert!(!compatible(&bytes, 0x1002, 123, uuid));
        assert!(!compatible(&bytes, 0x10de, 123, [0; 16]));
        assert!(!compatible(&bytes[..28], 0x10de, 123, uuid));
    }

    #[test]
    fn c4_pipeline_cache_default_windows_path_and_second_start_hit() {
        // 1. Windows default uses %LOCALAPPDATA%\PerfectWorldViewer\cache when PW_CACHE_DIR is unset.
        let win_default = resolve_cache_dir_from(
            None,
            Some(OsString::from("C:\\Users\\Player\\AppData\\Local")),
            None,
            None,
            true,
        )
        .expect("Windows LOCALAPPDATA fallback must resolve");
        assert_eq!(
            win_default,
            PathBuf::from("C:\\Users\\Player\\AppData\\Local")
                .join("PerfectWorldViewer")
                .join("cache")
        );

        // 2. PW_CACHE_DIR takes precedence over LOCALAPPDATA.
        let custom = resolve_cache_dir_from(
            Some(OsString::from("/tmp/custom-pw-cache")),
            Some(OsString::from("C:\\Users\\Player\\AppData\\Local")),
            None,
            None,
            true,
        )
        .unwrap();
        assert_eq!(custom, PathBuf::from("/tmp/custom-pw-cache"));

        // 3. First start is a MISS; after saving compatible cache bytes, second start is a HIT.
        let root = std::env::temp_dir().join(format!(
            "pwe-c4-cache-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&root);
        let cache_file = root.join("10de-2206-test.bin");
        let uuid = [0xABu8; 16];

        let (first_bytes, first_hit) = read_cached_bytes(Some(&cache_file), 0x10de, 0x2206, uuid);
        assert!(
            !first_hit,
            "first start with empty dir must be a cache miss"
        );
        assert!(first_bytes.is_empty());

        let mut payload: Vec<u8> = [32u32, 1, 0x10de, 0x2206]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        payload.extend(uuid);
        payload.extend([0x55u8; 256]);
        std::fs::write(&cache_file, &payload).unwrap();

        let (second_bytes, second_hit) = read_cached_bytes(Some(&cache_file), 0x10de, 0x2206, uuid);
        assert!(second_hit, "second start with same cache dir must be a hit");
        assert_eq!(second_bytes.len(), payload.len());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "requires a Vulkan ICD; does not compile a pipeline"]
    fn lighter_pipeline_cache_disk_round_trip() {
        let root = std::env::temp_dir().join(format!(
            "pwe-lighter-cache-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let path = root.join("cache.bin");
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        // Distinct device lifetimes; the second context takes its bytes from disk.
        for warm in [false, true] {
            let adapter =
                pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
            let (device, _) =
                pollster::block_on(adapter.request_device(&Default::default(), None)).unwrap();
            unsafe {
                device.as_hal::<wgpu::hal::api::Vulkan, _, _>(|hal| {
                    let hal = hal.expect("Vulkan device");
                    let id = hal
                        .shared_instance()
                        .raw_instance()
                        .get_physical_device_properties(hal.raw_physical_device());
                    let (cache, loaded) = PipelineCache::open(
                        hal.raw_device(),
                        id.vendor_id,
                        id.device_id,
                        id.pipeline_cache_uuid,
                        Some(path.clone()),
                    );
                    assert_ne!(cache.handle, vk::PipelineCache::null());
                    assert_eq!(loaded, warm);
                    cache.save();
                    assert!(compatible(
                        &std::fs::read(&path).unwrap(),
                        id.vendor_id,
                        id.device_id,
                        id.pipeline_cache_uuid
                    ));
                });
            }
        }
        eprintln!("S Vulkan cache saved and reopened across two device lifetimes (no pipeline speedup measured)");
    }
}
