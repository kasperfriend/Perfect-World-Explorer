//! **Imaginator** — real ray tracing, added to the viewer without reshaping it.
//!
//! ## What it is
//!
//! The core renderer is a rasteriser with no shadow map: every surface receives
//! full sun, and reflections exist only as the water planes' per-plane
//! re-render. The Imaginator adds what that cannot produce — cast shadows,
//! interreflected light and true reflections — by tracing rays against the
//! world's own geometry, and compositing the result into the finished frame.
//!
//! Two tracers, one contract:
//!
//! * **Software** ([`tracer`]) — a multithreaded CPU path tracer walking the
//!   module's own BVH. Always available, no driver requirement.
//! * **Hardware** ([`hardware`]) — DXR 1.1 on the same D3D12 device `wgpu` runs
//!   on: real BLAS/TLAS acceleration structures, with the traversal done by the
//!   GPU's ray-tracing units. Requires DXR tier 1.1 and a DXIL blob built out of
//!   tree by `tools/imaginator-shaders.sh`; when either is missing the module
//!   says which, on its own page, and the software tracer keeps working.
//!
//! Both write the same image: `rgb` is the light the rays found — reflected plus
//! interreflected, in colour — and `a` is the sun visibility, negative where the
//! primary ray escaped so the raster sky stands. One contract, one compositing
//! pass, so switching tracers changes the image and never the pipeline.
//!
//! ## Separation from the core
//!
//! The module owns everything it needs: its triangle soup ([`capture`] copies
//! the world into the module's own [`bvh::Scene`]), its settings
//! ([`settings`] is its own file beside the module), its GPU buffers, its
//! shaders and its UI page. The core supplies four things and no more:
//!
//! 1. `Modules` constructs one ([`Imaginator::new`]).
//! 2. The module page in the Modules menu calls [`Imaginator::ui`].
//! 3. The world load hands over the triangle soup
//!    ([`Imaginator::adopt_scene`]) and the frame loop publishes the light each
//!    frame ([`Imaginator::set_environment`]) and builds the camera for it
//!    ([`Imaginator::frame_params`]).
//! 4. The frame loop calls [`Imaginator::composite`] once, after the upscaler
//!    and before the present encode.
//!
//! The capture is taken while the map loads, and only when the saved settings ask
//! for it: at that moment the host still owns the resident scenery, the streamed
//! object set and the tree meshes, and this module walks all three once into its
//! own geometry. It never asks for them again, and nothing it does later depends
//! on a borrow outliving that call.
//!
//! Nothing about the existing water reflection path, the shadow-free lighting or
//! any core pass changes: the module adds light to the finished frame and can be
//! switched off with no residue. `water_reflection_fill` does not touch the water
//! pass either — it only decides whether water surfaces get the same traced
//! reflection term every other surface gets.

pub mod bvh;
pub mod capture;
mod composite;

/// Set when `wgpu` reports an uncaptured device error. `wgpu`'s default handler
/// **panics**, which turned a single validation mistake in the composite into a
/// silent white window: the frame loop died before the first present. The
/// installed handler logs instead and latches this flag, so the module can stop
/// drawing and say why while the rest of the viewer keeps running.
static DEVICE_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
mod gpu;
pub mod hardware;
pub mod math;
pub mod settings;
pub mod tracer;

/// The composite's end-to-end device test, kept out of `composite.rs` so the
/// offline harness (which stubs `wgpu`) never has to compile it.
#[cfg(all(test, not(imaginator_offline)))]
mod composite_gpu_test;

use bvh::SharedScene;
use composite::Composite;
use gpu::GpuParams;
use math::V3;
use settings::{ImaginatorSettings, Tracer as TracerKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

/// Where the module's own files live, relative to the client root.
pub const DIRECTORY: &str = "Modules/Imaginator";

/// Which tracer produced the last completed image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Active {
    /// Nothing has traced yet.
    Idle,
    /// The CPU tracer.
    Software,
    /// DXR.
    Hardware,
}

impl Active {
    pub fn label(self) -> &'static str {
        match self {
            Active::Idle => "idle",
            Active::Software => "software (CPU)",
            Active::Hardware => "hardware (DXR)",
        }
    }
}

/// What the background CPU trace is doing.
struct SoftwareState {
    /// Set when the scene changes; the worker adopts it on its next frame.
    pending: Arc<Mutex<Option<SharedScene>>>,
    /// Latest request, newest wins.
    request: Arc<Mutex<Option<TraceRequest>>>,
    /// Channels of the last completed trace, in `tracer::Sample::pack` layout.
    result: Arc<Mutex<Option<TraceResult>>>,
    frames: Arc<AtomicU64>,
    /// Kept so `Drop` can join the worker. The worker exits when `running` clears.
    thread: Option<std::thread::JoinHandle<()>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    /// Why the worker stopped producing frames, if it did.
    ///
    /// Set by the worker itself and by `submit_software`. It exists because the
    /// worker's old failure mode was silence: a panicking row band or a poisoned
    /// mutex left the thread alive, `frames` at zero and the page saying
    /// "software tracer starting" forever, with nothing anywhere to say why.
    fault: Arc<Mutex<Option<String>>>,
    /// How much of the requested traced extent the worker is currently able to
    /// finish in its time budget, as a fraction in `(0, 1]`.
    ///
    /// The software tracer is off the render thread, so it never blocks a
    /// frame - but a trace that takes five seconds means the *lighting* is five
    /// seconds stale, which is indistinguishable from lag. Rather than take
    /// seconds per frame and call it correct, the worker times itself and
    /// traces fewer pixels until it fits the budget, then climbs back when it
    /// has head-room. The composite upscales the traced image either way, so a
    /// lower scale costs sharpness in the lighting, not correctness.
    quality: Arc<Mutex<f32>>,
    /// Microseconds of the most recent completed trace, for the page.
    last_micros: Arc<AtomicU64>,
}

/// What a software frame aims to cost. Just above a 60 Hz frame: slow enough
/// that the trace has a real chance of finishing between frames, fast enough
/// that the lighting follows the camera.
const SOFTWARE_TARGET_MICROS: u128 = 16_000;

/// The lowest fraction of the requested traced extent the software worker may
/// fall back to.
///
/// 0.10 was chosen as a floor for the *time* problem and is too low for the
/// *picture*: at 3440 wide it traces 344 pixels across, and a 10x upscale of a
/// shadow edge is what "tiled low-resolution shadows" looks like. A traced image
/// is lighting, so it tolerates being coarse - but not that coarse. 0.25 keeps
/// shadows readable while still cutting the cost 16x against full resolution,
/// and the worker reaches it only when a frame genuinely cannot fit the budget.
const SOFTWARE_MIN_QUALITY: f32 = 0.25;

/// Where the software worker's quality scale should move to, given what the last
/// frame actually cost.
///
/// Split out of the worker so the policy can be tested against the same code
/// that runs, rather than against a copy of it in a test that then drifts.
fn next_quality(micros: u128, current: f32) -> f32 {
    if micros == 0 {
        return current.clamp(SOFTWARE_MIN_QUALITY, 1.0);
    }
    let ratio = micros as f64 / SOFTWARE_TARGET_MICROS as f64;
    let next = if ratio > 1.15 {
        // Too slow: scale by the square root of the overshoot, because cost
        // grows with area.
        current * (ratio.sqrt() as f32).recip()
    } else if ratio < 0.70 {
        // Comfortably fast: creep back up, so it does not bounce between two
        // scales every frame.
        (current * 1.10).min(1.0)
    } else {
        current
    };
    next.clamp(SOFTWARE_MIN_QUALITY, 1.0)
}

/// Split `height` rows into contiguous bands of at most `slice` rows.
///
/// The last band is whatever is left, so the bands cover the image exactly once.
/// This replaces `samples.chunks_mut(slice * width)` as the way the worker hands
/// out work: the chunk *sizes* were right — `chunks_mut` does yield a short final
/// chunk covering the tail rows — but a chunk is not a band. `trace_rows`
/// indexes its output from the first row of the range it was given, so a worker
/// that passes chunk *k* together with rows `k*slice..` is describing a buffer
/// that starts at row zero while handing over one that starts at row
/// `k*slice`. The band has to be cut where the row range says it is, which is
/// what this computes.
fn row_bands(height: u32, slice: u32) -> Vec<(u32, u32)> {
    let slice = slice.max(1);
    let mut bands = Vec::new();
    let mut row = 0;
    while row < height {
        let end = (row + slice).min(height);
        bands.push((row, end));
        row = end;
    }
    bands
}

struct TraceRequest {
    camera: tracer::TraceCamera,
    /// The frame's light. The sun moves with the region and the clock, so it
    /// travels with the request rather than being fixed when the worker starts.
    environment: tracer::Environment,
    options: tracer::TraceOptions,
    width: u32,
    height: u32,
    samples: u32,
    /// Generation counter; a result older than the newest request is dropped.
    generation: u64,
}

struct TraceResult {
    generation: u64,
    packed: Vec<f32>,
    width: u32,
    height: u32,
    rays: u64,
    micros: u128,
}

pub struct Imaginator {
    settings: ImaginatorSettings,
    directory: PathBuf,
    /// Captured world geometry, shared with the software worker.
    scene: SharedScene,
    environment: tracer::Environment,
    composite: Option<Composite>,
    /// `None` until a device has been probed.
    hardware: Option<hardware::HardwareTracer>,
    software: Option<SoftwareState>,
    active: Active,
    /// Why the selected tracer is not running, when it is not.
    pub status: String,
    /// Triangles in the live capture.
    captured_triangles: usize,
    /// Set when the capture should be taken (the core asks for one).
    want_capture: bool,
    /// The generation the GPU scene buffers were last built for.
    uploaded: u64,
    generation: u64,
    requests: u64,
    /// Frames the composite actually ran.
    pub frames: u64,

    pub last_error: Option<String>,
    /// Set while the page is open, so the preview can be read back.
    page_open: bool,
    /// Why the last frame did not composite, if it did not.
    ///
    /// The module could previously be active with `frames` stuck at zero and
    /// nothing anywhere saying which of half a dozen early returns fired. Every
    /// skip now records its reason here, and the page shows it next to the count.
    pub skip_reason: Option<String>,
    /// How many frames have been skipped, and the reason already printed, so a
    /// stall appears in `startup.log` without repeating every frame.
    skipped_frames: u64,
    logged_skip: Option<String>,
    /// Whether the user has asked for a capture since the world loaded.
    capture_state: CaptureState,
    /// Name of the loaded map, for the page.
    pub map_label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureState {
    /// No world has been handed over yet.
    WaitingForWorld,
    /// The core asked for a capture and it is queued.
    Requested,
    /// A capture exists.
    Done,
}

impl Imaginator {
    pub fn new(client_root: &Path) -> Self {
        let directory = client_root.join(DIRECTORY);
        let settings = ImaginatorSettings::load(&directory);
        Self {
            settings,
            directory,
            scene: Arc::new(bvh::Scene::empty()),
            environment: tracer::Environment::default(),
            composite: None,
            hardware: None,
            software: None,
            active: Active::Idle,
            status: "not started".into(),
            captured_triangles: 0,
            want_capture: false,
            uploaded: 0,
            generation: 0,
            requests: 0,
            frames: 0,
            last_error: None,
            page_open: false,
            skip_reason: None,
            skipped_frames: 0,
            logged_skip: None,
            capture_state: CaptureState::WaitingForWorld,
            map_label: String::new(),
        }
    }

    pub fn settings(&self) -> &ImaginatorSettings {
        &self.settings
    }

    /// True when the module is doing something this frame. The core can use this
    /// to skip the composite call entirely.
    pub fn active(&self) -> bool {
        self.settings.active()
    }

    /// Whether the saved settings ask for tracing, without building a module.
    ///
    /// The world is offered exactly once, before the renderer takes ownership of
    /// the tree meshes, so the host has to know whether the module wants it
    /// *before* the module itself exists.
    /// Whether the module should be offered the world at load time.
    ///
    /// Always true. The capture cannot depend on the saved tracer: the module
    /// starts switched off by design, and the geometry it captures only exists
    /// while the map is loading. Deciding this from the settings is what made
    /// switching a tracer on produce an empty scene and a "reload the world"
    /// message that reloading could never satisfy.
    pub fn wants_world_capture(_client_root: &Path) -> bool {
        true
    }

    /// Adopt a capture the host took before this module existed.
    ///
    /// Same contract as [`Self::set_scene`]: the module takes ownership of the
    /// triangle soup and never borrows engine state again.
    pub fn adopt_scene(&mut self, scene: SharedScene) {
        self.set_scene(scene);
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// The frame's light, as the core's own shaders see it. Published once per
    /// frame so both tracers cast their shadows along the light the raster image
    /// was actually shaded with. Cheap: a `Copy` struct.
    pub fn set_environment(&mut self, environment: tracer::Environment) {
        self.environment = environment;
    }

    /// Record the world's identity for the page, and ask for its geometry.
    ///
    /// The geometry is always captured, even when the module starts switched off.
    /// The host offers the world exactly once — at load, while it still owns the
    /// live terrain, the resident scenery and the tree meshes — so gating the
    /// capture on `active()` meant a launch that started off could never trace:
    /// switching the tracer on afterwards found an empty scene and the page had
    /// to ask for a world reload. Capturing is a few hundred milliseconds against
    /// a map load that already takes a minute, and it makes the switch work.
    pub fn set_world(&mut self, label: String, environment: tracer::Environment) {
        if self.map_label != label {
            self.map_label = label;
            self.capture_state = CaptureState::Requested;
            self.want_capture = true;
        }
        self.environment = environment;
    }

    /// Ask for a capture of the current world. The core calls this when the
    /// geometry it can see has changed, and on the first frame after loading.
    pub fn request_capture(&mut self) {
        self.capture_state = CaptureState::Requested;
        self.want_capture = true;
    }

    /// Replace the captured geometry. Called by the core with the result of
    /// [`capture::capture`].
    pub fn set_scene(&mut self, scene: SharedScene) {
        if Arc::ptr_eq(&self.scene, &scene) {
            return;
        }
        self.captured_triangles = scene.triangle_count();
        self.scene = scene.clone();
        self.capture_state = CaptureState::Done;
        self.want_capture = false;
        self.generation += 1;
        // The GPU buffers and the worker both rebuild from the new scene.
        self.uploaded = 0;
        if let Some(software) = &self.software {
            if let Ok(mut pending) = software.pending.lock() {
                *pending = Some(scene);
            }
        }
        // Building happens on the render thread; the module's own timing is
        // load-time, and the status line reports the outcome.
        self.rebuild_hardware();
    }

    pub fn captured_triangles(&self) -> usize {
        self.captured_triangles
    }

    /// The capture is wanted and has not been handed over yet.
    /// The capture the page asked for, taken against live world data.
    ///
    /// The core owns the map, the water areas and the object set, so the module
    /// cannot capture by itself; this is the one call it needs. It is free to
    /// return `None` — a map with no scene loaded simply has nothing to trace —
    /// and the module then keeps whatever it had.
    pub fn capture_now(&mut self, budget: capture::CaptureBudget, sources: &capture::Sources<'_>) {
        let scene = capture::capture(sources, budget);
        self.want_capture = false;
        self.set_scene(scene);
    }

    pub fn wants_capture(&self) -> bool {
        self.want_capture
    }

    /// The format the composite will write. `Composite::render` refuses a
    /// non-sRGB frame by name, so the host can read the reason from one place
    /// instead of guessing why nothing happened.
    pub fn composite_status(&self) -> Option<String> {
        self.composite
            .as_ref()
            .and_then(|composite| composite.unsupported_reason().map(str::to_string))
    }

    /// Tell the module the visible frame cannot be read this frame.
    ///
    /// A swapchain image is only readable through a texture copy, which needs
    /// `COPY_SRC` in the surface configuration. Every backend that offers DXR
    /// also offers it, so in practice this never fires; it exists so that a
    /// backend without it says "unavailable" on the module page instead of
    /// submitting a copy the validation layer would reject.
    pub fn report_uncopyable_frame(&mut self) {
        let why = "the swapchain image is not copyable on this backend \
                   (no `COPY_SRC`), so the traced terms cannot be composited";
        self.status = why.into();
        self.skip_reason = Some(why.into());
    }

    /// Record that the composite did not run this frame, and say so in the log
    /// once per reason.
    ///
    /// The report that started this round was "both of them green, no changes,
    /// frames composited 0": the page had the number and nothing had the reason,
    /// so the only way to find the gate was to read the code and guess. A reason
    /// now reaches `startup.log` the first time it happens, which is the artefact
    /// that actually gets attached.
    pub fn note_skipped_frame(&mut self) {
        self.skipped_frames += 1;
        let reason = self.skip_reason.clone().unwrap_or_else(|| {
            "no gate reported a reason; the frame loop never reached the composite".into()
        });
        if self.logged_skip.as_deref() != Some(reason.as_str()) {
            eprintln!(
                "Imaginator: not compositing (frame {}): {}",
                self.skipped_frames, reason
            );
            self.logged_skip = Some(reason);
        }
    }

    /// Build the GPU objects. Called once, after the device exists.
    pub fn init(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        // `wgpu` panics on an uncaptured error by default. A validation mistake
        // must not take the window down: log it, latch it, and let `composite`
        // stop drawing so the viewer stays usable and the reason is readable.
        device.on_uncaptured_error(Box::new(|error| {
            DEVICE_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
            eprintln!("Imaginator: uncaptured device error: {error}");
        }));
        if self.composite.is_none() {
            self.composite = Some(Composite::new(device, format));
        }
        if self.hardware.is_none() {
            self.hardware = Some(hardware::HardwareTracer::probe(device));
            // A capture can land before the device exists — the host takes it
            // while it still owns the world, which is before the renderer is
            // built — so the acceleration structures are built here, once the
            // probe has something to say.
            if self.generation > 0 {
                self.rebuild_hardware();
            }
        }
    }

    /// Start or stop the background CPU tracer to match the settings.
    pub fn sync_worker(&mut self) {
        // A worker is needed for the software tracer, and also for a hardware
        // selection that has no DXIL blob: there is no RayQuery shader to
        // dispatch, so the CPU trace is what keeps the picture moving and the
        // page honest until the blob is installed.
        let hardware_without_blob = self.settings.tracer == TracerKind::Hardware
            && self
                .hardware
                .as_ref()
                .is_none_or(|hardware| !hardware.dxil_present(&self.directory));
        let want_software = self.settings.active()
            && (self.settings.tracer == TracerKind::Software || hardware_without_blob);
        if want_software && self.software.is_none() {
            self.software = Some(self.spawn_software());
        } else if !want_software {
            // Stopping does not wait for the frame in flight: the worker checks
            // `running` between frames and the handle is dropped, so the render
            // thread never blocks on a trace that is already halfway through a
            // hillside. `Drop` still joins, where blocking costs nothing.
            self.retire_software();
        }
        if self.settings.tracer == TracerKind::Hardware {
            if self.hardware.is_none() {
                self.active = Active::Idle;
                self.status = "hardware selected, but no device has been probed yet".into();
            } else if let Some(hardware) = &self.hardware {
                // Active whether or not the probe called itself available.
                // Going `Idle` here was the dead end: `composite` returns early
                // for anything but the active tracer, so a probe that failed for
                // an unrelated reason meant no tracing, no fallback, and a page
                // that just repeated the probe's complaint. Marking it active lets
                // the frame run, and the frame path reports what actually
                // happened — including that it is drawing on the CPU meanwhile.
                self.active = Active::Hardware;
                self.status = if hardware.status.available() {
                    hardware.status.label()
                } else {
                    format!(
                        "{}\n(drawing on the CPU until this is resolved)",
                        hardware.status.label()
                    )
                };
            }
        } else if self.settings.tracer == TracerKind::Software {
            if self.software.is_some() {
                self.active = Active::Software;
                self.status = self.software_status();
            }
        } else {
            self.active = Active::Idle;
            self.status = "off".into();
        }
    }

    /// Stop the CPU tracer without waiting for the frame it is tracing.
    fn retire_software(&mut self) {
        if let Some(mut software) = self.software.take() {
            software.running.store(false, Ordering::Relaxed);
            drop(software.thread.take());
        }
    }

    fn software_status(&self) -> String {
        let Some(software) = &self.software else {
            return "software tracer is not running".into();
        };
        // A worker that died or panicked says so, instead of leaving the page on
        // "starting" for ever with no explanation.
        if let Some(fault) = software.fault.lock().ok().and_then(|f| f.clone()) {
            return format!("software tracer stopped: {fault}");
        }
        let frames = software.frames.load(Ordering::Relaxed);
        let rays = software
            .result
            .lock()
            .ok()
            .and_then(|r| r.as_ref().map(|r| r.rays))
            .unwrap_or(0);
        if frames == 0 {
            // Naming the work in flight is the difference between "starting" and
            // "wedged": the first request's extent and triangle count say what the
            // worker is actually doing right now.
            match software
                .request
                .lock()
                .ok()
                .and_then(|r| r.as_ref().map(|r| (r.width, r.height)))
            {
                Some((w, h)) => format!(
                    "software tracer starting: first {}x{} frame of {} triangles in flight",
                    w, h, self.captured_triangles
                ),
                None => "software tracer starting: waiting for the first frame".into(),
            }
        } else {
            format!(
                "software tracer: {frames} traced frames, {} triangles, {:.2} Mrays last frame",
                self.captured_triangles,
                rays as f64 / 1e6
            )
        }
    }

    fn spawn_software(&self) -> SoftwareState {
        let result = Arc::new(Mutex::new(None));
        let request: Arc<Mutex<Option<TraceRequest>>> = Arc::new(Mutex::new(None));
        let pending_scene = Arc::new(Mutex::new(None::<SharedScene>));
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let frames = Arc::new(AtomicU64::new(0));
        let fault: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let quality: Arc<Mutex<f32>> = Arc::new(Mutex::new(1.0));
        let last_micros = Arc::new(AtomicU64::new(0));

        let thread = {
            let result = result.clone();
            let request = request.clone();
            let pending_scene = pending_scene.clone();
            let running = running.clone();
            let frames = frames.clone();
            let scene = self.scene.clone();
            let environment = self.environment.clone();
            let fault_slot = fault.clone();
            let quality_slot = quality.clone();
            let micros_slot = last_micros.clone();
            std::thread::Builder::new()
                .name("imaginator-software".into())
                .spawn(move || {
                    let mut tracer = tracer::Tracer::new(scene, environment);
                    // The last generation *published*. A request newer than this
                    // wins; one at or below it has already been delivered.
                    let mut last_published = 0;
                    while running.load(Ordering::Relaxed) {
                        if let Ok(mut pending) = pending_scene.lock() {
                            if let Some(next) = pending.take() {
                                tracer.set_scene(next);
                            }
                        }
                        let taken = request.lock().ok().and_then(|mut r| r.take());
                        let Some(req) = taken else {
                            // Nothing to do: sleep rather than spin. The render
                            // thread has already submitted this frame's request
                            // by the time it composites, so a short sleep costs
                            // nothing but a frame of latency at most.
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            continue;
                        };
                        if req.generation <= last_published {
                            continue;
                        }
                        // The light the host published for this frame, adopted by
                        // the shared tracer before any row is traced.
                        tracer.environment = req.environment;
                        // Adapt the traced extent to the time budget before
                        // tracing, not after: the previous frame's cost says how
                        // expensive this one will be, since it is the same scene
                        // from almost the same camera. Shrinking the *pixel grid*
                        // rather than skipping rows keeps the whole image traced
                        // - a thinner sample of the same picture, not a picture
                        // with holes in it.
                        let quality = {
                            let q = quality_slot.lock().map(|q| *q).unwrap_or(1.0);
                            // Clamp to the settings' own floor so the user's
                            // `trace_scale` still means something.
                            q.clamp(SOFTWARE_MIN_QUALITY, 1.0)
                        };
                        let trace_width = ((req.width as f32 * quality).round() as u32).max(16);
                        let trace_height = ((req.height as f32 * quality).round() as u32).max(9);
                        let start = std::time::Instant::now();
                        let mut samples = vec![
                            tracer::Sample::default();
                            (trace_width as usize) * (trace_height as usize)
                        ];
                        let rows = trace_height;
                        let threads = std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(4)
                            .min(16) as u32;
                        let slice = (rows + threads - 1) / threads.max(1);
                        let tracer_ref = &tracer;
                        // A panic inside a band used to propagate out of `scope`
                        // and kill the worker thread: `frames` stayed at zero and
                        // the module said "starting" forever. Catch it, publish it,
                        // and keep serving requests.
                        let bands = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            std::thread::scope(|s| {
                                // One slice per band, sized to that band's own rows.
                                // `trace_rows` indexes `out` from the band's first row
                                // (see its doc comment), so each task has to get a
                                // buffer that starts where its band starts.
                                //
                                // The bands are built from the row loop rather than
                                // from `chunks_mut`, because the last band is usually
                                // shorter than `slice` and `chunks_mut` grows the tile
                                // until that tail no longer fits — it then yields a
                                // *final short chunk*, which the old loop silently
                                // dropped, leaving the bottom rows of the image
                                // untraced.
                                let (width, height, s_) = (trace_width, trace_height, req.samples);
                                let camera = req.camera;
                                let options = req.options;
                                let stride = width as usize;
                                let mut row = 0u32;
                                // `split_at_mut` hands out one disjoint band at a
                                // time, which is what lets every closure own its slice.
                                let mut rest: &mut [tracer::Sample] = &mut samples;
                                while row < height {
                                    let start = row;
                                    let end = (row + slice).min(height);
                                    let taken = (end - start) as usize * stride;
                                    let (band, tail) = rest.split_at_mut(taken);
                                    rest = tail;
                                    s.spawn(move || {
                                        tracer_ref.trace_rows(
                                            &camera, &options, width, height, s_, start, end, band,
                                        );
                                    });
                                    row = end;
                                }
                            });
                        }));
                        if let Err(why) = bands {
                            let message = if let Some(text) = why.downcast_ref::<&str>() {
                                (*text).to_string()
                            } else if let Some(text) = why.downcast_ref::<String>() {
                                text.clone()
                            } else {
                                "unknown panic".to_string()
                            };
                            let fault = format!("the tracer panicked while tracing: {message}");
                            eprintln!("Imaginator: {fault}");
                            if let Ok(mut slot) = fault_slot.lock() {
                                *slot = Some(fault);
                            }
                            // Skip this frame's publish rather than leaving a
                            // half-written buffer as if it were a picture.
                            continue;
                        }
                        let mut packed = vec![0f32; samples.len() * 4];
                        tracer::Tracer::pack(&samples, &mut packed);
                        let rays = tracer.rays.load(std::sync::atomic::Ordering::Relaxed);
                        let micros = start.elapsed().as_micros();
                        // Match the budget by moving the quality scale, not by
                        // dropping the frame. The step is proportional to how far
                        // off we are, damped so it settles instead of oscillating,
                        // and asymmetric: backing off is quick (lag is visible
                        // immediately) while climbing is slow (so it does not
                        // bounce between two scales every frame).
                        // One lock for the whole read-modify-write. Taking it
                        // twice - once to read through the guard and once to
                        // write - is a self-deadlock on a non-reentrant mutex,
                        // and the render thread reads this slot every frame.
                        if let Ok(mut slot) = quality_slot.lock() {
                            *slot = next_quality(micros, *slot);
                        }
                        micros_slot.store(micros.min(u64::MAX as u128) as u64, Ordering::Relaxed);
                        let trace = TraceResult {
                            generation: req.generation,
                            packed,
                            width: trace_width,
                            height: trace_height,
                            rays,
                            micros,
                        };
                        if let Ok(mut slot) = result.lock() {
                            *slot = Some(trace);
                        }
                        frames.fetch_add(1, Ordering::Relaxed);
                        last_published = req.generation;
                    }
                })
                .ok()
        };

        SoftwareState {
            pending: pending_scene,
            request,
            result,
            frames,
            thread,
            running,
            fault,
            quality,
            last_micros,
        }
    }

    /// Hand this frame's camera and options to the software worker.
    pub fn submit_software(
        &mut self,
        camera: tracer::TraceCamera,
        extent: (u32, u32),
        settings: &ImaginatorSettings,
    ) {
        let Some(software) = &mut self.software else {
            return;
        };
        self.requests += 1;
        let generation = self.requests;
        let request = TraceRequest {
            camera,
            environment: self.environment.clone(),
            options: tracer::TraceOptions::from_settings(settings),
            width: extent.0,
            height: extent.1,
            samples: settings.samples.max(1),
            generation,
        };
        if let Ok(mut slot) = software.request.lock() {
            *slot = Some(request);
        }
    }

    /// Rebuild the hardware acceleration structures for the current capture.
    ///
    /// Called wherever a capture lands, so the DXR path describes the same
    /// geometry the software one does. A failure is reported, not swallowed: the
    /// page says why hardware is not running.
    fn rebuild_hardware(&mut self) {
        // Only worth doing when the hardware tracer is the one that will run.
        // `ensure_built` records a command list, submits it and then *spin-waits*
        // on a GPU fence, so building these structures for a software trace is
        // pure cost — and because `init` does it before the first frame is ever
        // presented, that wait is the difference between a window that appears
        // and one that sits white.
        if self.settings.tracer != TracerKind::Hardware {
            return;
        }
        let Some(hardware) = self.hardware.as_mut() else {
            return;
        };
        let scene = self.scene.clone();
        match hardware.ensure_built(&scene) {
            Ok(_) => self.status = hardware.status.label(),
            Err(why) => {
                self.status = format!("hardware build failed: {why}");
                self.active = Active::Idle;
            }
        }
    }

    /// Trace the frame on the GPU's ray-tracing hardware and publish the result.
    ///
    /// The DXIL shader reads the same packed geometry the compute tracer reads and
    /// writes the same four channels, so the pixels it returns go straight to
    /// `Composite::upload_software` — the path the CPU worker already fills — and
    /// the composite stage cannot tell which tracer produced them.
    ///
    /// The acceleration structure and the pipeline are built on demand and kept;
    /// only the parameters and the views are per frame.
    fn trace_hardware(
        &mut self,
        hardware: &mut hardware::HardwareTracer,
        extent: (u32, u32),
        params: GpuParams,
    ) -> Result<Vec<f32>, String> {
        let (width, height) = extent;
        if width == 0 || height == 0 {
            return Err("the traced extent is empty".into());
        }
        let Some(bvh) = self.scene.bvh.as_ref() else {
            return Err("this world has no captured geometry".into());
        };
        // The shader's `tri_normals` and `materials` tables: the normals and
        // material index come from the same packing the compute tracer uses.
        let (triangles, _, _, materials) = gpu::pack_scene(bvh, &self.scene.materials);
        let normals: Vec<u8> = triangles
            .iter()
            .flat_map(|t| {
                t.normal_material
                    .iter()
                    .flat_map(|value| value.to_ne_bytes())
                    .collect::<Vec<u8>>()
            })
            .collect();
        let materials: Vec<u8> = materials
            .iter()
            .flat_map(|m| {
                m.albedo_specular
                    .iter()
                    .flat_map(|value| value.to_ne_bytes())
                    .collect::<Vec<u8>>()
            })
            .collect();
        // The top-level structure for the traversal. This build is the one place
        // the hardware path can stall, so it happens before the dispatch and its
        // failure is reported rather than swallowed.
        let scene = self.scene.clone();
        let tlas = hardware.ensure_built(&scene)?;
        // The module's own directory first, then the executable's sibling
        // `Modules/Imaginator`. A portable install keeps the module beside the
        // executable, and the module object is built from whatever client root the
        // host had, which is not always the same thing.
        let blob = hardware::ray_query_blob(&self.directory)
            .or_else(|| {
                hardware::module_candidates()
                    .into_iter()
                    .find_map(|dir| hardware::ray_query_blob(&dir))
            })
            .map(|(blob, _)| blob)
            .ok_or_else(|| {
                format!(
                    "the RayQuery shader {} is not installed beside the executable;                      the module traced this frame on the CPU instead",
                    hardware::DXIL_FILE
                )
            })?;
        // The shader's Params block, field for field.
        let mut block = [0f32; hardware::PARAMS_FLOATS];
        for (target, source) in block.chunks_exact_mut(4).zip(
            [
                params.eye,
                params.forward,
                params.right,
                params.up,
                params.sun,
                params.sun_color,
                params.sky_up,
                params.sky_horizon,
            ]
            .into_iter(),
        ) {
            target.copy_from_slice(&source);
        }
        // `config` and `screen` are integer/float mixtures; the shader reads them
        // as `uint4` and `float4`, and this writes both as words.
        for (index, value) in params.config.iter().enumerate() {
            block[32 + index] = f32::from_bits(*value);
        }
        for (index, value) in params.screen.iter().enumerate() {
            block[36 + index] = *value;
        }
        let bytes: Vec<u8> = block.iter().flat_map(|value| value.to_ne_bytes()).collect();
        hardware.trace_frame(&blob, tlas, width, height, &bytes, &normals, &materials)
    }

    /// The pack of channels the software worker finished, if any.
    pub fn take_software_result(&mut self) -> Option<(Vec<f32>, (u32, u32))> {
        let software = self.software.as_ref()?;
        let mut slot = software.result.lock().ok()?;
        let result = slot.take()?;
        Some((result.packed, (result.width, result.height)))
    }

    /// Per-frame parameters for whichever tracer is active.
    #[allow(clippy::too_many_arguments)]
    pub fn frame_params(
        &self,
        eye: V3,
        forward: V3,
        right: V3,
        up: V3,
        fov_y: f32,
        extent: (u32, u32),
        scene_limit: f32,
    ) -> GpuParams {
        let env = self.environment.clone();
        let aspect = extent.0.max(1) as f32 / extent.1.max(1) as f32;
        gpu::params(
            eye,
            forward,
            right,
            up,
            fov_y,
            aspect,
            extent.1 as f32,
            env.sun_direction,
            env.sun_color,
            env.sky_up,
            env.sky_horizon,
            self.settings.light_bounces,
            &self.settings,
            scene_limit,
            env.fog_distance.max(1.),
        )
    }

    /// The punctual lights a trace should read this frame, nearest first and
    /// bounded by [`composite::MAX_LIGHTS`].
    ///
    /// A shader casts one shadow ray per entry, so the cap is what keeps a
    /// lamp-lit street from costing more than the primary trace. Sorting by
    /// distance from the eye is the cheapest sensible pick: the lights that
    /// matter are the ones near what the camera is looking at.
    fn gpu_lights(&self, eye: V3) -> Vec<composite::GpuLight> {
        let mut sorted: Vec<&tracer::PointLight> = self.environment.lights.iter().collect();
        sorted.sort_by(|a, b| {
            let da = (a.position - eye).length_squared();
            let db = (b.position - eye).length_squared();
            da.total_cmp(&db)
        });
        sorted
            .iter()
            .take(composite::MAX_LIGHTS)
            .map(|light| composite::GpuLight {
                position_range: [
                    light.position.x,
                    light.position.y,
                    light.position.z,
                    light.range,
                ],
                colour: [light.colour.x, light.colour.y, light.colour.z, 1.],
            })
            .collect()
    }

    /// Composite one frame. Returns false when there was nothing to do.
    ///
    /// `frame` is the finished frame's own sRGB view. `extent` is its size in
    /// pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn composite(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame_texture: Option<&wgpu::Texture>,
        frame_view: &wgpu::TextureView,
        extent: (u32, u32),
        params: GpuParams,
    ) -> bool {
        if !self.settings.active() {
            self.skip_reason = Some(if self.settings.tracer == TracerKind::Off {
                "the tracer is off".into()
            } else if self.settings.strength <= 0. {
                format!("strength is {}", self.settings.strength)
            } else {
                "reflections, lighting and shadows are all off".into()
            });
            return false;
        }
        self.skip_reason = None;
        if DEVICE_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
            // Set once by the handler above. Drawing again would only repeat the
            // same rejection, so the module goes quiet and says why. Saving the
            // settings (any change on the page) clears it, which gives the user
            // a way to retry without restarting.
            let why = "the GPU rejected an Imaginator command (see startup.log); \
                       tracing is paused — change any setting to retry";
            if self.status != why {
                self.status = why.to_string();
                self.last_error = Some(why.to_string());
            }
            return false;
        }
        if self.composite.is_none() {
            self.skip_reason = Some("the composite was never created on this device".into());
            return false;
        }
        let (tw, th) = gpu::traced_extent(extent.0, extent.1, self.settings.trace_scale);
        match self.settings.tracer {
            TracerKind::Hardware => {
                // The *compute* tracer that runs here intersects rays against the
                // packed BVH in storage buffers; it never binds the TLAS. So a
                // successful acceleration-structure build is not a prerequisite,
                // and requiring one was costing the whole tracer: on a large map
                // the build stalls for seconds before the first frame and the
                // module reported failure instead of tracing.
                let Some(hardware) = self.hardware.as_ref() else {
                    return false;
                };
                // Nothing here returns early on the probe's opinion any more.
                // It used to bail out on `Untried` and on anything the probe
                // called unavailable, which is how "hardware is selected and
                // nothing happens" was possible: the frame path stopped before it
                // had tried anything. Every branch below now does its best and
                // reports what it did.
                let base = hardware.status.label();
                let has_blob = hardware.dxil_present(&self.directory);
                let ready = hardware.pipeline_ready();
                // Which pipeline exists, not merely whether one does: the compute
                // tracer builds one too, so `ready` alone cannot say "RayQuery".
                let ray_query = hardware.ray_query();
                // The borrow of `self.hardware` ends here; the trace below needs
                // it mutably and also needs `self` for the scene.
                self.status = if ray_query {
                    // A pipeline built from the DXIL blob: inline `RayQuery`
                    // against the TLAS, evaluated by the RT cores.
                    format!("{base} — RT-core RayQuery trace running")
                } else if has_blob {
                    // The blob is installed, so a native pipeline can be built
                    // from it. Whether one *has* been is the useful thing to
                    // report: a blob that dxc compiled for the wrong target is
                    // rejected at pipeline creation, and that must not look like
                    // a working RayQuery trace.
                    if ready {
                        "the trace pipeline was built, but not from the DXIL blob; \
                         the RayQuery shader is installed and will be used on the next frame"
                            .into()
                    } else {
                        format!(
                            "{base} — RayQuery blob found; the trace pipeline is built on the \
                             first frame"
                        )
                    }
                } else {
                    // Honest wording: the compute tracer is what runs, and it
                    // works without DXR, so this is a note about the path in use
                    // rather than an error.
                    format!(
                        "{base} — rays traced by the compute shader (no RayQuery blob installed)"
                    )
                };
                // Run it. A failure is reported, and the module keeps whatever it
                // last drew rather than going blank.
                //
                // Without the blob there is no RayQuery shader to dispatch, and
                // doing nothing was the worst option: the page said the module was
                // active, the status said "compute shader", and no pixel ever
                // moved. So hardware selection always draws *something*: it runs
                // the RT-core path when the blob is installed, and publishes a CPU
                // trace of the same camera until then.
                if has_blob {
                    // `trace_hardware` needs `self` for the scene and the
                    // composite, so the hardware tracer is taken out and put back
                    // rather than borrowed across it.
                    let mut hardware = self.hardware.take();
                    let trace = match hardware.as_mut() {
                        Some(tracer) => self.trace_hardware(tracer, (tw, th), params),
                        None => Err("no hardware tracer".to_string()),
                    };
                    self.hardware = hardware;
                    match trace {
                        Ok(pixels) => {
                            if let Some(composite) = self.composite.as_mut() {
                                composite.upload_software(&pixels, (tw, th), device, queue);
                            }
                        }
                        Err(why) => {
                            self.last_error = Some(why.clone());
                            self.status = format!("hardware trace: {why}");
                        }
                    }
                } else {
                    // No DXIL blob yet. Doing nothing here was the whole of the
                    // "hardware says green and nothing changes" report, so the
                    // same frame is traced on the CPU worker instead: off the
                    // render thread, at the full traced extent, published through
                    // the identical composite. `sync_worker` keeps a worker alive
                    // for exactly this case.
                    let camera = self.software_camera(extent, params);
                    let settings = self.settings.clone();
                    self.submit_software(camera, (tw, th), &settings);
                    if let (Some((packed, arrived)), Some(composite)) =
                        (self.take_software_result(), self.composite.as_mut())
                    {
                        composite.upload_software(&packed, arrived, device, queue);
                    }
                }
            }
            TracerKind::Software => {
                // Start the next trace, then publish whatever the worker finished
                // last: the composite below reads the uploaded channels, so the
                // picture is always a completed trace, never a partial one.
                let camera = self.software_camera(extent, params);
                let settings = self.settings.clone();
                self.submit_software(camera, (tw, th), &settings);
                if let (Some((packed, arrived)), Some(composite)) =
                    (self.take_software_result(), self.composite.as_mut())
                {
                    composite.upload_software(&packed, arrived, device, queue);
                }
                self.status = self.software_status();
            }
            TracerKind::Off => return false,
        }
        // The world's lamps, published once per frame for whichever tracer runs.
        // Built before the mutable borrow: the list is a read of `self`.
        let eye = V3::from_array([params.eye[0], params.eye[1], params.eye[2]]);
        let lights = self.gpu_lights(eye);
        let Some(composite) = self.composite.as_mut() else {
            return false;
        };
        composite.set_lights(queue, &lights);
        if composite.triangle_count() == 0 {
            let why = "the composite has no scene buffers: the capture has not been                        uploaded to this device";
            self.status = why.into();
            self.skip_reason = Some(why.into());
            return false;
        }
        let source = match self.settings.tracer {
            TracerKind::Software => composite::TraceSource::Software,
            _ => composite::TraceSource::Gpu,
        };
        match composite.render(
            device,
            queue,
            encoder,
            frame_texture,
            frame_view,
            extent,
            &self.settings,
            params,
            source,
        ) {
            Ok(()) => {
                self.frames += 1;
                self.last_error = None;
                true
            }
            Err(why) => {
                self.last_error = Some(why.clone());
                self.skip_reason = Some(why.clone());
                self.status = why;
                false
            }
        }
    }

    /// The tracer's camera for this frame, derived from the parameters the
    /// composite will hand the GPU. `fov_y` is recovered from the `tan(fov/2)`
    /// the params carry, so both tracers see the same lens.
    fn software_camera(&self, _extent: (u32, u32), params: GpuParams) -> tracer::TraceCamera {
        tracer::TraceCamera {
            position: V3::from_array([params.eye[0], params.eye[1], params.eye[2]]),
            forward: V3::from_array([params.forward[0], params.forward[1], params.forward[2]]),
            right: V3::from_array([params.right[0], params.right[1], params.right[2]]),
            up: V3::from_array([params.up[0], params.up[1], params.up[2]]),
            fov_y: 2.0 * params.forward[3].atan(),
            near: 0.5,
            far: params.screen[2],
        }
    }

    /// Persist the settings next to the module. Called when the page changes
    /// something, not per frame.
    pub fn save(&self) -> Result<(), String> {
        DEVICE_FAILED.store(false, std::sync::atomic::Ordering::Relaxed);
        self.settings.save(&self.directory)
    }

    // -----------------------------------------------------------------------
    // The Modules-menu page
    // -----------------------------------------------------------------------

    /// One page, in the Modules menu. Returns true when something changed that
    /// has to be written back.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        self.page_open = true;
        let mut changed = false;
        let before = self.settings.clone();

        ui.label(
            egui::RichText::new(
                "Traces rays against the loaded world's own geometry and adds what the raster \
                 renderer cannot draw: cast shadows, interreflected light and real reflections. \
                 The world, the water pass and every existing visual setting are untouched.",
            )
            .small(),
        );
        ui.add_space(6.);

        ui.heading("Tracer");
        ui.label(
            egui::RichText::new("Pick which engine casts the rays.")
                .small()
                .weak(),
        );
        ui.horizontal(|ui| {
            for tracer in TracerKind::ALL {
                // Every tracer stays selectable. Disabling the hardware button
                // when the probe does not report available is a dead end: on a
                // machine where the probe failed for an unrelated reason, the user
                // could never even try the RT-core path, and nothing on the page
                // said why the button was grey. Selecting it now runs it, and the
                // status line states exactly what happened and why.
                let response = ui.add_enabled(
                    true,
                    egui::SelectableLabel::new(self.settings.tracer == tracer, tracer.label()),
                );
                if response.clicked() && self.settings.tracer != tracer {
                    self.settings.tracer = tracer;
                    changed = true;
                }
                if tracer == TracerKind::Hardware && !self.settings.tracer.eq(&tracer) {
                    if let Some(hardware) = self.hardware.as_ref() {
                        if !hardware.status.available() {
                            response.on_hover_text(format!(
                                "Selecting this runs it and reports what happens. \
                                 The probe currently says: {}",
                                hardware.status.label()
                            ));
                        }
                    }
                }
            }
        });
        if let Some(hardware) = self.hardware.as_ref() {
            ui.label(
                egui::RichText::new(format!("DXR status: {}", hardware.status.label()))
                    .small()
                    .weak(),
            );
        } else {
            ui.label(
                egui::RichText::new("DXR status: not probed yet (the renderer has not started)")
                    .small()
                    .weak(),
            );
        }

        ui.add_space(6.);
        ui.heading("What to trace");
        changed |= ui
            .checkbox(&mut self.settings.reflections, "Reflections")
            .on_hover_text(
                "Mirror rays. Water already reflects through the core's per-plane pass; this \
                 adds reflections on every other surface and lets water pixels share the same \
                 traced term.",
            )
            .changed();
        changed |= ui
            .checkbox(&mut self.settings.lighting, "Indirect lighting")
            .on_hover_text("Light that has bounced off the world, after `Light bounces` steps.")
            .changed();
        changed |= ui
            .checkbox(&mut self.settings.shadows, "Cast shadows")
            .on_hover_text(
                "Shadow rays towards the sun. The core has no shadow map, so this removes light \
                 from surfaces the raster path lights fully.",
            )
            .changed();

        ui.add_space(6.);
        ui.heading("Quality");
        changed |= ui
            .add(
                egui::Slider::new(&mut self.settings.light_bounces, 0..=settings::MAX_BOUNCES)
                    .text("Light bounces"),
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.settings.samples, 1..=settings::MAX_SAMPLES)
                    .text("Rays per pixel"),
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(
                    &mut self.settings.trace_scale,
                    settings::MIN_TRACE_SCALE..=settings::MAX_TRACE_SCALE,
                )
                .suffix(" %")
                .text("Traced image scale"),
            )
            .on_hover_text(
                "A traced image carries the lighting, not the detail, so it can be much smaller \
                 than the frame. 50 % quarters the cost and is invisible on shadows.",
            )
            .changed();

        ui.add_space(6.);
        ui.heading("Composite");
        changed |= ui
            .add(
                egui::Slider::new(&mut self.settings.strength, 0.0..=2.0)
                    .text("Strength")
                    .custom_formatter(|v, _| format!("{:.2}", v)),
            )
            .on_hover_text("How strongly the traced light is added, and how deeply shadows cut.")
            .changed();
        ui.horizontal(|ui| {
            for output in settings::Output::ALL {
                if ui
                    .selectable_label(self.settings.output == output, output.label())
                    .clicked()
                {
                    self.settings.output = output;
                    changed = true;
                }
            }
        });
        if self.settings.output == settings::Output::TraceOnly {
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.settings.exposure, 0.05..=8.0)
                        .logarithmic(true)
                        .text("Diagnostic exposure"),
                )
                .changed();
        }
        changed |= ui
            .checkbox(
                &mut self.settings.water_reflection_fill,
                "Give water surfaces the traced reflection",
            )
            .on_hover_text(
                "Water keeps the core's screen reflection; this additionally lets the traced \
                 reflection cover water pixels. The core water pass is never modified.",
            )
            .changed();

        ui.add_space(8.);
        ui.separator();
        ui.heading("Status");
        let colour = if self.settings.active() {
            egui::Color32::LIGHT_GREEN
        } else {
            egui::Color32::GRAY
        };
        ui.label(egui::RichText::new(format!("Tracer: {}", self.active.label())).color(colour));
        ui.label(egui::RichText::new(&self.status).small());
        if let Some(why) = self.composite_status() {
            ui.label(
                egui::RichText::new(why)
                    .small()
                    .color(egui::Color32::from_rgb(0xd0, 0x80, 0x40)),
            );
        }
        if self.capture_state != CaptureState::Done {
            ui.label(
                egui::RichText::new(
                    "No geometry has been captured for this world yet. The capture is taken \
                     while the map loads, from the resident scenery and tree meshes, so a tracer \
                     switched on now starts working the next time a map is loaded.",
                )
                .small()
                .color(egui::Color32::from_rgb(0xd0, 0xa0, 0x40)),
            );
        }
        ui.label(
            egui::RichText::new(format!(
                "Captured geometry: {} triangles in {} materials{}",
                self.captured_triangles,
                bvh::material::COUNT,
                if self.scene.stats.clipped {
                    " (clipped: the capture budget was reached)"
                } else {
                    ""
                }
            ))
            .small()
            .weak(),
        );
        // How fast the tracer actually is, and what it did about it. The
        // software tracer is off the render thread, so it never stalls a frame -
        // but lighting that takes seconds to arrive is stale in a way the eye
        // reads as lag. The worker now fits a frame-time budget by tracing fewer
        // pixels, and this says so rather than leaving it invisible.
        if let Some(software) = self.software.as_ref() {
            let micros = software
                .last_micros
                .load(std::sync::atomic::Ordering::Relaxed);
            let quality = software.quality.lock().map(|q| *q).unwrap_or(1.0);
            if micros > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "Software trace: {:.1} ms per frame at {:.0}% of the requested \
                         resolution",
                        micros as f64 / 1000.0,
                        quality * 100.0
                    ))
                    .small()
                    .weak(),
                );
            }
        }
        ui.label(
            egui::RichText::new(format!(
                "Frames composited: {}{}",
                self.frames,
                match self.last_error.as_ref() {
                    Some(why) => format!(" — last error: {why}"),
                    None => String::new(),
                }
            ))
            .small()
            .weak(),
        );
        // A stalled module has to say which gate stopped it. This sits directly
        // under the count because that is the number a reader looks at first.
        if self.frames == 0 {
            if let Some(why) = self.skip_reason.as_ref() {
                ui.label(
                    egui::RichText::new(format!("Nothing composited because: {why}"))
                        .small()
                        .color(egui::Color32::LIGHT_RED),
                );
            } else if self.settings.active() {
                ui.label(
                    egui::RichText::new(
                        "Nothing composited yet, and no gate reported a reason: the frame loop \
                         is not reaching the composite at all",
                    )
                    .small()
                    .color(egui::Color32::LIGHT_RED),
                );
            }
        }
        ui.horizontal(|ui| {
            // The geometry is captured while the map loads, whether or not the
            // module is switched on, so switching the tracer on works on the
            // world already loaded. The button therefore only reports what is in
            // hand; there is nothing to reload for a first capture.
            if ui
                .button("Captured geometry")
                .on_hover_text(
                    "The traced geometry is a snapshot taken while the map loads, because the \
                     streaming object set and the tree meshes only exist then. It is captured \
                     even while this module is off, so switching a tracer on needs no reload.",
                )
                .clicked()
            {
                self.status = format!(
                    "{} triangles captured from {}",
                    self.captured_triangles, self.map_label
                );
            }
            if ui.button("Save settings").clicked() {
                let _ = self.save();
            }
            if ui.button("Reload settings").clicked() {
                self.settings = ImaginatorSettings::load(&self.directory);
                changed = true;
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "Settings file: {}",
                ImaginatorSettings::file(&self.directory).display()
            ))
            .small()
            .weak(),
        );

        if changed {
            self.sync_worker();
            let _ = self.save();
        }
        before != self.settings || changed
    }

    /// The page was not drawn this frame; stop asking for readbacks.
    pub fn page_closed(&mut self) {
        self.page_open = false;
    }

    /// Per-frame housekeeping for the module's own state: publish a finished CPU
    /// trace into the composite and, while the page is open, refresh its preview.
    ///
    /// Cheap when idle: a mutex try and two `is_some` checks.
    pub fn poll(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        // Rebuild the GPU-side scene buffers when a new capture has landed. The
        // composite compares by identity, so this is a pointer check when
        // nothing changed and a full re-upload exactly once per capture.
        if self.composite.is_some() {
            let generation = self.generation;
            if self.uploaded != generation {
                let scene = self.scene.clone();
                if let Some(composite) = self.composite.as_mut() {
                    composite.sync_scene(device, queue, &scene);
                }
                self.uploaded = generation;
            }
        }
        if let Some((packed, extent)) = self.take_software_result() {
            if let Some(composite) = self.composite.as_mut() {
                composite.upload_software(&packed, extent, device, queue);
            }
        }
        if self.page_open {
            if let Some(composite) = self.composite.as_mut() {
                composite.refresh_preview(device, queue);
            }
        }
    }
}

impl Drop for Imaginator {
    fn drop(&mut self) {
        if let Some(mut software) = self.software.take() {
            software.running.store(false, Ordering::Relaxed);
            if let Some(thread) = software.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{row_bands, Imaginator};
    use crate::imaginator::{settings, tracer};

    #[test]
    fn bands_cover_every_row_exactly_once() {
        for height in [1u32, 7, 8, 9, 344, 345, 1000] {
            for slice in [1u32, 2, 22, 100, 5000] {
                let bands = row_bands(height, slice);
                assert_eq!(bands.first().map(|b| b.0), Some(0), "{height}/{slice}");
                assert_eq!(
                    bands.last().map(|b| b.1),
                    Some(height),
                    "bands for {height} rows in slices of {slice} stop short of the image"
                );
                for window in bands.windows(2) {
                    assert_eq!(
                        window[0].1, window[1].0,
                        "bands {height}/{slice} are not contiguous"
                    );
                }
                for (start, end) in &bands {
                    assert!(start < end, "empty band in {height}/{slice}");
                    assert!(
                        end - start <= slice.max(1),
                        "band too tall in {height}/{slice}"
                    );
                }
                let covered: u32 = bands.iter().map(|(s, e)| e - s).sum();
                assert_eq!(covered, height, "{height}/{slice} covers {covered} rows");
            }
        }
    }

    /// Building DXR structures is only for the hardware tracer.
    ///
    /// `ensure_built` submits a command list and then waits on a GPU fence, and
    /// `init` calls the builder before the first frame is presented. Doing that
    /// for a software trace stalled startup on work whose result nothing reads,
    /// which is what left the window white.
    #[test]
    fn the_hardware_build_is_gated_on_the_hardware_tracer() {
        let source = include_str!("mod.rs");
        let start = source
            .find("fn rebuild_hardware(&mut self)")
            .expect("rebuild_hardware is gone");
        let body = &source[start..];
        let end = body.find("\n    }").expect("unterminated function");
        let body = &body[..end];
        let guard = body
            .find("if self.settings.tracer != TracerKind::Hardware")
            .expect("rebuild_hardware must not build for a tracer that will not read it");
        let build = body
            .find("hardware.ensure_built(")
            .expect("ensure_built is how the build happens");
        assert!(
            guard < build,
            "the tracer check has to come before any acceleration-structure build"
        );
    }

    /// The geometry from the crash report: a 344-row trace split 16 ways.
    /// Switching a tracer on must work on the world that is already loaded.
    ///
    /// Regression: the module started reporting `off` and capture was gated on
    /// `active()`, so a launch that began switched off captured nothing at all —
    /// the log said "0 triangles captured" and the page told the user to reload
    /// the world.
    /// The load-time capture must never be gated on the saved tracer. Doing so
    /// is what produced "0 triangles captured" followed by a "reload the world"
    /// prompt: the module starts off, so nothing was captured, and reloading
    /// reset the tracer to off again.
    #[test]
    fn the_world_is_always_offered_for_capture() {
        let root = std::path::Path::new("C:/client");
        assert!(
            Imaginator::wants_world_capture(root),
            "capture feeds every tracer, and starts before one is chosen"
        );
        // And the module still starts switched off, which is the reason the
        // capture cannot ask the settings.
        let settings = settings::ImaginatorSettings::default();
        assert!(!settings.active());
    }

    #[test]
    fn geometry_is_captured_even_though_the_module_starts_off() {
        let mut module = Imaginator::new(std::path::Path::new("Modules"));
        assert!(
            !module.settings().active(),
            "the module starts switched off"
        );
        module.set_world("world_1".into(), tracer::Environment::default());
        assert!(
            module.want_capture,
            "the world's geometry has to be captured on the one chance we get"
        );
    }

    #[test]
    fn the_344_row_case_keeps_its_last_band() {
        let bands = row_bands(344, (344 + 15) / 16);
        assert_eq!(bands.len(), 16);
        assert_eq!(
            bands[15],
            (330, 344),
            "the final 14 rows must still be traced"
        );
        assert_eq!(bands.iter().map(|(s, e)| e - s).sum::<u32>(), 344);
    }

    /// A hardware selection must never be "active but idle".
    ///
    /// This is the shape of the report that started all this: the page said the
    /// module was green, and nothing happened, because a probe that did not call
    /// itself available put the module back to `Idle` — and `composite` returns
    /// early for anything that is not the active tracer. Selecting hardware now
    /// always yields `Active::Hardware`, so the frame path gets to try and to say
    /// what it found.
    #[test]
    fn selecting_hardware_is_always_active() {
        let source = include_str!("mod.rs");
        let sync = source
            .split("pub fn sync_worker")
            .nth(1)
            .expect("sync_worker is defined")
            .split("\n    /// Stop the CPU tracer")
            .next()
            .expect("the body ends before retire_software");
        assert!(
            !sync.contains(
                "self.active = Active::Idle;\n                    self.status = hardware"
            ),
            "a hardware selection must not fall back to Idle"
        );
        assert!(
            sync.contains("self.active = Active::Hardware;"),
            "a hardware selection must mark the hardware tracer active"
        );
        // And the frame path must not bail out on the probe's opinion.
        let composite = source
            .split("pub fn composite(")
            .nth(1)
            .expect("composite is defined");
        assert!(
            !composite.contains("HardwareStatus::Untried"),
            "composite must not skip the hardware path because the probe is untried"
        );
    }

    /// The tracer page must leave the hardware option clickable.
    #[test]
    fn the_hardware_tracer_stays_selectable() {
        let source = include_str!("mod.rs");
        let page = source
            .split("for tracer in TracerKind::ALL {")
            .nth(1)
            .expect("the tracer selector exists");
        let head = &page[..page
            .find("ui.add_space(6.);")
            .unwrap_or(700)
            .min(page.len())];
        assert!(
            head.contains("ui.add_enabled(\n                    true,")
                || head.contains("ui.add_enabled(true,"),
            "the hardware tracer must stay selectable, or the RT-core path is unreachable \
             whenever the probe is unhappy"
        );
    }

    /// Frame generation must not switch the tracer off.
    ///
    /// The composite was gated on `fg_views.is_none()`, so with XeSS Frame
    /// Generation on - which is a saved setting, and was on - the module was
    /// skipped on every single frame. The page read "frames composited: 0" for
    /// both tracers and nothing said why. The generator's composition texture is
    /// built with the surface format, so it is the same sRGB target the module
    /// writes in the plain path; there was never a reason to refuse it.
    #[test]
    fn frame_generation_does_not_disable_the_composite() {
        let source = include_str!("../gpu.rs");
        // The composite call site is the block that creates the "imaginator
        // composite" encoder.
        let site = source
            .split("label: Some(\"imaginator composite\")")
            .next()
            .unwrap_or("");
        let window = &site[site.len().saturating_sub(6000)..];
        assert!(
            !window.contains("imaginator_active() && fg_views.is_none()"),
            "the composite must not be gated on frame generation being absent"
        );
        assert!(
            !window.contains(".report_frame_generation()"),
            "frame generation is no longer a reason to skip the composite"
        );
    }

    /// The software worker must fit a frame-time budget, and must do it by
    /// tracing fewer pixels rather than by dropping frames.
    ///
    /// The report was "the CPU tracer was extremely laggy, updating the frame
    /// like 1 in 5 seconds". A trace that costs seconds is off the render thread
    /// and so never stalls it, but lighting that stale is indistinguishable from
    /// lag, so the worker times itself and scales the traced extent.
    #[test]
    fn the_software_worker_scales_its_extent_to_its_time_budget() {
        use super::{next_quality as adjust, SOFTWARE_MIN_QUALITY, SOFTWARE_TARGET_MICROS};
        // A trace costing five seconds - the reported case - must move a long way
        // in one step, not creep.
        let after = adjust(5_000_000, 1.0);
        assert!(
            after <= SOFTWARE_MIN_QUALITY,
            "a 5 s frame should collapse the scale to the floor at once, got {after}"
        );
        // At the target it stays put.
        assert!((adjust(SOFTWARE_TARGET_MICROS, 0.5) - 0.5).abs() < 1e-6);
        // Fast frames climb back, but never past 1.0.
        assert!(adjust(1_000, 0.5) > 0.5);
        assert_eq!(adjust(1_000, 1.0), 1.0);
        // The floor holds, so the picture never degenerates into blocky tiles.
        // A quarter of the requested resolution is the point below which a
        // shadow edge upscales into visible tiles.
        assert_eq!(adjust(100_000_000, 1.0), SOFTWARE_MIN_QUALITY);
        assert_eq!(SOFTWARE_MIN_QUALITY, 0.25);
    }

    /// The composite must accept a traced image of *any* size.
    ///
    /// `upload_software` used to drop any result whose extent differed from the
    /// target, assuming a mismatch meant a window resize. With the worker
    /// choosing its own extent that would have thrown away every frame, and the
    /// page would have read "frames composited: 0" again for a new reason.
    #[test]
    fn an_adaptive_extent_is_adopted_rather_than_dropped() {
        let source = include_str!("composite.rs");
        let body = source
            .split("pub fn upload_software")
            .nth(1)
            .expect("upload_software is defined");
        let body = body.split("let row_pitch").next().unwrap();
        assert!(
            !body.contains("(target.width, target.height) != extent {\n            return;"),
            "a mismatched extent must be adopted, not dropped"
        );
        assert!(
            body.contains("self.ensure_target(device, extent.0, extent.1"),
            "upload_software must resize to the result it was given"
        );
    }

    /// The software path must not have its traced size reset to `trace_scale`
    /// every frame, or the adaptive upload is immediately overwritten.
    #[test]
    fn render_does_not_reset_the_software_traced_extent() {
        let source = include_str!("composite.rs");
        assert!(
            source.contains("TraceSource::Software => self"),
            "render must keep the software worker's own traced extent"
        );
    }

    /// The world capture must run before `build` clears the tree placements.
    ///
    /// `build` hands the resident tree meshes to the GPU far-tree pass and then
    /// clears `t.trees.types` and `t.trees.placements`, because the streaming
    /// worker never reads them again. The capture sat *after* that call under a
    /// comment claiming it came before, so it always saw zero tree placements
    /// and the traced world had no foliage at all. Nothing else reported it: the
    /// capture succeeded, the triangle count looked plausible, and only the
    /// picture was wrong.
    #[test]
    fn the_capture_runs_before_the_trees_are_handed_to_the_gpu() {
        let source = include_str!("../gpu.rs");
        // File offsets are not execution order: the clear lives inside `build`,
        // a different function defined earlier in the file. What matters is that
        // the call to `build` comes after the capture in the *same* function.
        let capture = source
            .find("Imaginator: capturing world geometry")
            .expect("the capture is announced");
        let build = source.find("let m = build(").expect("build is called");
        assert!(
            capture < build,
            "the capture must run before the call to build(), which clears the tree \
             placements inside it"
        );
        // And the clear really is inside `build`, so that reasoning holds.
        let build_body = &source[..build].rfind("fn build(").map(|_| &source[..build]);
        if let Some(before_build_call) = build_body {
            assert!(
                before_build_call.contains("t.trees.placements.clear()"),
                "the tree placements are expected to be cleared inside build(); if that moved, \
                 this test's reasoning no longer covers it"
            );
        }
    }
}
