//! Opt-in, normal-present telemetry. A path advances only after a present on
//! the explicitly requested route. There are no GPU waits or pixel readbacks.
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

struct Sample {
    interval_ms: f64,
    redraw_ms: f64,
    surface_ms: [f64; 2],
    pose: [f32; 5],
    chunk: (isize, isize),
    streaming: bool,
    committed: bool,
    heap_bytes: Option<u64>,
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
        Self::open(
            PathBuf::from(file),
            path,
            map,
            (mode, hybrid),
            adapter,
            device,
        )
        .map(Some)
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
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let _ = device;
        Ok(Self {
            plan,
            path,
            mode,
            hybrid,
            adapter,
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            heap: Heap::new(device),
            warm: 0,
            last: None,
            samples,
            setup: None,
            finished: false,
        })
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
    ) {
        if self.finished {
            return;
        }
        if settings.mode != self.mode || raster != self.hybrid {
            self.invalidate("drawn route does not match travel experiment");
            return;
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
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        let heap_bytes = (committed || self.samples.len() % 30 == 0)
            .then(|| self.heap.as_ref().map(Heap::bytes))
            .flatten();
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let heap_bytes = None;
        self.samples.push(Sample {
            interval_ms: (now - previous).as_secs_f64() * 1000.,
            redraw_ms: (now - started).as_secs_f64() * 1000.,
            surface_ms,
            pose,
            chunk,
            streaming,
            committed,
            heap_bytes,
        });
        if self.samples.len() == self.plan.samples() as usize {
            self.finished = true;
            match self.save() {
                Ok(()) => eprintln!(
                    "Lighter travel: {} normal present calls saved to {}",
                    self.samples.len(),
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

    fn save(&self) -> std::io::Result<()> {
        let mut out = std::io::BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.path)?,
        );
        writeln!(out,"# schema=1\n# map={}\n# plan_sha256={}\n# hour={}\n# warmup={}\n# count={}\n# hybrid={}\n# adapter={:?}",
            self.plan.map,self.plan.hash,self.plan.hour,self.plan.warmup,self.samples.len(),self.hybrid,self.adapter)?;
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
        writeln!(out,"frame,interval_ms,redraw_ms,acquire_ms,present_call_ms,x,y,z,yaw,pitch,chunk_x,chunk_z,streaming,committed,device_local_heap_estimate_bytes")?;
        for (i, s) in self.samples.iter().enumerate() {
            let [x, y, z, yaw, pitch] = s.pose;
            let [acquire, present] = s.surface_ms;
            writeln!(
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
        }
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
