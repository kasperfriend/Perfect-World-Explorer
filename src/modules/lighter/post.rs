//! Ray-resolution radiance processing, shared by software and Vulkan queries.
use super::{
    binding, image_limit, textures::COLOR_SIDE, Bindings, Frame, Mode, Storage, PIXELS, PIXEL_BYTES,
};

pub(super) const SHADER: &str = include_str!("post.wgsl");
// Match pixels.wgsl. These bits describe which animation inputs a ray used.
const ANIM_WAVES: u32 = 32;
const ANIM_SCROLL: u32 = 64;
const ANIM_SKY: u32 = 128;

pub(super) struct HistoryUse {
    reuse: bool,
    changed_animations: u32,
}

struct Stamp {
    stable: Frame,
    revision: u64,
    texture_len: usize,
    mode: Mode,
    waves: [u32; 4],
    scroll: u32,
    sky_anchor: [f32; 2],
}

#[derive(Default)]
pub(super) struct History {
    previous: Option<Stamp>,
    sequence: u32,
}

impl History {
    pub(super) fn reset(&mut self) {
        self.previous = None;
    }

    pub(super) fn prepare(
        &mut self,
        frame: &mut Frame,
        revision: u64,
        texture_len: usize,
        mode: Mode,
        moving_unchanged: bool,
    ) -> HistoryUse {
        // No reprojection: match view, projection, extent, static scene,
        // moving geometry AND lights, options and lighting exactly. A mover
        // can change another pixel's shadow or reflection without moving it.
        let mut stable = *frame;
        stable.waves = [0; 4];
        stable.air[3] = 0.;
        stable.sky_uv[..2].fill(0.);
        stable.sampling = [0; 4];
        let mut stamp = Stamp {
            stable,
            revision,
            texture_len,
            mode,
            waves: frame.waves,
            scroll: frame.air[3].to_bits(),
            sky_anchor: [frame.sky_uv[0], frame.sky_uv[1]],
        };
        let mut changed_animations = 0;
        let reuse = moving_unchanged
            && self.previous.as_ref().is_some_and(|old| {
                if old.revision != revision
                    || old.texture_len != texture_len
                    || old.mode != mode
                    || bytemuck::bytes_of(&old.stable) != bytemuck::bytes_of(&stable)
                {
                    return false;
                }
                if old.waves != stamp.waves {
                    changed_animations |= ANIM_WAVES;
                }
                if old.scroll != stamp.scroll {
                    changed_animations |= ANIM_SCROLL;
                }
                // Don't let subtexel roof drift discard every diffuse bounce on an
                // otherwise still view. Bound the TOTAL drift since the last sky
                // reset to a quarter texel of the largest roof tile; never freeze
                // the animation or ignore a sky set/fade/lighting change.
                let limit = 0.25 / COLOR_SIDE as f32;
                if stamp
                    .sky_anchor
                    .iter()
                    .zip(old.sky_anchor)
                    .all(|(a, b)| (a - b).abs() < limit)
                {
                    stamp.sky_anchor = old.sky_anchor;
                } else {
                    changed_animations |= ANIM_SKY;
                }
                true
            });
        self.previous = Some(stamp);
        frame.sampling = [self.sequence, 0, 0, 0];
        self.sequence = self.sequence.wrapping_add(1);
        HistoryUse {
            reuse,
            changed_animations,
        }
    }
}

pub(super) struct Post {
    layout: wgpu::BindGroupLayout,
    accumulate: wgpu::ComputePipeline,
    denoise: wgpu::ComputePipeline,
    parameters: wgpu::Buffer,
    history: Option<Storage>,
    filtered: Option<Storage>,
    accumulate_bind: Bindings<3>,
    denoise_bind: Bindings<3>,
    buffers_created: u64,
}

impl Post {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let stage = wgpu::ShaderStages::COMPUTE;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Lighter radiance processing bindings"),
            entries: &[
                binding(0, stage, wgpu::BufferBindingType::Uniform),
                binding(
                    1,
                    stage,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                binding(
                    2,
                    stage,
                    wgpu::BufferBindingType::Storage { read_only: false },
                ),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Lighter radiance processing layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Lighter radiance processing"),
            source: wgpu::ShaderSource::Wgsl([PIXELS, SHADER].concat().into()),
        });
        let pipeline = |label, entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                module: &module,
                entry_point,
            })
        };
        let accumulate = pipeline("Lighter progressive accumulation", "accumulate");
        let denoise = pipeline("Lighter edge-aware denoiser", "denoise");
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Lighter radiance processing parameters"),
            size: 32,
            mapped_at_creation: false,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Self {
            layout,
            accumulate,
            denoise,
            parameters,
            history: None,
            filtered: None,
            accumulate_bind: Bindings::default(),
            denoise_bind: Bindings::default(),
            buffers_created: 1,
        }
    }

    pub(super) fn allocation_counts(&self) -> (u64, u64) {
        (
            self.buffers_created,
            self.accumulate_bind.created + self.denoise_bind.created,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn process<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        raw: &'a wgpu::Buffer,
        extent: [u32; 4],
        history: Option<HistoryUse>,
        denoise: bool,
    ) -> &'a wgpu::Buffer {
        let needed = u64::from(extent[0]) * u64::from(extent[1]) * PIXEL_BYTES;
        let allocate = |slot: &mut Option<Storage>, label: &'static str| {
            if let Some(buffer) = slot {
                buffer.ensure(device, label, needed, image_limit(device))
            } else {
                *slot = Some(Storage::new(
                    device,
                    label,
                    needed,
                    wgpu::BufferUsages::empty(),
                ));
                true
            }
        };
        let progress = history.is_some();
        if progress {
            self.buffers_created +=
                u64::from(allocate(&mut self.history, "Lighter accumulated image"));
        }
        if denoise {
            self.buffers_created +=
                u64::from(allocate(&mut self.filtered, "Lighter filtered image"));
        }
        if !denoise && !progress {
            return raw;
        }
        let history = history.map_or([0; 4], |h| [u32::from(h.reuse), h.changed_animations, 0, 0]);
        queue.write_buffer(&self.parameters, 0, bytemuck::bytes_of(&[extent, history]));
        let dispatch = |encoder: &mut wgpu::CommandEncoder,
                        label: &str,
                        pipeline: &wgpu::ComputePipeline,
                        bind: &wgpu::BindGroup| {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.dispatch_workgroups(extent[0].div_ceil(8), extent[1].div_ceil(8), 1);
        };
        let mut image = raw;
        if progress {
            let history = &self.history.as_ref().unwrap().0;
            let label = "Lighter accumulate radiance";
            let bind = self.accumulate_bind.get(
                device,
                label,
                &self.layout,
                [&self.parameters, image, history],
            );
            dispatch(encoder, label, &self.accumulate, bind);
            image = history;
        }
        if denoise {
            let filtered = &self.filtered.as_ref().unwrap().0;
            let label = "Lighter filter radiance";
            let bind = self.denoise_bind.get(
                device,
                label,
                &self.layout,
                [&self.parameters, image, filtered],
            );
            dispatch(encoder, label, &self.denoise, bind);
            image = filtered;
        }
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    #[test]
    fn history_is_sensitive_to_scene_state_but_tracks_continuous_inputs_separately() {
        let mut history = History::default();
        let mut frame = Frame::zeroed();
        let step = |h: &mut History, f: &mut Frame| h.prepare(f, 1, 8, Mode::Software, true);
        assert!(!step(&mut history, &mut frame).reuse);
        assert_eq!(frame.sampling[0], 0);
        assert!(step(&mut history, &mut frame).reuse);
        assert_eq!(frame.sampling[0], 1);

        frame.sky_uv[0] = 0.15 / COLOR_SIDE as f32;
        frame.waves[2] = 42;
        frame.air[3] = 0.1;
        let h = step(&mut history, &mut frame);
        assert!(h.reuse);
        assert_eq!(h.changed_animations, ANIM_WAVES | ANIM_SCROLL);
        frame.sky_uv[0] *= 2.;
        assert_eq!(step(&mut history, &mut frame).changed_animations, ANIM_SKY);
        assert_eq!(step(&mut history, &mut frame).changed_animations, 0);

        frame.extent[2] = 1;
        assert!(!step(&mut history, &mut frame).reuse);
        frame.sun_color[0] = 0.7;
        assert!(!step(&mut history, &mut frame).reuse);
        assert!(
            !history
                .prepare(&mut frame, 1, 8, Mode::Hardware, true)
                .reuse
        );
        assert!(
            !history
                .prepare(&mut frame, 2, 8, Mode::Hardware, true)
                .reuse
        );
        assert!(
            !history
                .prepare(&mut frame, 2, 16, Mode::Hardware, true)
                .reuse
        );
        assert!(
            !history
                .prepare(&mut frame, 2, 16, Mode::Hardware, false)
                .reuse
        );
        history.reset();
        assert!(
            !history
                .prepare(&mut frame, 2, 16, Mode::Hardware, true)
                .reuse
        );
    }
}
