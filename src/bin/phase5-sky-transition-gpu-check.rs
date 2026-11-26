//! Phase 5 sky transition, reflection and altitude regression (P5.07/P5.08).
//!
//! Four independent checks, all on the production code paths:
//!
//! 1. the real `sky_gpu::SHADER` compiles and renders, with the transition lane
//!    exercised at a non-trivial weight;
//! 2. the crossfade between two *different* texture sets is a real blend, not a
//!    no-op: the same sky is rendered with weight 0 and weight 1 and the two
//!    results must differ;
//! 3. altitude cannot move the sky: the identical frame is uploaded for a camera
//!    at sea level and at 4000 units and both images must be bit-identical;
//! 4. the reflected upload is the same snapshot mirrored, and the transition
//!    machine never advances from a secondary view.
//!
//! Images are synthetic gradients, so this is state and shader coverage, not a
//! claim about client screenshots or authored sky art.
#![allow(dead_code)]
#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[path = "../pck.rs"]
mod pck;
#[path = "../scenery_visibility.rs"]
mod scenery_visibility;
#[path = "../sky_config.rs"]
mod sky_config;
#[path = "../sky_gpu.rs"]
mod sky_gpu;
#[path = "../sky_transition.rs"]
mod sky_transition;

use glam::{Mat4, Vec3};
use scenery_visibility::Pass;
use sky_transition::{Machine, Request, Triple};

const W: u32 = 64;
const H: u32 = 64;
// 9 set layers (3 sets x 3 textures), 2 discs (9, 10), 2 shine quads
// (11 sun, 12 moon) and the 2 flare sprites (13 sunflare, 14 flares): the
// indices the production shader samples by name.
const LAYERS: u32 = 15;

/// Deterministic gradient per texture layer, so two sets are visibly different
/// while staying byte-stable across machines.
fn layer_pixels(layer: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let u = x as f32 / (W - 1) as f32;
            let v = y as f32 / (H - 1) as f32;
            let base = (layer as f32 + 1.) / (LAYERS as f32 + 1.);
            px.extend_from_slice(&[
                ((base * 200. + u * 55.) as u8).min(255),
                ((base * 120. + v * 120.) as u8).min(255),
                ((base * 60. + (u * v) * 180.) as u8).min(255),
                255,
            ]);
        }
    }
    px
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    read: wgpu::Buffer,
}

fn target(device: &wgpu::Device) -> Target {
    let size = wgpu::Extent3d {
        width: W,
        height: H,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("phase5 sky target"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("phase5 sky readback"),
        size: (W * H * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    Target {
        texture,
        view,
        read,
    }
}

fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut encoder: wgpu::CommandEncoder,
    t: &Target,
) -> Vec<u8> {
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &t.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &t.read,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let slice = t.read.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let bytes = slice.get_mapped_range().to_vec();
    t.read.unmap();
    bytes
}

/// One frame through the production shader. `params` is the exact 208-byte
/// `Params` uniform: `inv`, `time`, `celestial`, `celestial2`, `background`,
/// `tint`, `sets`, `glow_center`, `moon_glow`, `skyrot`.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::RenderPipeline,
    layout: &wgpu::BindGroupLayout,
    sky: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    roof_sampler: &wgpu::Sampler,
    t: &Target,
    params: &[f32; 52],
) -> Vec<u8> {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("phase5 sky params"),
        size: std::mem::size_of_val(params) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buf, 0, bytemuck::cast_slice(params));
    // The binding layout still carries its legacy depth slots; use inert 1x1
    // resources because the production sky shader no longer samples them.
    let depth_dummy = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("phase5 sky depth dummy (render)"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let depth_view = depth_dummy.create_view(&Default::default());
    let depth_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("phase5 sky depth sampler (render)"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("phase5 sky bind"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(sky),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(roof_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&depth_view),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(&depth_sampler),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("phase5 sky"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &t.view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
    read_back(device, queue, encoder, t)
}

fn main() {
    std::panic::set_hook(Box::new(|p| {
        eprintln!("::error::{}", p.to_string().replace('\n', "%0A"))
    }));
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .unwrap();

    // (1) The production shader, unchanged, must compile on this adapter.
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("production sky shader"),
        source: wgpu::ShaderSource::Wgsl(sky_gpu::SHADER.into()),
    });
    let sky = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("phase5 sky array"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: LAYERS,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    for layer in 0..LAYERS {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &sky,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &layer_pixels(layer),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
            wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
        );
    }
    let sky_view = sky.create_view(&wgpu::TextureViewDescriptor {
        label: Some("phase5 sky array view"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("phase5 sky sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    // The production shader's second sampler: the roof sheet is sampled past
    // one tile, so it wraps instead of clamping.
    let roof_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("phase5 sky roof sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("phase5 sky layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("phase5 sky pipeline layout"),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("phase5 sky"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: "vs",
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: "fs",
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview: None,
    });

    // The production uniform layout: inv(16) time(4) celestial(4) celestial2(4)
    // background(4) tint(4) sets(4) glow_center(4) skyrot(4) - `skyrot.xy` is the
    // stage-0 sky texture-matrix translation and `skyrot.w` its diagonal scale,
    // both from `A3DSky`'s descriptor (`0x7995e0` scale 8, `0x42a98d` rates).
    // The translation stays at zero here so the bands this probe measures are
    // not slid, but the scale is the client's own 8 because the ceiling UVs run
    // through that matrix in the shader.
    //
    // `eye` only reaches `look_to_rh`, exactly like `SkyGpu::update`, and is then
    // dropped by clearing the translation column. The native sphere compares the
    // eye against a cached tuple (0x79acbd..0x79acd4) but never lets it into a
    // ray direction, so any altitude difference in the output is a real defect.
    let sky_inverse = |eye: Vec3, reflected: bool| {
        let view = Mat4::look_to_rh(eye, Vec3::new(0.2, 0.1, -1.).normalize(), Vec3::Y);
        let mut rotation = view;
        rotation.w_axis = glam::Vec4::W;
        let projection = Mat4::from_scale(Vec3::new(-1., 1., 1.))
            * Mat4::perspective_rh(70f32.to_radians(), W as f32 / H as f32, 1., 16000.);
        let inv = (projection * rotation).inverse();
        if reflected {
            Mat4::from_scale(Vec3::new(1., -1., 1.)) * inv
        } else {
            inv
        }
    };
    let uniform = |inv: Mat4, sets: (f32, f32, f32)| {
        let mut u = [0f32; 52];
        u[..16].copy_from_slice(&inv.to_cols_array());
        u[16] = 19.; // hour
        u[17] = 0.; // night weight: this probe is about sets and altitude
        u[19] = 0.5; // disc brightness: the recovered half colour
        u[20] = 0.3; // celestial direction
        u[21] = 0.4;
        u[22] = -0.5;
        u[23] = 0.; // no discs, and celestial2 stays zero, for the same reason
        u[28] = 0.2; // pass background the open band blends with
        u[29] = 0.3;
        u[30] = 0.4;
        u[31] = 0.143; // the sun's own NDC half extent, as `update` writes it
        u[32] = 1.; // tint: no phase darkening here
        u[33] = 1.;
        u[34] = 1.;
        u[35] = W as f32 / H as f32; // screen aspect for the flare chain
        u[36] = sets.0;
        u[37] = sets.1;
        u[38] = sets.2;
        u[39] = 0.; // night strength: day, so the sun keeps its native size
                    // u[44..48] disable the moon halo in this neutral probe.
                    // u[48..50] are zero drift; u[50] is pixel scale and u[51] the native scale.
        u[50] = 2. / H as f32; // NDC pixel scale for lens-flare sprites
        u[51] = 8.; // descriptor scale
        u
    };

    let t = target(&device);
    let sea_level = sky_inverse(Vec3::new(512., 0., -2048.), false);
    let high_up = sky_inverse(Vec3::new(512., 4000., -2048.), false);
    let reflected = sky_inverse(Vec3::new(512., 0., -2048.), true);

    // (2) The crossfade must actually change pixels at both ends.
    let probe = |inv: Mat4, sets: (f32, f32, f32)| {
        render(
            &device,
            &queue,
            &pipeline,
            &layout,
            &sky_view,
            &sampler,
            &roof_sampler,
            &t,
            &uniform(inv, sets),
        )
    };
    let base = probe(sea_level, (0., 0., 0.));
    let weight_one = probe(sea_level, (0., 2., 1.));
    let differing = base.iter().zip(&weight_one).filter(|(a, b)| a != b).count();
    assert!(
        differing > base.len() / 10,
        "sky set crossfade did not change the frame ({differing} of {} bytes)",
        base.len()
    );

    // (3) Altitude must be invisible.
    let elevated = probe(high_up, (0., 0., 0.));
    assert_eq!(
        base, elevated,
        "sky changed with camera altitude: the view translation leaked into the ray"
    );

    // (4) The reflected frame is the same snapshot mirrored, and the machine
    // never advances from a secondary view.
    let mirrored = probe(reflected, (0., 0., 0.));
    assert_ne!(base, mirrored, "the reflected upload was not mirrored");
    let mut machine = Machine::new(Triple::DAY);
    machine.request(Request::over(Triple::NIGHT, 660)).unwrap();
    machine.resources_ready();
    machine.step();
    let frame = sky_transition::frame_sets(&machine);
    let counter = machine.counter();
    for pass in Pass::ALL {
        assert_eq!(
            sky_transition::view_sets(frame, pass.is_secondary()),
            frame,
            "{pass:?} saw a different sky than the snapshot"
        );
    }
    assert_eq!(machine.counter(), counter, "a view advanced the transition");

    // Keep authored celestial sprites; they are not volumetric shafts.
    let sun_disc = image::RgbaImage::from_fn(W, H, |x, y| {
        image::Rgba([
            0,
            if (24..40).contains(&x) && (24..40).contains(&y) {
                255
            } else {
                0
            },
            0,
            255,
        ])
    });
    let moon_disc = image::RgbaImage::from_fn(W, H, |x, y| {
        image::Rgba([
            0,
            0,
            if (24..40).contains(&x) && (24..40).contains(&y) {
                255
            } else {
                0
            },
            255,
        ])
    });
    let sunshine = image::RgbaImage::from_pixel(W, H, image::Rgba([255, 0, 0, 255]));
    let moon_halo = image::RgbaImage::from_pixel(W, H, image::Rgba([255, 0, 0, 255]));
    let sunflare = image::RgbaImage::from_pixel(W, H, image::Rgba([0, 0, 255, 255]));
    let flare_atlas = image::RgbaImage::from_pixel(W, H, image::Rgba([0, 255, 0, 255]));
    let black = image::RgbaImage::new(W, H);
    for layer in 0..LAYERS {
        let image = match layer {
            9 => &sun_disc,
            10 => &moon_disc,
            11 => &sunshine,
            12 => &moon_halo,
            13 => &sunflare,
            14 => &flare_atlas,
            _ => &black,
        };
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &sky,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            image.as_raw(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
            wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
        );
    }
    let channel_peak =
        |pixels: &[u8], channel: usize| pixels.chunks_exact(4).map(|p| p[channel]).max().unwrap();
    let channel_delta = |after: &[u8], before: &[u8], channel: usize| -> usize {
        after
            .chunks_exact(4)
            .zip(before.chunks_exact(4))
            .map(|(a, b)| a[channel].abs_diff(b[channel]) as usize)
            .sum()
    };
    let el = 0.4f32;
    let az = -0.5f32;
    let sun = Vec3::new(az.cos() * el.cos(), el.sin(), az.sin() * el.cos());
    for mirrored in [false, true] {
        for pitch in [-0.025f32, 0., 0.025] {
            let direction = (sun + Vec3::Y * pitch).normalize();
            let direction = if mirrored {
                Vec3::new(direction.x, -direction.y, direction.z)
            } else {
                direction
            };
            let view = Mat4::look_to_rh(Vec3::ZERO, direction, Vec3::Y);
            let projection = Mat4::from_scale(Vec3::new(-1., 1., 1.))
                * Mat4::perspective_rh(60f32.to_radians(), 1., 1., 16000.);
            let inv = (projection * view).inverse();
            let inv = if mirrored {
                Mat4::from_scale(Vec3::new(1., -1., 1.)) * inv
            } else {
                inv
            };

            // Isolate the recovered 120x40 sunshine strip before enabling lens flares.
            let mut sun_params = uniform(inv, (0., 0., 0.));
            sun_params[20..24].copy_from_slice(&[1., el, az, 0.]);
            sun_params[24..28].fill(0.);
            sun_params[39] = 0.45; // dusk shine ramp
            sun_params[40..44].copy_from_slice(&[0.5, 0.5, 0., 0.]);
            sun_params[44..48].copy_from_slice(&[0., 0., 1., 0.]);
            let shine_pixels = render(
                &device,
                &queue,
                &pipeline,
                &layout,
                &sky_view,
                &sampler,
                &roof_sampler,
                &t,
                &sun_params,
            );
            assert!(channel_peak(&shine_pixels, 1) > 0, "missing sun disc");
            assert!(
                channel_peak(&shine_pixels, 0) > 0,
                "sunshine.bmp strip missing: mirrored={mirrored}, pitch={pitch}"
            );

            // sunflare.bmp and the flares.bmp atlas each have a unique channel.
            sun_params[23] = 1.;
            let sun_flare_pixels = render(
                &device,
                &queue,
                &pipeline,
                &layout,
                &sky_view,
                &sampler,
                &roof_sampler,
                &t,
                &sun_params,
            );
            assert!(
                channel_delta(&sun_flare_pixels, &shine_pixels, 2) > 0,
                "sunflare.bmp sprite missing: mirrored={mirrored}, pitch={pitch}"
            );
            assert!(
                channel_delta(&sun_flare_pixels, &shine_pixels, 1) > 0,
                "flares.bmp atlas ghosts missing: mirrored={mirrored}, pitch={pitch}"
            );

            // Only the moon halo enable lane differs between these two renders.
            let mut moon_params = sun_params;
            moon_params[20..24].fill(0.);
            moon_params[24..28].copy_from_slice(&[1., el, az, 0.]);
            moon_params[23] = 0.;
            let moon_with_halo = render(
                &device,
                &queue,
                &pipeline,
                &layout,
                &sky_view,
                &sampler,
                &roof_sampler,
                &t,
                &moon_params,
            );
            assert!(channel_peak(&moon_with_halo, 2) > 0, "missing moon disc");
            assert!(
                channel_peak(&moon_with_halo, 0) > 0,
                "moonflare.bmp halo missing: mirrored={mirrored}, pitch={pitch}"
            );
            moon_params[46] = 0.;
            let moon_without_halo = render(
                &device,
                &queue,
                &pipeline,
                &layout,
                &sky_view,
                &sampler,
                &roof_sampler,
                &t,
                &moon_params,
            );
            assert!(
                channel_delta(&moon_with_halo, &moon_without_halo, 0) > 0,
                "moon halo gate had no effect"
            );
        }
    }
    println!("sunshine strip, moon halo, sunflare sprite and flares atlas rendered across normal/reflected views");

    // Empty terrain must reveal the authored fog, not the sky texture's pale
    // bottom row. All pixels face down so this also catches double encoding.
    let mut fog_params = uniform(Mat4::IDENTITY, (0., 0., 0.));
    fog_params[..16].fill(0.);
    fog_params[13] = -1.;
    fog_params[15] = 1.;
    fog_params[18] = 1.;
    let pixels = render(
        &device,
        &queue,
        &pipeline,
        &layout,
        &sky_view,
        &sampler,
        &roof_sampler,
        &t,
        &fog_params,
    );
    let expected = [0.2f32, 0.3, 0.4].map(|c| {
        let linear = if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        };
        (linear * 255.).round() as u8
    });
    assert!(
        pixels
            .chunks_exact(4)
            .all(|p| (0..3).all(|c| (p[c] as i32 - expected[c] as i32).abs() <= 1)),
        "fog skirt is missing or encoded twice"
    );
    fog_params[18] = 0.;
    let no_fog = render(
        &device,
        &queue,
        &pipeline,
        &layout,
        &sky_view,
        &sampler,
        &roof_sampler,
        &t,
        &fog_params,
    );
    assert_ne!(
        no_fog, pixels,
        "fog disable gate did not restore authored sky"
    );
    println!("::notice::fog background GPU test passed: native encoded colour decoded once, no pale panorama below horizon, independent fog gate");
    println!(
        "phase5 sky transition: shader compiled, crossfade moved {differing} bytes, \
         altitude stable, reflection mirrored, transition counter {counter} unchanged over \
         {} views",
        Pass::ALL.len()
    );
}
