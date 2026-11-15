//! End-to-end test of the actual post pipeline and sRGB/UNORM views.
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
fn main() {
    let inst = wgpu::Instance::default();
    let adapter = pollster::block_on(inst.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .unwrap();
    let fmt = wgpu::TextureFormat::Rgba8UnormSrgb;
    let post = post_gpu::PostGpu::new(&device, fmt, 64, 64);
    let scene = post.scene_view();
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
        format: fmt,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let tv = target.create_view(&Default::default());
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let dv = depth.create_view(&Default::default());
    let fog = environment::Fog {
        start: 0.,
        end: 4.,
        density: 0.,
        color: [0.; 3],
    };
    let read = |bloom: bool,
                strength: f32,
                gamma: bool,
                setting: f32,
                atmosphere: bool,
                mono: [f32; 4]| {
        let mut e = device.create_command_encoder(&Default::default());
        {
            let _p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &scene,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.2140411405,
                            g: 0.2140411405,
                            b: 0.2140411405,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.5),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        queue.submit(Some(e.finish()));
        post.render(
            &device,
            &queue,
            &tv,
            &dv,
            false,
            64,
            64,
            0.,
            100.,
            0.,
            fog,
            atmosphere,
            true,
            bloom,
            strength,
            [0.25, 0.25, mono[0], mono[1], mono[2], mono[3]],
            gamma,
            setting,
        );
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 64 * 64 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut e = device.create_command_encoder(&Default::default());
        e.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buf,
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
        let sl = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        sl.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let data = sl.get_mapped_range();
        let start = (32 * 64 + 32) * 4;
        let pixel = [data[start], data[start + 1], data[start + 2]];
        drop(data);
        buf.unmap();
        pixel
    };
    let passthrough = read(false, 0., false, 41., false, [0.; 4])[0];
    let default = read(false, 0., true, 41., false, [0.; 4])[0];
    let dark = read(false, 0., true, 0., false, [0.; 4])[0];
    let bright = read(false, 0., true, 100., false, [0.; 4])[0];
    let glow = read(true, 1., false, 41., false, [0.; 4])[0];
    let fog_only = read(false, 0., false, 41., true, [0.; 4])[0];
    let mono = read(
        true,
        1.,
        false,
        41.,
        false,
        [187. / 255., 234. / 255., 187. / 255., 0.5],
    );
    assert!(
        (mono[0] as i32 - 134).abs() <= 2
            && (mono[1] as i32 - 150).abs() <= 2
            && (mono[2] as i32 - 134).abs() <= 2,
        "mono {mono:?}"
    );
    println!("Underwater mono glow GPU RGB={mono:?}");
    let fog_glow = read(true, 1., false, 41., true, [0.; 4])[0];
    assert!((fog_only as i32 - 65).abs() <= 2, "fog {fog_only}");
    assert!(
        (fog_glow as i32 - 69).abs() <= 2,
        "fog before glow {fog_glow}"
    );
    println!("Fog before bloom: fog {fog_only}, then fog+bloom {fog_glow}");
    assert!((passthrough as i32 - 128).abs() <= 1);
    assert!((default as i32 - passthrough as i32).abs() <= 1);
    assert!((dark as i32 - 25).abs() <= 2);
    assert!((bright as i32 - 181).abs() <= 2);
    assert!((glow as i32 - 143).abs() <= 2, "glow {glow}");
    println!("Post GPU sRGB target: neutral {passthrough}, gamma41 {default}, gamma0 {dark}, gamma100 {bright}, encoded-space glow {glow}");
    // Exercise the actual underwater warp with a 2D gradient instead of a
    // solid image; compare against a CPU bilinear/8-bit bump-map reference.
    let raw = post.scene_raw_view();
    let shader=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:None,source:wgpu::ShaderSource::Wgsl(r#"
struct O {@builtin(position) p:vec4<f32>,@location(0) uv:vec2<f32>};
@vertex fn vs(@builtin(vertex_index) id:u32)->O { var o:O;let x=f32((id<<1u)&2u);let y=f32(id&2u);o.uv=vec2<f32>(x,y);o.p=vec4<f32>(x*2.-1.,1.-y*2.,0.,1.);return o;}
@fragment fn fs(i:O)->@location(0) vec4<f32>{return vec4<f32>(i.uv,0.,1.);}"#.into())});
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                format: fmt.remove_srgb_suffix(),
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview: None,
    });
    let mut e = device.create_command_encoder(&Default::default());
    {
        let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &raw,
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
        p.set_pipeline(&pipeline);
        p.draw(0..3, 0..1);
    }
    queue.submit(Some(e.finish()));
    let time = 0.3f32;
    post.render(
        &device, &queue, &tv, &dv, false, 64, 64, time, 100., 1., fog, false, false, false, 0., [0.; 6],
        false, 41.,
    );
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 64 * 64 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = device.create_command_encoder(&Default::default());
    e.copy_texture_to_buffer(
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
    let sl = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    sl.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let bytes = sl.get_mapped_range();
    let uv = [32.5f32 / 64., 32.5 / 64.];
    let bump_uv = [uv[0] * 3., uv[1] * 3. + time * 0.5];
    let p = [
        bump_uv[0].fract() * 256. - 0.5,
        bump_uv[1].fract() * 256. - 0.5,
    ];
    let base = [p[0].floor(), p[1].floor()];
    let f = [p[0] - base[0], p[1] - base[1]];
    let sample = |x: f32, y: f32| {
        let a = std::f32::consts::TAU * 2. * ((x + y) / 256. - 1.);
        [
            (128. - (-32. * a.cos()).trunc()) / 255.,
            (128. - (-32. * a.sin()).trunc()) / 255.,
        ]
    };
    let a = sample(base[0], base[1]);
    let b = sample(base[0] + 1., base[1]);
    let c = sample(base[0], base[1] + 1.);
    let d = sample(base[0] + 1., base[1] + 1.);
    let at = (32 * 64 + 32) * 4;
    for k in 0..2 {
        let t = (a[k] * (1. - f[0]) + b[k] * f[0]) * (1. - f[1])
            + (c[k] * (1. - f[0]) + d[k] * f[0]) * f[1];
        let offset = (2. * t - 1.) * if k == 0 { -0.005 } else { 0.005 };
        let expected = ((uv[k] + offset) * 255.).round() as i32;
        assert!(
            (bytes[at + k] as i32 - expected).abs() <= 2,
            "warp channel {k}: {} != {expected}",
            bytes[at + k]
        );
    }
    println!(
        "Underwater quantized warp GPU sample {:?}",
        &bytes[at..at + 4]
    );
    drop(bytes);
    buffer.unmap();
}
