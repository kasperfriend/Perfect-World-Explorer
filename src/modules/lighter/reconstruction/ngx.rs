//! The optional bridge links the official SDK loader. Keep the C++ parameter
//! interface and vendor function signatures on its side of the boundary.
use super::{Images, Raw, R};
use crate::modules::lighter::temporal::Prepared;
use ash::vk;
use std::{
    ffi::{c_char, c_void, CStr, CString},
    path::Path,
    ptr,
    sync::Arc,
};

// v1 had a larger Images struct; reject old bridges before any SDK call.
const ABI_VERSION: u32 = 2;

#[repr(C)]
struct Eval {
    jitter: [f32; 2],
    ms: f32,
    reset: u32,
    view: [f32; 16],
    projection: [f32; 16],
}

pub(super) struct Api {
    extensions: unsafe extern "C" fn(
        vk::Instance,
        vk::PhysicalDevice,
        *const c_char,
        *const c_char,
        *mut vk::ExtensionProperties,
        u32,
        *mut u32,
    ) -> u32,
    open: unsafe extern "C" fn(
        vk::Instance,
        vk::PhysicalDevice,
        vk::Device,
        vk::PFN_vkGetInstanceProcAddr,
        vk::PFN_vkGetDeviceProcAddr,
        *const c_char,
        *const c_char,
        *const u32,
        *mut *mut c_void,
    ) -> u32,
    record: unsafe extern "C" fn(*mut c_void, vk::CommandBuffer, *const Images, *const Eval) -> u32,
    close: unsafe extern "C" fn(*mut c_void),
    sdk: CString,
    log: CString,
    _lib: libloading::Library,
}

fn check(what: &str, code: u32) -> R<()> {
    if code == 1 {
        Ok(())
    } else {
        Err(format!("NGX {what}: 0x{code:08x}"))
    }
}

fn path_cstring(path: &Path) -> R<CString> {
    CString::new(path.to_str().ok_or("NGX requires UTF-8 paths")?).map_err(|e| e.to_string())
}

impl Api {
    pub(super) fn load(bridge: &Path, sdk: &Path) -> R<Self> {
        if std::mem::size_of::<usize>() != 8 {
            return Err("DLSS RR bridge requires x64".into());
        }
        let sdk = sdk
            .canonicalize()
            .map_err(|e| format!("{}: {e}", sdk.display()))?;
        let (feature, size, sha256) = if cfg!(windows) {
            (
                "nvngx_dlssd.dll",
                48_343_664,
                "b49435ae346f227feafe7bbe937d88277ea0ee7323975ec06488f377fdb46cee",
            )
        } else {
            (
                "libnvidia-ngx-dlssd.so",
                48_238_232,
                "a5d59de18841f15779039dda567a15dd723ba737915f489271fdb5f109fbfb96",
            )
        };
        super::verified_binary(&sdk.join(feature), size, sha256)?;
        let log = super::super::Settings::path()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| Path::new("cache").to_path_buf())
            .join("ngx-ray-reconstruction");
        std::fs::create_dir_all(&log).map_err(|e| e.to_string())?;
        let log = log.canonicalize().map_err(|e| e.to_string())?;
        let bridge = bridge
            .canonicalize()
            .map_err(|e| format!("{}: {e}", bridge.display()))?;
        // SAFETY: explicit opt-in, user-provided native bridge built from
        // tools/ray-reconstruction. Retain the module through all contexts.
        unsafe {
            let err = |e: libloading::Error| e.to_string();
            #[cfg(not(windows))]
            let lib = libloading::Library::new(&bridge).map_err(err)?;
            #[cfg(windows)]
            let lib: libloading::Library = {
                use libloading::os::windows::{
                    Library, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
                };
                Library::load_with_flags(
                    &bridge,
                    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                )
                .map_err(err)?
                .into()
            };
            let version = lib
                .get::<unsafe extern "C" fn() -> u32>(b"pwe_rr_api_version\0")
                .map_err(err)?;
            if version() != ABI_VERSION {
                return Err(format!(
                    "mismatched PWE Vulkan RR bridge ABI (expected {ABI_VERSION})"
                ));
            }
            Ok(Self {
                extensions: *lib.get(b"pwe_rr_extensions\0").map_err(err)?,
                open: *lib.get(b"pwe_rr_open\0").map_err(err)?,
                record: *lib.get(b"pwe_rr_record\0").map_err(err)?,
                close: *lib.get(b"pwe_rr_close\0").map_err(err)?,
                sdk: path_cstring(&sdk)?,
                log: path_cstring(&log)?,
                _lib: lib,
            })
        }
    }

    pub(super) fn extensions(
        &self,
        instance: vk::Instance,
        physical: vk::PhysicalDevice,
    ) -> R<Vec<CString>> {
        let mut props = [vk::ExtensionProperties::default(); 32];
        let mut count = 0;
        check("extension discovery", unsafe {
            (self.extensions)(
                instance,
                physical,
                self.sdk.as_ptr(),
                self.log.as_ptr(),
                props.as_mut_ptr(),
                props.len() as u32,
                &mut count,
            )
        })?;
        props
            .get(..count as usize)
            .ok_or("NGX returned too many Vulkan extensions")?
            .iter()
            .map(|p| {
                let name: &[u8] = bytemuck::cast_slice(&p.extension_name);
                let name = CStr::from_bytes_until_nul(name).map_err(|e| e.to_string())?;
                if !name.to_bytes().starts_with(b"VK_") {
                    return Err("NGX returned an invalid extension name".into());
                }
                Ok(name.to_owned())
            })
            .collect()
    }
}

pub(super) struct Context {
    api: Arc<Api>,
    handle: *mut c_void,
}
impl Context {
    pub(super) fn new(api: &Arc<Api>, raw: &Raw, extent: [u32; 4]) -> R<Self> {
        let mut handle = ptr::null_mut();
        check(
            "Vulkan RR context/optimal extent (set a supported fixed ray size)",
            unsafe {
                (api.open)(
                    raw.instance.handle(),
                    raw.physical,
                    raw.device.handle(),
                    raw.gipa,
                    raw.gdpa,
                    api.sdk.as_ptr(),
                    api.log.as_ptr(),
                    extent.as_ptr(),
                    &mut handle,
                )
            },
        )?;
        if handle.is_null() {
            return Err("NGX returned no Vulkan RR context".into());
        }
        Ok(Self {
            api: Arc::clone(api),
            handle,
        })
    }

    pub(super) fn record(
        &mut self,
        cmd: vk::CommandBuffer,
        images: &Images,
        frame: &Prepared,
        ms: f32,
    ) -> R<()> {
        let eval = Eval {
            jitter: frame.jitter,
            ms,
            reset: u32::from(frame.reset),
            // NGX expects row-major matrices. Jitter is supplied separately.
            view: frame.camera.view.transpose().to_cols_array(),
            projection: frame.camera.projection.transpose().to_cols_array(),
        };
        check("Vulkan RR evaluation", unsafe {
            (self.api.record)(self.handle, cmd, images, &eval)
        })
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.handle) };
    }
}

#[cfg(test)]
#[test]
fn rr_bridge_abi() {
    if std::mem::size_of::<usize>() == 8 {
        assert_eq!(std::mem::size_of::<super::Image>(), 32);
        assert_eq!(std::mem::offset_of!(super::Image, size), 20);
        assert_eq!(std::mem::size_of::<Images>(), 256);
        assert_eq!(std::mem::offset_of!(Images, guides), 128);
        assert_eq!(std::mem::size_of::<Eval>(), 144);
        assert_eq!(std::mem::offset_of!(Eval, view), 16);
        assert_eq!(std::mem::offset_of!(Eval, projection), 80);
    }
}
