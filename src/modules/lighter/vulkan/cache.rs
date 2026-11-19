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
            super::super::Settings::path().and_then(|path| {
                let key = uuid.iter().map(|b| format!("{b:02x}")).collect::<String>();
                Some(
                    path.parent()?
                        .join("vulkan-cache")
                        .join(format!("{vendor:04x}-{id:04x}-{key}.bin")),
                )
            })
        };
        let mut data = Vec::new();
        if let Some(file) = path.as_ref().and_then(|p| std::fs::File::open(p).ok()) {
            if file.take(LIMIT + 1).read_to_end(&mut data).is_err()
                || !compatible(&data, vendor, id, uuid)
            {
                data.clear();
            }
        }
        unsafe {
            let handle = device
                .create_pipeline_cache(
                    &vk::PipelineCacheCreateInfo::builder().initial_data(&data),
                    None,
                )
                .or_else(|_| {
                    device.create_pipeline_cache(&vk::PipelineCacheCreateInfo::default(), None)
                })
                .unwrap_or_default();
            Self {
                handle,
                path,
                device: device.clone(),
            }
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
}
