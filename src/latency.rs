//! Settings → Graphics → **Latency**: `VSync` vs `XeLL`.
//!
//! This module owns the *standalone* XeLL session — the one the viewer creates
//! when the user picks `Latency = XeLL` and XeSS Frame Generation is not
//! already providing a XeLL context of its own (`crate::latency_owner` decides
//! between the two).
//!
//! ## What XeLL does
//!
//! Intel Xe Low Latency (`libxell.dll`) is given per-frame timing and decides
//! when the CPU may start the next frame. Two effects (see `xell_developer_
//! guide_english.md` in the SDK):
//!
//! * **render-queue reduction** — `xellSleep` holds the CPU back until the GPU
//!   queue is nearly empty, so a submitted command list starts executing almost
//!   immediately instead of waiting behind the previous frame;
//! * **vsync alignment** — with `Present(sync interval = 1)` the library lines
//!   the end of rendering up with the display's refresh while keeping the wait
//!   as short as the measured frame time allows.
//!
//! Neither replaces vsync: Intel requires a sync interval of 1 while low-latency
//! mode is on, which is why `LatencyMode::vsync` and `LatencyMode::xell` both
//! present with [`wgpu::PresentMode::Fifo`] and only the pacing differs.
//!
//! ## What the integration must do every frame
//!
//! Per Intel's integration checklist, the API is called on **every** frame, even
//! when the feature is off, and the features are controlled only through
//! `xellSetSleepMode`:
//!
//! 1. `xellSleep(frame_id)` — before any simulation work for the frame;
//! 2. `xellAddMarkerData(SIMULATION_START/SIMULATION_END)` — this viewer has no
//!    simulation phase of its own, so the pair is delivered back to back, which
//!    is what the SDK's own sample does in its idle path;
//! 3. `xellAddMarkerData(RENDERSUBMIT_START)` before the world/UI command lists
//!    are submitted and `RENDERSUBMIT_END` after the last of them;
//! 4. `xellAddMarkerData(PRESENT_START)` before `present`, `PRESENT_END` after.
//!
//! [`LatencySession`] exposes exactly those four edges, and its `Drop` destroys
//! the context. The per-frame calls are cheap no-ops when the driver decides
//! nothing is needed, so they are unconditional while the session exists.
//!
//! ## Ownership rules that are not obvious
//!
//! * XeSS Frame Generation *requires* a XeLL context and refuses to initialise
//!   its proxy swapchain without one. That context is created inside
//!   [`crate::xess::XessFrameGeneration`], so when frame generation is XeSS this
//!   module creates nothing and the frame generator's context performs the
//!   latency reduction ([`crate::LatencyOwner::FrameGeneration`]).
//! * Intel only enables latency reduction on a **non-Intel** GPU while XeSS-FG
//!   is active, so on NVIDIA/AMD hardware the Graphics tab leaves the XeLL
//!   option disabled until XeSS-FG is selected (`crate::support_latency`).
//! * All GPU work must be finished before `xellSetSleepMode` is called; the
//!   session therefore waits for an idle device first.
//!
//! ## Verification status
//!
//! Everything here compiles for Windows and is linked against DLLs whose exports
//! and constants are checked by `tools/verify-vendor-sdks.py`, but **no GPU has
//! ever run it** — there is no Windows/DX12 hardware in the development
//! environment or on the CI runners. If the context cannot be created the
//! session fails with a readable reason (stderr *and* `session.log`), the
//! on-screen mark says so, and the viewer keeps presenting exactly as it does
//! with `Latency = VSync`; it never drops the swapchain.

/// Single channel for latency diagnostics: stderr for console runs and the
/// always-on `session.log` (the viewer is a GUI-subsystem app, so stderr alone
/// vanishes when it is launched from Explorer). Lifecycle events only.
pub(crate) fn diag(line: &str) {
    eprintln!("{line}");
    crate::diagnostics::session(line);
}

#[cfg(windows)]
mod imp {
    use super::diag;
    use crate::xess::xell::{
        Xell, XELL_PRESENT_END, XELL_PRESENT_START, XELL_RENDERSUBMIT_END, XELL_RENDERSUBMIT_START,
        XELL_SIMULATION_END, XELL_SIMULATION_START,
    };
    use std::ffi::c_void;

    /// One XeLL context plus the bookkeeping the per-frame markers need.
    ///
    /// The session is created when `Latency = XeLL` becomes the active owner and
    /// dropped when it stops being one, so the context never outlives the
    /// settings that asked for it.
    pub struct LatencySession {
        xell: Xell,
        /// 1-based per-frame id, the same value handed to `xellSleep` and to
        /// every `xellAddMarkerData` call for that frame.
        frame_id: u32,
        frames: u64,
        /// Guards against a marker pair being delivered out of order when a
        /// frame is abandoned early (surface lost, frame generation failure).
        render_open: bool,
        present_open: bool,
        version: String,
    }

    impl LatencySession {
        /// Create the context on the wgpu device and enable low-latency mode.
        ///
        /// `sdk_dir` is the folder the probe scanned (Settings → "Upscale SDK
        /// folder", `PW_UPSCALER_SDK_DIR`, or the `sdk` folder next to the
        /// executable / in the repository).
        pub fn new(device: &wgpu::Device, sdk_dir: &str) -> Result<Self, String> {
            // Intel: "application must ensure any GPU activity is finished
            // before xellSetSleepMode is called".
            device.poll(wgpu::Maintain::Wait);
            let (raw_device, _queue) = crate::fsr::raw_dx12(device)?;
            let device_ptr = raw_device.as_mut_ptr() as *mut c_void;
            let xell = Xell::create(device_ptr, sdk_dir)?;
            let version = xell.version_string().to_string();
            diag(&format!(
                "[XeLL] low latency active (XeLL {version}); presents stay vsync'd with sync interval 1"
            ));
            Ok(Self {
                xell,
                frame_id: 0,
                frames: 0,
                render_open: false,
                present_open: false,
                version,
            })
        }

        /// Human-readable XeLL version, for the on-screen mark.
        pub fn version(&self) -> &str {
            &self.version
        }

        /// Frames paced by XeLL so far.
        pub fn frames(&self) -> u64 {
            self.frames
        }

        /// Start of a frame: sleep until work may begin, then deliver the
        /// simulation markers. Call once per frame before touching anything
        /// else.
        pub fn begin_frame(&mut self) {
            self.frame_id = self.frame_id.wrapping_add(1).max(1);
            self.frames += 1;
            self.render_open = false;
            self.present_open = false;
            let id = self.frame_id;
            self.xell.sleep(id);
            // This viewer has no simulation phase; the pair is delivered back to
            // back, exactly as the SDK's low-latency sample does when idle.
            self.xell.marker(id, XELL_SIMULATION_START);
            self.xell.marker(id, XELL_SIMULATION_END);
        }

        /// Before the first command list of the frame is submitted.
        pub fn render_submit_start(&mut self) {
            if self.render_open {
                return;
            }
            self.render_open = true;
            let id = self.frame_id;
            self.xell.marker(id, XELL_RENDERSUBMIT_START);
        }

        /// After the last command list (world, upscaler, UI) is submitted and
        /// before the present.
        pub fn render_submit_end(&mut self) {
            if !self.render_open {
                return;
            }
            self.render_open = false;
            let id = self.frame_id;
            self.xell.marker(id, XELL_RENDERSUBMIT_END);
        }

        /// Immediately before `Present`.
        pub fn present_start(&mut self) {
            if self.present_open {
                return;
            }
            // A frame that never opened the submit phase (e.g. the surface was
            // lost) still gets a well-formed submit pair.
            self.render_submit_end();
            self.present_open = true;
            let id = self.frame_id;
            self.xell.marker(id, XELL_PRESENT_START);
        }

        /// Immediately after `Present`.
        pub fn present_end(&mut self) {
            if !self.present_open {
                return;
            }
            self.present_open = false;
            let id = self.frame_id;
            self.xell.marker(id, XELL_PRESENT_END);
        }
    }
}

#[cfg(windows)]
pub use imp::LatencySession;

#[cfg(not(windows))]
mod imp_stub {
    /// Stub of the standalone XeLL session; creation always fails off Windows,
    /// where the whole XeLL model (DXGI flip-model timing) does not exist.
    pub struct LatencySession;

    impl LatencySession {
        pub fn new(_device: &wgpu::Device, _sdk_dir: &str) -> Result<Self, String> {
            Err("XeLL requires Windows and a DX12 flip-model swapchain".to_string())
        }

        pub fn version(&self) -> &str {
            "unavailable"
        }

        pub fn frames(&self) -> u64 {
            0
        }

        pub fn begin_frame(&mut self) {}

        pub fn render_submit_start(&mut self) {}

        pub fn render_submit_end(&mut self) {}

        pub fn present_start(&mut self) {}

        pub fn present_end(&mut self) {}
    }
}

#[cfg(not(windows))]
pub use imp_stub::LatencySession;

#[cfg(test)]
mod tests {
    /// The owner rule this module relies on lives in `main.rs`; keep the two
    /// names in sync and pin the one case that would otherwise create two XeLL
    /// contexts on the same device.
    #[test]
    fn xess_frame_generation_keeps_its_own_xell_context() {
        use crate::{latency_owner, FrameGenMethod, LatencyMode, LatencyOwner};
        assert_eq!(
            latency_owner(LatencyMode::Xell, FrameGenMethod::Xess),
            LatencyOwner::FrameGeneration
        );
        assert_eq!(
            latency_owner(LatencyMode::Xell, FrameGenMethod::Fsr),
            LatencyOwner::Independent
        );
        assert_eq!(
            latency_owner(LatencyMode::Vsync, FrameGenMethod::Xess),
            LatencyOwner::Vsync
        );
    }
}
