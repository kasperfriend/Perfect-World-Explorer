//! Opt-in, normal-present telemetry. A path advances only after a present on
//! the explicitly requested route. The normal benchmark does not read pixels
//! or add GPU waits; separate motion-analysis mode opts into readback.
use crate::modules::lighter::{Mode, Settings};
use sha2::{Digest, Sha256};
use std::{io::Write, path::PathBuf, time::Instant};

const MAX_FRAMES: u32 = 30_000;

pub(super) struct Plan {
    map: String,
    hour: f32,
    warmup: u32,
    nodes: Vec<(u32, [f32; 5])>,
    hash: String,
}
impl Plan {
    pub(super) fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text
            .lines()
            .map(|s| s.split('#').next().unwrap().trim())
            .filter(|s| !s.is_empty());
        let mut header = lines
            .next()
            .ok_or("empty Lighter travel plan")?
            .split_whitespace();
        let map = header
            .next()
            .ok_or("travel plan needs a map ID")?
            .to_owned();
        let hour = header
            .next()
            .and_then(|x| x.parse().ok())
            .filter(|h| (0. ..24.).contains(h))
            .ok_or("travel hour must be in [0, 24)")?;
        let warmup = header
            .next()
            .and_then(|x| x.parse().ok())
            .filter(|&n| (1..=600).contains(&n))
            .ok_or("travel warmup must be 1..600 presents")?;
        if header.next().is_some() {
            return Err("travel header: map hour warmup".into());
        }
        let mut nodes = Vec::<(u32, [f32; 5])>::new();
        for line in lines {
            let mut parts = line.split_whitespace();
            let at: u32 = parts
                .next()
                .and_then(|x| x.parse().ok())
                .filter(|&n| n < MAX_FRAMES)
                .ok_or("invalid travel frame index")?;
            if nodes.last().map_or(at != 0, |&(prev, _)| at <= prev) {
                return Err("travel frame indices must start at zero and strictly increase".into());
            }
            let mut pose = [0.; 5];
            for dst in &mut pose {
                *dst = parts
                    .next()
                    .and_then(|x| x.parse::<f32>().ok())
                    .filter(|x| x.is_finite())
                    .ok_or("invalid travel position or orientation")?;
            }
            if parts.next().is_some() || !(-1.5..=1.5).contains(&pose[4]) {
                return Err("travel row: frame x y z yaw pitch (radians, pitch -1.5..1.5)".into());
            }
            nodes.push((at, pose));
        }
        if nodes.last().is_none_or(|&(end, _)| end < 29) {
            return Err("travel plan needs at least 30 measured presents".into());
        }
        Ok(Self {
            map,
            hour,
            warmup,
            nodes,
            hash: format!("{:x}", Sha256::digest(text.as_bytes())),
        })
    }
    pub(super) fn preview_path(&self, map: &str, hour: f32, warmup: u32) -> Result<&str, String> {
        if self.map != map
            || self.hour != hour
            || self.warmup != warmup
            || self.samples() != warmup + 1
        {
            return Err(
                "preview path must match map/hour/warm-up and end at the capture frame".into(),
            );
        }
        Ok(&self.hash)
    }

    pub(super) fn samples(&self) -> u32 {
        self.nodes.last().unwrap().0 + 1
    }
    pub(super) fn pose(&self, at: u32) -> [f32; 5] {
        let upper = self.nodes.partition_point(|&(index, _)| index <= at);
        let (start, a) = self.nodes[upper - 1];
        let Some(&(end, b)) = self.nodes.get(upper) else {
            return a;
        };
        let fraction = (at - start) as f32 / (end - start) as f32;
        std::array::from_fn(|i| a[i].mul_add(1. - fraction, b[i] * fraction))
    }
}

/// Driver estimate for this device's process. Includes resources not known
/// to the ray allocator, but is neither dedicated-residency nor OS VRAM.
#[cfg(any(target_os = "linux", target_os = "windows"))]
struct Heap {
    instance: ash::Instance,
    physical: ash::vk::PhysicalDevice,
    local_heaps: Vec<usize>,
}
#[cfg(any(target_os = "linux", target_os = "windows"))]
impl Heap {
    fn new(device: &wgpu::Device) -> Option<Self> {
        use ash::vk;
        use std::ffi::CStr;
        unsafe {
            device
                .as_hal::<wgpu::hal::api::Vulkan, _, _>(|hal| {
                    let hal = hal?;
                    let instance = hal.shared_instance().raw_instance();
                    let physical = hal.raw_physical_device();
                    if !instance
                        .enumerate_device_extension_properties(physical)
                        .ok()?
                        .iter()
                        .any(|ext| {
                            CStr::from_ptr(ext.extension_name.as_ptr())
                                == vk::ExtMemoryBudgetFn::name()
                        })
                    {
                        return None;
                    }
                    let mem = instance.get_physical_device_memory_properties(physical);
                    let local_heaps = mem.memory_heaps[..mem.memory_heap_count as usize]
                        .iter()
                        .enumerate()
                        .filter_map(|(i, h)| {
                            h.flags
                                .contains(vk::MemoryHeapFlags::DEVICE_LOCAL)
                                .then_some(i)
                        })
                        .collect();
                    Some(Self {
                        instance: instance.clone(),
                        physical,
                        local_heaps,
                    })
                })
                .flatten()
        }
    }
    fn bytes(&self) -> u64 {
        use ash::vk;
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        unsafe {
            self.instance.get_physical_device_memory_properties2(
                self.physical,
                &mut vk::PhysicalDeviceMemoryProperties2::builder().push_next(&mut budget),
            );
        }
        self.local_heaps.iter().map(|&i| budget.heap_usage[i]).sum()
    }
}

/// Opt-in Native queue timestamps bracketing world rendering, before UI.
/// Multiple world passes submit separately; includes queue idle between them.
/// The normal-present path does not create this query or its two submissions.
pub(super) struct NativeWorldTimer {
    query: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    readback: wgpu::Buffer,
}

impl NativeWorldTimer {
    pub(super) fn new(device: &wgpu::Device) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 16,
                usage,
                mapped_at_creation: false,
            })
        };
        Some(Self {
            query: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("Native synchronized world timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            }),
            resolved: buffer(
                "Native timestamp resolve",
                wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            ),
            readback: buffer(
                "Native timestamp readback",
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            ),
        })
    }

    pub(super) fn stamp(&self, device: &wgpu::Device, queue: &wgpu::Queue, end: bool) {
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.write_timestamp(&self.query, u32::from(end));
        if end {
            encoder.resolve_query_set(&self.query, 0..2, &self.resolved, 0);
            encoder.copy_buffer_to_buffer(&self.resolved, 0, &self.readback, 0, 16);
        }
        queue.submit(Some(encoder.finish()));
    }

    pub(super) fn map(&self) -> std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        rx
    }

    pub(super) fn elapsed_ms(&self, queue: &wgpu::Queue) -> Option<f64> {
        let values = self.readback.slice(..).get_mapped_range();
        let ticks: &[u64] = bytemuck::cast_slice(&values);
        let span =
            ticks[1].wrapping_sub(ticks[0]) as f64 * f64::from(queue.get_timestamp_period()) / 1e6;
        drop(values);
        self.readback.unmap();
        (span.is_finite() && span > 0.).then_some(span)
    }
}

pub(super) struct SyncSample {
    pub(super) wait_ms: f64,
    pub(super) ray_stages: Option<[f64; 6]>,
    pub(super) sdk_ms: Option<f64>,
    pub(super) native_world_span_ms: Option<f64>,
}

pub(super) struct MotionSample {
    pub(super) rgba8_sha256: String,
    pub(super) elapsed_ms: u64,
    pub(super) camera_cut: bool,
    pub(super) underwater_depth: f32,
}

pub(super) struct RaySample {
    pub(super) primary: [u64; 2],
    pub(super) acceleration: Option<[u64; 2]>,
    pub(super) game_profile_active: bool,
    pub(super) textures: [u64; 5],
    pub(super) pacing: Option<[u64; 6]>,
}

struct Sample {
    interval_ms: f64,
    redraw_ms: f64,
    surface_ms: [f64; 2],
    pose: [f32; 5],
    chunk: (isize, isize),
    streaming: bool,
    committed: bool,
    heap_bytes: Option<u64>,
    sync: Option<SyncSample>,
    motion: Option<MotionSample>,
    ray: Option<RaySample>,
}

struct Setup {
    extents: [(u32, u32); 3],
    present: wgpu::PresentMode,
    ray: Settings,
    view: crate::view_settings::ViewSettings,
    experiments: (u32, bool, bool),
}

pub(super) struct Travel {
    plan: Plan,
    path: PathBuf,
    mode: Mode,
    hybrid: bool,
    adapter: wgpu::AdapterInfo,
    expected_provider: &'static str,
    motion_dir: Option<PathBuf>,
    sync: bool,
    heap_peak: Option<u64>,
    heap_warned: bool,
    largest_local_heap_bytes: Option<u64>,
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    heap: Option<Heap>,
    warm: u32,
    last: Option<Instant>,
    samples: Vec<Sample>,
    setup: Option<Setup>,
    finished: bool,
}

impl Travel {
    pub fn from_env(
        map: &str,
        adapter: wgpu::AdapterInfo,
        device: &wgpu::Device,
    ) -> Result<Option<Self>, String> {
        let Some(file) = std::env::var_os("PW_LIGHTER_TRAVEL_PLAN") else {
            return Ok(None);
        };
        let path = PathBuf::from(
            std::env::var_os("PW_LIGHTER_TRAVEL_CSV")
                .ok_or("travel needs PW_LIGHTER_TRAVEL_CSV")?,
        );
        let (mode, hybrid) = match std::env::var("PW_LIGHTER_TRAVEL_ROUTE").as_deref() {
            Ok("hybrid") => (Mode::Hardware, true),
            Ok("off") => (Mode::Off, false),
            Ok("software") => (Mode::Software, false),
            Ok("hardware") => (Mode::Hardware, false),
            _ => return Err("PW_LIGHTER_TRAVEL_ROUTE: off, software, hardware or hybrid".into()),
        };
        let mut travel = Self::open(
            PathBuf::from(file),
            path,
            map,
            (mode, hybrid),
            adapter,
            device,
        )?;
        travel.expected_provider =
            match (mode, std::env::var("PW_RAY_RECONSTRUCTION").as_deref()) {
                (Mode::Off, _) | (_, Ok("off" | "") | Err(std::env::VarError::NotPresent)) => {
                    "portable"
                }
                (_, Ok("fsr")) if !hybrid => "Vulkan temporal FSR",
                (_, Ok("rr")) if !hybrid => "Vulkan DLSS Ray Reconstruction",
                _ => return Err(
                    "invalid travel ray reconstruction request (SDKs require full primary rays)"
                        .into(),
                ),
            };
        if let Some(dir) = std::env::var_os("PW_LIGHTER_TRAVEL_CAPTURE_DIR") {
            travel.enable_motion(PathBuf::from(dir))?;
        }
        match std::env::var("PW_LIGHTER_TRAVEL_SYNC").as_deref() {
            Ok("1") => travel.enable_sync()?,
            Ok("0") | Err(std::env::VarError::NotPresent) => (),
            _ => return Err("PW_LIGHTER_TRAVEL_SYNC must be 0 or 1".into()),
        }
        Ok(Some(travel))
    }

    pub(super) fn open(
        file: PathBuf,
        path: PathBuf,
        map: &str,
        (mode, hybrid): (Mode, bool),
        adapter: wgpu::AdapterInfo,
        device: &wgpu::Device,
    ) -> Result<Self, String> {
        let text =
            std::fs::read_to_string(file).map_err(|e| format!("read Lighter travel plan: {e}"))?;
        let plan = Plan::parse(&text)?;
        if plan.map != map {
            return Err(format!("travel plan for {}, not {map}", plan.map));
        }
        if path.exists() {
            return Err("travel CSV must be a new file".into());
        }
        let samples = Vec::with_capacity(plan.samples() as usize);
        Ok(Self {
            plan,
            path,
            mode,
            hybrid,
            adapter,
            expected_provider: "portable",
            motion_dir: None,
            sync: false,
            heap_peak: None,
            heap_warned: false,
            largest_local_heap_bytes: super::native_inventory::device_local_heap(device),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            heap: Heap::new(device),
            warm: 0,
            last: None,
            samples,
            setup: None,
            finished: false,
        })
    }

    pub(super) fn enable_motion(&mut self, dir: PathBuf) -> Result<(), String> {
        if self.sync || self.plan.samples() > 600 {
            return Err(
                "motion sequences are limited to 600 frames and cannot synchronize timing".into(),
            );
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("travel capture directory: {e}"))?;
        if std::fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
        {
            return Err("travel capture directory must be empty".into());
        }
        self.motion_dir = Some(dir);
        Ok(())
    }

    pub(super) fn enable_sync(&mut self) -> Result<(), String> {
        if self.motion_dir.is_some() || self.plan.samples() < 500 {
            return Err(
                "synchronized travel needs at least 500 measured frames and no motion readback"
                    .into(),
            );
        }
        self.sync = true;
        Ok(())
    }

    pub(super) fn synchronized(&self) -> bool {
        self.sync && !self.finished
    }

    pub(super) fn motion_frame(&self) -> Option<PathBuf> {
        if self.finished || self.warm < self.plan.warmup {
            return None;
        }
        self.motion_dir
            .as_ref()
            .map(|dir| dir.join(format!("{:06}.png", self.samples.len())))
    }

    pub(super) fn check_provider(&mut self, active: Option<&str>) -> bool {
        if active.unwrap_or("portable") != self.expected_provider {
            self.invalidate(&format!(
                "expected {}, got {}",
                self.expected_provider,
                active.unwrap_or("portable")
            ));
            return false;
        }
        true
    }

    pub fn apply(
        &mut self,
        presented_once: bool,
        active: Mode,
        selected: Mode,
    ) -> Option<([f32; 5], f32)> {
        if self.finished || !presented_once {
            return None;
        }
        if active != self.mode || selected != self.mode {
            if self.last.is_some() {
                self.invalidate("renderer switched or fell back");
            }
            return None;
        }
        Some((self.plan.pose(self.samples.len() as u32), self.plan.hour))
    }

    pub(super) fn invalidate(&mut self, reason: &str) {
        self.finished = true;
        eprintln!("Lighter travel not captured: {reason}");
    }

    #[allow(clippy::too_many_arguments)]
    pub fn presented(
        &mut self,
        now: Instant,
        started: Instant,
        surface_ms: [f64; 2],
        settings: Settings,
        raster: bool,
        extents: [(u32, u32); 3],
        present_mode: wgpu::PresentMode,
        pose: [f32; 5],
        chunk: (isize, isize),
        streaming: bool,
        committed: bool,
        view: &crate::view_settings::ViewSettings,
        experiments: (u32, bool, bool),
        sync: Option<SyncSample>,
        motion: Option<MotionSample>,
        ray: Option<RaySample>,
    ) {
        if self.finished {
            return;
        }
        if settings.mode != self.mode || raster != self.hybrid {
            self.invalidate("drawn route does not match travel experiment");
            return;
        }
        if (settings.mode != Mode::Off) != ray.is_some() {
            self.invalidate("missing ray renderer counters");
            return;
        }
        if self.sync != sync.is_some() {
            self.invalidate("missing synchronized completion sample");
            return;
        }
        if motion.is_some() != (self.motion_dir.is_some() && self.warm >= self.plan.warmup) {
            self.invalidate("missing motion readback sample");
            return;
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        let heap_bytes = (self.sync || committed || self.samples.len() % 30 == 0)
            .then(|| self.heap.as_ref().map(Heap::bytes))
            .flatten();
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let heap_bytes = None;
        if let Some(bytes) = heap_bytes {
            self.heap_peak = Some(self.heap_peak.unwrap_or(0).max(bytes));
            if !self.heap_warned && bytes >= 7_000_000_000 {
                self.heap_warned = true;
                eprintln!("Lighter travel: sampled driver device-local heap use {bytes} bytes (at or above 7 GB)");
            }
        }
        if self.warm < self.plan.warmup {
            // Warm up at a fixed pose. If loading is underway at its end,
            // keep warming until the first measured pose is actually settled.
            if self.warm + 1 < self.plan.warmup || !streaming {
                self.warm += 1;
            }
            if self.warm == self.plan.warmup {
                self.setup = Some(Setup {
                    extents,
                    present: present_mode,
                    ray: settings,
                    view: view.clone(),
                    experiments,
                });
            }
            self.last = Some(now);
            return;
        }
        if !self.setup.as_ref().is_some_and(|s| {
            s.extents == extents
                && s.ray == settings
                && s.present == present_mode
                && s.view == *view
                && s.experiments == experiments
        }) {
            self.invalidate("extent, present mode or graphics settings changed");
            return;
        }
        let Some(previous) = self.last.replace(now) else {
            return;
        };
        self.samples.push(Sample {
            interval_ms: (now - previous).as_secs_f64() * 1000.,
            redraw_ms: (now - started).as_secs_f64() * 1000.,
            surface_ms,
            pose,
            chunk,
            streaming,
            committed,
            heap_bytes,
            sync,
            motion,
            ray,
        });
        if self.samples.len() == self.plan.samples() as usize {
            self.finished = true;
            match self.save() {
                Ok(()) => eprintln!(
                    "Lighter travel: {} {} presents saved to {}",
                    self.samples.len(),
                    self.capture_policy(),
                    self.path.display()
                ),
                Err(e) => eprintln!("Lighter travel could not save: {e}"),
            }
        }
    }

    #[cfg(test)]
    pub(super) fn recorded(&self) -> bool {
        self.finished && self.samples.len() == self.plan.samples() as usize
    }

    fn capture_policy(&self) -> &'static str {
        if self.sync {
            "synchronized"
        } else if self.motion_dir.is_some() {
            "motion_readback"
        } else {
            "normal_present"
        }
    }

    fn save(&self) -> std::io::Result<()> {
        let mut out = std::io::BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.path)?,
        );
        writeln!(out,"# schema=1\n# map={}\n# plan_sha256={}\n# hour={}\n# warmup={}\n# count={}\n# hybrid={}\n# adapter={:?}",
            self.plan.map,self.plan.hash,self.plan.hour,self.plan.warmup,self.samples.len(),self.hybrid,self.adapter)?;
        writeln!(
            out,
            "# ray_reconstruction={}\n# capture_policy={}\n# platform={}\n# architecture={}",
            self.expected_provider,
            self.capture_policy(),
            std::env::consts::OS,
            std::env::consts::ARCH,
        )?;
        if let Some(bytes) = self.largest_local_heap_bytes {
            writeln!(out, "# vulkan_largest_device_local_heap_bytes={bytes}")?;
        }
        if let Some(s) = &self.setup {
            let [(w, h), (rw, rh), (tw, th)] = s.extents;
            writeln!(out,"# display={w}x{h}\n# world={rw}x{rh}\n# trace={tw}x{th}\n# present_mode={:?}\n# ray={:?}",s.present,s.ray)?;
            let (cache, compact, sampled) = s.experiments;
            writeln!(
                out,
                "# screen_cache_mask={cache}\n# compact_terrain={compact}\n# sampled_bc={sampled}"
            )?;
            let mut v = s.view.clone();
            // Do not write local directories to a shareable capture. The route
            // pins the day/night clock independently of the saved slider.
            v.upscale_sdk_dir.clear();
            v.logging_dir.clear();
            v.time_of_day = self.plan.hour;
            let stamp = format!("{v:?}");
            writeln!(
                out,
                "# view_sha256={:x}\n# view={stamp}",
                Sha256::digest(stamp.as_bytes())
            )?;
        }
        if let Some(peak) = self.heap_peak {
            writeln!(out, "# peak_sampled_device_local_heap_estimate_bytes={peak}\n# heap_at_or_above_7_gb={}", self.heap_warned)?;
        }
        write!(out,"frame,interval_ms,redraw_ms,acquire_ms,present_call_ms,x,y,z,yaw,pitch,chunk_x,chunk_z,streaming,committed,device_local_heap_estimate_bytes")?;
        if self.sync {
            write!(out, ",queue_drain_wall_ms,gpu_as_ms,gpu_raster_primary_ms,gpu_trace_ms,gpu_reproject_ms,gpu_filter_ms,gpu_ray_composite_ms,gpu_sdk_ms,gpu_native_world_span_ms")?;
        }
        if self.motion_dir.is_some() {
            write!(
                out,
                ",animation_elapsed_ms,camera_cut,eye_underwater_depth,rgba8_sha256"
            )?;
        }
        if self.mode != Mode::Off {
            write!(out, ",primary_solid_queries_traced,primary_solid_queries_skipped,rt_unique_blas_triangles,rt_tlas_instances,game_profile_active,ray_texture_bank_bytes,ray_bc_tiles,ray_bc5_tiles,ray_bc7_tiles,ray_bc_image_allocated_bytes,resident_copy_batches,resident_host_bytes,resident_reused_bytes,resident_max_batch_bytes,resident_blas_batches,resident_blas_builds")?;
        }
        writeln!(out)?;
        for (i, s) in self.samples.iter().enumerate() {
            let [x, y, z, yaw, pitch] = s.pose;
            let [acquire, present] = s.surface_ms;
            write!(
                out,
                "{i},{:.6},{:.6},{acquire:.6},{present:.6},{x},{y},{z},{yaw},{pitch},{},{},{},{},{}",
                s.interval_ms,
                s.redraw_ms,
                s.chunk.0,
                s.chunk.1,
                u8::from(s.streaming),
                u8::from(s.committed),
                s.heap_bytes.map_or_else(String::new, |n| n.to_string())
            )?;
            if let Some(sync) = &s.sync {
                write!(out, ",{:.6}", sync.wait_ms)?;
                for j in 0..6 {
                    let meaningful = (j != 0 || self.mode == Mode::Hardware)
                        && (j != 1 || self.hybrid)
                        && (!(j == 3 || j == 4) || self.expected_provider == "portable");
                    let ms = sync.ray_stages.filter(|_| meaningful).map(|v| v[j]);
                    write!(
                        out,
                        ",{}",
                        ms.map_or_else(String::new, |v| format!("{v:.6}"))
                    )?;
                }
                write!(
                    out,
                    ",{},{}",
                    sync.sdk_ms.map_or_else(String::new, |v| format!("{v:.6}")),
                    sync.native_world_span_ms
                        .map_or_else(String::new, |v| format!("{v:.6}"))
                )?;
            }
            if let Some(motion) = &s.motion {
                write!(
                    out,
                    ",{},{},{},{}",
                    motion.elapsed_ms,
                    u8::from(motion.camera_cut),
                    motion.underwater_depth,
                    motion.rgba8_sha256
                )?;
            }
            if let Some(ray) = &s.ray {
                for counter in ray.primary {
                    write!(out, ",{counter}")?;
                }
                if let Some([triangles, instances]) = ray.acceleration {
                    write!(out, ",{triangles},{instances}")?;
                } else {
                    write!(out, ",,")?;
                }
                write!(out, ",{}", u8::from(ray.game_profile_active))?;
                for counter in ray.textures {
                    write!(out, ",{counter}")?;
                }
                if let Some(pacing) = ray.pacing {
                    for counter in pacing {
                        write!(out, ",{counter}")?;
                    }
                } else {
                    write!(out, ",,,,,,")?;
                }
            }
            writeln!(out)?;
        }
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires Vulkan timestamp queries"]
    fn native_travel_queue_span_reads_current_submission() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::TIMESTAMP_QUERY,
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        ))
        .unwrap();
        let timer = NativeWorldTimer::new(&device).unwrap();
        let dst = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 1 << 20,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        for bytes in [1 << 20, 512] {
            timer.stamp(&device, &queue, false);
            let mut enc = device.create_command_encoder(&Default::default());
            enc.clear_buffer(&dst, 0, Some(bytes));
            queue.submit(Some(enc.finish()));
            timer.stamp(&device, &queue, true);
            let map = timer.map();
            super::super::wait_for_capture(&device, || {
                Ok(device
                    .poll(wgpu::Maintain::Poll)
                    .is_queue_empty()
                    .then_some(()))
            })
            .unwrap();
            map.try_recv().unwrap().unwrap();
            eprintln!(
                "Native queue timestamp span around {bytes} B clear: {} ms on {:?}",
                timer.elapsed_ms(&queue).unwrap(),
                adapter.get_info()
            );
        }
    }

    #[test]
    fn travel_plan_is_bounded_and_interpolates_exact_waypoints() {
        let p = Plan::parse(
            "# viewer route\na07 12 40\n0 1 2 3 0 -0.1\n30 11 2 -7 0.8 -0.3\n90 51 2 -7 1 -0.5\n",
        )
        .unwrap();
        assert_eq!(p.samples(), 91);
        assert_eq!(p.pose(0), [1., 2., 3., 0., -0.1]);
        assert_eq!(p.pose(15), [6., 2., -2., 0.4, -0.2]);
        assert_eq!(p.pose(90), [51., 2., -7., 1., -0.5]);
        for input in [
            "",
            "a07 12 0\n0 0 0 0 0 0\n40 1 0 0 0 0",
            "a07 12 4\n0 0 0 0 0 0\n30000 1 0 0 0 0",
            "a07 0 40\n1 1 1 1 0 0\n40 1 1 1 0 0",
            "a07 0 40\n0 1 1 1 0 0\n40 NaN 1 1 0 0",
        ] {
            assert!(Plan::parse(input).is_err());
        }
    }
}
