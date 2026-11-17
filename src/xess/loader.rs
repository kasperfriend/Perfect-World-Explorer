//! Shared LoadLibraryW/GetProcAddress helper for the vendor upscaling SDK
//! DLLs (Intel XeSS, NVIDIA NGX).
//!
//! The DLLs live in the same SDK directory the probe scanned (Settings →
//! `upscale_sdk_dir`, `PW_UPSCALER_SDK_DIR`, or `<exe>\sdk\`). Nothing is
//! loaded implicitly; every export the engine needs is resolved eagerly at
//! context creation so a wrong-version SDK fails with a named missing
//! export instead of a crash mid-frame.

use std::ffi::c_void;

/// The set of exports a DLL must resolve. Order matches the indexes used by
/// the caller to transmute the raw pointers.
pub(crate) fn load_sdk_dll(
    sdk_dir: &str,
    dll_name: &str,
    exports: &[&[u8]],
) -> Result<(winapi::shared::minwindef::HMODULE, Vec<*const c_void>), String> {
    use std::os::windows::ffi::OsStrExt;
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    let trimmed = sdk_dir.trim();
    if !trimmed.is_empty() {
        candidates.push(std::path::Path::new(trimmed).join(dll_name));
    }
    if let Some(dir) = std::env::var_os("PW_UPSCALER_SDK_DIR") {
        candidates.push(std::path::Path::new(&dir).join(dll_name));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("sdk").join(dll_name));
        }
        // Cargo layout: `<repo>/target/<profile>/*.exe` -> `<repo>/sdk`.
        if let Some(repo) = exe
            .parent()
            .and_then(|dir| dir.parent())
            .and_then(|dir| dir.parent())
        {
            candidates.push(repo.join("sdk").join(dll_name));
        }
    }
    candidates.push(std::path::PathBuf::from("sdk").join(dll_name));
    let mut errors: Vec<String> = Vec::new();
    for candidate in candidates {
        if !candidate.is_file() {
            errors.push(format!("{}: not found", candidate.display()));
            continue;
        }
        let wide: Vec<u16> = candidate
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { winapi::um::libloaderapi::LoadLibraryW(wide.as_ptr()) };
        if handle.is_null() {
            errors.push(format!("{}: LoadLibraryW failed", candidate.display()));
            continue;
        }
        let mut pointers: Vec<*const c_void> = Vec::with_capacity(exports.len());
        let mut failed: Option<String> = None;
        for name in exports {
            let proc = unsafe {
                winapi::um::libloaderapi::GetProcAddress(handle, name.as_ptr() as *const i8)
            };
            if proc.is_null() {
                failed = Some(format!(
                    "GetProcAddress({}) failed",
                    String::from_utf8_lossy(name)
                ));
                break;
            }
            pointers.push(proc as *const c_void);
        }
        if let Some(error) = failed {
            unsafe { winapi::um::libloaderapi::FreeLibrary(handle) };
            errors.push(format!("{}: {error}", candidate.display()));
            continue;
        }
        return Ok((handle, pointers));
    }
    Err(format!(
        "{dll_name} could not be loaded (download the XeSS SDK 3.0.2 release and copy it into sdk/):\n  {}",
        errors.join("\n  ")
    ))
}

/// Free a library obtained from [`load_sdk_dll`].
///
/// # Safety
/// All contexts created from this library must have been destroyed first.
pub(crate) unsafe fn free_sdk_dll(handle: winapi::shared::minwindef::HMODULE) {
    winapi::um::libloaderapi::FreeLibrary(handle);
}
