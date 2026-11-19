//! Vulkan's opaque cache, admitted only for the same driver/device UUID.
//! A miss or unwritable cache must not disable ray tracing.
use ash::vk;
use std::{io::Read, path::PathBuf};

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

pub(super) struct PipelineCache {
    pub(super) handle: vk::PipelineCache,
    path: Option<PathBuf>,
    device: ash::Device,
}

impl PipelineCache {
    pub(super) fn new(device: &ash::Device, vendor: u32, id: u32, uuid: [u8; 16]) -> Self {
        let path = if cfg!(test) {
            None
        } else {
            std::env::var_os("PW_CACHE_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    super::super::Settings::path().and_then(|p| Some(p.parent()?.to_owned()))
                })
                .map(|root| {
                    let key = uuid.iter().map(|b| format!("{b:02x}")).collect::<String>();
                    root.join("vulkan-cache")
                        .join(format!("{vendor:04x}-{id:04x}-{key}.bin"))
                })
        };
        Self::open(device, vendor, id, uuid, path).0
    }

    fn open(
        device: &ash::Device,
        vendor: u32,
        id: u32,
        uuid: [u8; 16],
        path: Option<PathBuf>,
    ) -> (Self, bool) {
        let mut data = Vec::new();
        if let Some(file) = path.as_ref().and_then(|p| std::fs::File::open(p).ok()) {
            if file.take(LIMIT + 1).read_to_end(&mut data).is_err()
                || !compatible(&data, vendor, id, uuid)
            {
                data.clear();
            }
        }
        let mut loaded = false;
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
                    path,
                    device: device.clone(),
                },
                loaded,
            )
        }
    }

    pub(super) fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if self.handle == vk::PipelineCache::null() {
            return;
        }
        let Ok(data) = (unsafe { self.device.get_pipeline_cache_data(self.handle) }) else {
            return;
        };
        if data.len() as u64 > LIMIT || std::fs::create_dir_all(path.parent().unwrap()).is_err() {
            return;
        }
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&tmp, &data)
            .and_then(|_| std::fs::rename(&tmp, path))
            .is_err()
        {
            let _ = std::fs::remove_file(tmp);
        }
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
