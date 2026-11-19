//! Bounded colour readback. The event loop polls first and only consumes
//! callbacks from earlier frames; the GPU queue and PNG writer never block it.
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
};

const SLOT_COUNT: usize = 3;

#[derive(Clone, Copy)]
struct Layout {
    width: u32,
    height: u32,
    row: u32,
    pitch: u32,
    bgra: bool,
}

impl Layout {
    fn new(source: &wgpu::Texture) -> Result<Self, String> {
        let bgra = match source.format() {
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
            f => return Err(format!("cannot read back {f:?} as RGBA8")),
        };
        let width = source.width();
        let row = width * 4;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        Ok(Self {
            width,
            height: source.height(),
            row,
            pitch: row.div_ceil(alignment) * alignment,
            bgra,
        })
    }

    fn size(self) -> u64 {
        u64::from(self.pitch) * u64::from(self.height)
    }

    fn unpack(self, data: &[u8]) -> Image {
        let mut rgba = Vec::with_capacity((u64::from(self.row) * u64::from(self.height)) as usize);
        for row in data.chunks_exact(self.pitch as usize) {
            rgba.extend_from_slice(&row[..self.row as usize]);
        }
        if self.bgra {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        Image {
            width: self.width,
            height: self.height,
            rgba,
        }
    }
}

pub(super) struct Image {
    width: u32,
    height: u32,
    pub(super) rgba: Vec<u8>,
}

impl Image {
    pub(super) fn save(self, path: &Path) -> Result<(), String> {
        png_rgba(path, self.width, self.height, &self.rgba)
    }
}

pub(super) fn png_rgba(path: &Path, width: u32, height: u32, bytes: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(bytes).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

struct Pending<T> {
    layout: Layout,
    tag: T,
    completion: Receiver<Result<(), wgpu::BufferAsyncError>>,
}

struct Slot<T> {
    buffer: Option<wgpu::Buffer>,
    pending: Option<Pending<T>>,
}

pub(super) struct Ring<T>([Slot<T>; SLOT_COUNT]);

impl<T> Default for Ring<T> {
    fn default() -> Self {
        Self(std::array::from_fn(|_| Slot {
            buffer: None,
            pending: None,
        }))
    }
}

impl<T> Ring<T> {
    /// Returns false when all staging slots are still mapped or in flight.
    /// A busy slot is never resized, unmapped, or overwritten by the producer.
    pub(super) fn submit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Texture,
        tag: T,
    ) -> Result<bool, String> {
        let Some(slot) = self.0.iter_mut().find(|s| s.pending.is_none()) else {
            return Ok(false);
        };
        let layout = Layout::new(source)?;
        let size = layout.size();
        if size > device.limits().max_buffer_size {
            return Err("frame readback exceeds adapter buffer limit".into());
        }
        if slot.buffer.as_ref().is_none_or(|b| b.size() < size) {
            slot.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame readback staging"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }));
        }
        let buffer = slot.buffer.as_ref().unwrap();
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            source.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(layout.pitch),
                    rows_per_image: Some(layout.height),
                },
            },
            wgpu::Extent3d {
                width: layout.width,
                height: layout.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        let (tx, completion) = mpsc::channel();
        buffer
            .slice(..size)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        slot.pending = Some(Pending {
            layout,
            tag,
            completion,
        });
        Ok(true)
    }

    /// Poll the device separately. This never waits for a callback. Consume
    /// mapped bytes on the render thread (not inside wgpu's callback).
    pub(super) fn try_read(&mut self) -> Option<(T, Result<Image, String>)> {
        for slot in &mut self.0 {
            let Some(pending) = &slot.pending else {
                continue;
            };
            let result = match pending.completion.try_recv() {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => {
                    Err("frame readback callback disconnected".into())
                }
            };
            let Pending { layout, tag, .. } = slot.pending.take().unwrap();
            if let Err(error) = result {
                slot.buffer = None;
                return Some((tag, Err(error)));
            }
            let buffer = slot.buffer.as_ref().unwrap();
            let image = layout.unpack(&buffer.slice(..layout.size()).get_mapped_range());
            buffer.unmap();
            return Some((tag, Ok(image)));
        }
        None
    }
}

#[derive(Default)]
pub(super) struct ViewerCapture {
    pub(super) requested: Option<PathBuf>,
    ring: Ring<PathBuf>,
    writer: Option<SyncSender<(PathBuf, Image)>>,
}

impl ViewerCapture {
    pub(super) fn submit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Texture,
    ) {
        if let Some(path) = self.requested.take() {
            match self.ring.submit(device, queue, source, path) {
                Ok(true) => {}
                Ok(false) => report("Frame capture busy; try again after an earlier readback"),
                Err(e) => report(&format!("Frame capture failed: {e}")),
            }
        }
    }

    pub(super) fn collect(&mut self) {
        while let Some((path, result)) = self.ring.try_read() {
            match result {
                Ok(image) => {
                    let writer = self.writer.get_or_insert_with(|| {
                        let (tx, rx) = mpsc::sync_channel::<(PathBuf, Image)>(SLOT_COUNT);
                        std::thread::spawn(move || {
                            while let Ok((path, image)) = rx.recv() {
                                let result = path
                                    .parent()
                                    .map_or(Ok(()), std::fs::create_dir_all)
                                    .map_err(|e| e.to_string())
                                    .and_then(|()| image.save(&path));
                                match result {
                                    Ok(()) => report(&format!("Saved frame: {}", path.display())),
                                    Err(e) => {
                                        report(&format!("Could not save {}: {e}", path.display()))
                                    }
                                }
                            }
                        });
                        tx
                    });
                    if writer.try_send((path, image)).is_err() {
                        report("Frame capture writer busy; dropping screenshot");
                    }
                }
                Err(e) => report(&format!("Frame capture failed: {e}")),
            }
        }
    }
}

fn report(message: &str) {
    eprintln!("{message}");
    crate::diagnostics::session(message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a headless Vulkan adapter"]
    fn live_readback_ring_pixels() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default(), None)).unwrap();
        let extent = wgpu::Extent3d {
            width: 3,
            height: 2,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("unaligned BGRA readback fixture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut ring = Ring::default();
        for cycle in 0..2u8 {
            for i in 0..SLOT_COUNT as u8 {
                let red = 40 * cycle + i;
                queue.write_texture(
                    texture.as_image_copy(),
                    &[200, 30, red, 255].repeat(6),
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(12),
                        rows_per_image: Some(2),
                    },
                    extent,
                );
                assert!(ring.submit(&device, &queue, &texture, red).unwrap());
            }
            assert!(!ring.submit(&device, &queue, &texture, 255).unwrap());
            // Only now, on later ticks, poll and consume each map callback. The
            // test waits for the outcome; the live consumer calls try_read once.
            let mut tags = Vec::new();
            for _ in 0..SLOT_COUNT {
                let (red, image) =
                    crate::gpu::wait_for_capture(&device, || Ok(ring.try_read())).unwrap();
                let image = image.unwrap();
                assert_eq!((image.width, image.height), (3, 2));
                assert_eq!(image.rgba, [red, 30, 200, 255].repeat(6));
                tags.push(red);
            }
            tags.sort_unstable();
            assert_eq!(
                tags,
                (0..SLOT_COUNT as u8)
                    .map(|i| 40 * cycle + i)
                    .collect::<Vec<_>>()
            );
            assert!(ring.try_read().is_none());
        }
    }
}
