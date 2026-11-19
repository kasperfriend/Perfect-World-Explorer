//! Read the completed frame later, never wait for timestamps or render into a
//! mapped buffer. Keep the GPU budget controller separate from wall-clock FPS.
use super::Mode;
use std::{
    collections::VecDeque,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

pub(super) const INTERVAL: Duration = Duration::from_secs(1);
const QUERIES: u32 = 5;
const SIZE: u64 = QUERIES as u64 * 8;

#[derive(Clone, Copy)]
struct Sample {
    mode: Mode,
    extent: [u32; 4],
    native_ms: f64,
    order: u64,
}

#[derive(Clone, Copy)]
pub(super) struct GpuSample {
    pub mode: Mode,
    pub extent: [u32; 4],
    pub trace_ms: f64,
    pub reproject_ms: f64,
    pub filter_ms: f64,
    pub upscale_ms: f64,
    pub total_ms: f64,
}

struct Slot {
    resolved: wgpu::Buffer,
    readback: wgpu::Buffer,
    pending: Option<(Sample, Receiver<Result<(), wgpu::BufferAsyncError>>)>,
}

pub(super) struct Timings {
    query: wgpu::QuerySet,
    slots: [Slot; 3],
    recorded: Option<(usize, Sample)>,
    serial: u64,
    latest: u64,
    last_log: Option<Instant>,
    pub(super) collected: u64,
}

impl Timings {
    pub(super) fn new(device: &wgpu::Device) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: SIZE,
                usage,
                mapped_at_creation: false,
            })
        };
        Some(Self {
            query: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("Lighter GPU stage timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: QUERIES,
            }),
            slots: std::array::from_fn(|_| Slot {
                resolved: buffer(
                    "Lighter timestamp resolve",
                    wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                ),
                readback: buffer(
                    "Lighter timestamp readback",
                    wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                ),
                pending: None,
            }),
            recorded: None,
            serial: 0,
            latest: 0,
            last_log: None,
            collected: 0,
        })
    }

    pub(super) fn collect(&mut self, queue: &wgpu::Queue) -> Option<GpuSample> {
        let mut newest = None;
        for slot in &mut self.slots {
            let Some((sample, rx)) = slot.pending.take() else {
                continue;
            };
            match rx.try_recv() {
                Ok(Ok(())) => {
                    let values = slot.readback.slice(..).get_mapped_range();
                    let ticks: &[u64] = bytemuck::cast_slice(&values);
                    let elapsed = |i: usize| {
                        ticks[i + 1].wrapping_sub(ticks[i]) as f64
                            * f64::from(queue.get_timestamp_period())
                            / 1.0e6
                    };
                    if sample.order > self.latest {
                        self.latest = sample.order;
                        let trace_ms = if sample.mode == Mode::Software {
                            elapsed(0)
                        } else {
                            0.
                        };
                        let [reproject_ms, filter_ms, upscale_ms] =
                            [elapsed(1), elapsed(2), elapsed(3)];
                        newest = Some(GpuSample {
                            mode: sample.mode,
                            extent: sample.extent,
                            trace_ms,
                            reproject_ms,
                            filter_ms,
                            upscale_ms,
                            total_ms: trace_ms
                                + reproject_ms
                                + filter_ms
                                + upscale_ms
                                + sample.native_ms,
                        });
                    }
                    self.collected += 1;
                    drop(values);
                    slot.readback.unmap();
                }
                Err(mpsc::TryRecvError::Empty) => slot.pending = Some((sample, rx)),
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if let Some(s) = newest {
            if self.last_log.is_none_or(|t| t.elapsed() >= INTERVAL) {
                let walk = if s.mode == Mode::Software {
                    format!("{:.3}", s.trace_ms)
                } else {
                    "raw-vulkan".into()
                };
                eprintln!("Lighter gpu stage {:?}: traversal_ms={walk} reproject_ms={:.3} filter_ms={:.3} upscale_ms={:.3}", s.mode,s.reproject_ms,s.filter_ms,s.upscale_ms);
                self.last_log = Some(Instant::now());
            }
        }
        newest
    }

    pub(super) fn due(&self) -> bool {
        self.recorded.is_none() && self.slots.iter().any(|s| s.pending.is_none())
    }

    pub(super) fn stamp(&self, encoder: &mut wgpu::CommandEncoder, i: u32) {
        encoder.write_timestamp(&self.query, i);
    }

    pub(super) fn record(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        mode: Mode,
        extent: [u32; 4],
        native_ms: f64,
    ) {
        let index = self.slots.iter().position(|s| s.pending.is_none()).unwrap();
        let slot = &self.slots[index];
        encoder.resolve_query_set(&self.query, 0..QUERIES, &slot.resolved, 0);
        encoder.copy_buffer_to_buffer(&slot.resolved, 0, &slot.readback, 0, SIZE);
        self.serial += 1;
        self.recorded = Some((
            index,
            Sample {
                mode,
                extent,
                native_ms,
                order: self.serial,
            },
        ));
    }

    pub(super) fn submitted(&mut self) {
        if let Some((index, sample)) = self.recorded.take() {
            let slot = &mut self.slots[index];
            let (tx, rx) = mpsc::channel();
            slot.readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |status| {
                    let _ = tx.send(status);
                });
            slot.pending = Some((sample, rx));
        }
    }
}

#[derive(Default)]
pub(super) struct Resolution {
    pub percent: Option<u32>,
    average_ms: Option<f64>,
    streak: i32,
    last_step: Option<Instant>,
}
impl Resolution {
    pub(super) fn observe(&mut self, seed: u32, cost: f64, target: f64, now: Instant) {
        // Ignore invalid ticks and cold pipeline/streaming spikes. Trust only
        // several completed measurements made at the same render extent.
        if !cost.is_finite() || !(0.01..=250.).contains(&cost) {
            return;
        }
        let ms = self.average_ms.map_or(cost, |old| old * 0.85 + cost * 0.15);
        self.average_ms = Some(ms);
        if ms > target * 1.12 {
            self.streak = self.streak.max(0) + 1;
        } else if ms < target * 0.78 {
            self.streak = self.streak.min(0) - 1;
        } else {
            self.streak = 0;
        }
        if self
            .last_step
            .is_some_and(|then| now.duration_since(then) < Duration::from_millis(700))
        {
            return;
        }
        let prev = self.percent.unwrap_or(seed).clamp(50, 100);
        let next = if self.streak >= 6 {
            prev.saturating_sub(5).max(50)
        } else if self.streak <= -18 {
            (prev + 5).min(100)
        } else {
            prev
        };
        if next != prev {
            self.percent = Some(next);
            self.last_step = Some(now);
            self.average_ms = None;
            self.streak = 0;
        }
    }
}

#[derive(Default)]
pub(super) struct Cadence {
    last: Option<Instant>,
    frames: VecDeque<f64>,
    pub(super) low: Option<f64>,
    report: Option<Instant>,
}
impl Cadence {
    pub(super) fn present(&mut self, now: Instant) {
        let Some(old) = self.last.replace(now) else {
            return;
        };
        let dt = now.duration_since(old).as_secs_f64();
        if dt <= 0. {
            return;
        }
        if self.frames.len() == 240 {
            self.frames.pop_front();
        }
        self.frames.push_back(dt);
        if self.frames.len() >= 30
            && self
                .report
                .is_none_or(|t| now.duration_since(t) >= INTERVAL)
        {
            let mut slow: Vec<_> = self.frames.iter().copied().collect();
            slow.sort_unstable_by(|a, b| b.total_cmp(a));
            let n = slow.len().div_ceil(100);
            self.low = Some(n as f64 / slow[..n].iter().sum::<f64>());
            self.report = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolution_descends_only_after_sustained_pressure_and_recovers_slowly() {
        let mut r = Resolution::default();
        let start = Instant::now();
        for i in 0..5 {
            r.observe(75, 20., 12., start + Duration::from_millis(i * 16));
        }
        assert_eq!(r.percent, None);
        r.observe(75, 20., 12., start + Duration::from_millis(80));
        assert_eq!(r.percent, Some(70));
        for i in 0..20 {
            r.observe(75, 3., 12., start + Duration::from_millis(100 + i * 16));
        }
        assert_eq!(r.percent, Some(70));
        r.observe(75, 3., 12., start + Duration::from_secs(1));
        assert_eq!(r.percent, Some(75));
        let mut floor = Resolution::default();
        for _ in 0..10 {
            floor.observe(50, 100., 12., start);
        }
        assert_eq!(floor.percent, None);
    }

    #[test]
    fn one_percent_low_samples_the_slowest_present_intervals() {
        let mut c = Cadence::default();
        let mut now = Instant::now();
        c.present(now);
        for _ in 0..128 {
            now += Duration::from_millis(16);
            c.present(now);
        }
        now += Duration::from_millis(80);
        c.present(now);
        now += Duration::from_millis(1120);
        c.present(now);
        assert!((c.low.unwrap() - (2. / 1.2)).abs() < 1.0e-8);
    }
}
