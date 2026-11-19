//! AMD FidelityFX SDK v2.4.0's signed Vulkan upscaler. The optional Windows
//! DLL is loaded from an explicit path, never a DX12 DLL nor the DLL search path.
use super::{extent2, Image, Images, Raw, R};
use crate::modules::lighter::temporal::Prepared;
use ash::{vk, vk::Handle};
use std::{
    ffi::{c_char, c_void, CStr},
    path::Path,
    ptr,
    sync::Arc,
};

const UPSCALE: u64 = 0x10000;
const UPSCALE_VERSION: u32 = (4 << 22) | (1 << 12) | 2;
const COMPUTE_READ: u32 = 1 << 2;
const UAV: u32 = 1 << 1;

#[repr(C)]
struct Header(u64, *mut Header);
#[repr(C)]
struct DeviceContext(vk::Device, vk::PhysicalDevice, vk::PFN_vkGetDeviceProcAddr);
#[repr(C)]
struct Backend(Header, DeviceContext);
#[repr(C)]
struct Version(Header, u32);
#[repr(C)]
struct Create {
    header: Header,
    flags: u32,
    render: [u32; 2],
    display: [u32; 2],
    message: Option<extern "C" fn(u32, *const u16)>,
}
#[repr(C)]
struct ProviderVersion(Header, u64, *const c_char);
#[repr(C)]
struct GetVersions {
    header: Header,
    effect: u64,
    device: *mut DeviceContext,
    count: *mut u64,
    ids: *mut u64,
    names: *mut *const c_char,
}

// FfxApiResource: 8-byte VkImage, eight u32 description fields, state.
#[repr(C)]
struct Resource(u64, [u32; 8], u32);
impl Resource {
    const EMPTY: Self = Self(0, [0; 8], 0);
    fn new(image: Image, state: u32) -> Self {
        let format = match image.format {
            vk::Format::R16G16B16A16_SFLOAT => 4,
            vk::Format::R32G32_SFLOAT => 6,
            vk::Format::R32_SFLOAT => 28,
            _ => unreachable!("only declared input formats can reach FFX"),
        };
        let [w, h] = image.size;
        Self(image.image.as_raw(), [2, format, w, h, 1, 1, 0, UAV], state)
    }
}
#[repr(C)]
struct Dispatch {
    header: Header,
    command: vk::CommandBuffer,
    color: Resource,
    depth: Resource,
    motion: Resource,
    exposure: Resource,
    reactive: Resource,
    composition: Resource,
    output: Resource,
    jitter: [f32; 2],
    motion_scale: [f32; 2],
    render: [u32; 2],
    display: [u32; 2],
    sharpen: bool,
    sharpness: f32,
    frame_ms: f32,
    pre_exposure: f32,
    reset: bool,
    near: f32,
    far: f32,
    fov_y: f32,
    meters: f32,
    flags: u32,
}

pub(super) struct Api {
    create: unsafe extern "C" fn(*mut *mut c_void, *mut Header, *const c_void) -> u32,
    destroy: unsafe extern "C" fn(*mut *mut c_void, *const c_void) -> u32,
    query: unsafe extern "C" fn(*mut *mut c_void, *mut Header) -> u32,
    dispatch: unsafe extern "C" fn(*mut *mut c_void, *const Header) -> u32,
    _lib: libloading::Library,
}

#[cfg(any(windows, test))]
pub(super) fn verified_dll(path: &Path) -> R<std::path::PathBuf> {
    super::verified_binary(
        path,
        36_135_696,
        "4a7e895af6e0b615d40a84d59eb8c2b1054ad91cdd0c739567940270d51bdab7",
    )
}

impl Api {
    #[cfg(windows)]
    pub(super) fn load(path: &Path) -> R<Self> {
        use libloading::os::windows::{
            Library, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        };
        if std::mem::size_of::<usize>() != 8 {
            return Err("FSR Vulkan needs a 64-bit process".into());
        }
        let path = verified_dll(path)?;
        // SAFETY: known, explicitly provided SDK binary with the pinned ABI.
        unsafe {
            let lib: libloading::Library = Library::load_with_flags(
                &path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
            .map_err(|e| e.to_string())?
            .into();
            let err = |e: libloading::Error| e.to_string();
            Ok(Self {
                create: *lib.get(b"ffxCreateContext\0").map_err(err)?,
                destroy: *lib.get(b"ffxDestroyContext\0").map_err(err)?,
                query: *lib.get(b"ffxQuery\0").map_err(err)?,
                dispatch: *lib.get(b"ffxDispatch\0").map_err(err)?,
                _lib: lib,
            })
        }
    }
    #[cfg(not(windows))]
    pub(super) fn load(_: &Path) -> R<Self> {
        Err("the pinned FidelityFX Vulkan runtime is only distributed as a Windows DLL".into())
    }

    fn choose_version(&self, device: &mut DeviceContext) -> R<(u64, &'static str)> {
        // Ask the actual Vulkan provider on the actual device. Do not allow
        // the loader's automatic selection of an ML variant with different
        // feature requirements, or reuse the DX12 version ID.
        let mut ids = [0; 16];
        let mut names = [ptr::null(); 16];
        let mut count = ids.len() as u64;
        let mut versions = GetVersions {
            header: Header(4, ptr::null_mut()),
            effect: UPSCALE,
            device,
            count: &mut count,
            ids: ids.as_mut_ptr(),
            names: names.as_mut_ptr(),
        };
        checked("ffxQuery(Vulkan upscaler versions)", unsafe {
            (self.query)(ptr::null_mut(), &mut versions.header)
        })?;
        let count = usize::try_from(count)
            .ok()
            .filter(|&n| n <= ids.len())
            .ok_or("too many FidelityFX Vulkan providers")?;
        for want in ["3.1.5", "2.3.4"] {
            for (&id, &name) in ids[..count].iter().zip(&names[..count]) {
                if id != 0
                    && !name.is_null()
                    && unsafe { CStr::from_ptr(name) }.to_bytes() == want.as_bytes()
                {
                    return Ok((id, want));
                }
            }
        }
        Err("no supported FSR 3.1.5 or 2.3.4 provider reported by the pinned Vulkan runtime".into())
    }
}

pub(super) struct Context {
    api: Arc<Api>,
    handle: *mut c_void,
    extent: [u32; 4],
    pub(super) provider: (&'static str, u64),
}

fn checked(op: &str, rc: u32) -> R<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("{op}: FidelityFX code 0x{rc:08x}"))
    }
}

impl Context {
    pub(super) fn new(api: &Arc<Api>, raw: &Raw, extent: [u32; 4]) -> R<Self> {
        let (render, display) = extent2(extent);
        let mut backend = Backend(
            Header(0x01000003, ptr::null_mut()),
            DeviceContext(raw.device.handle(), raw.physical, raw.gdpa),
        );
        let (id, name) = api.choose_version(&mut backend.1)?;
        #[repr(C)]
        struct Override(Header, u64);
        let mut version = Version(Header(UPSCALE + 0x0b, &mut backend.0), UPSCALE_VERSION);
        let mut selection = Override(Header(5, &mut version.0), id);
        let mut create = Create {
            header: Header(UPSCALE, &mut selection.0),
            flags: (1 << 0) | (1 << 3) | (1 << 5), // linear HDR, reverse Z, automatic exposure
            render,
            display,
            message: None,
        };
        let mut handle = ptr::null_mut();
        // Header chain and all data remain valid through the call.
        checked("ffxCreateContext(Vulkan)", unsafe {
            (api.create)(&mut handle, &mut create.header, ptr::null())
        })?;
        if handle.is_null() {
            return Err("FidelityFX returned no Vulkan context".into());
        }
        let mut this = Self {
            api: Arc::clone(api),
            handle,
            extent,
            provider: (name, id),
        };
        let mut info = ProviderVersion(Header(6, ptr::null_mut()), 0, ptr::null());
        checked("ffxQuery(active Vulkan provider)", unsafe {
            (api.query)(&mut this.handle, &mut info.0)
        })?;
        if info.1 != id {
            return Err("FidelityFX created a different Vulkan provider than requested".into());
        }
        eprintln!("Lighter Vulkan FSR context: {name} (provider {id:#x})");
        Ok(this)
    }

    pub(super) fn record(
        &mut self,
        cmd: vk::CommandBuffer,
        images: &Images,
        masks: [Image; 2],
        frame: &Prepared,
        ms: f32,
    ) -> R<()> {
        let (render, display) = extent2(self.extent);
        let depth = frame.camera.lens;
        let dispatch = Dispatch {
            header: Header(UPSCALE + 1, ptr::null_mut()),
            command: cmd,
            color: Resource::new(images.color, COMPUTE_READ),
            depth: Resource::new(images.depth, COMPUTE_READ),
            motion: Resource::new(images.velocity, COMPUTE_READ),
            exposure: Resource::EMPTY,
            reactive: Resource::new(masks[0], COMPUTE_READ),
            composition: Resource::new(masks[1], COMPUTE_READ),
            output: Resource::new(images.output, UAV),
            jitter: frame.jitter,
            motion_scale: [1.; 2],
            render,
            display,
            sharpen: false,
            sharpness: 0.,
            frame_ms: ms,
            pre_exposure: 1.,
            reset: frame.reset,
            near: depth.near,
            far: depth.far,
            fov_y: depth.fov_y_radians,
            meters: 1.,
            flags: 0,
        };
        // All images are on the same device. The owner has transitioned them
        // into these layouts; the FFX Vulkan backend unregisters to initial states.
        checked("ffxDispatch(Vulkan)", unsafe {
            (self.api.dispatch)(&mut self.handle, &dispatch.header)
        })
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        let _ = unsafe { (self.api.destroy)(&mut self.handle, ptr::null()) };
    }
}

#[cfg(test)]
#[test]
fn vulkan_ffx_abi_layout() {
    // Matches v2.4.0 ffx_api_vk.h / ffx_upscale.h on x64. Null callbacks and
    // descriptor chains are passed with the official C layout (not Rust bools in uint32 fields).
    if std::mem::size_of::<usize>() == 8 {
        assert_eq!(std::mem::size_of::<Backend>(), 40);
        assert_eq!(std::mem::size_of::<GetVersions>(), 56);
        assert_eq!(std::mem::size_of::<Create>(), 48);
        assert_eq!(std::mem::size_of::<Resource>(), 48);
        assert_eq!(std::mem::offset_of!(Dispatch, reset), 408);
        assert_eq!(std::mem::size_of::<Dispatch>(), 432);
    }
}
