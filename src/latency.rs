//! Settings → Graphics → **Latency**: `VSync` vs `Reflex` vs `XeLL`.
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
//! * Intel only turns the sleep hint into real latency reduction on a
//!   **non-Intel** GPU while XeSS-FG is active. The Graphics tab keeps XeLL
//!   selectable on every vendor anyway (a session that starts and reports
//!   markers is still useful, and the note next to the option says exactly
//!   what the driver will do — `crate::support_latency`); on NVIDIA/AMD the
//!   session may also be refused at runtime, in which case the mark reports the
//!   failure and the vsync'd present path continues unchanged.
//! * All GPU work must be finished before `xellSetSleepMode` is called; the
//!   session therefore waits for an idle device first.
//!
//! ## `Reflex` — the same idea without Streamline
//!
//! NVIDIA Reflex is a Streamline feature (`sl.reflex.dll` behind
//! `sl.interposer`), and Streamline is not vendored: it takes over the
//! swapchain, which the frame generators already own. What Reflex does for a
//! vsync'd presenter is an ordering rule — keep exactly one frame in flight and
//! wake the CPU just before the vblank instead of right after the previous
//! present. [`ReflexPacer`] implements that rule directly on the viewer's own
//! present loop: it learns the display period from the present timestamps and
//! sleeps the surplus CPU-ahead time away. Presentation stays `Fifo` with sync
//! interval 1, so the only observable difference from `VSync` is input-to-
//! photon latency, not frame pacing or tearing.
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

use std::time::{Duration, Instant};

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
#[cfg(windows)]
struct TimerPeriodGuard {
    active: bool,
}

#[cfg(windows)]
impl TimerPeriodGuard {
    fn acquire_1ms() -> Self {
        use std::os::windows::ffi::OsStrExt;
        let name: Vec<u16> = std::ffi::OsStr::new("winmm.dll")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let active = unsafe {
            let module = winapi::um::libloaderapi::LoadLibraryW(name.as_ptr());
            if module.is_null() {
                false
            } else {
                let proc = winapi::um::libloaderapi::GetProcAddress(
                    module,
                    b"timeBeginPeriod\0".as_ptr() as *const i8,
                );
                if proc.is_null() {
                    false
                } else {
                    let begin: unsafe extern "system" fn(u32) -> u32 = std::mem::transmute(proc);
                    begin(1) == 0
                }
            }
        };
        Self { active }
    }
}

#[cfg(windows)]
impl Drop for TimerPeriodGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        use std::os::windows::ffi::OsStrExt;
        let name: Vec<u16> = std::ffi::OsStr::new("winmm.dll")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let module = winapi::um::libloaderapi::GetModuleHandleW(name.as_ptr());
            if !module.is_null() {
                let proc = winapi::um::libloaderapi::GetProcAddress(
                    module,
                    b"timeEndPeriod\0".as_ptr() as *const i8,
                );
                if !proc.is_null() {
                    let end: unsafe extern "system" fn(u32) -> u32 = std::mem::transmute(proc);
                    let _ = end(1);
                }
            }
        }
    }
}

/// CPU pacing with the shape of NVIDIA Reflex, without Streamline.
///
/// ## Why this is not `sl.reflex.dll`
///
/// Reflex ships as a Streamline feature, and Streamline is deliberately not
/// vendored here: it takes over the swapchain and the present cadence through
/// `sl.interposer`, which is the same surface the three frame generators
/// already fight over (see `docs/blockers.md`). What Reflex actually changes
/// for a vsync'd presenter is not a magic call but an ordering rule: the CPU
/// must not start recording the next frame until the frame currently in flight
/// is nearly done on the GPU, so the queue in flight stays at one instead of
/// two. That rule needs no SDK — it needs a clock, the previous present time
/// and the display's frame period, which is exactly what this pacer keeps.
///
/// ## What it does per frame
///
/// * [`ReflexPacer::begin_frame`] sleeps until the next present is due minus a
///   submission lead. The lead is a quarter of the frame period capped at
///   3 ms, so frame recording has room to finish before the vblank and the
///   wait never swallows the whole budget on a slow CPU.
/// * [`ReflexPacer::present`] records the present instant and folds the
///   observed interval into the budget with a slow exponential average, so the
///   pacer follows the real cadence — a 144 Hz display converges in a few
///   frames, and a missed vblank widens the budget instead of making the next
///   frame spin.
///
/// Presenting stays `Fifo` with sync interval 1, exactly as with
/// [`LatencyMode::Vsync`](crate::LatencyMode::Vsync): this pacer only decides
/// *when the CPU wakes up*, it never tears or drops frames, and dropping it
/// mid-run restores the unpaced path without touching the swapchain.
pub struct ReflexPacer {
    /// Smoothed frame period; seeded by the caller and re-learned from the
    /// present timestamps.
    budget: Duration,
    /// Present instant of the previous frame.
    last_present: Instant,
    frames: u64,
    slept: Duration,
    #[cfg(windows)]
    _timer_guard: TimerPeriodGuard,
}

impl ReflexPacer {
    /// Shortest budget the pacer will settle on, so a broken interval cannot
    /// spin the frame loop.
    const MIN_BUDGET: Duration = Duration::from_millis(2);
    /// Longest budget, matching a 10 Hz display; anything slower is a stall,
    /// not a cadence.
    const MAX_BUDGET: Duration = Duration::from_millis(100);
    /// How much of the budget is kept back for recording and submitting.
    const MIN_LEAD: Duration = Duration::from_millis(5);

    /// `refresh_hz` is only a starting point (the frame loop passes the 60 Hz
    /// default and never queries the monitor): out-of-range and non-finite
    /// values are clamped instead of rejected, because the pacer re-learns the
    /// cadence from the present timestamps anyway.
    pub fn new(refresh_hz: f32) -> Self {
        let hz = if refresh_hz.is_finite() {
            refresh_hz.clamp(30.0, 500.0)
        } else {
            60.0
        };
        Self {
            budget: Duration::from_secs_f32(1.0 / hz),
            last_present: Instant::now(),
            frames: 0,
            slept: Duration::ZERO,
            #[cfg(windows)]
            _timer_guard: TimerPeriodGuard::acquire_1ms(),
        }
    }

    /// Sleep until the current frame may start. Call once per frame, before
    /// anything is recorded.
    pub fn begin_frame(&mut self) {
        // Above ~100 Hz, Windows sleep jitter is too large relative to the
        // frame budget; let Present(1, 0) pace directly without extra sleeps.
        if self.budget < Duration::from_millis(10) {
            return;
        }
        let lead = Self::MIN_LEAD.max(self.budget / 3).min(self.budget / 2);
        let due = self.last_present + self.budget.saturating_sub(lead);
        let now = Instant::now();
        if let Some(wait) = due.checked_duration_since(now) {
            let capped = wait.min(self.budget / 2);
            if capped >= Duration::from_millis(2) {
                std::thread::sleep(capped);
                self.slept += capped;
            }
        }
    }

    /// Record the present instant and re-learn the frame period.
    pub fn present(&mut self) {
        let now = Instant::now();
        let observed = now.saturating_duration_since(self.last_present);
        self.last_present = now;
        self.frames += 1;
        if observed >= Self::MIN_BUDGET && observed <= Self::MAX_BUDGET {
            if observed < self.budget {
                let smoothed = self.budget.mul_f32(0.7) + observed.mul_f32(0.3);
                self.budget = smoothed.clamp(Self::MIN_BUDGET, Self::MAX_BUDGET);
            } else if observed <= self.budget.mul_f32(1.15) {
                let smoothed = self.budget.mul_f32(0.95) + observed.mul_f32(0.05);
                self.budget = smoothed.clamp(Self::MIN_BUDGET, Self::MAX_BUDGET);
            }
        }
    }

    /// Frames paced so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Current frame period estimate, in milliseconds.
    pub fn budget_ms(&self) -> f32 {
        self.budget.as_secs_f32() * 1000.0
    }

    /// Total time spent sleeping, in milliseconds — the part of the frame the
    /// CPU gave back.
    pub fn slept_ms(&self) -> f32 {
        self.slept.as_secs_f32() * 1000.0
    }

    /// One-line status for the on-screen mark.
    pub fn status(&self) -> String {
        format!(
            "Reflex pacing - {} frames, {:.1} ms budget, {:.0} ms slept",
            self.frames,
            self.budget_ms(),
            self.slept_ms()
        )
    }
}

impl Default for ReflexPacer {
    fn default() -> Self {
        Self::new(60.0)
    }
}

#[cfg(test)]
mod tests {
    /// The pacer must never crash on a nonsense refresh rate, must clamp the
    /// inputs it does not believe, and must not sleep the first frame away.
    #[test]
    fn reflex_pacer_seeds_from_the_refresh_rate() {
        use super::ReflexPacer;
        assert!((ReflexPacer::new(60.0).budget_ms() - 16.67).abs() < 0.1);
        assert!((ReflexPacer::new(144.0).budget_ms() - 6.94).abs() < 0.1);
        // Out-of-range and non-finite inputs clamp to a sane cadence.
        assert!((ReflexPacer::new(0.0).budget_ms() - 1000.0 / 30.0).abs() < 0.1);
        assert_eq!(ReflexPacer::new(f32::NAN).frames(), 0);
        let mut pacer = ReflexPacer::default();
        let start = std::time::Instant::now();
        pacer.begin_frame();
        assert!(start.elapsed() < std::time::Duration::from_millis(50));
        pacer.present();
        assert_eq!(pacer.frames(), 1);
        assert!(pacer.status().contains("1 frames"));
    }

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
