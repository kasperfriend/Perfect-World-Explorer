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

use bvh::SharedScene;
use composite::Composite;
use gpu::GpuParams;
use hardware::HardwareStatus;
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
    pub fn saved_active(client_root: &Path) -> bool {
        ImaginatorSettings::load(&client_root.join(DIRECTORY)).active()
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
    /// The host offers the world exactly once — at load, while it still owns the
    /// live terrain, the resident scenery and the tree meshes — so a module that
    /// is switched off does not spend a map's worth of triangles on an image
    /// nobody draws. Turning the tracer on later therefore takes effect on the
    /// next map load, which is what the page says rather than pretending.
    pub fn set_world(&mut self, label: String, environment: tracer::Environment) {
        if self.map_label != label {
            self.map_label = label;
            self.capture_state = CaptureState::Requested;
            self.want_capture = self.settings.active();
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
        self.status = "the swapchain image is not copyable on this backend \
                       (no `COPY_SRC`), so the traced terms cannot be composited"
            .into();
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
        let want_software = self.settings.active() && self.settings.tracer == TracerKind::Software;
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
                if hardware.status.available() {
                    self.active = Active::Hardware;
                    self.status = hardware.status.label();
                } else {
                    // Selecting hardware does not silently fall back: the page
                    // says why it is not running and the software option is one
                    // click away.
                    self.active = Active::Idle;
                    self.status = hardware.status.label();
                }
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
        let frames = software.frames.load(Ordering::Relaxed);
        let rays = software
            .result
            .lock()
            .ok()
            .and_then(|r| r.as_ref().map(|r| r.rays))
            .unwrap_or(0);
        if frames == 0 {
            "software tracer starting".into()
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

        let thread = {
            let result = result.clone();
            let request = request.clone();
            let pending_scene = pending_scene.clone();
            let running = running.clone();
            let frames = frames.clone();
            let scene = self.scene.clone();
            let environment = self.environment;
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
                        let start = std::time::Instant::now();
                        let mut samples = vec![
                            tracer::Sample::default();
                            (req.width as usize) * (req.height as usize)
                        ];
                        let rows = req.height;
                        let threads = std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(4)
                            .min(16) as u32;
                        let slice = (rows + threads - 1) / threads.max(1);
                        let tracer_ref = &tracer;
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
                            let (width, height, s_) = (req.width, req.height, req.samples);
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
                        let mut packed = vec![0f32; samples.len() * 4];
                        tracer::Tracer::pack(&samples, &mut packed);
                        let rays = tracer.rays.load(std::sync::atomic::Ordering::Relaxed);
                        let trace = TraceResult {
                            generation: req.generation,
                            packed,
                            width: req.width,
                            height: req.height,
                            rays,
                            micros: start.elapsed().as_micros(),
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
            environment: self.environment,
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
        let env = self.environment;
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
        )
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
            return false;
        }
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
                // Only a probe that says the GPU cannot do this at all is fatal.
                // A build failure is a note, not a veto.
                if matches!(hardware.status, HardwareStatus::Untried) {
                    self.status = hardware.status.label();
                    return false;
                }
                // Honesty about what "GPU" means here. The acceleration
                // structures are built, but the shader that runs is the WGSL
                // compute tracer: it walks the packed BVH in `nodes`/`order` as
                // plain storage buffers and never binds the TLAS. On a world
                // this size that loop is long enough to look like a hang. Say so
                // rather than let the module appear to be tracing with DXR.
                let _ = (tw, th);
                let base = hardware.status.label();
                self.status = if hardware.dxil_present(&self.directory) {
                    base
                } else {
                    // Honest wording: the compute tracer is what runs, and it
                    // works without DXR, so this is a note about the path in use
                    // rather than an error.
                    format!(
                        "{base} — rays traced by the compute shader (no RayQuery blob installed)"
                    )
                };
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
        let Some(composite) = self.composite.as_mut() else {
            return false;
        };
        if composite.triangle_count() == 0 {
            self.status = "no geometry captured yet".into();
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
                let enabled = match tracer {
                    TracerKind::Off => true,
                    TracerKind::Software => true,
                    TracerKind::Hardware => self
                        .hardware
                        .as_ref()
                        .map(|h| h.status.available())
                        .unwrap_or(false),
                };
                let response = ui.add_enabled(
                    enabled,
                    egui::SelectableLabel::new(self.settings.tracer == tracer, tracer.label()),
                );
                if response.clicked() {
                    self.settings.tracer = tracer;
                    changed = true;
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
                    "This world has not been captured. A capture is taken while the map loads, \
                     where the resident scenery and the tree meshes still exist, so switching the \
                     tracer on now takes effect the next time a map loads.",
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
        ui.horizontal(|ui| {
            // A capture walks the whole resident world, so the host takes it
            // where it still owns the live terrain, the resident scenery and the
            // tree meshes: at load time. There is no second window into that
            // geometry later, so the button that used to say "recapture now"
            // would only ever ask again for a moment that has passed; instead it
            // says what actually brings new geometry in, and the status line
            // says which world the triangles belong to.
            if ui
                .button("Reload the world to recapture")
                .on_hover_text(
                    "The captured geometry is a snapshot taken while the map loads, because the \
                     streaming object set and the tree meshes only exist then. Loading the map \
                     again takes a fresh snapshot.",
                )
                .clicked()
            {
                self.status = "the capture is taken when a map loads: reload the world".into();
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
    use super::row_bands;

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
}
