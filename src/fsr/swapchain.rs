//! The window hand-over from wgpu's swapchain to a frame generation proxy.
//!
//! ## Why this module exists
//!
//! Both frame generation SDKs replace the swapchain: FSR's
//! `ffxCreateContextDescFrameGenerationSwapChainForHwndDX12` and XeSS-FG's
//! `xefgSwapChainD3D12InitFromSwapChainDesc` ask DXGI for a flip-model
//! swapchain of their own on our HWND and present through it. DXGI allows
//! exactly one flip-model swapchain per HWND:
//!
//! > Because you can associate only one flip presentation model swap chain at
//! > a time with an HWND, the Microsoft Direct3D 11 policy of deferring the
//! > destruction of objects can cause problems if you attempt to destroy a
//! > flip presentation model swap chain and replace it with another swap
//! > chain.
//! >   — `IDXGIFactory2::CreateSwapChainForHwnd`, Microsoft docs
//!
//! Dropping `wgpu::Surface` is necessary but not sufficient. wgpu keeps the
//! backbuffer textures of a submission alive until that submission retires
//! (`wgpu-core`'s present tracker), and every one of those textures holds a
//! reference to the swapchain, so a create call that runs immediately after
//! the drop can still lose the race and fail with `DXGI_ERROR_INVALID_CALL`
//! (0x887A0001). Both SDKs report that as a generic runtime error — FSR as
//! `FFX_API_RETURN_ERROR_RUNTIME_ERROR` (0x3), XeSS-FG as
//! `XEFG_SWAPCHAIN_RESULT_ERROR_HRESULT_FAILURE` (-17) — and neither message
//! names the real problem.
//!
//! So the hand-over drains the device, then probes the window with a real
//! `CreateSwapChainForHwnd`: it creates a throwaway swapchain and releases it
//! immediately, without ever presenting, so nothing is left behind in the
//! compositor. It retries for a few hundred milliseconds while the runtime
//! catches up, and falls back through a small matrix of descriptions in case
//! the primary one is what DXGI rejects. The description DXGI accepted is the
//! one the SDK is given, together with a factory parented to our device's
//! adapter (the recipe the FSR and XeSS-FG samples both use) and a real
//! windowed fullscreen description.
//!
//! ## What the probe does not do
//!
//! It never presents and never keeps the swapchain: a create/release pair on
//! a swapchain that has not presented leaves no window association behind.
//! The caller therefore re-hands `desc`/`fullscreen`/`factory` straight to the
//! SDK and releases `factory` afterwards.

use std::ffi::c_void;
use std::time::Duration;

use winapi::shared::dxgi::{
    IDXGIAdapter, IDXGIDevice, IDXGIOutput, DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_DISCARD,
};
use winapi::shared::dxgi1_2::{
    IDXGIFactory2, IDXGISwapChain1, DXGI_SCALING_NONE, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
};
use winapi::shared::dxgi1_3;
use winapi::shared::dxgi1_5::IDXGISwapChain4;
use winapi::shared::dxgiformat::DXGI_FORMAT;
use winapi::shared::dxgitype::{
    DXGI_MODE_SCALING_UNSPECIFIED, DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED, DXGI_RATIONAL,
    DXGI_SAMPLE_DESC, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use winapi::um::unknwnbase::IUnknown;
use winapi::Interface;

/// `DXGI_ALPHA_MODE_IGNORE`; `0` stays `DXGI_ALPHA_MODE_UNSPECIFIED`. The
/// value is spelled out because `winapi`'s `dxgicommon` module is not part of
/// the import surface the rest of the crate uses.
const DXGI_ALPHA_MODE_IGNORE: u32 = 2;

/// How many passes over the candidate descriptions to make before giving up.
/// Each pass ends with a device drain and a growing sleep, so the whole
/// hand-over is bounded at roughly a third of a second — a lifecycle event,
/// never per frame.
const ROUNDS: u32 = 16;

/// A proxy swapchain description DXGI has already accepted for this HWND, the
/// factory to create it with, and the windowed fullscreen description that
/// belongs with it.
pub(crate) struct Prepared {
    pub(crate) desc: DXGI_SWAP_CHAIN_DESC1,
    pub(crate) fullscreen: DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
    /// Owning reference on a factory parented to the device's adapter (or, as
    /// a fallback, a freshly created global factory). The caller must release
    /// it once the SDK call that consumes it has returned.
    pub(crate) factory: *mut IDXGIFactory2,
    /// What happened, for the diagnostic log.
    pub(crate) report: String,
}

/// A named proxy swapchain description to try.
struct Candidate {
    label: &'static str,
    desc: DXGI_SWAP_CHAIN_DESC1,
}

/// The description both SDKs get first: three buffers, stretching, unspecified
/// alpha, waitable (XeLL and FSR's own present queue both want the waitable
/// object, and wgpu's swapchain used the same flag).
fn base_desc(width: u32, height: u32, format: DXGI_FORMAT) -> DXGI_SWAP_CHAIN_DESC1 {
    DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: format,
        Stereo: 0,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 3,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: 0, // DXGI_ALPHA_MODE_UNSPECIFIED
        Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
    }
}

fn candidates(width: u32, height: u32, format: DXGI_FORMAT) -> Vec<Candidate> {
    let base = base_desc(width, height, format);
    let mut list = vec![Candidate {
        label: "3 buffers, stretch, alpha unspecified, waitable",
        desc: base,
    }];
    let mut desc = base;
    desc.Flags |= DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING;
    list.push(Candidate {
        label: "3 buffers, stretch, alpha unspecified, waitable + allow tearing",
        desc,
    });
    let mut desc = base;
    desc.AlphaMode = DXGI_ALPHA_MODE_IGNORE;
    list.push(Candidate {
        label: "3 buffers, stretch, alpha ignore, waitable",
        desc,
    });
    let mut desc = base;
    desc.Scaling = DXGI_SCALING_NONE;
    list.push(Candidate {
        label: "3 buffers, no scaling, alpha unspecified, waitable",
        desc,
    });
    let mut desc = base;
    desc.BufferCount = 2;
    list.push(Candidate {
        label: "2 buffers, stretch, alpha unspecified, waitable",
        desc,
    });
    let mut desc = base;
    desc.BufferCount = 2;
    desc.Flags = DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING;
    list.push(Candidate {
        label: "2 buffers, stretch, alpha unspecified, allow tearing",
        desc,
    });
    list
}

/// The windowed fullscreen description: `CreateSwapChainForHwnd` takes one and
/// both SDKs forward it to DXGI, so hand them a real one instead of `NULL`.
fn windowed_fullscreen() -> DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
    DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
        RefreshRate: DXGI_RATIONAL {
            Numerator: 0,
            Denominator: 0,
        },
        ScanlineOrdering: DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED,
        Scaling: DXGI_MODE_SCALING_UNSPECIFIED,
        Windowed: 1,
    }
}

fn hresult_name(hr: i32) -> &'static str {
    match hr as u32 {
        0x887A_0001 => "DXGI_ERROR_INVALID_CALL",
        0x887A_0002 => "DXGI_ERROR_NOT_FOUND",
        0x887A_0004 => "DXGI_ERROR_UNSUPPORTED",
        0x887A_0005 => "DXGI_ERROR_DEVICE_REMOVED",
        0x887A_0006 => "DXGI_ERROR_DEVICE_HUNG",
        0x887A_0007 => "DXGI_ERROR_DEVICE_RESET",
        0x887A_0020 => "DXGI_ERROR_DRIVER_INTERNAL_ERROR",
        0x887A_0022 => "DXGI_ERROR_NOT_CURRENTLY_AVAILABLE",
        0x8007_0057 => "E_INVALIDARG",
        0x8000_4005 => "E_FAIL",
        _ => "unrecognised",
    }
}

/// The factory parented to the device's adapter:
/// `ID3D12Device` → `IDXGIDevice` → `IDXGIAdapter` → `IDXGIFactory2`.
unsafe fn adapter_factory(device_ptr: *mut c_void) -> Option<*mut IDXGIFactory2> {
    if device_ptr.is_null() {
        return None;
    }
    let device = device_ptr as *mut IUnknown;
    let mut dxgi_device: *mut IDXGIDevice = std::ptr::null_mut();
    let hr = (*device).QueryInterface(
        &IDXGIDevice::uuidof(),
        &mut dxgi_device as *mut *mut IDXGIDevice as *mut *mut c_void,
    );
    if hr < 0 || dxgi_device.is_null() {
        return None;
    }
    let mut adapter: *mut IDXGIAdapter = std::ptr::null_mut();
    let hr = (*dxgi_device).GetAdapter(&mut adapter);
    (*dxgi_device).Release();
    if hr < 0 || adapter.is_null() {
        return None;
    }
    let mut factory: *mut IDXGIFactory2 = std::ptr::null_mut();
    let hr = (*(adapter as *mut IUnknown)).QueryInterface(
        &IDXGIFactory2::uuidof(),
        &mut factory as *mut *mut IDXGIFactory2 as *mut *mut c_void,
    );
    (*adapter).Release();
    if hr < 0 || factory.is_null() {
        return None;
    }
    Some(factory)
}

/// A fresh global factory. Second choice: the adapter-parented one above is
/// what the SDK samples pass, and it cannot disagree with the device about
/// which adapter may present to the window.
unsafe fn global_factory() -> Option<*mut IDXGIFactory2> {
    let mut factory: *mut IDXGIFactory2 = std::ptr::null_mut();
    let hr = dxgi1_3::CreateDXGIFactory2(
        0,
        &IDXGIFactory2::uuidof(),
        &mut factory as *mut *mut IDXGIFactory2 as *mut *mut c_void,
    );
    if hr < 0 || factory.is_null() {
        None
    } else {
        Some(factory)
    }
}

/// Create a swapchain for the window and release it again, without presenting.
///
/// A success means DXGI considers the window free and the description valid.
/// The second half of the answer is the `IDXGISwapChain4` query that both
/// SDKs perform right after creating their proxy (FFX stores the result, XeSS-FG
/// needs the interface), so a failure there is visible before the SDK call.
unsafe fn attempt(
    factory: *mut IDXGIFactory2,
    queue: *mut c_void,
    hwnd: *mut c_void,
    desc: &DXGI_SWAP_CHAIN_DESC1,
    fullscreen: &DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
) -> (i32, bool) {
    let mut probe: *mut IDXGISwapChain1 = std::ptr::null_mut();
    let hr = (*factory).CreateSwapChainForHwnd(
        queue as *mut IUnknown,
        hwnd as *mut winapi::shared::windef::HWND__,
        desc,
        fullscreen,
        std::ptr::null_mut::<IDXGIOutput>(),
        &mut probe,
    );
    let mut swapchain4 = false;
    if !probe.is_null() {
        let mut four: *mut IDXGISwapChain4 = std::ptr::null_mut();
        (*(probe as *mut IUnknown)).QueryInterface(
            &IDXGISwapChain4::uuidof(),
            &mut four as *mut *mut IDXGISwapChain4 as *mut *mut c_void,
        );
        if !four.is_null() {
            (*four).Release();
            swapchain4 = true;
        }
        (*probe).Release();
    }
    (hr, swapchain4)
}

/// Prepare the hand-over of `hwnd` for a proxy swapchain of `width`×`height`
/// in `backbuffer_format`.
///
/// Returns the description DXGI accepted, so the SDK is asked for a swapchain
/// that is known to be creatable on this window right now.
pub(crate) fn prepare(
    device: &wgpu::Device,
    hwnd: *mut c_void,
    width: u32,
    height: u32,
    backbuffer_format: DXGI_FORMAT,
) -> Result<Prepared, String> {
    let (raw_device, raw_queue) = super::raw_dx12(device)?;
    let device_ptr = raw_device.as_mut_ptr() as *mut c_void;
    let queue_ptr = raw_queue.as_mut_ptr() as *mut c_void;

    // The frames that were queued before the surface was dropped are still
    // holding their backbuffer textures — and with them the old swapchain.
    // Drain them before asking DXGI for the window.
    device.poll(wgpu::Maintain::Wait);

    let mut factories: Vec<(&'static str, *mut IDXGIFactory2)> = Vec::new();
    if let Some(factory) = unsafe { adapter_factory(device_ptr) } {
        factories.push(("adapter factory", factory));
    }
    if let Some(factory) = unsafe { global_factory() } {
        factories.push(("global factory", factory));
    }
    if factories.is_empty() {
        return Err(
            "no DXGI factory could be created for the frame generation swapchain".to_string(),
        );
    }

    let fullscreen = windowed_fullscreen();
    let list = candidates(width, height, backbuffer_format);
    let mut report = String::new();
    let mut attempts = 0u32;
    let mut last_hr: Option<i32> = None;

    for round in 0..ROUNDS {
        for (factory_name, factory) in factories.iter() {
            for candidate in list.iter() {
                let (hr, swapchain4) =
                    unsafe { attempt(*factory, queue_ptr, hwnd, &candidate.desc, &fullscreen) };
                attempts += 1;
                if hr >= 0 {
                    report.push_str(&format!(
                        "accepted '{}' from the {factory_name} after {attempts} attempt(s) (round {round}); IDXGISwapChain4 {}",
                        candidate.label,
                        if swapchain4 { "yes" } else { "no" }
                    ));
                    // The probe's own release has to be over before the SDK
                    // asks for its swapchain, or the SDK loses the same race
                    // the probe just won.
                    device.poll(wgpu::Maintain::Wait);
                    std::thread::sleep(Duration::from_millis(2));
                    let winner = candidate.desc;
                    // Release every factory but the one handed over.
                    let keep = *factory;
                    for (_, other) in factories.iter() {
                        if *other != keep {
                            unsafe { (**other).Release() };
                        }
                    }
                    return Ok(Prepared {
                        desc: winner,
                        fullscreen,
                        factory: keep,
                        report,
                    });
                }
                if last_hr != Some(hr) {
                    report.push_str(&format!(
                        "round {round}, {factory_name}, '{}': 0x{:08x} ({}); ",
                        candidate.label,
                        hr as u32,
                        hresult_name(hr)
                    ));
                    last_hr = Some(hr);
                }
            }
        }
        // Nothing worked yet. Let the GPU retire the frames that still hold
        // the old swapchain and give the runtime a moment before retrying.
        device.poll(wgpu::Maintain::Wait);
        std::thread::sleep(Duration::from_millis(2 + u64::from(round) * 2));
    }

    for (_, factory) in factories.iter() {
        unsafe { (**factory).Release() };
    }
    Err(format!(
        "no swapchain could be created for the window ({attempts} attempts over ~{} ms); \
         DXGI kept answering {}. {report}That answer means the window is still associated with a \
         flip-model swapchain, which would be ours or the desktop compositor's; closing other \
         windows that captured the viewer (screen recorders, overlays) usually lets the hand-over \
         through.",
        (0..ROUNDS).map(|r| 2 + u64::from(r) * 2).sum::<u64>(),
        last_hr
            .map(|hr| format!("0x{:08x} ({})", hr as u32, hresult_name(hr)))
            .unwrap_or_else(|| "nothing at all".to_string())
    ))
}
