//! Paged static-scene transfers. A candidate has its own buffers; no write
//! is allowed to alter the scene the current frame is still reading.
use super::textures::{suffix_segments, Textures, UPLOAD_PAGE_BYTES};
use super::{storage_limit, Storage};
use std::sync::mpsc::{self, Receiver, TryRecvError};

const MAX_SPANS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Packet,
    Texels,
}

#[derive(Clone, Copy)]
pub(super) enum Source<'a> {
    Host(&'a [u8]),
    /// Copy an immutable prefix from the preceding GPU texel bank.
    Resident(usize),
}

pub(super) struct Span<'a> {
    pub target: Target,
    pub offset: usize,
    pub source: Source<'a>,
}

impl Span<'_> {
    pub fn len(&self) -> usize {
        match self.source {
            Source::Host(bytes) => bytes.len(),
            Source::Resident(bytes) => bytes,
        }
    }
}

#[derive(Default)]
pub(super) struct Cursor {
    packet: usize,
    texels: usize,
}

impl Cursor {
    pub fn next<'a>(
        &mut self,
        packet: &'a [u8],
        texels: &'a Textures,
        resident_prefix: usize,
        mut budget: usize,
    ) -> Vec<Span<'a>> {
        debug_assert!(budget > 0 && budget % 4 == 0);
        debug_assert!(resident_prefix <= texels.byte_len());
        let mut spans = Vec::new();
        let bytes = (packet.len() - self.packet).min(budget);
        if bytes > 0 {
            let start = self.packet;
            self.packet += bytes;
            budget -= bytes;
            spans.push(Span {
                target: Target::Packet,
                offset: start,
                source: Source::Host(&packet[start..self.packet]),
            });
        }
        let bytes = resident_prefix.saturating_sub(self.texels).min(budget);
        if bytes > 0 {
            spans.push(Span {
                target: Target::Texels,
                offset: self.texels,
                source: Source::Resident(bytes),
            });
            self.texels += bytes;
            budget -= bytes;
        }
        if budget != 0 {
            for (offset, part) in suffix_segments(texels.byte_segments(), self.texels) {
                debug_assert_eq!(offset, self.texels);
                let bytes = part.len().min(budget);
                spans.push(Span {
                    target: Target::Texels,
                    offset,
                    source: Source::Host(&part[..bytes]),
                });
                self.texels += bytes;
                budget -= bytes;
                if budget == 0 || spans.len() == MAX_SPANS {
                    break;
                }
            }
        }
        spans
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Stats {
    pub batches: u64,
    pub host_bytes: u64,
    pub resident_bytes: u64,
    pub max_batch_bytes: usize,
    pub blas_batches: u64,
    pub blas_builds: u64,
}

impl Stats {
    pub fn copied(&mut self, spans: &[Span<'_>]) {
        self.batches += 1;
        self.max_batch_bytes = self
            .max_batch_bytes
            .max(spans.iter().map(Span::len).sum::<usize>());
        for span in spans {
            match span.source {
                Source::Host(_) => self.host_bytes += span.len() as u64,
                Source::Resident(_) => self.resident_bytes += span.len() as u64,
            }
        }
    }
}

pub(super) struct SoftwareScene {
    pub revision: u64,
    pub world: Storage,
    pub texels: Storage,
    prefix: usize,
    cursor: Cursor,
    pending: Option<Receiver<()>>,
    ready: bool,
    pub stats: Stats,
}

impl SoftwareScene {
    pub fn new(
        device: &wgpu::Device,
        revision: u64,
        packet_bytes: usize,
        texture_bytes: usize,
        prefix: usize,
    ) -> Result<Self, String> {
        let usage = wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
        let limit = storage_limit(device);
        assert!(packet_bytes <= limit && texture_bytes <= limit);
        // Buffer allocation is synchronous in wgpu-core; the scope catches
        // allocation failure without waiting for previously queued work.
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let scene = Self {
            revision,
            world: Storage::new(
                device,
                "Lighter staged world",
                packet_bytes.max(16) as u64,
                usage,
            ),
            texels: Storage::new(
                device,
                "Lighter staged texels",
                texture_bytes.max(16) as u64,
                usage,
            ),
            prefix,
            cursor: Cursor::default(),
            pending: None,
            ready: false,
            stats: Stats::default(),
        };
        match pollster::block_on(device.pop_error_scope()) {
            Some(error) => Err(format!("Lighter staged scene allocation: {error}")),
            None => Ok(scene),
        }
    }

    pub fn poll(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &[u8],
        texels: &Textures,
        preceding: &wgpu::Buffer,
        limit: usize,
    ) -> Result<bool, String> {
        if self.ready {
            return Ok(true);
        }
        if let Some(pending) = &self.pending {
            match pending.try_recv() {
                Ok(()) => self.pending = None,
                Err(TryRecvError::Empty) => return Ok(false),
                Err(TryRecvError::Disconnected) => {
                    return Err("Lighter scene transfer callback lost".into())
                }
            }
        }
        let spans = self.cursor.next(packet, texels, self.prefix, limit);
        if spans.is_empty() {
            self.ready = true;
            return Ok(true);
        }
        let mut encoder = None;
        for span in &spans {
            let dst = match span.target {
                Target::Packet => &self.world.0,
                Target::Texels => &self.texels.0,
            };
            let offset = span.offset as u64;
            match span.source {
                Source::Host(bytes) => queue.write_buffer(dst, offset, bytes),
                Source::Resident(bytes) => encoder
                    .get_or_insert_with(|| {
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Lighter reuse immutable texel pages"),
                        })
                    })
                    .copy_buffer_to_buffer(preceding, offset, dst, offset, bytes as u64),
            }
        }
        queue.submit(encoder.map(wgpu::CommandEncoder::finish));
        let (tx, rx) = mpsc::channel();
        queue.on_submitted_work_done(move || {
            let _ = tx.send(());
        });
        self.pending = Some(rx);
        self.stats.copied(&spans);
        Ok(false)
    }

    pub fn ready(&self, revision: u64) -> bool {
        self.revision == revision && self.ready
    }
}

pub(super) const TRANSFER_BYTES: usize = UPLOAD_PAGE_BYTES;

/// Leave bounded room for the moving feed's late-bound materials. Already
/// admitted static tiles keep their addresses even when the suffix grows.
pub(super) fn texel_capacity(texels: &Textures) -> usize {
    let bytes = texels.byte_len();
    let reserve = (bytes / 4).clamp(64 << 10, 16 << 20);
    bytes + texels.remaining_bytes().min(reserve)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_preserve_offsets_across_buffer_and_prefix_boundaries() {
        let packet: Vec<_> = (0..92).collect();
        let mut texels = Textures::default();
        texels.seal();
        let prefix = texels.byte_len();
        texels.reserve(5).unwrap();
        texels.seal();
        texels.reserve(18).unwrap();
        let source: Vec<u8> = texels.byte_segments().flatten().copied().collect();
        let mut copied_packet = vec![255; packet.len()];
        let mut copied_texels = vec![255; source.len()];
        let mut cursor = Cursor::default();
        let mut stats = Stats::default();
        loop {
            let spans = cursor.next(&packet, &texels, prefix, 32);
            if spans.is_empty() {
                break;
            }
            stats.copied(&spans);
            for span in spans {
                let end = span.offset + span.len();
                let bytes = match span.source {
                    Source::Host(bytes) => bytes,
                    Source::Resident(_) => &source[span.offset..end],
                };
                let dst = match span.target {
                    Target::Packet => &mut copied_packet,
                    Target::Texels => &mut copied_texels,
                };
                dst[span.offset..end].copy_from_slice(bytes);
            }
        }
        assert_eq!(copied_packet, packet);
        assert_eq!(copied_texels, source);
        assert_eq!(stats.max_batch_bytes, 32);
        assert_eq!(stats.resident_bytes, prefix as u64);
        assert_eq!(
            stats.host_bytes,
            (packet.len() + source.len() - prefix) as u64
        );
        assert_eq!(
            stats.batches,
            (packet.len() + source.len()).div_ceil(32) as u64
        );
    }
}
