//! Optional DirectStorage range reads for large archive payloads.
//!
//! This is deliberately a best-effort accelerator: the Microsoft SDK and
//! runtime are not redistributed with the viewer, and all calls fall back to
//! ordinary positioned reads when they are absent or reject a request. The
//! current client formats still require CPU-side parsing/decompression; this
//! does not claim GPU decompression or instant startup.

#[cfg(all(windows, pwe_directstorage))]
mod enabled {
    use std::{fs::File, os::windows::io::AsRawHandle};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFinalPathNameByHandleW(
            file: *mut core::ffi::c_void,
            path: *mut u16,
            size: u32,
            flags: u32,
        ) -> u32;
    }

    extern "C" {
        fn pwe_dstorage_read(
            path: *const u16,
            offset: u64,
            size: u32,
            output: *mut core::ffi::c_void,
        ) -> i32;
    }

    pub fn read(file: &File, offset: u64, output: &mut [u8]) -> bool {
        let Ok(size) = u32::try_from(output.len()) else {
            return false;
        };
        if output.is_empty() {
            return true;
        }
        let handle = file.as_raw_handle();
        // First query the required capacity, then obtain the canonical path
        // from the already-open handle. Never reconstruct a path from archive
        // metadata or trust an entry name for filesystem access.
        let needed = unsafe {
            GetFinalPathNameByHandleW(handle, core::ptr::null_mut(), 0, 0)
        };
        if needed == 0 || needed > 32767 {
            return false;
        }
        let mut path = vec![0u16; needed as usize + 1];
        let written = unsafe {
            GetFinalPathNameByHandleW(handle, path.as_mut_ptr(), path.len() as u32, 0)
        };
        if written == 0 || written as usize >= path.len() {
            return false;
        }
        path.truncate(written as usize);
        path.push(0);
        unsafe {
            pwe_dstorage_read(path.as_ptr(), offset, size, output.as_mut_ptr().cast()) != 0
        }
    }
}

/// Read a range through the optional DirectStorage runtime. `false` means the
/// caller should use its regular file-I/O path instead.
#[cfg(windows)]
pub fn read_range(file: &std::fs::File, offset: u64, output: &mut [u8]) -> bool {
    #[cfg(pwe_directstorage)]
    {
        enabled::read(file, offset, output)
    }
    #[cfg(not(pwe_directstorage))]
    {
        let _ = (file, offset, output);
        false
    }
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
