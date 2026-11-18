//! One asynchronous timing sample per second. Never wait for a queue or reuse
//! a mapped buffer. Native AS build/trace timings come from the Vulkan fence.
use super::Mode;
use std::{
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

pub(super) const INTERVAL: Duration = Duration::from_secs(1);
const SIZE: u64 = 4 * 8;

#[derive(Clone, Copy)]
struct Sample {
    mode: Mode,
    filter: bool,
}

pub(super) struct Timings {
    query: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    readback: wgpu::Buffer,
    last: Option<Instant>,
    recorded: Option<Sample>,
    pending: Option<(Sample, Receiver<Result<(), wgpu::BufferAsyncError>>)>,
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
                count: 4,
            }),
            resolved: buffer(
                "Lighter timestamp resolve",
                wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            ),
            readback: buffer(
                "Lighter timestamp readback",
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            ),
            last: None,
            recorded: None,
            pending: None,
            collected: 0,
        })
    }

    pub(super) fn collect(&mut self, queue: &wgpu::Queue) {
        let Some((sample, rx)) = self.pending.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(())) => {
                let values = self.readback.slice(..).get_mapped_range();
                let ticks: &[u64] = bytemuck::cast_slice(&values);
                let elapsed = |i: usize| {
                    ticks[i + 1].wrapping_sub(ticks[i]) as f64
                        * f64::from(queue.get_timestamp_period())
                        / 1.0e6
                };
                let walk = if sample.mode == Mode::Software {
                    format!("{:.3}", elapsed(0))
                } else {
                    "raw-vulkan".into()
                };
                let filter = if sample.filter { elapsed(2) } else { 0. };
                eprintln!(
                    "Lighter gpu stage {:?}: traversal_ms={walk} filter_ms={filter:.3}",
                    sample.mode
                );
                self.collected += 1;
                drop(values);
                self.readback.unmap();
            }
            Err(mpsc::TryRecvError::Empty) => self.pending = Some((sample, rx)),
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    pub(super) fn due(&self) -> bool {
        self.pending.is_none()
            && self.recorded.is_none()
            && self.last.is_none_or(|t| t.elapsed() >= INTERVAL)
    }

    pub(super) fn stamp(&self, encoder: &mut wgpu::CommandEncoder, i: u32) {
        encoder.write_timestamp(&self.query, i);
    }

    pub(super) fn record(&mut self, encoder: &mut wgpu::CommandEncoder, mode: Mode, filter: bool) {
        encoder.resolve_query_set(&self.query, 0..4, &self.resolved, 0);
        encoder.copy_buffer_to_buffer(&self.resolved, 0, &self.readback, 0, SIZE);
        self.last = Some(Instant::now());
        self.recorded = Some(Sample { mode, filter });
    }

    pub(super) fn submitted(&mut self) {
        if let Some(sample) = self.recorded.take() {
            let (tx, rx) = mpsc::channel();
            self.readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |status| {
                    let _ = tx.send(status);
                });
            self.pending = Some((sample, rx));
        }
    }
}
