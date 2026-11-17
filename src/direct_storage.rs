//! Optional DirectStorage range reads for large archive payloads.
//!
//! This is deliberately a best-effort accelerator: the Microsoft SDK is
//! optional, the runtime may be bundled with a Windows package when available,
//! and calls fall back to ordinary positioned reads when it is absent or rejects
//! a request. The current client formats still require CPU-side
//! parsing/decompression; this does not claim GPU decompression or instant startup.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static SUCCESSES: AtomicU64 = AtomicU64::new(0);
static FALLBACKS: AtomicU64 = AtomicU64::new(0);
static BYTES_READ: AtomicU64 = AtomicU64::new(0);
static ACTIVE_READS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ReadStats {
    pub attempts: u64,
    pub successes: u64,
    pub fallbacks: u64,
    pub bytes: u64,
    pub active: usize,
}

/// A process-wide telemetry snapshot for the loading window. Only successfully
/// completed DirectStorage reads contribute to bytes/throughput; failed
/// attempts are counted separately because PCK falls back to ordinary I/O.
pub(crate) fn read_stats() -> ReadStats {
    ReadStats {
        attempts: ATTEMPTS.load(Ordering::Relaxed),
        successes: SUCCESSES.load(Ordering::Relaxed),
        fallbacks: FALLBACKS.load(Ordering::Relaxed),
        bytes: BYTES_READ.load(Ordering::Relaxed),
        active: ACTIVE_READS.load(Ordering::Relaxed),
    }
}

pub(crate) const fn compiled_in() -> bool {
    cfg!(pwe_directstorage)
}

#[cfg(all(windows, pwe_directstorage))]
mod enabled {
    use std::{
        fs::File,
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
        path::PathBuf,
        sync::Once,
    };

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFinalPathNameByHandleW(
            file: *mut core::ffi::c_void,
            path: *mut u16,
            size: u32,
            flags: u32,
        ) -> u32;
    }

    #[repr(C)]
    struct DirectStorageRange {
        offset: u64,
        size: u32,
        output: *mut core::ffi::c_void,
    }

    extern "C" {
        fn pwe_dstorage_set_runtime_dir(directory: *const u16);
        fn pwe_dstorage_read_batch(
            path: *const u16,
            ranges: *const DirectStorageRange,
            count: u32,
        ) -> i32;
    }

    fn configure_runtime_directory() {
        static CONFIGURED: Once = Once::new();
        CONFIGURED.call_once(|| {
            let runtime_dir = std::env::var_os("DIRECTSTORAGE_RUNTIME_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("DIRECTSTORAGE_SDK_DIR")
                        .map(|sdk| PathBuf::from(sdk).join("native").join("bin").join("x64"))
                })
                .unwrap_or_else(|| PathBuf::from(env!("PWE_DIRECTSTORAGE_RUNTIME_DIR")));
            let mut wide: Vec<u16> = runtime_dir.as_os_str().encode_wide().collect();
            wide.push(0);
            unsafe { pwe_dstorage_set_runtime_dir(wide.as_ptr()) };
        });
    }

    pub fn read_ranges(file: &File, ranges: &mut [(u64, &mut [u8])]) -> bool {
        if ranges.is_empty() || ranges.len() > 64 {
            return false;
        }
        configure_runtime_directory();
        let mut descriptors = Vec::with_capacity(ranges.len());
        for (offset, output) in ranges.iter_mut() {
            let Ok(size) = u32::try_from(output.len()) else {
                return false;
            };
            if size == 0 {
                return false;
            }
            descriptors.push(DirectStorageRange {
                offset: *offset,
                size,
                output: output.as_mut_ptr().cast(),
            });
        }
        let handle = file.as_raw_handle();
        // First query the required capacity, then obtain the canonical path
        // from the already-open handle. Never reconstruct a path from archive
        // metadata or trust an entry name for filesystem access.
        let needed = unsafe { GetFinalPathNameByHandleW(handle, core::ptr::null_mut(), 0, 0) };
        if needed == 0 || needed > 32767 {
            return false;
        }
        let mut path = vec![0u16; needed as usize + 1];
        let written =
            unsafe { GetFinalPathNameByHandleW(handle, path.as_mut_ptr(), path.len() as u32, 0) };
        if written == 0 || written as usize >= path.len() {
            return false;
        }
        path.truncate(written as usize);
        path.push(0);
        unsafe {
            pwe_dstorage_read_batch(
                path.as_ptr(),
                descriptors.as_ptr(),
                descriptors.len() as u32,
            ) != 0
        }
    }
}

/// Read several ranges from one archive in a single DirectStorage submit/wait.
/// `false` means the caller should use its regular file-I/O path instead.
#[cfg(windows)]
pub fn read_ranges(file: &std::fs::File, ranges: &mut [(u64, &mut [u8])]) -> bool {
    #[cfg(pwe_directstorage)]
    {
        if ranges.is_empty() {
            return false;
        }
        if ranges.len() > 64 {
            return false;
        }
        let bytes = ranges.iter().map(|(_, output)| output.len() as u64).sum();
        let count = ranges.len() as u64;
        ATTEMPTS.fetch_add(count, Ordering::Relaxed);
        ACTIVE_READS.fetch_add(count as usize, Ordering::Relaxed);
        let succeeded = enabled::read_ranges(file, ranges);
        ACTIVE_READS.fetch_sub(count as usize, Ordering::Relaxed);
        if succeeded {
            SUCCESSES.fetch_add(count, Ordering::Relaxed);
            BYTES_READ.fetch_add(bytes, Ordering::Relaxed);
        } else {
            FALLBACKS.fetch_add(count, Ordering::Relaxed);
        }
        succeeded
    }
    #[cfg(not(pwe_directstorage))]
    {
        let _ = (file, ranges);
        false
    }
}

/// Convenience wrapper for callers with one large range.
#[cfg(windows)]
pub fn read_range(file: &std::fs::File, offset: u64, output: &mut [u8]) -> bool {
    let mut ranges = [(offset, output)];
    read_ranges(file, &mut ranges)
}

#[cfg(test)]
mod tests {
    #[test]
    fn build_configuration_is_optional() {
        // The Linux CI path, and Windows builds without the proprietary SDK,
        // intentionally compile without linking or loading DirectStorage.
        assert!(cfg!(not(pwe_directstorage)) || cfg!(windows));
    }
}
