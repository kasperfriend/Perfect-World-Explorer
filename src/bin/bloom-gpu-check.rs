//! Production glow passes against an independent CPU reference on patterned,
//! non-quarter-divisible images (constant-color checks cannot test blur UVs).
#![allow(dead_code)]
mod environment {
    #[derive(Clone, Copy)]
    pub struct Fog {
        pub start: f32,
        pub end: f32,
        pub density: f32,
        pub color: [f32; 3],
    }
}
#[path = "../post_gpu.rs"]
mod post_gpu;
#[path = "../post_reference.rs"]
mod post_reference;

fn main() {
    // Surface fixture failures through the Checks API as well as runner logs.
    // This preserves diagnostics when log downloads use an unavailable host.
    std::panic::set_hook(Box::new(|info| {
        eprintln!(
            "::error::{}",
            info.to_string()
                .replace('%', "%25")
                .replace('\r', "%0D")
                .replace('\n', "%0A")
        );
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
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("encoded patterned glow fixture"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
@vertex fn vs(@builtin(vertex_index) id:u32)->@builtin(position) vec4<f32> {
 let x=f32((id<<1u)&2u); let y=f32(id&2u);
 return vec4<f32>(x*2.-1.,1.-y*2.,0.,1.);
}
@fragment fn fs(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32> {
 let x=u32(p.x);let y=u32(p.y);
 let rgb=vec3<u32>((17u*x+29u*y)%256u,(71u*x+13u*y+31u)%256u,(3u*x+113u*y+67u)%256u);
 return vec4<f32>(vec3<f32>(rgb)/255.,1.);
}
"#
            .into(),
        ),
    });
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Bgra8UnormSrgb,
    ] {
        let fixture_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: format.remove_srgb_suffix(),
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
        });
        for (w, h) in [(64u32, 64u32), (23, 19), (67, 61)] {
            let size = wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            };
            let mut post = post_gpu::PostGpu::new(&device, format, 1, 1);
            post.resize(&device, w, h);
            let source = post.scene_raw_view();
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let depth = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let target_view = target.create_view(&Default::default());
            let depth_view = depth.create_view(&Default::default());
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &source,
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
                pass.set_pipeline(&fixture_pipeline);
                pass.draw(0..3, 0..1);
            }
            queue.submit(Some(encoder.finish()));
            let image = post_reference::Image::pattern(w as usize, h as usize);
            let mut cases = vec![
                (0.25, 0.25, [0.; 4], false, 0.),
                (
                    0.5,
                    0.6,
                    [187. / 255., 234. / 255., 187. / 255., 0.5],
                    false,
                    0.,
                ),
                (0.25, 0.25, [0.; 4], true, 0.37),
                (
                    0.5,
                    0.6,
                    [187. / 255., 234. / 255., 187. / 255., 0.5],
                    true,
                    1.2,
                ),
                (1., 0.7, [226. / 255., 1., 1., 1.], false, 0.),
                (0.1, 0.25, [0.; 4], false, 0.),
            ];
            // Production environment -> presenter transitions -> shader pixels.
            // Independently recovered native c0.r/c0.g expectations catch the
            // old level/power swap (static shader tests alone could not).
            let mut state = post_gpu::WorldGlow::default();
            state.select(true, 1., false, 0, 0xffffffff);
            state.tick(5000);
            state.select(true, 1., true, 0, 0xffffffff);
            state.tick(1000);
            let p = state.parameters();
            assert_eq!(&p[..2], &[0.425, 0.375]);
            assert_eq!(p[5], 1. / 3.);
            cases.push((p[0], p[1], [p[2], p[3], p[4], p[5]], true, 0.7));
            state.tick(1000);
            let p = state.parameters();
            assert_eq!(&p[..2], &[0.6, 0.5]);
            cases.push((p[0], p[1], [p[2], p[3], p[4], p[5]], true, 1.7));
            state.select(true, 1., false, 4, 0xffffffff);
            state.tick(2000);
            let p = state.parameters();
            assert_eq!(&p[..2], &[0.7, 1.]);
            cases.push((p[0], p[1], [p[2], p[3], p[4], p[5]], false, 0.));
            state.select(true, 1., false, 5, 0xffffffff);
            state.tick(1000);
            let p = state.parameters();
            assert_eq!(&p[..2], &[0.25, 0.1]);
            cases.push((p[0], p[1], [p[2], p[3], p[4], p[5]], false, 0.));
            state.set_fade(0., 1., 1000);
            state.tick(250);
            let runtime_fade = state.brightness_fade();
            assert_eq!(runtime_fade, 0.25);
            for (level, power, mono, underwater, time) in cases {
                for (argb, brightness_fade, opacity_fade, modulation) in [
                    (0xffffffff, 0., 0., [1.; 4]),
                    (0xffffffff, runtime_fade, 0., [0.75, 0.75, 0.75, 1.]),
                    (
                        0xff40a0e0,
                        0.,
                        0.,
                        [64. / 255., 160. / 255., 224. / 255., 1.],
                    ),
                    (
                        0x8040a0e0,
                        0.25,
                        0.5,
                        [48. / 255., 120. / 255., 168. / 255., 64. / 255.],
                    ),
                    (
                        0x0040a0e0,
                        0.,
                        0.,
                        [64. / 255., 160. / 255., 224. / 255., 0.],
                    ),
                ] {
                    for (sharpen, bloom, gamma) in [
                        (false, true, false),
                        (true, true, false),
                        (true, false, false),
                        (true, false, true),
                    ] {
                        // Exact unwarped pattern isolates sharpen/display-LUT
                        // order without amplifying a warp interpolation error.
                        if gamma && underwater {
                            continue;
                        }
                        post.set_sharpen(sharpen);
                        post.set_glow_color(argb, brightness_fade, opacity_fade);
                        post.render(
                            &device,
                            &queue,
                            &target_view,
                            &depth_view,
                            false,
                            w,
                            h,
                            time,
                            100.,
                            if underwater { 1. } else { 0. },
                            environment::Fog {
                                start: 0.,
                                end: 100.,
                                density: 0.,
                                color: [0.; 3],
                            },
                            false,
                            false,
                            bloom,
                            1.,
                            [level, power, mono[0], mono[1], mono[2], mono[3]],
                            gamma,
                            67.,
                        );
                        let row = (w * 4 + 255) / 256 * 256;
                        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                            label: None,
                            size: row as u64 * h as u64,
                            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                        let mut encoder = device.create_command_encoder(&Default::default());
                        encoder.copy_texture_to_buffer(
                            wgpu::ImageCopyTexture {
                                texture: &target,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::ImageCopyBuffer {
                                buffer: &buffer,
                                layout: wgpu::ImageDataLayout {
                                    offset: 0,
                                    bytes_per_row: Some(row),
                                    rows_per_image: Some(h),
                                },
                            },
                            size,
                        );
                        queue.submit(Some(encoder.finish()));
                        let slice = buffer.slice(..);
                        let (tx, rx) = std::sync::mpsc::channel();
                        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
                        device.poll(wgpu::Maintain::Wait);
                        rx.recv().unwrap().unwrap();
                        let pixels = slice.get_mapped_range();
                        let input = if underwater {
                            image.underwater_warp(time)
                        } else {
                            image.clone()
                        };
                        let reference = if bloom {
                            input.bloom_modulated(level, power, mono, modulation)
                        } else {
                            input
                        };
                        let reference = if sharpen {
                            reference.sharpen()
                        } else {
                            reference
                        };
                        let reference = if gamma {
                            reference.display_gamma(67.)
                        } else {
                            reference
                        };
                        let mut worst = 0;
                        let mut worst_pixel = (0, 0, 0, 0, 0);
                        for y in 0..h as usize {
                            for x in 0..w as usize {
                                for c in 0..3 {
                                    let memory_channel = if matches!(
                                        format,
                                        wgpu::TextureFormat::Bgra8Unorm
                                            | wgpu::TextureFormat::Bgra8UnormSrgb
                                    ) {
                                        2 - c
                                    } else {
                                        c
                                    };
                                    let actual = pixels[y * row as usize + x * 4 + memory_channel];
                                    let expected = reference.pixels[y * w as usize + x][c];
                                    let error = (actual as i32 - expected as i32).abs();
                                    if error > worst {
                                        worst = error;
                                        worst_pixel = (x, y, c, actual, expected);
                                    }
                                }
                            }
                        }
                        // Sharpen is 2*C-N, so prior per-pixel error E propagates
                        // to at most 3*E. Ordinary tests retain their old bounds.
                        let input_tolerance = if bloom {
                            post_reference::comparison_tolerance(level, power, mono)
                        } else {
                            1
                        };
                        let tolerance = if sharpen {
                            3 * input_tolerance
                        } else {
                            input_tolerance
                        };
                        assert!(
                    worst <= tolerance,
                    "{format:?} {w}x{h}, level {level}, power {power}, mono {}, underwater {underwater}, time {time}, sharpen {sharpen}, bloom {bloom}, gamma {gamma}, ARGB {argb:08x}, fades {brightness_fade}/{opacity_fade}: max byte error {worst}, tolerance {tolerance}, pixel {worst_pixel:?}",
                    mono[3]
                );
                        println!(
                    "Glow UV/reference check: {format:?} {w}x{h}, mono {}, max byte error {worst}",
                    mono[3]
                );
                        drop(pixels);
                        buffer.unmap();
                    }
                }
            }
        }
    }
}
