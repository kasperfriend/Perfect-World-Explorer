//! GPU regression for depth ordering of overlapping water planes.
#![allow(dead_code)]
#[path = "../water_gpu.rs"]
mod water_gpu;
mod gpu {
    use bytemuck::{Pod, Zeroable};
    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    pub struct Vertex {
        pub pos: [f32; 3],
        pub normal: [f32; 3],
        pub uv: [f32; 2],
        pub color: [f32; 4],
        pub kind: f32,
        pub diffuse: u32,
        pub night_diffuse: u32,
    }
    pub fn depth(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }
}
use wgpu::util::DeviceExt;
fn main() {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    println!("GPU {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        },
        None,
    ))
    .unwrap();
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ] {
        let normal = image::RgbaImage::from_pixel(4, 4, image::Rgba([128, 255, 128, 255]));
        let mip1 = image::RgbaImage::from_pixel(2, 2, image::Rgba([128, 255, 128, 255]));
        let mip2 = image::RgbaImage::from_pixel(1, 1, image::Rgba([128, 255, 128, 255]));
        let frames = vec![vec![normal, mip1, mip2]; 29];
        let water = water_gpu::WaterGpu::new(&device, &queue, format, 64, 64, &frames);
        water.update(
            &queue,
            &water_gpu::WaterUniform {
                matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
                inverse_matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
                eye_time: [0., 2., 1., 0.],
                size: [64., 64., 100., 1.],
                animation: [0., 100., 0., 0.],
                edge_mode: [0.; 4],
                fog: [0., 1., 0., 0.],
                fog_color: [1., 1., 1., 1.],
            },
        );
        let v = |x, y, z| gpu::Vertex {
            pos: [x, y, z],
            normal: [0., 1., 0.],
            uv: [0., 0.],
            color: [1.; 4],
            kind: 1.,
            diffuse: 0,
            night_diffuse: 0xffff_ffff,
        };
        let verts = [
            v(-1., -1., 0.3),
            v(1., -1., 0.3),
            v(-1., 1., 0.3),
            v(1., 1., 0.3),
            v(-1., -1., 0.7),
            v(1., -1., 0.7),
            v(-1., 1., 0.7),
            v(1., 1., 0.7),
        ];
        let ix = [0u32, 1, 2, 1, 3, 2, 4, 5, 6, 5, 7, 6];
        let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&ix),
            usage: wgpu::BufferUsages::INDEX,
        });
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let depth = gpu::depth(&device, 64, 64);
        let dv = depth.create_view(&Default::default());
        let mut outputs = Vec::new();
        for (case, (near_first, eye_y, shallow, clipped)) in [
            (true, 2., false, false),
            (false, 2., false, false),
            (false, 200., false, false),
            (false, 2., true, false),
            (false, 2., true, true),
            (false, -2., false, true),
            (false, -0.05, false, true),
            (false, 2., false, false), // full atmospheric fog
            (false, 2., false, false), // halfway
            (false, 2., false, false), // before fog start
            (false, 2., false, false), // distant, fog disabled
            (false, -2., false, true), // below-surface shader, full fog
            (false, 2., false, false), // reflection disabled, full fog
        ]
        .into_iter()
        .enumerate()
        {
            let distance = match case {
                7 | 10 | 11 | 12 => 8.,
                8 => 4.,
                _ => 1.,
            };
            let fog_on = case >= 7 && case != 10;
            water.update(
                &queue,
                &water_gpu::WaterUniform {
                    matrix: (glam::Mat4::IDENTITY * distance).to_cols_array_2d(),
                    inverse_matrix: glam::Mat4::from_translation(glam::Vec3::new(
                        0.,
                        if shallow { -0.33 } else { 0. },
                        0.,
                    ))
                    .to_cols_array_2d(),
                    eye_time: [0., eye_y, 1., 0.],
                    size: [64., 64., 100., if case == 12 { 0. } else { 1. }],
                    animation: [0., 100., 0., 0.],
                    edge_mode: [0.; 4],
                    fog: [2., 6., if fog_on { 1. } else { 0. }, 0.],
                    fog_color: [0.2, 0.4, 0.6, 1.],
                },
            );
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 64 * 64 * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let color_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 64 * 64 * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut e = device.create_command_encoder(&Default::default());
            for (color, clr) in [
                (&view, wgpu::Color::BLACK),
                (
                    &water.scene(),
                    wgpu::Color {
                        r: 0.1,
                        g: 0.3,
                        b: 0.5,
                        a: 1.,
                    },
                ),
                (
                    &water.refraction(),
                    wgpu::Color {
                        r: if clipped { 0.2 } else { 0.1 },
                        g: if clipped { 0.4 } else { 0.3 },
                        b: if clipped { 0.6 } else { 0.5 },
                        a: 1.,
                    },
                ),
                (
                    &water.reflection(),
                    wgpu::Color {
                        r: 0.3,
                        g: 0.7,
                        b: 0.9,
                        a: 1.,
                    },
                ),
            ] {
                let _p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: color,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(clr),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
            }
            {
                let _p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &dv,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(if shallow { 0.1 } else { 0. }),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
            }
            water.copy_terrain_depth(&mut e, &depth);
            if shallow {
                // Simulate nearer scenery after the terrain-only depth snapshot.
                // The shore edge must still use the terrain, not this depth.
                let _pass = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("synthetic scenery depth"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &dv,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.2),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
            }
            water.copy_depth(&mut e, &depth);
            let draws = if near_first {
                [(0, 6), (6, 12)]
            } else {
                [(6, 12), (0, 6)]
            };
            for (a, b) in draws {
                water.draw_water(&mut e, &view, &dv, &vb, &ib, a, b);
            }
            e.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &depth,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::DepthOnly,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(64),
                    },
                },
                wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
            );
            e.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &target,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &color_buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(64),
                    },
                },
                wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(e.finish()));
            let slice = buffer.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().unwrap();
            let bytes = slice.get_mapped_range();
            let at = (32 * 64 + 32) * 4;
            outputs.push(f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()));
            drop(bytes);
            buffer.unmap();
            let cs = color_buffer.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            cs.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().unwrap();
            let c = cs.get_mapped_range();
            let encode = |x: f32| {
                if format.is_srgb() {
                    if x <= 0.0031308 {
                        12.92 * x
                    } else {
                        1.055 * x.powf(1. / 2.4) - 0.055
                    }
                } else {
                    x
                }
            };
            // The fifth-power angular term is negligible at the center;
            // exercise both limits of the recovered reflectance range.
            let f0 = if eye_y > 100. { 0.23 } else { 0.11 };
            for (k, (a, b)) in [(0.1f32, 0.3f32), (0.3, 0.7), (0.5, 0.9)]
                .into_iter()
                .enumerate()
            {
                let aq = (encode(a) * 255.).round() / 255.;
                let bq = (encode(b) * 255.).round() / 255.;
                let rq = if clipped {
                    (encode(a + 0.1) * 255.).round() / 255.
                } else {
                    aq
                };
                let mixed = rq * 0.95 * (1. - f0) + bq * f0;
                let edge = if shallow { 0.5 } else { 1. };
                let expected = if eye_y < 0. {
                    // waterrefract.txt uses eye-minus-position directly in
                    // texcrd, dot with the negated decoded normal, a fifth
                    // power, and the authored (0,.3,.3) tint.
                    let world_x = 1. / 64.;
                    let world_y = -1. / 64.;
                    let world_z = 0.7;
                    let n = 1. / 255.;
                    let facing =
                        ((0. - world_x) * -n + (eye_y - world_y) * -1. + (1. - world_z) * -n)
                            .clamp(0., 1.);
                    let fresnel = (1f32 - facing).powi(5);
                    let tint = [0., 0.3, 0.3][k];
                    (255. * rq * 0.85 * (1. - fresnel + fresnel * tint)).round() as i32
                } else {
                    (255. * (aq * (1. - edge) + mixed * edge)).round() as i32
                };
                let f = if fog_on {
                    ((distance - 2.) / 4f32).clamp(0., 1.)
                } else {
                    0.
                };
                let expected =
                    (expected as f32 * (1. - f) + [51., 102., 153.][k] * f).round() as i32;
                let observed = c[at + k] as i32;
                assert!(
                    (observed - expected).abs() <= 2,
                    "{format:?} case {case} channel {k}: {observed} vs {expected}"
                );
            }
            drop(c);
            color_buffer.unmap();
        }
        assert!(
            outputs.iter().all(|z| (*z - 0.7).abs() < 1e-5),
            "nearest water plane did not win: {outputs:?}"
        );
        // Real alpha-only edge pass: preserve color, produce the client
        // height/depth ramp and respect nearer scene depth.
        for cutoff in [false, true] {
            for occluded in [false, true] {
                let mut e = device.create_command_encoder(&Default::default());
                {
                    let edge_target = water.refraction();
                    let _p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("edge test clear"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &edge_target,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: 0.2,
                                    g: 0.4,
                                    b: 0.6,
                                    a: 1.,
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &dv,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(if occluded { 0.8 } else { 0. }),
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                }
                queue.submit(Some(e.finish()));
                let mut edge_encoder = device.create_command_encoder(&Default::default());
                water.render_edge(
                    &mut edge_encoder,
                    &queue,
                    glam::Mat4::IDENTITY,
                    0.33,
                    [5., 6., 5., 6.],
                    &dv,
                    if cutoff { None } else { Some((&vb, &ib, 0)) },
                    (&vb, &ib, 6),
                );
                queue.submit(Some(edge_encoder.finish()));
                let read = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 64 * 64 * 4,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut e = device.create_command_encoder(&Default::default());
                e.copy_texture_to_buffer(
                    wgpu::ImageCopyTexture {
                        texture: water.refraction_texture(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyBuffer {
                        buffer: &read,
                        layout: wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(256),
                            rows_per_image: Some(64),
                        },
                    },
                    wgpu::Extent3d {
                        width: 64,
                        height: 64,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit(Some(e.finish()));
                let slice = read.slice(..);
                let (tx, rx) = std::sync::mpsc::channel();
                slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
                device.poll(wgpu::Maintain::Wait);
                rx.recv().unwrap().unwrap();
                let mapped = slice.get_mapped_range();
                let at = (32 * 64 + 32) * 4;
                let pixel = &mapped[at..at + 4];
                let alpha = if occluded {
                    255
                } else {
                    ((0.33f32 + 1. / 64.) / 0.66 * 255.).round() as i32
                };
                assert!(
                    (pixel[3] as i32 - alpha).abs() <= 2,
                    "{format:?} edge {occluded} pixel={pixel:?}, alpha={alpha}"
                );
                let encode = |x: f32| {
                    if format.is_srgb() {
                        1.055 * x.powf(1. / 2.4) - 0.055
                    } else {
                        x
                    }
                };
                for (i, x) in [0.2, 0.4, 0.6].into_iter().enumerate() {
                    assert!(
                        (pixel[i] as i32 - (encode(x) * 255.).round() as i32).abs() <= 1,
                        "edge RGB modified: {pixel:?}"
                    );
                }
                drop(mapped);
                read.unmap();
            }
        }
        println!("Water {format:?}: near/far draw orders -> {outputs:?}; encoded blend and shallow-edge matched");
    }
}
