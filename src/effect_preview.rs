//! Headless effect previews: the production effect pass, rendered offscreen.
//!
//! The effect itself is never drawn by this module: a preview builds a [`World`] exactly
//! as the viewer does, installs the placements the map authors, runs the frame
//! loop so the loader queue and the activation gates do their work, and then
//! renders the same pass the window renders — into a texture instead of the
//! swap chain. The result is written as a PNG, one per placement per frame.
//!
//! ```text
//! cargo test --release --bin perfect-world-explorer -- --ignored --nocapture effect_previews
//! ```
//!
//! `effect_preview_smoke` needs no client files: it renders a synthetic root
//! through the same path and asserts that pixels appeared, so the harness itself
//! is gated on every run. `effect_previews` renders the shipped roots and reads:
//!
//! | variable | meaning |
//! | --- | --- |
//! | `PWE_PREVIEW_ELEMENT` | client `element` directory holding `gfx.pck` |
//! | `PWE_PREVIEW_WORLD` | an `.ecwld` to take placements from |
//! | `PWE_PREVIEW_ROOTS` | `;`-separated path substrings to keep (empty: all) |
//! | `PWE_PREVIEW_FRAMES` | `,`-separated shot times in milliseconds |
//! | `PWE_PREVIEW_MAX` | how many placements to render |
//! | `PWE_PREVIEW_SIZE` | PNG edge in pixels |
//! | `PWE_PREVIEW_FADE` | the distance fade to shoot with, 0 for off |
//! | `PWE_PREVIEW_OUT` | output directory |
//! | `PWE_PREVIEW_REBASE` | 1 moves the placement to the origin, keeping its authored scale and direction — the single-effect lab |
//! | `PWE_PREVIEW_ZOOMS` | `,`-separated distances, 1 fits the measured box and 0.45 is twice as close |
//! | `PWE_PREVIEW_ANGLES` | `,`-separated `azimuth:elevation` degrees to view the effect from |
//!
//! Every PNG carries a **ruler**: a one-metre (or one-spacing) grid on the
//! ground plane and a tick ladder up the effect's own height, drawn by this
//! module and by nothing else. It is a yardstick, not a claim about the effect.
//!
//! The number that decides whether an effect is the size the map authored is
//! printed beside every shot and collected into `metrics.md`: the world-space
//! box the quads occupy as `width x height x depth`, its diagonal, the ruler's
//! spacing and the world units one pixel covers. A sheet drawn at twice its
//! authored size says so in that line whatever it looks like on screen.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::scene::{Block, Effect, Scene};
use crate::world_models_gpu::World;

/// The pass insists on an encoded UNORM target, and a preview wants the same
/// numbers the window would get, so no sRGB suffix here.
const PREVIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The empty-world colour. A dark slate makes an additive sheet and an
/// alpha-blended one look different, which a black background would hide.
const CLEAR: [f64; 4] = [0.05, 0.06, 0.09, 1.];

fn clear_color() -> wgpu::Color {
    wgpu::Color {
        r: CLEAR[0],
        g: CLEAR[1],
        b: CLEAR[2],
        a: CLEAR[3],
    }
}

/// A device for the preview, or `None` when this machine has no adapter at
/// all (a test runner without a Vulkan ICD). Callers decide whether that is a
/// skip or a failure: the smoke test skips, the corpus test refuses.
fn preview_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let options = wgpu::RequestAdapterOptions::default();
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&options))?;
    let descriptor = wgpu::DeviceDescriptor {
        label: Some("effect preview"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
    };
    pollster::block_on(adapter.request_device(&descriptor, None)).ok()
}

fn empty_scene() -> Scene {
    let block = Block {
        row: 0,
        col: 0,
        trees: Vec::new(),
        water: Vec::new(),
        grass: Vec::new(),
        ornaments: Vec::new(),
        boxes: Vec::new(),
        effects: Vec::new(),
        models: Vec::new(),
        critters: Vec::new(),
        beziers: Vec::new(),
        sounds: Vec::new(),
    };
    Scene {
        width: 8192.,
        length: 11264.,
        block_size: 64.,
        rows: 1,
        cols: 1,
        tree_types: Vec::new(),
        grass_types: Vec::new(),
        blocks: vec![block],
    }
}

/// One placement on its own, so a shot cannot be explained by its neighbours.
///
/// `iValidTime` is forced to 2: the preview is asked for a root, and whether the
/// map draws that root at noon is a different question, which the census in the
/// findings answers.
fn one_effect_scene(record: &Effect) -> Scene {
    scene_with_one_effect(record, false)
}

/// `rebase` moves the placement to the origin and keeps everything else — the
/// authored scale, direction and up. That is the lab: one effect on its own,
/// at a known position, with the map's own numbers rather than invented ones.
fn scene_with_one_effect(record: &Effect, rebase: bool) -> Scene {
    let mut scene = empty_scene();
    if let Some(block) = scene.blocks.first_mut() {
        let mut copy = record.clone();
        copy.valid_time = 2;
        if rebase {
            copy.position = [0., 0., 0.];
        }
        block.effects.push(copy);
    }
    scene
}

#[derive(Clone, Copy)]
struct Shot {
    width: u32,
    height: u32,
    eye: [f32; 3],
    target: [f32; 3],
    /// World units a pixel covers at the target, so a PNG reads in metres.
    pixel_scale: f32,
}

impl Shot {
    fn matrix(self) -> [[f32; 4]; 4] {
        let aspect = self.width as f32 / self.height.max(1) as f32;
        let projection = glam::Mat4::perspective_rh(45f32.to_radians(), aspect, 0.05, 20_000.);
        let eye = glam::Vec3::from_array(self.eye);
        let target = glam::Vec3::from_array(self.target);
        let view = glam::Mat4::look_at_rh(eye, target, glam::Vec3::Y);
        (projection * view).to_cols_array_2d()
    }
}

/// Frame a world box from one fixed direction, so two shots of two roots are
/// comparable: the distance follows the box, not the camera.
/// Frame a world box from a chosen direction: `azimuth` is degrees around the
/// up axis, `elevation` degrees above the horizon, `zoom` multiplies the
/// distance (1 fits the box, 0.5 is twice as close). The distance follows the
/// box, so two shots of two roots are comparable.
fn framing_shot_at(
    centre: [f32; 3],
    radius: f32,
    size: u32,
    azimuth_deg: f32,
    elevation_deg: f32,
    zoom: f32,
) -> Shot {
    let azimuth = azimuth_deg.to_radians();
    let elevation = elevation_deg.clamp(-89., 89.).to_radians();
    let half_angle = 22.5f32.to_radians().tan();
    let distance = ((radius.max(0.25) / half_angle) * 1.15 + 0.5) * zoom.max(0.01);
    let direction = glam::Vec3::new(
        azimuth.sin() * elevation.cos(),
        elevation.sin(),
        azimuth.cos() * elevation.cos(),
    );
    let target = glam::Vec3::from_array(centre);
    Shot {
        width: size,
        height: size,
        eye: (target + direction * distance).to_array(),
        target: centre,
        pixel_scale: distance * 2. * half_angle / size.max(1) as f32,
    }
}

/// The world's frame constants with the neutral light the reader falls back to
/// when no environment record matches the eye.
fn effect_frame(
    matrix: [[f32; 4]; 4],
    eye: [f32; 3],
    target: [f32; 3],
) -> crate::model_draw_gpu::Frame {
    let lighting = crate::model_material::Lighting {
        environment_ambient: [0.85, 0.85, 0.9, 1.],
        additional_ambient: [0.; 4],
        directional_diffuse: [0.35, 0.35, 0.3, 1.],
        point_ambient: [0.; 4],
        point_diffuse: [0.; 4],
        transparency: -1.,
        point_enabled: false,
    };
    let forward = glam::Vec3::from_array(target) - glam::Vec3::from_array(eye);
    let forward = if forward.length_squared() > 1e-6 {
        forward.normalize()
    } else {
        glam::Vec3::NEG_Z
    };
    crate::model_draw_gpu::Frame {
        blend_enabled: false,
        matrix,
        lighting,
        direction: [0., -1., 0.],
        point_position: [0.; 3],
        attenuation: [0., 0.4, 0.],
        eye_direction: forward.to_array(),
        directional_specular: [1., 1., 1., 1.],
        specular_variant: true,
        vertex_fog: crate::fog_matrix::MODEL_VERTEX_FOG,
        fog_color: [0., 0., 0., 0.],
        fog_range: [0., 0., 0., 0.],
        clip_plane: [0.; 4],
        reverse_front: false,
        alpha_compare: 5,
        alpha_reference: 84. / 255.,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    quads: usize,
    covered: usize,
    min: [f32; 3],
    max: [f32; 3],
    /// The ruler's grid step, in world units, that the PNG was drawn with.
    spacing: f32,
    /// The clock the shot asked for, in milliseconds of wall time.
    clock: u32,
    /// What the admitted root's own clock had reached, in milliseconds. This is
    /// the number the tile/`TexSpeed` branch reads, so it is the one that says
    /// whether the sheet moved; `clock` above is only what the harness asked for.
    player_clock: f32,
    /// The speed the root is playing at, and the `PlaySpeed` its file authors.
    /// A placement replaces the second with the first (`SetPlaySpeed`), so a pair
    /// that differs is a map overriding an asset — and a live speed of zero is a
    /// frozen effect however many frames are ticked.
    play_speed: f32,
    authored_speed: f32,
    /// The shortest and longest edge of any submitted quad: a shaft card is a
    /// long thin rectangle, and this is what says so without a picture.
    card: (f32, f32),
}

impl Stats {
    fn radius(&self) -> f32 {
        let span = glam::Vec3::from_array(self.max) - glam::Vec3::from_array(self.min);
        if span.is_finite() {
            span.length() * 0.5
        } else {
            1.
        }
    }

    fn centre(&self) -> [f32; 3] {
        let min = glam::Vec3::from_array(self.min);
        let max = glam::Vec3::from_array(self.max);
        ((min + max) * 0.5).to_array()
    }

    /// `size=` is the one number a report can be checked against: the box the
    /// quads occupy, in the map's own units, printed as width x height x depth.
    fn describe(&self) -> String {
        format!(
            "player clock {:.0} ms play {:.3} (file {:.3}), quads={} pixels={} size={:.2}x{:.2}x{:.2} card={:.2}x{:.2} box=({:.2},{:.2},{:.2})..({:.2},{:.2},{:.2})",
            self.player_clock,
            self.play_speed,
            self.authored_speed,
            self.quads,
            self.covered,
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
            self.card.0,
            self.card.1,
            self.min[0],
            self.min[1],
            self.min[2],
            self.max[0],
            self.max[1],
            self.max[2]
        )
    }
}

fn quad_bounds(quads: &[crate::gfx_player::EffectQuad]) -> Option<([f32; 3], [f32; 3])> {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut any = false;
    let mut add = |corner: &[f32; 3]| {
        if !corner.iter().all(|value| value.is_finite()) {
            return;
        }
        any = true;
        for axis in 0..3 {
            min[axis] = min[axis].min(corner[axis]);
            max[axis] = max[axis].max(corner[axis]);
        }
    };
    for quad in quads {
        // An element-160 mesh carries its triangle soup in `mesh.vertices` and
        // leaves the four card corners at the origin as filler. Reading those
        // corners framed the camera on a box running from the world origin to
        // the placement: `蓝蘑菇2`, a two-unit mushroom, reported
        // `size=2594.18x216.54x3399.64` and was shot at 12.8 m per pixel, so the
        // published frame showed a speck.
        match &quad.mesh {
            Some(mesh) => {
                for vertex in &mesh.vertices {
                    add(&vertex.position);
                }
            }
            None => {
                for corner in &quad.positions {
                    add(corner);
                }
            }
        }
    }
    any.then_some((min, max))
}

/// A world with one placement installed, ready to be ticked: the archive
/// attached, the gates open, and the fade set. The count says how many
/// placements `install_scene_effects` accepted.
fn preview_world(
    device: &wgpu::Device,
    element: &Path,
    record: &Effect,
    fade: f32,
    rebase: bool,
) -> (World, usize) {
    let mut world = World::without_models(device, PREVIEW_FORMAT);
    world.attach_gfx_archive(element).expect("attach gfx.pck");
    let installed = world.install_scene_effects(&scene_with_one_effect(record, rebase));
    if rebase {
        world.set_view_center(0., 0., 1.0e6);
    } else {
        world.set_view_center(record.position[0], record.position[2], 1.0e6);
    }
    world.set_effects_enabled(true);
    world.set_effect_distance(fade);
    (world, installed)
}

/// Tick the world `ticks` times at 33 ms. One root per frame is the loader's
/// own budget (0x5b1a00) and an element needs its clock past its first
/// keypoint, so a shot always advances a short run instead of a single frame.
fn advance(
    world: &mut World,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    shot: Shot,
    ticks: u32,
) -> u32 {
    let frame = effect_frame(shot.matrix(), shot.eye, shot.target);
    let lights = crate::scene_lights::SceneLights::default();
    let ticks = ticks.max(1);
    for _ in 0..ticks {
        world.update(device, queue, frame, shot.target, &lights, 0., 33);
    }
    // The root's own clock, which is what the elements schedule against. A warm
    // up pass means this is never exactly the requested time, so the report says
    // what the effect had actually been running.
    ticks * 33
}

/// A measuring stick, not an effect: a one-metre grid on the ground plane and
/// a tick ladder up the effect's own height, drawn into the same target. Without
/// it a PNG says "something is here" and nothing about how big it is, and a
/// 1,200-unit beam and a 12-unit card look identical once the framing adapts.
///
/// The ruler is the only thing in this module that draws with its own shader.
/// It is a yardstick drawn under the real pass, never a claim about the effect.
const RULER_WGSL: &str = r#"
struct Uniforms { matrix:mat4x4<f32> };
@group(0) @binding(0) var<uniform> u:Uniforms;
struct In { @location(0) position:vec3<f32>, @location(1) color:vec3<f32> };
struct Out { @builtin(position) position:vec4<f32>, @location(0) color:vec3<f32> };
@vertex fn vs(v:In)->Out {
 var o:Out;
 o.position=u.matrix*vec4<f32>(v.position,1.);
 o.color=v.color;
 return o;
}
@fragment fn fs(v:Out)->@location(0) vec4<f32> { return vec4<f32>(v.color,1.); }
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineVertex {
    position: [f32; 3],
    color: [f32; 3],
}

struct Ruler {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    bind: wgpu::BindGroup,
    vertices: u32,
}

impl Ruler {
    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(0..self.vertices, 0..1);
    }
}

/// One grid step, chosen so a shot shows a handful of labelled decades: 1, 2 or
/// 5 times a power of ten.
fn nice_spacing(span: f32) -> f32 {
    let target = (span / 10.).max(0.05);
    let decade = 10f32.powf(target.log10().floor());
    let step = target / decade;
    let step = if step <= 1. {
        1.
    } else if step <= 2. {
        2.
    } else if step <= 5. {
        5.
    } else {
        10.
    };
    decade * step
}

fn ruler_vertices(min: [f32; 3], max: [f32; 3]) -> (Vec<LineVertex>, f32) {
    let minor = [0.26, 0.28, 0.32];
    let major = [0.42, 0.45, 0.52];
    let axis_x = [0.60, 0.24, 0.24];
    let axis_z = [0.24, 0.42, 0.66];
    let ladder = [0.28, 0.62, 0.32];
    let mut lines: Vec<LineVertex> = Vec::new();
    let span = ((max[0] - min[0]).max(max[1] - min[1]).max(max[2] - min[2])).max(1.);
    let spacing = nice_spacing(span);
    // The dashes are a measuring scale, so they live on the ground under the
    // effect and run out far enough that the box edge is never the grid edge.
    let floor = min[1];
    let centre_x = (min[0] + max[0]) * 0.5;
    let centre_z = (min[2] + max[2]) * 0.5;
    let half = ((max[0] - min[0]).max(max[2] - min[2])) * 0.5 + spacing * 3.;
    let count = (half / spacing).ceil() as i32;
    let mut line = |from: [f32; 3], to: [f32; 3], color: [f32; 3]| {
        lines.push(LineVertex {
            position: from,
            color,
        });
        lines.push(LineVertex {
            position: to,
            color,
        });
    };
    for i in -count..=count {
        let offset = i as f32 * spacing;
        let color = if i == 0 {
            major
        } else if i % 10 == 0 {
            major
        } else {
            minor
        };
        line(
            [centre_x + offset, floor, centre_z - half],
            [centre_x + offset, floor, centre_z + half],
            color,
        );
        line(
            [centre_x - half, floor, centre_z + offset],
            [centre_x + half, floor, centre_z + offset],
            color,
        );
    }
    // The two ground axes are coloured so a shot says which way the effect faces.
    line(
        [centre_x - half, floor, centre_z],
        [centre_x + half, floor, centre_z],
        axis_x,
    );
    line(
        [centre_x, floor, centre_z - half],
        [centre_x, floor, centre_z + half],
        axis_z,
    );
    // A ladder up the effect's height, ticked every step and longer every ten.
    let height = (max[1] - floor).max(spacing);
    let steps = (height / spacing).ceil() as i32;
    line(
        [centre_x, floor, centre_z],
        [centre_x, floor + height, centre_z],
        ladder,
    );
    for k in 0..=steps {
        let y = floor + k as f32 * spacing;
        let length = if k % 10 == 0 {
            spacing * 0.6
        } else {
            spacing * 0.3
        };
        line(
            [centre_x, y, centre_z],
            [centre_x + length, y, centre_z],
            ladder,
        );
    }
    (lines, spacing)
}

fn ruler(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    shot: Shot,
    stats: &Stats,
) -> (Ruler, f32) {
    let (vertices, spacing) = ruler_vertices(stats.min, stats.max);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("effect preview ruler"),
        source: wgpu::ShaderSource::Wgsl(RULER_WGSL.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("effect preview ruler layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("effect preview ruler matrix"),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let matrix = shot.matrix();
    let mut words = [0f32; 16];
    for (row, values) in matrix.iter().enumerate() {
        for (column, value) in values.iter().enumerate() {
            words[row * 4 + column] = *value;
        }
    }
    queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&words));
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("effect preview ruler binding"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("effect preview ruler pipeline layout"),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let attributes = [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 12,
            shader_location: 1,
        },
    ];
    let pipeline_descriptor = wgpu::RenderPipelineDescriptor {
        label: Some("effect preview ruler"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: "vs",
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<LineVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: "fs",
            targets: &[Some(wgpu::ColorTargetState {
                format: PREVIEW_FORMAT,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            cull_mode: None,
            ..Default::default()
        },
        // The pass owns a depth attachment, so the ruler must declare one; it
        // neither writes nor tests, which keeps it under the effect instead of
        // punching one-pixel holes in a sheet.
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
    };
    let pipeline = device.create_render_pipeline(&pipeline_descriptor);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("effect preview ruler vertices"),
        size: (vertices.len() * std::mem::size_of::<LineVertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&vertices));
    (
        Ruler {
            pipeline,
            buffer,
            bind,
            vertices: vertices.len() as u32,
        },
        spacing,
    )
}

/// Render the effect pass into an RGBA texture and write the frame as a PNG.
fn capture(
    world: &mut World,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    shot: Shot,
    path: &Path,
) -> Result<Stats, String> {
    let mut stats = Stats::default();
    if let Some((min, max)) = quad_bounds(world.effect_quads()) {
        stats.min = min;
        stats.max = max;
    }
    stats.quads = world.effect_quad_count();
    stats.card = quad_edges(world.effect_quads());
    let extent = wgpu::Extent3d {
        width: shot.width,
        height: shot.height,
        depth_or_array_layers: 1,
    };
    let color_descriptor = wgpu::TextureDescriptor {
        label: Some("effect preview colour"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: PREVIEW_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    };
    let color = device.create_texture(&color_descriptor);
    let depth_descriptor = wgpu::TextureDescriptor {
        label: Some("effect preview depth"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    };
    let depth = device.create_texture(&depth_descriptor);
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
    // The effect pipelines are built with the world's compare function, so a
    // depth attachment has to be present. Cleared to zero and compared with
    // `GreaterEqual`, every quad at or in front of the far plane passes: a
    // preview shows the root rather than its occlusion.
    let color_attachment = wgpu::RenderPassColorAttachment {
        view: &color_view,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(clear_color()),
            store: wgpu::StoreOp::Store,
        },
    };
    let depth_attachment = wgpu::RenderPassDepthStencilAttachment {
        view: &depth_view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(0.),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: None,
    };
    let pass_descriptor = wgpu::RenderPassDescriptor {
        label: Some("effect preview pass"),
        color_attachments: &[Some(color_attachment)],
        depth_stencil_attachment: Some(depth_attachment),
        occlusion_query_set: None,
        timestamp_writes: None,
    };
    // The yardstick is built from the measured box, so it frames exactly what
    // was submitted rather than a guess at the root's size.
    let (yardstick, spacing) = ruler(device, queue, shot, &stats);
    let encoder_descriptor = wgpu::CommandEncoderDescriptor {
        label: Some("effect preview encoder"),
    };
    let mut encoder = device.create_command_encoder(&encoder_descriptor);
    {
        let mut pass = encoder.begin_render_pass(&pass_descriptor);
        yardstick.draw(&mut pass);
        world.draw(&mut pass);
    }
    let padded = (shot.width * 4).div_ceil(256) * 256;
    let readback_descriptor = wgpu::BufferDescriptor {
        label: Some("effect preview readback"),
        size: (padded as u64) * (shot.height as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    };
    let readback = device.create_buffer(&readback_descriptor);
    let source = wgpu::ImageCopyTexture {
        texture: &color,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    };
    let destination = wgpu::ImageCopyBuffer {
        buffer: &readback,
        layout: wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(padded),
            rows_per_image: Some(shot.height),
        },
    };
    encoder.copy_texture_to_buffer(source, destination, extent);
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::Maintain::Wait);
    let mapped = receiver.recv().map_err(|error| error.to_string())?;
    mapped.map_err(|error| format!("preview readback failed: {error}"))?;
    let bytes = slice.get_mapped_range();
    let row_bytes = (shot.width * 4) as usize;
    let mut pixels = Vec::with_capacity(row_bytes * shot.height as usize);
    for row in 0..shot.height as usize {
        let start = row * padded as usize;
        pixels.extend_from_slice(&bytes[start..start + row_bytes]);
    }
    drop(bytes);
    readback.unmap();
    let clear = [
        (CLEAR[0] * 255.).round() as i32,
        (CLEAR[1] * 255.).round() as i32,
        (CLEAR[2] * 255.).round() as i32,
    ];
    let mut covered = 0usize;
    for pixel in pixels.chunks_exact(4) {
        if (0..3).any(|axis| (pixel[axis] as i32 - clear[axis]).abs() > 6) {
            covered += 1;
        }
    }
    stats.covered = covered;
    write_png(path, shot.width, shot.height, &pixels)?;
    stats.spacing = spacing;
    Ok(stats)
}

/// The shortest and longest edge over every submitted quad. A card the file
/// authors as `0.5 x 10` must come out with those two numbers in that ratio,
/// whatever the framing does to the picture.
fn quad_edges(quads: &[crate::gfx_player::EffectQuad]) -> (f32, f32) {
    // The *longest quad's* own edges, not the extremes over the whole frame: a
    // shaft card next to a one-unit spark reports nothing useful if the two are
    // folded together, and the shaft is the thing a size report is about.
    let mut best = (0f32, 0f32);
    for quad in quads {
        if quad.mesh.is_some() {
            // A model element is a triangle soup; it has no four corners.
            continue;
        }
        let corners = quad.positions;
        // Corner order is (top-left, top-right, bottom-left, bottom-right): the
        // height is corner 2 and corner 0. Corner 3 is the diagonal, and reading
        // it reported a square 404-unit ring as `404 x 571` and a 71.83 x 2514
        // shaft as `71.83 x 2515.07` — which is exactly the signature of the
        // diagonal (x sqrt 2 for a square).
        let width = (glam::Vec3::from_array(corners[1]) - glam::Vec3::from_array(corners[0])).length();
        let height = (glam::Vec3::from_array(corners[2]) - glam::Vec3::from_array(corners[0])).length();
        if !width.is_finite() || !height.is_finite() {
            continue;
        }
        let longest = width.max(height);
        if longest > best.1 {
            best = (width.min(height), longest);
        }
    }
    best
}

/// One submitted card, measured: what a size report is actually about.
struct Card {
    short: f32,
    long: f32,
    centre: glam::Vec3,
    /// The card's own height direction. A shaft that stands up has a vertical
    /// axis whatever its box says.
    axis: glam::Vec3,
    texture: String,
    source: u32,
    destination: u32,
    depth: bool,
    /// The card's top-left texture coordinate and the window it spans.
    ///
    /// A tiled sheet writes `TexSpeed * seconds` here, so two shots of the same
    /// card at two clocks differ in `uv` by exactly the authored speed even when
    /// the picture is too washed out to see it. A flipbook card steps a cell out
    /// of an atlas instead, and its window never leaves its grid.
    uv0: [f32; 2],
    uv_span: [f32; 2],
}

impl Card {
    fn of(quad: &crate::gfx_player::EffectQuad) -> Option<Self> {
        if quad.mesh.is_some() {
            return None;
        }
        let corners = quad.positions;
        // Height is corner 2 (bottom-left), not corner 3: the corner order is
        // (top-left, top-right, bottom-left, bottom-right), so corner 3 minus
        // corner 0 is the *diagonal*. The difference is invisible on a 35:1
        // shaft (2515.07 against 2514.04) and glaring on a square ring, whose
        // diagonal is exactly sqrt(2) times its side: `404.04 x 571.40` is a
        // 404-unit square, and `206.51 x 292.05` is a 206.51-unit square.
        let width =
            (glam::Vec3::from_array(corners[1]) - glam::Vec3::from_array(corners[0])).length();
        let height =
            (glam::Vec3::from_array(corners[2]) - glam::Vec3::from_array(corners[0])).length();
        if !width.is_finite() || !height.is_finite() {
            return None;
        }
        let centre = (glam::Vec3::from_array(corners[0])
            + glam::Vec3::from_array(corners[1])
            + glam::Vec3::from_array(corners[2])
            + glam::Vec3::from_array(corners[3]))
            * 0.25;
        let axis = glam::Vec3::from_array(corners[2]) - glam::Vec3::from_array(corners[0]);
        let axis = if axis.length_squared() > 1e-9 {
            axis.normalize()
        } else {
            glam::Vec3::ZERO
        };
        Some(Self {
            short: width.min(height),
            long: width.max(height),
            centre,
            axis,
            texture: quad.texture.clone(),
            source: quad.source_blend,
            destination: quad.destination_blend,
            depth: quad.depth_test,
            uv0: quad.uv[0],
            uv_span: [
                quad.uv[1][0] - quad.uv[0][0],
                quad.uv[2][1] - quad.uv[0][1],
            ],
        })
    }
}

/// Per-quad lines, largest card first. The biggest card is what a size report is
/// about: a shaft next to a hundred sparks is line 90 of the raw order and line
/// 1 of this one.
fn dump_quads(quads: &[crate::gfx_player::EffectQuad], limit: usize) -> Vec<String> {
    // A mesh element comes first: `Card::of` skips it because a triangle soup
    // has no four corners, and its *world* extent is the number a size report
    // about a model element is actually about. Two meshes in one root are
    // printed separately, so a plant that is the right size next to one that is
    // five times too big cannot hide inside a union box.
    let mut lines: Vec<String> = Vec::new();
    for quad in quads {
        let Some(mesh) = &quad.mesh else { continue };
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut any = false;
        for vertex in &mesh.vertices {
            if !vertex.position.iter().all(|value| value.is_finite()) {
                continue;
            }
            any = true;
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex.position[axis]);
                max[axis] = max[axis].max(vertex.position[axis]);
            }
        }
        if !any {
            continue;
        }
        lines.push(format!(
            "- mesh {} verts={} extent {:.2} x {:.2} x {:.2} at ({:.1}, {:.1}, {:.1}) \
             blend {}/{} depth {}",
            mesh.texture,
            mesh.vertices.len(),
            max[0] - min[0],
            max[1] - min[1],
            max[2] - min[2],
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
            quad.source_blend,
            quad.destination_blend,
            quad.depth_test
        ));
    }
    let mut cards: Vec<Card> = quads.iter().filter_map(Card::of).collect();
    cards.sort_by(|a, b| b.long.partial_cmp(&a.long).unwrap_or(std::cmp::Ordering::Equal));
    for card in cards.iter().take(limit) {
        lines.push(format!(
            "- card {:.2} x {:.2} at ({:.1}, {:.1}, {:.1}) axis ({:.2}, {:.2}, {:.2}) \
             uv ({:.3}, {:.3}) + ({:.3}, {:.3}) tex {} blend {}/{} depth {}",
            card.short,
            card.long,
            card.centre.x,
            card.centre.y,
            card.centre.z,
            card.axis.x,
            card.axis.y,
            card.axis.z,
            card.uv0[0],
            card.uv0[1],
            card.uv_span[0],
            card.uv_span[1],
            card.texture,
            card.source,
            card.destination,
            card.depth
        ));
    }
    lines.truncate(limit);
    lines
}

/// Advance the world and then capture it: the one call a caller needs.
fn render_shot(
    world: &mut World,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    shot: Shot,
    path: &Path,
    ticks: u32,
) -> Result<Stats, String> {
    let clock = advance(world, device, queue, shot, ticks);
    let mut stats = capture(world, device, queue, shot, path)?;
    stats.clock = clock;
    // What the root itself reached, not what the harness asked for: a warm-up run
    // of `ticks` frames only lands on the requested time when the placement plays
    // at speed 1, and the whole point of quoting a clock is that it is read out of
    // the player that scrolled the sheet.
    if let Some((_, player_clock, play_speed, authored_speed)) =
        world.effect_clocks().into_iter().next()
    {
        stats.player_clock = player_clock;
        stats.play_speed = play_speed;
        stats.authored_speed = authored_speed;
    }
    Ok(stats)
}

/// Write RGBA8 as a PNG, gamma-encoded so the file looks like the game rather
/// than like linear light: the pass renders into an encoded UNORM target, so
/// what comes back is scene-linear.
fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut table = [0u8; 256];
    for (value, entry) in table.iter_mut().enumerate() {
        let linear = value as f32 / 255.;
        *entry = (linear.powf(1. / 2.2) * 255.).round().clamp(0., 255.) as u8;
    }
    let mut encoded = rgba.to_vec();
    for pixel in encoded.chunks_exact_mut(4) {
        pixel[0] = table[pixel[0] as usize];
        pixel[1] = table[pixel[1] as usize];
        pixel[2] = table[pixel[2] as usize];
    }
    let shown = path.display();
    let file = std::fs::File::create(path).map_err(|error| format!("{shown}: {error}"))?;
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let header = encoder.write_header();
    let mut writer = header.map_err(|error| format!("{shown}: {error}"))?;
    writer
        .write_image_data(&encoded)
        .map_err(|error| format!("{shown}: {error}"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn env_list(name: &str, separator: char, default: &str) -> Vec<String> {
    let text = std::env::var(name).unwrap_or_else(|_| default.to_string());
    let mut items = Vec::new();
    for item in text.split(separator) {
        let item = item.trim();
        if !item.is_empty() {
            items.push(item.to_string());
        }
    }
    items
}

fn env_u32(name: &str, default: u32) -> u32 {
    let parsed = std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok());
    parsed.unwrap_or(default)
}

fn env_f32(name: &str, default: f32) -> f32 {
    let parsed = std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok());
    parsed.unwrap_or(default)
}

fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    out
}

/// The synthetic root the smoke test renders: one sprite card, no archive.
const FAKE_ROOT: &str = "MOXTVersion: 37\r\nDedaultScale: 1\r\nPlaySpeed: 1\r\nDefaultAlpha: 1\r\n\
     Raytrace: 0\r\nFadeByDist: 0\r\nFadeStart: 0.000000\r\nFadeEnd: 0.000000\r\n\
     Vec: 0.100000, 0.200000, 0.300000\r\nVec: 1.000000, 2.000000, 3.000000\r\n\
     ShakeCam: 0\r\nGFXELEMENTCOUNT: 1\r\n\
     GFXELEMENTID: 100\r\nName: glow\r\nSrcBlend: 5\r\nDestBlend: 2\r\n\
     RepeatCount: 1\r\nRepeatDelay: 0\r\nTexFile: 粒子\\glow.dds\r\nBindEle: \r\nZEnable: 0\r\n\
     MatchGrnd: 0\r\nTexRow: 1\r\nTexCol: 1\r\nTexInterval: 50\r\nPriority: 2\r\nIsDummy: 0\r\n\
     DummyEle: \r\nWarp: 0\r\nWidth: 4.000000\r\nHeight: 4.000000\r\nRotFromView: 0\r\n\
     GrndNormOnly: 0\r\nStartTime: 0\r\nKEYPOINTCOUNT: 1\r\nInterpolateMode: 0\r\n\
     TimeSpan: -1\r\nPosition: 0, 0, 0\r\nColor: -1\r\nScale: 1\r\nDirection: 0, 0, 0, 1\r\n\
     Rad_2D: 0\r\nCtrlMethodCount: 0\r\n";

/// A 4x4 opaque white TGA, built here rather than shipped: the smoke test has
/// to have a decodable map and takes no client file.
fn white_tga() -> Vec<u8> {
    let mut bytes = vec![0u8; 18];
    bytes[2] = 2; // uncompressed true-colour
    bytes[12] = 4; // width, little endian
    bytes[14] = 4; // height
    bytes[16] = 32; // bits per pixel
    bytes[17] = 8 | 0x20; // eight alpha bits, top-left origin
    for _ in 0..16 {
        bytes.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
    }
    bytes
}

fn fake_record() -> Effect {
    Effect {
        index: 0,
        export_id: 1,
        scale: 1.,
        position: [0., 0., 0.],
        direction: [0., 0., -1.],
        up: [0., 1., 0.],
        speed: 1.,
        valid_time: 2,
        alpha: 1.,
        attenuation: false,
        path: "场景\\preview卡.gfx".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The framing box has to come from a mesh element's triangle soup, not from
    /// the four card corners it leaves at the origin.
    ///
    /// `蓝蘑菇2` is two element-160 meshes and six cards. Reading the filler
    /// corners framed it on a box running from the world origin to its
    /// placement — `size=2594.18x216.54x3399.64` for a mushroom two units tall,
    /// shot at 12.8 m per pixel — so the published frame showed a speck and the
    /// size line described the map, not the effect.
    #[test]
    fn a_mesh_elements_soup_is_what_the_framing_box_is_made_of() {
        let vertex = |position: [f32; 3], uv: [f32; 2]| crate::gfx_player::EffectVertex {
            position,
            uv,
            normal: [0., 1., 0.],
        };
        let quad = crate::gfx_player::EffectQuad {
            texture: "a.png".into(),
            source_blend: 5,
            destination_blend: 2,
            depth_test: false,
            layer: 0,
            priority: 0,
            tile: true,
            positions: [[0.; 3]; 4],
            uv: [[0.; 2]; 4],
            color: [1.; 4],
            vertex_colors: None,
            mesh: Some(crate::gfx_player::EffectMesh {
                texture: "a.png".into(),
                vertices: vec![
                    vertex([100., 0., -50.], [0., 0.]),
                    vertex([102., 4., -50.], [1., 0.]),
                    vertex([100., 4., -48.], [0., 1.]),
                ],
                write_depth: false,
                material: None,
            }),
        };
        let (min, max) = quad_bounds(&[quad]).expect("a mesh has bounds");
        assert_eq!(
            (min, max),
            ([100., 0., -50.], [102., 4., -48.]),
            "the box must be the mesh's own extent, not the origin"
        );
    }

    /// The harness itself, with no client files: one synthetic card through the
    /// real world and effect pass has to cover pixels. If this fails, the
    /// previews are not evidence of anything.
    #[test]
    fn effect_preview_smoke() {
        // The plain unit-test job runs on a machine with no Vulkan ICD, and a
        // test that needs a GPU must not turn that into a red build. The GPU
        // job in `pr-checks.yml` is where this actually runs.
        let Some((device, queue)) = preview_device() else {
            crate::pck::corpus_absent("Vulkan adapter");
            return;
        };
        // Roots are decoded text, so the smoke archive is a text reader. Its
        // texture reader answers every name with one 4x4 white TGA: a bucket
        // whose map will not load is painted with a *transparent* stand-in, so
        // a preview with no texture archive draws nothing at all -- the first
        // version of this test proved exactly that (quads=1 pixels=0).
        let root = FAKE_ROOT.to_string();
        let texture = white_tga();
        let read = move |_path: &str| Ok(texture.clone());
        let assets = crate::world_models::Assets::default();
        let mut world = World::with_reader(&device, PREVIEW_FORMAT, assets, read)
            .expect("a world with no models");
        world.attach_gfx_reader(move |path: &str| {
            if path.ends_with("preview卡.gfx") {
                Ok(root.clone())
            } else {
                Err(format!("the smoke archive has no root: {path}"))
            }
        });
        let scene = one_effect_scene(&fake_record());
        let installed = world.install_scene_effects(&scene);
        assert_eq!(installed, 1, "one placement installed");
        world.set_view_center(0., 0., 1.0e6);
        world.set_effects_enabled(true);
        world.set_effect_distance(0.);
        let shot = Shot {
            width: 128,
            height: 128,
            eye: [0., 1.5, 9.],
            target: [0., 0., 0.],
            pixel_scale: 9. * 2. * 22.5f32.to_radians().tan() / 128.,
        };
        let out = std::env::temp_dir().join("pwe-effect-preview-smoke.png");
        let stats =
            render_shot(&mut world, &device, &queue, shot, &out, 24).expect("a rendered frame");
        eprintln!("smoke: {}", stats.describe());
        assert_eq!(stats.quads, 1, "the card reached the pass");
        assert!(stats.covered > 200, "the card has to cover pixels");
    }

    /// The shipped roots, one shot per placement, auto-framed so two effects of
    /// different sizes fill the frame the same way. The printed line carries the
    /// world-space box, which is what says whether an effect is oversized.
    #[test]
    #[ignore]
    fn effect_previews() {
        let default_element = PathBuf::from("google_drive");
        let element = env_path("PWE_PREVIEW_ELEMENT").unwrap_or(default_element);
        if !element.join("gfx.pck").is_file() {
            crate::pck::corpus_absent("`gfx.pck`");
            return;
        }
        let default_world = PathBuf::from("google_drive/world.ecwld");
        let world_path = env_path("PWE_PREVIEW_WORLD").unwrap_or(default_world);
        if !world_path.is_file() {
            crate::pck::corpus_absent("`world.ecwld`");
            return;
        }
        let default_out = PathBuf::from("target/effect-preview");
        let out = env_path("PWE_PREVIEW_OUT").unwrap_or(default_out);
        // A test binary never runs `main`, so nothing has read the settings file:
        // `PWE_LOG` is the only way to turn a channel on here, and the channel
        // files go next to the frames so the run that prints them keeps them.
        crate::diagnostics::set_dir(&out);
        crate::diagnostics::apply_env();
        let size = env_u32("PWE_PREVIEW_SIZE", 384);
        let limit = env_u32("PWE_PREVIEW_MAX", 8) as usize;
        // The map's blocks are walked in file order, so a per-placement limit
        // alone spends every shot on whichever root happens to come first: the
        // first run with three roots rendered forty-nine `树林光束` placements and
        // no waterfall at all. This caps each root on its own, so `--roots a;b;c`
        // with `per_root 3` renders three of each.
        let per_root = env_u32("PWE_PREVIEW_PER_ROOT", limit as u32) as usize;
        let mut taken: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        let fade = env_f32("PWE_PREVIEW_FADE", 0.);
        let mut frames = Vec::new();
        for value in env_list("PWE_PREVIEW_FRAMES", ',', "900") {
            if let Ok(parsed) = value.parse::<f32>() {
                frames.push(parsed);
            }
        }
        let wanted = env_list("PWE_PREVIEW_ROOTS", ';', "");
        let (device, queue) = preview_device().expect("a Vulkan adapter for the previews");
        let scene = crate::scene::Scene::load(&world_path).expect("parse the world file");
        eprintln!(
            "preview: {} blocks, roots {:?}, out {}",
            scene.blocks.len(),
            wanted,
            out.display()
        );
        let rebase = env_u32("PWE_PREVIEW_REBASE", 0) != 0;
        // Zooms and views are what make a PNG readable: one distance cannot show
        // both the shape of a 1,200-unit shaft and the width of its card.
        let mut zooms: Vec<f32> = Vec::new();
        for value in env_list("PWE_PREVIEW_ZOOMS", ',', "1,0.45") {
            if let Ok(parsed) = value.parse::<f32>() {
                if parsed > 0. {
                    zooms.push(parsed);
                }
            }
        }
        if zooms.is_empty() {
            zooms.push(1.);
        }
        let mut angles: Vec<(f32, f32)> = Vec::new();
        for value in env_list("PWE_PREVIEW_ANGLES", ',', "35:20") {
            let mut parts = value.split(':');
            let azimuth = parts.next().and_then(|v| v.trim().parse::<f32>().ok());
            let elevation = parts
                .next()
                .and_then(|v| v.trim().parse::<f32>().ok())
                .unwrap_or(20.);
            if let Some(azimuth) = azimuth {
                angles.push((azimuth, elevation));
            }
        }
        if angles.is_empty() {
            angles.push((35., 20.));
        }
        std::fs::create_dir_all(&out).expect("create the preview directory");
        let mut report = String::from("# Effect preview shots\n\n");
        report.push_str(
            "One line per PNG. `grid` is the ruler's spacing in world units, `m/px` the world units\n\
             one pixel covers at the targeted box, `size` the measured box of the quads that were\n\
             submitted (the map's own units), and `quads`/`pixels` what reached the pass and made\n\
             it to the image.\n\n",
        );
        // `PWE_PREVIEW_CENSUS=1` answers a question no single shot can: is the
        // scale a report is about the map's own number, or an outlier that
        // suggests the record was read wrong? It prints the scale distribution
        // of every placement and the range each wanted root is placed with.
        if env_u32("PWE_PREVIEW_CENSUS", 0) != 0 {
            let mut scales: Vec<f32> = Vec::new();
            for block in &scene.blocks {
                for record in &block.effects {
                    scales.push(record.scale);
                }
            }
            scales.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let total = scales.len();
            let pick = |q: f32| -> f32 {
                if total == 0 {
                    0.
                } else {
                    scales[(((total - 1) as f32) * q) as usize]
                }
            };
            let summary = format!(
                "census: {total} effect records, scale min {:.3} p10 {:.3} median {:.3} p90 {:.3} max {:.3}",
                pick(0.),
                pick(0.1),
                pick(0.5),
                pick(0.9),
                pick(1.)
            );
            eprintln!("{summary}");
            report.push_str(&format!("\n## Placement scales\n\n- {summary}\n"));
            let mut by_root: std::collections::BTreeMap<&str, (usize, f32, f32)> =
                std::collections::BTreeMap::new();
            for block in &scene.blocks {
                for record in &block.effects {
                    let entry = by_root
                        .entry(record.path.as_str())
                        .or_insert((0, f32::MAX, f32::MIN));
                    entry.0 += 1;
                    entry.1 = entry.1.min(record.scale);
                    entry.2 = entry.2.max(record.scale);
                }
            }
            for (path, (count, low, high)) in &by_root {
                if wanted.is_empty() || wanted.iter().any(|key| path.contains(key)) {
                    let line = format!("- census: {count:4} x {path} scale {low:.3}..{high:.3}");
                    eprintln!("{line}");
                    report.push_str(&line);
                    report.push('\n');
                }
            }
        }
        let mut shots = 0usize;
        let mut written = 0usize;
        'placements: for block in &scene.blocks {
            for record in &block.effects {
                if !wanted.is_empty() && !wanted.iter().any(|key| record.path.contains(key)) {
                    continue;
                }
                if shots >= limit {
                    eprintln!("preview: stopping at the {limit} placement limit");
                    break 'placements;
                }
                let seen = taken.entry(record.path.as_str()).or_insert(0);
                if *seen >= per_root {
                    continue;
                }
                *seen += 1;
                shots += 1;
                let centre = if rebase {
                    [0., 0., 0.]
                } else {
                    record.position
                };
                // A wide first shot, so the real frame can be fitted to what the
                // root actually emits instead of to a guess at its size. The
                // probe runs to the largest requested time, so a sheet that
                // grows is framed at its largest state and the earlier frames
                // are not cropped.
                let wide = Shot {
                    width: 128,
                    height: 128,
                    eye: [centre[0] + 60., centre[1] + 30., centre[2] + 60.],
                    target: centre,
                    pixel_scale: 85. * 2. * 22.5f32.to_radians().tan() / 128.,
                };
                let latest = frames.iter().cloned().fold(0f32, f32::max);
                let probe_ticks = (latest / 33.).ceil() as u32 + 24;
                let (mut probe_world, installed) =
                    preview_world(&device, &element, record, fade, rebase);
                let probe = render_shot(
                    &mut probe_world,
                    &device,
                    &queue,
                    wide,
                    &out.join("probe.png"),
                    probe_ticks,
                );
                let fitted = match probe {
                    Ok(stats) if stats.quads > 0 => (stats.centre(), stats.radius()),
                    _ => {
                        let textures = probe_world.effect_texture_failures();
                        eprintln!(
                            "preview: {} at {:?} emitted no quads (installed {installed}, textures failed {})",
                            record.path,
                            record.position,
                            textures.len()
                        );
                        continue;
                    }
                };
                for (azimuth, elevation) in &angles {
                    for zoom in &zooms {
                        let shot = framing_shot_at(
                            fitted.0, fitted.1, size, *azimuth, *elevation, *zoom,
                        );
                        for time in &frames {
                            let name = format!(
                                "{:02}-{:05}-{:04}ms-az{:.0}el{:.0}-z{:.2}.png",
                                shots,
                                record.export_id,
                                *time as i64,
                                *azimuth,
                                *elevation,
                                *zoom
                            );
                            let path = out.join(&name);
                            // One world per time: the clock then reads as the
                            // milliseconds since the placement started.
                            let (mut world, _) =
                                preview_world(&device, &element, record, fade, rebase);
                            let ticks = (*time / 33.).ceil() as u32 + 24;
                            match render_shot(&mut world, &device, &queue, shot, &path, ticks) {
                                Ok(stats) => {
                                    // A shot with quads but no pixels is a map
                                    // the reader refused, not an empty root.
                                    let textures = world.effect_texture_failures();
                                    let note = match textures.first() {
                                        Some(first) => format!(
                                            " textures-failed={} first={first}",
                                            textures.len()
                                        ),
                                        None => String::new(),
                                    };
                                    let line = format!(
                                        "- `{name}` — {} at ({:.1}, {:.1}, {:.1}) dir ({:.2}, {:.2}, {:.2}) up ({:.2}, {:.2}, {:.2}) night {} scale {:.2} alpha {:.2}, asked {} ms (shot {}), view az {:.0}/el {:.0} zoom {:.2}, grid {:.2} u, {:.3} m/px: {}{note}",
                                        record.path,
                                        record.position[0],
                                        record.position[1],
                                        record.position[2],
                                        record.direction[0],
                                        record.direction[1],
                                        record.direction[2],
                                        record.up[0],
                                        record.up[1],
                                        record.up[2],
                                        record.valid_time,
                                        record.scale,
                                        record.alpha,
                                        stats.clock,
                                        *time as i64,
                                        *azimuth,
                                        *elevation,
                                        *zoom,
                                        stats.spacing,
                                        shot.pixel_scale,
                                        stats.describe()
                                    );
                                    report.push_str(&line);
                                    report.push('\n');
                                    eprintln!("preview: {line}");
                                    // Only the fitted view dumps cards: the same
                                    // quads from every zoom would be a wall of
                                    // identical lines.
                                    if env_u32("PWE_PREVIEW_QUADS", 0) != 0 && *zoom == zooms[0] {
                                        for quad in dump_quads(world.effect_quads(), 24) {
                                            report.push_str(&quad);
                                            report.push('\n');
                                            eprintln!("preview: {quad}");
                                        }
                                    }
                                    written += 1;
                                }
                                Err(error) => {
                                    let line = format!(
                                        "- `{name}` — {} at {} ms: {error}",
                                        record.path, *time as i64
                                    );
                                    report.push_str(&line);
                                    report.push('\n');
                                    eprintln!("preview: {line}");
                                }
                            }
                        }
                    }
                }
            }
        }
        let report_path = out.join("metrics.md");
        std::fs::write(&report_path, &report).expect("write the shot report");
        eprintln!(
            "preview: {written} shots from {shots} placements, report {}",
            report_path.display()
        );
    }
}
