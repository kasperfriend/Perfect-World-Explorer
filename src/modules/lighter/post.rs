//! Ray-resolution radiance processing, shared by software and Vulkan queries.
use super::{
    binding, image_limit, textures::COLOR_SIDE, Bindings, Frame, Mode, Storage, PIXELS, PIXEL_BYTES,
};
use glam::{Mat4, Vec4};

pub(super) const SHADER: &str = include_str!("post.wgsl");
// Match pixels.wgsl. These bits describe which animation inputs a ray used.
const ANIM_WAVES: u32 = 32;
const ANIM_SCROLL: u32 = 64;
const ANIM_SKY: u32 = 128;

pub(super) struct HistoryUse {
    pub(super) reuse: bool,
    pub(super) changed_animations: u32,
    pub(super) previous_from_current: Option<Mat4>,
}

#[repr(C)]
#[derive(bytemuck::Pod, bytemuck::Zeroable, Clone, Copy)]
struct Params {
    extent: [u32; 4],
    history: [u32; 4],
    previous_from_current: [[f32; 4]; 4],
}

/// Keep reprojection local to roughly one ray pixel, even after many tiny
/// changes. Project the current clip volume into the view at the last reset.
/// Looking at the full frustum (including near Z) also catches zoom changes.
fn camera_continuous(anchor_from_current: Mat4, [width, height, ..]: [u32; 4]) -> bool {
    for z in [0., 1.] {
        for y in [-1., 1.] {
            for x in [-1., 1.] {
                let p = anchor_from_current * Vec4::new(x, y, z, 1.);
                if p.w <= 0. || !p.is_finite() {
                    return false;
                }
                let p = p / p.w;
                if (p.x - x).abs() * width as f32 * 0.5 > 1.
                    || (p.y - y).abs() * height as f32 * 0.5 > 1.
                    || (p.z - z).abs() > 0.02
                {
                    return false;
                }
            }
        }
    }
    true
}

struct Stamp {
    stable: Frame,
    vp: Mat4,
    inv_vp: Mat4,
    eye: [f32; 3],
    camera_anchor: Mat4,
    revision: u64,
    texture_len: usize,
    mode: Mode,
    waves: [u32; 4],
    water_art: [u32; 4],
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
        // A mover can change another pixel's shadow/reflection without
        // moving it. Only reproject in the same lighting and scene snapshot.
        let mut stable = *frame;
        stable.vp = [[0.; 4]; 4];
        stable.inv_vp = [[0.; 4]; 4];
        stable.eye[..3].fill(0.);
        stable.waves = [0; 4];
        stable.water_effects[0] = 0;
        stable.shore_offsets = [0.; 4];
        stable.air[3] = 0.;
        stable.sky_uv[..2].fill(0.);
        stable.sampling = [0; 4];
        let vp = Mat4::from_cols_array_2d(&frame.vp);
        let inv_vp = Mat4::from_cols_array_2d(&frame.inv_vp);
        let eye = [frame.eye[0], frame.eye[1], frame.eye[2]];
        let mut stamp = Stamp {
            stable,
            vp,
            inv_vp,
            eye,
            camera_anchor: vp,
            revision,
            texture_len,
            mode,
            waves: frame.waves,
            water_art: [
                frame.water_effects[0],
                frame.shore_offsets[0].to_bits(),
                frame.shore_offsets[1].to_bits(),
                frame.shore_offsets[2].to_bits(),
            ],
            scroll: frame.air[3].to_bits(),
            sky_anchor: [frame.sky_uv[0], frame.sky_uv[1]],
        };
        let mut changed_animations = 0;
        let mut previous_from_current = None;
        let reuse = moving_unchanged
            && self.previous.as_ref().is_some_and(|old| {
                if old.revision != revision
                    || old.texture_len != texture_len
                    || old.mode != mode
                    || bytemuck::bytes_of(&old.stable) != bytemuck::bytes_of(&stable)
                {
                    return false;
                }
                if old.vp != vp || old.inv_vp != inv_vp || old.eye != eye {
                    // Multi-sample primary depth can come from a jittered hit,
                    // not the center used by reprojection. Reset for motion.
                    if frame.options[2] != 1
                        || !camera_continuous(old.camera_anchor * inv_vp, frame.extent)
                    {
                        return false;
                    }
                    previous_from_current = Some(old.vp * inv_vp);
                }
                stamp.camera_anchor = old.camera_anchor;
                if old.waves != stamp.waves || old.water_art != stamp.water_art {
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
            previous_from_current,
        }
    }
}

pub(super) struct Post {
    layout: wgpu::BindGroupLayout,
    accumulate: wgpu::ComputePipeline,
    denoise: wgpu::ComputePipeline,
    parameters: wgpu::Buffer,
    history: [Option<Storage>; 2],
    filtered: Option<Storage>,
    history_write: usize,
    accumulate_bind: Bindings<4, 2>,
    denoise_bind: Bindings<4, 2>,
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
                binding(
                    3,
                    stage,
                    wgpu::BufferBindingType::Storage { read_only: true },
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
            size: std::mem::size_of::<Params>() as u64,
            mapped_at_creation: false,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Self {
            layout,
            accumulate,
            denoise,
            parameters,
            history: [None, None],
            filtered: None,
            history_write: 0,
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
                    if cfg!(test) {
                        wgpu::BufferUsages::COPY_SRC
                    } else {
                        wgpu::BufferUsages::empty()
                    },
                ));
                true
            }
        };
        let progress = history.is_some();
        let mut fresh = false;
        if progress {
            for slot in &mut self.history {
                let changed = allocate(slot, "Lighter accumulated image");
                fresh |= changed;
                self.buffers_created += u64::from(changed);
            }
        }
        if denoise {
            self.buffers_created +=
                u64::from(allocate(&mut self.filtered, "Lighter filtered image"));
        }
        if !denoise && !progress {
            return raw;
        }
        let mut params = Params {
            extent,
            history: [0; 4],
            previous_from_current: Mat4::IDENTITY.to_cols_array_2d(),
        };
        if let Some(h) = history {
            params.history = [
                u32::from(h.reuse && !fresh),
                h.changed_animations,
                u32::from(h.previous_from_current.is_some()),
                0,
            ];
            if let Some(transform) = h.previous_from_current {
                params.previous_from_current = transform.to_cols_array_2d();
            }
        }
        queue.write_buffer(&self.parameters, 0, bytemuck::bytes_of(&params));
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
            let next = &self.history[self.history_write].as_ref().unwrap().0;
            let previous = &self.history[self.history_write ^ 1].as_ref().unwrap().0;
            self.history_write ^= 1;
            let label = "Lighter accumulate radiance";
            let bind = self.accumulate_bind.get(
                device,
                label,
                &self.layout,
                [&self.parameters, image, next, previous],
            );
            dispatch(encoder, label, &self.accumulate, bind);
            image = next;
        }
        if denoise {
            let filtered = &self.filtered.as_ref().unwrap().0;
            let label = "Lighter filter radiance";
            let bind = self.denoise_bind.get(
                device,
                label,
                &self.layout,
                [&self.parameters, image, filtered, image],
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
    fn camera_reprojects_subpixel_drift_but_resets_relative_to_last_cut() {
        let mut history = History::default();
        let mut frame = Frame {
            vp: Mat4::IDENTITY.to_cols_array_2d(),
            inv_vp: Mat4::IDENTITY.to_cols_array_2d(),
            extent: [100, 100, 200, 200],
            options: [0, 1, 1, 0],
            ..Frame::zeroed()
        };
        let step = |h: &mut History, f: &mut Frame| h.prepare(f, 1, 8, Mode::Software, true);
        assert!(!step(&mut history, &mut frame).reuse);
        let set_view = |frame: &mut Frame, translation| {
            let vp = Mat4::from_translation(glam::Vec3::new(translation, 0., 0.));
            frame.vp = vp.to_cols_array_2d();
            frame.inv_vp = vp.inverse().to_cols_array_2d();
        };
        set_view(&mut frame, 0.014);
        let sample = step(&mut history, &mut frame);
        assert!(sample.reuse);
        assert!((sample.previous_from_current.unwrap().w_axis.x + 0.014).abs() < 1e-7);
        set_view(&mut frame, 0.028);
        assert!(!step(&mut history, &mut frame).reuse);
        let parked = step(&mut history, &mut frame);
        assert!(parked.reuse && parked.previous_from_current.is_none());
        frame.options[2] = 2;
        assert!(!step(&mut history, &mut frame).reuse);
        set_view(&mut frame, 0.029);
        assert!(!step(&mut history, &mut frame).reuse);
    }

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
