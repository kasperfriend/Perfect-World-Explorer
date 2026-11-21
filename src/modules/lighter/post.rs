//! Ray-resolution radiance processing, shared by software and Vulkan queries.
use super::{
    binding, image_limit, textures::COLOR_SIDE, Bindings, Frame, Mode, Storage, PIXELS, PIXEL_BYTES,
};
use glam::{Mat4, Vec4};

pub(super) const SHADER: &str = include_str!("post.wgsl");
// Match pixels.wgsl. These bits describe which animation inputs a ray used.
pub(super) const ANIM_WAVES: u32 = 32;
pub(super) const ANIM_SCROLL: u32 = 64;
pub(super) const ANIM_SKY: u32 = 128;
pub(super) const ANIM_GEOMETRY: u32 = 256;

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

/// Test the step, not the total travel since a cut. Near-plane corners move
/// dramatically even for small translations; reject a near silhouette using
/// the per-pixel reprojection instead of invalidating the whole screen.
pub(super) fn camera_continuous(previous_from_current: Mat4, delta_eye: glam::Vec3) -> bool {
    // Default free flight is 240 units/s (4 at 60 Hz). Let ordinary
    // flight at lower frame rates use per-pixel validation too; a jump
    // beyond this is a cut even when the far frustum barely changes.
    const MAX_EYE_STEP: f32 = 16.0;
    const MAX_FAR_NDC_SHIFT: f32 = 0.10;
    if !delta_eye.is_finite() || delta_eye.length_squared() > MAX_EYE_STEP * MAX_EYE_STEP {
        return false;
    }
    for y in [-1., 1.] {
        for x in [-1., 1.] {
            let p = previous_from_current * Vec4::new(x, y, 0., 1.);
            if p.w <= 0. || !p.is_finite() {
                return false;
            }
            let p = p / p.w;
            if (p.x - x).abs() > MAX_FAR_NDC_SHIFT
                || (p.y - y).abs() > MAX_FAR_NDC_SHIFT
                || p.z.abs() > 0.02
            {
                return false;
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
    revision: u64,
    texture_len: usize,
    mode: Mode,
    waves: [u32; 4],
    water_art: [u32; 4],
    scroll: u32,
    grass: [[f32; 4]; super::scene::GRASS_WIND_SLOTS],
    sky_anchor: [f32; 2],
}

#[derive(Default)]
pub(super) struct History {
    previous: Option<Stamp>,
    sequence: u32,
}

impl History {
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn from_sequence(sequence: u32) -> Self {
        Self {
            previous: None,
            sequence,
        }
    }

    pub(super) fn reset(&mut self) {
        self.previous = None;
    }

    pub(super) fn prepare(
        &mut self,
        frame: &mut Frame,
        revision: u64,
        texture_len: usize,
        mode: Mode,
        lighting_unchanged: bool,
    ) -> HistoryUse {
        // Geometry dependencies are tagged per pixel, including shadow and
        // secondary hits. Do not reproject a changed light/decal snapshot.
        let mut stable = *frame;
        stable.vp = [[0.; 4]; 4];
        stable.previous_screen_vp = [[0.; 4]; 4];
        stable.inv_vp = [[0.; 4]; 4];
        stable.eye[..3].fill(0.);
        stable.waves = [0; 4];
        stable.water_effects[0] = 0;
        stable.shore_offsets = [0.; 4];
        stable.air[3] = 0.;
        stable.sky_uv[..2].fill(0.);
        stable.sampling = [0; 4];
        stable.grass_history.fill([0.; 4]);
        let vp = Mat4::from_cols_array_2d(&frame.vp);
        let inv_vp = Mat4::from_cols_array_2d(&frame.inv_vp);
        let eye = [frame.eye[0], frame.eye[1], frame.eye[2]];
        let mut stamp = Stamp {
            stable,
            vp,
            inv_vp,
            eye,
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
            grass: frame.grass_history,
            sky_anchor: [frame.sky_uv[0], frame.sky_uv[1]],
        };
        let mut changed_animations = 0;
        let mut previous_from_current = None;
        frame.previous_screen_vp = [[0.; 4]; 4];
        let reuse = lighting_unchanged
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
                    let reprojection = old.vp * inv_vp;
                    let step = glam::Vec3::from_array(eye) - glam::Vec3::from_array(old.eye);
                    if frame.options[2] != 1 || !camera_continuous(reprojection, step) {
                        return false;
                    }
                    previous_from_current = Some(reprojection);
                    frame.previous_screen_vp = old.vp.to_cols_array_2d();
                }
                if old.waves != stamp.waves || old.water_art != stamp.water_art {
                    changed_animations |= ANIM_WAVES;
                }
                if old.grass != stamp.grass {
                    changed_animations |= ANIM_GEOMETRY;
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
    atrous: [wgpu::ComputePipeline; 5],
    parameters: wgpu::Buffer,
    history: [Option<Storage>; 2],
    filtered: Option<Storage>,
    history_write: usize,
    accumulate_bind: Bindings<4, 2>,
    denoise_bind: Bindings<4, 4>,
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
        let atrous = ["filter_1", "filter_2", "filter_4", "filter_8", "filter_16"]
            .map(|entry| pipeline("Lighter variance-guided à-trous", entry));
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Lighter radiance processing parameters"),
            size: std::mem::size_of::<Params>() as u64,
            mapped_at_creation: false,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Self {
            layout,
            accumulate,
            atrous,
            parameters,
            history: [None, None],
            filtered: None,
            history_write: 0,
            accumulate_bind: Bindings::default(),
            denoise_bind: Bindings::default(),
            buffers_created: 1,
        }
    }

    #[cfg(test)]
    pub(super) fn last_history(&self) -> &wgpu::Buffer {
        &self.history[self.history_write ^ 1].as_ref().unwrap().0
    }

    pub(super) fn allocation_bytes(&self) -> u64 {
        self.parameters.size()
            + self
                .history
                .iter()
                .chain(std::iter::once(&self.filtered))
                .filter_map(|s| s.as_ref())
                .map(|s| s.0.size())
                .sum::<u64>()
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
        overlay: bool,
        timed: Option<&super::profile::Timings>,
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
            if let Some(t) = timed {
                t.stamp(encoder, 2);
            }
            return raw;
        }
        let mut params = Params {
            extent,
            history: [0, 0, 0, u32::from(overlay)],
            previous_from_current: Mat4::IDENTITY.to_cols_array_2d(),
        };
        if let Some(h) = history {
            params.history = [
                u32::from(h.reuse && !fresh),
                h.changed_animations,
                u32::from(h.previous_from_current.is_some()),
                u32::from(overlay),
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
        if let Some(t) = timed {
            t.stamp(encoder, 2);
        }
        if denoise {
            let filtered = &self.filtered.as_ref().unwrap().0;
            // Transport output is dead after the first filter dispatch. Ping
            // pong it with one scratch allocation, never touch next history.
            let label = "Lighter à-trous radiance";
            // A fresh image cannot use the wide variance passes. Avoid
            // dispatching three full-image copies on nonprogressive frames.
            let stages = if overlay {
                3
            } else if params.history[0] != 0 {
                5
            } else {
                2
            };
            for (step, pipeline) in self.atrous[..stages].iter().enumerate() {
                let next = if step % 2 == 0 { filtered } else { raw };
                let bind = self.denoise_bind.get(
                    device,
                    label,
                    &self.layout,
                    [&self.parameters, image, next, image],
                );
                dispatch(encoder, label, pipeline, bind);
                image = next;
            }
        }
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    #[test]
    fn continuous_camera_compares_each_step_and_detects_cuts() {
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
        for i in 2..=20 {
            set_view(&mut frame, 0.014 * i as f32);
            assert!(
                step(&mut history, &mut frame).reuse,
                "reset after accumulated travel at {i}"
            );
        }
        set_view(&mut frame, 0.5);
        assert!(!step(&mut history, &mut frame).reuse, "rapid turn or zoom");
        let parked = step(&mut history, &mut frame);
        assert!(parked.reuse && parked.previous_from_current.is_none());
        assert!(camera_continuous(Mat4::IDENTITY, glam::Vec3::X * 8.));
        frame.eye[2] += 32.;
        assert!(!step(&mut history, &mut frame).reuse, "position cut");
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
