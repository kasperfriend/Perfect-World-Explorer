//! Render instanced grass and CPU-expanded reference through the same shader.
#![allow(dead_code)]
#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[path = "../bmd.rs"]
mod bmd;
#[path = "../client_weather.rs"]
mod client_weather;
#[path = "../environment.rs"]
mod environment;
#[path = "../grass_instances.rs"]
mod grass_instances;
#[path = "../indirect_args.rs"]
mod indirect_args;
#[path = "../pck.rs"]
mod pck;
#[path = "../refraction_fog.rs"]
mod refraction_fog;
#[path = "../scene.rs"]
mod scene;
#[path = "../sky_config.rs"]
mod sky_config;
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
}
use wgpu::util::DeviceExt;
fn main() {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    println!("GPU {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            // Ask for the single-call draw path; the frame loop below renders
            // both paths and compares them.
            required_features: adapter.features()
                & (wgpu::Features::MULTI_DRAW_INDIRECT | wgpu::Features::INDIRECT_FIRST_INSTANCE),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        },
        None,
    ))
    .unwrap();
    let code = include_str!("../gpu.rs");
    let shader = code
        .split("const SHADER: &str = r#\"")
        .nth(1)
        .unwrap()
        .split("\"#;")
        .next()
        .unwrap();
    let v = |x, y| gpu::Vertex {
        pos: [x, y, 0.5],
        normal: [0., 1., 0.],
        uv: [0.25, 0.25],
        color: [0., 0., 1., 0.5],
        kind: 3.75,
        diffuse: 1.0f32.to_bits(),
        // `0x653e93` weight lane: the emitter stores the float bits of an
        // authored 0..1 weight and the shader reads them back with
        // `bitcast<f32>`, so this is the per-vertex scale the history applies.
        night_diffuse: (if y > 0. { 1.0f32 } else { 0.0f32 }).to_bits(),
    };
    let vertices = vec![v(-0.3, -0.3), v(0.3, -0.3), v(-0.3, 0.3), v(0.3, 0.3)];
    let inds = vec![0, 1, 2, 1, 3, 2];
    let positions = [([-0.5, 0., 0.], 1., 0.0f32), ([0.5, 0., 0.], 0.7, 0.35f32)];
    let mut builder = grass_instances::Builder::default();
    builder.mesh((1, 0), vertices.clone(), inds.clone());
    for (p, s, yaw) in positions {
        builder.instance_oriented((1, 0), p, s, yaw, 0.);
    }
    let data = builder.finish();
    assert_eq!(data.counts(), (4, 2, 1));
    let mut uni = Vec::<f32>::from(glam::Mat4::IDENTITY.to_cols_array());
    uni.extend_from_slice(&[
        0., 0., -1., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 1., 0., 1., 1., 0.,
        1., 1.,
    ]);
    uni.extend_from_slice(&[0.; 20]);
    uni.extend_from_slice(&[0.; 328]);
    // The camera uniform ends with the mip-bias `lod` lane: no bias here, and a
    // gradient scale of 1 keeps the atlas LOD this check measures.
    uni.extend_from_slice(&[0., 1., 0., 0.]);
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&uni),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[30, 240, 40, 255].repeat(4),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    let tv = texture.create_view(&Default::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
                    view_dimension: wgpu::TextureViewDimension::D2,
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
        ],
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&tv),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    let light_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
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
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let values: [u32; 12] = [
        8.0f32.to_bits(),
        8.0f32.to_bits(),
        1.0f32.to_bits(),
        8.0f32.to_bits(),
        1,
        0,
        0,
        0,
        256,
        16,
        256,
        1,
    ];
    let setting = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&values),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let light_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 2,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut lm = [128u8, 128, 128, 255, 32, 32, 32, 255];
    if let Some(dir) = std::env::var_os("PW_REAL_LIGHTMAP_DIR") {
        let mut imgs = Vec::new();
        for ext in ["t2lm", "t2lm1"] {
            let b =
                std::fs::read(std::path::Path::new(&dir).join(format!("world_1.{ext}"))).unwrap();
            let offset = u32::from_le_bytes(b[24..28].try_into().unwrap()) as usize;
            let end = u32::from_le_bytes(b[28..32].try_into().unwrap()) as usize;
            imgs.push(
                image::load_from_memory_with_format(&b[offset..end], image::ImageFormat::Dds)
                    .unwrap()
                    .to_rgba8(),
            );
        }
        let (x, y) = (0..256u32)
            .flat_map(|y| (0..256u32).map(move |x| (x, y)))
            .max_by_key(|&(x, y)| {
                (imgs[0].get_pixel(x, y).0[1] as i32 - imgs[1].get_pixel(x, y).0[1] as i32).abs()
            })
            .unwrap();
        for k in 0..2 {
            lm[k * 4..k * 4 + 4].copy_from_slice(&imgs[k].get_pixel(x, y).0);
        }
        println!("Sample coordinates {x},{y}");
        println!("Real world_1 lightmap sample: {lm:?}");
    }
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &light_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &lm,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 2,
        },
    );
    let lv = light_texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let availability = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("per-area authored map availability"),
        contents: bytemuck::cast_slice(&[3u32; 4]),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let light_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &light_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: setting.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&lv),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: availability.as_entire_binding(),
            },
        ],
    });
    let mut light_builder = grass_instances::Builder::default();
    light_builder.mesh((1, 0), vertices.clone(), inds.clone());
    for (p, s, yaw) in positions {
        light_builder.instance_oriented((1, 0), p, s, yaw, 0.);
    }
    let light_grass = grass_instances::Gpu::new(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        &layout,
        shader,
        light_builder.finish(),
        Some(&light_layout),
    );
    let mut grass = grass_instances::Gpu::new(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        &layout,
        shader,
        data,
        None,
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(shader.into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    const ATTR: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32,5=>Uint32,7=>Uint32];
    let refpipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: "vs",
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<gpu::Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &ATTR,
            }],
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
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Greater,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
    });
    let mut expanded = Vec::new();
    let mut expanded_ix = Vec::new();
    for (p, s, yaw) in positions {
        let base = expanded.len() as u32;
        for a in &vertices {
            let mut a = *a;
            let (sn, cs) = yaw.sin_cos();
            a.pos = [
                (a.pos[0] * cs + a.pos[2] * sn) * s + p[0],
                a.pos[1] * s + p[1],
                (-a.pos[0] * sn + a.pos[2] * cs) * s + p[2],
            ];
            expanded.push(a);
        }
        expanded_ix.extend(inds.iter().map(|i| i + base));
    }
    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&expanded),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&expanded_ix),
        usage: wgpu::BufferUsages::INDEX,
    });
    let size = wgpu::Extent3d {
        width: 128,
        height: 128,
        depth_or_array_layers: 1,
    };
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let dep = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let dv = dep.create_view(&Default::default());
    let mut clear_frame = Vec::new();
    let device_multi = device
        .features()
        .contains(wgpu::Features::MULTI_DRAW_INDIRECT | wgpu::Features::INDIRECT_FIRST_INSTANCE);
    println!(
        "::notice::grass drawn as {}",
        if device_multi {
            "one multi-draw"
        } else {
            "per-batch draws"
        }
    );
    for (clip, fog) in [(false, false), (true, false), (false, true)] {
        uni[380..384].copy_from_slice(if fog { &[0., 0.5, 1., 0.] } else { &[0.; 4] });
        uni[384..388].copy_from_slice(&[0.5, 0.25, 0.75, 1.]);
        uni[20] = 0.15;
        uni[21] = if clip { 1. } else { 0. };
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        // Instanced through the per-batch path, instanced through the single
        // multi-draw call when the adapter has it, and the CPU-expanded
        // reference: all three have to paint the same picture.
        let mut variants: Vec<(bool, bool)> = vec![(false, false), (true, false)];
        if device_multi {
            variants.insert(1, (false, true));
        }
        let mut frames = Vec::new();
        for (expanded_path, multi_draw) in variants {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 128 * 128 * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut e = device.create_command_encoder(&Default::default());
            {
                let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &dv,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                if expanded_path {
                    p.set_pipeline(&refpipe);
                    p.set_bind_group(0, &bind, &[]);
                    p.set_vertex_buffer(0, vb.slice(..));
                    p.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                    p.draw_indexed(0..expanded_ix.len() as u32, 0, 0..1);
                } else {
                    grass.set_multi_draw(multi_draw);
                    grass.draw(&mut p, &bind, None);
                }
            }
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
                        bytes_per_row: Some(512),
                        rows_per_image: Some(128),
                    },
                },
                size,
            );
            queue.submit(Some(e.finish()));
            let slice = buf.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().unwrap();
            frames.push(slice.get_mapped_range().to_vec());
            buf.unmap();
        }
        let n = frames[0].chunks_exact(4).filter(|p| p[1] > 0).count();
        assert!(n > 100, "missing grass {n}");
        for (variant, frame) in frames.iter().enumerate().skip(1) {
            assert_eq!(
                frame, &frames[0],
                "grass draw paths disagree (clip={clip}, fog={fog}, variant={variant})"
            );
        }
        if !clip && !fog {
            clear_frame = frames[0].clone();
        }
        if fog {
            let pixels = frames[0]
                .chunks_exact(4)
                .zip(clear_frame.chunks_exact(4))
                .filter(|(_, old)| old[1] > 0)
                .filter(|(pixel, _)| {
                    (pixel[0] as i32 - 55).abs() <= 1
                        && (pixel[1] as i32 - 13).abs() <= 1
                        && (pixel[2] as i32 - 133).abs() <= 1
                })
                .count();
            assert_eq!(pixels, n, "per-pass encoded fog color");
            println!(
                "Grass and generic material per-pass fog: {pixels} matched encoded RGB reference"
            );
        }
        println!("clip={clip}, fog={fog}: {n} pixels match CPU-expanded grass");
    }
    // Refraction uses the actual instanced vertex fog path, not table fog.
    // Equal clip W makes the CPU interpolation unambiguous for this fixture.
    let saved_uni = uni.clone();
    let bmd_refraction_vertices: Vec<_> = expanded
        .iter()
        .map(|v| {
            let mut v = *v;
            v.kind = 2.1;
            // `kind < 2.5` makes the vertex stage take `color.w` as the
            // vertical scale, so leaving the shared `color` at its
            // `[0, 0, 1, 0.5]` value silently halved this fixture's geometry
            // and moved it away from the positions the reference expansion
            // below reconstructs. Keep the scale uniform, as the sibling BMD
            // fixtures do, so the pixel the oracle reads is the pixel it
            // predicts.
            v.color = [0., 0., 1., 1.];
            v.diffuse = 0xffffffff;
            v.night_diffuse = 0xffffffff;
            v
        })
        .collect();
    let bmd_refraction_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("BMD refraction fixture"),
        contents: bytemuck::cast_slice(&bmd_refraction_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let mut failures: Vec<String> = Vec::new();
    for (plane, wind, bmd) in [
        (0.0f32, 0.0f32, false),
        (0., 0.2, false),
        (0.2, 0., false),
        (0., 0., true),
        (0.2, 0., true),
    ] {
        uni[20] = plane;
        uni[21] = -1.;
        uni[52..56].copy_from_slice(&[-3., 2., 1., 0.]);
        uni[57] = if wind > 0. { 1. } else { 0. };
        uni[60..64].copy_from_slice(&[0., wind, 0., 0.]);
        uni[380..384].copy_from_slice(&[0., 0.5, 1., 0.]);
        uni[384..388].copy_from_slice(&[0.5, 0.25, 0.75, 1.]);
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 128 * 128 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut e = device.create_command_encoder(&Default::default());
        {
            let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grass refraction vertex fog"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if bmd {
                p.set_pipeline(&refpipe);
                p.set_bind_group(0, &bind, &[]);
                p.set_vertex_buffer(0, bmd_refraction_buffer.slice(..));
                p.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                p.draw_indexed(0..expanded_ix.len() as u32, 0, 0..1);
            } else {
                grass.draw(&mut p, &bind, None);
            }
        }
        e.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &read,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(512),
                    rows_per_image: Some(128),
                },
            },
            size,
        );
        queue.submit(Some(e.finish()));
        let slice = read.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let bytes = slice.get_mapped_range();
        let (px, py) = (32usize, 73usize);
        let x = (px as f32 + 0.5) / 64. - 1.;
        let y = 1. - (py as f32 + 0.5) / 64.;
        let u = (x + 0.8) / 0.6;
        let v = (y + 0.3) / (0.6 + wind);
        assert!(u > 0. && v > 0. && u + v < 1.);
        // Do not re-derive the formula here: call the shipped evaluator, so a
        // change to the retail sequence (for example the base of the
        // exponential, which is `e` and not `2`) cannot leave this oracle
        // behind.
        let mut factor = 0.;
        for (pos, displaced_y, w) in [
            ([-0.8f32, -0.3, 0.5], -0.3, 1. - u - v),
            ([-0.2, -0.3, 0.5], -0.3, u),
            ([-0.8, 0.3, 0.5], 0.3 + wind, v),
        ] {
            // The fixture eye, exactly as the original reference expansion
            // reads it off the pixel probe: (-3, 2, 1).
            let eye = [-3., 2., 1.];
            let displaced = [pos[0], displaced_y, pos[2]];
            // `refpipe` draws the raw scene shader, and its vertex stage
            // routes `kind` 1.5..2.5 through `bmd_refraction_transmittance`:
            // above-plane vertices get zero submerged depth (the clamp), while
            // the grass fixture keeps the original Y unclamped.
            factor += if bmd {
                refraction_fog::bmd_transmittance(pos, eye, plane) * w
            } else {
                refraction_fog::grass_transmittance(pos, displaced, eye, plane) * w
            };
        }
        factor = factor.clamp(0., 1.);
        let pixel = &bytes[(py * 128 + px) * 4..][..3];
        // Report every row and channel before failing. Panicking on the first
        // mismatch turns each CI round-trip into a single data point, which
        // makes a systematic error indistinguishable from a local one.
        let mut report = Vec::new();
        for (channel, (base, fog)) in [
            (30.0f32 / 255., 0.5),
            (240. / 255., 0.25),
            (40. / 255., 0.75),
        ]
        .into_iter()
        .enumerate()
        {
            let base = if bmd {
                (base * 2. * 254. / 255.).min(1.)
            } else {
                base
            };
            let encoded = base * factor + fog * (1. - factor);
            let linear = if encoded <= 0.04045 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            };
            let expected = (linear * 255.).round() as i32;
            let actual = pixel[channel] as i32;
            if (actual - expected).abs() > 2 {
                report.push(format!(
                    "channel {channel}: got {actual}, expected {expected}"
                ));
            }
        }
        println!(
            "refraction bmd={bmd} plane={plane} wind={wind}: {pixel:?}, vertex fog factor {factor}, {report:?}"
        );
        if !report.is_empty() {
            println!(
                "refraction bmd={bmd} plane={plane} wind={wind}: base={}, fog={:?}",
                if bmd { 2. * 254. / 255. } else { 1. },
                [0.5, 0.25, 0.75]
            );
        }
        failures.extend(
            report
                .into_iter()
                .map(|r| format!("bmd={bmd} plane={plane} wind={wind} {r}")),
        );
        drop(bytes);
        read.unmap();
    }
    assert!(
        failures.is_empty(),
        "refraction vertex fog does not match the shader:\n  {}",
        failures.join("\n  ")
    );
    uni = saved_uni;
    uni[380..388].fill(0.);
    // The actual GPU shader: compare near, half-range and beyond-range
    // visibility with the PS alpha test, plus a toggle-off regression.
    uni[21] = 0.;
    let mut fade_frames = Vec::new();
    for (case, eye) in [
        [-0.5, 0., 0.5, 0.],
        [-0.5, 0., 0.5, 1.],
        [-0.7, 0., 1.05, 1.],
        [-0.5, 0., 2., 1.],
        [-0.5, 0., 2., 0.],
        [-0.7, 0., 1.05, 1.],
    ]
    .into_iter()
    .enumerate()
    {
        uni[52..56].copy_from_slice(&eye);
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 128 * 128 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut e = device.create_command_encoder(&Default::default());
        {
            let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if case == 5 {
                light_grass.draw(&mut p, &bind, Some(&light_bind));
            } else {
                grass.draw(&mut p, &bind, None);
            }
        }
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
                    bytes_per_row: Some(512),
                    rows_per_image: Some(128),
                },
            },
            size,
        );
        queue.submit(Some(e.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        fade_frames.push(slice.get_mapped_range().to_vec());
        buf.unmap();
    }
    let count = |b: &Vec<u8>| b.chunks_exact(4).filter(|p| p[1] > 0).count();
    let counts: Vec<_> = fade_frames.iter().map(count).collect();
    assert_eq!(
        fade_frames[0], fade_frames[4],
        "grass fade toggle off must restore the previous result"
    );
    assert!(
        counts[1] > 0 && counts[2] > 0 && counts[1] > counts[2] && counts[3] == 0,
        "incorrect grass visibility vs distance: {counts:?}"
    );
    assert_eq!(
        counts[2], counts[5],
        "lightmapped grass did not use the same fade mask"
    );
    for (a, b) in fade_frames[2]
        .chunks_exact(4)
        .zip(fade_frames[5].chunks_exact(4))
    {
        assert_eq!(a[1] > 0, b[1] > 0, "lightmap altered grass fade coverage");
    }
    let mut mismatch = 0;
    for py in 0..128 {
        for px in 0..64 {
            let x = 2. * (px as f32 + 0.5) / 128. - 1.;
            let y = 1. - 2. * (py as f32 + 0.5) / 128.;
            // Each vertex's original, un-waved world position; the two triangles
            // linearly interpolate alpha, not squared distance at the fragment.
            let left = 1. - (0.1f32.powi(2) + 0.3f32.powi(2) + 0.55f32.powi(2));
            let right = 1. - (0.5f32.powi(2) + 0.3f32.powi(2) + 0.55f32.powi(2));
            let alpha = left + (right - left) * (x + 0.8) / 0.6;
            let expected = x > -0.8 && x < -0.2 && y > -0.3 && y < 0.3 && alpha > 0.5;
            let actual = fade_frames[2][(py * 128 + px) * 4 + 1] > 0;
            if expected != actual {
                mismatch += 1;
            }
        }
    }
    assert!(
        mismatch <= 2,
        "distance-fade GPU versus vertex/alpha-test reference differs at {mismatch} pixels"
    );
    println!("Grass cutout fade visibility at disabled/near/mid/far/disabled: {counts:?}; CPU reference mismatch {mismatch}");
    uni[55] = 0.;
    // A grass area may cross a terrain lightmap boundary. The client chooses
    // one map from the area's center, then clamps that map's UVs, not the world
    // vertex's map. Use a small real atlas to expose tile-edge bleed/addressing.
    let edge_saved = uni.clone();
    let edge_values: [u32; 12] = [
        8f32.to_bits(),
        8f32.to_bits(),
        1f32.to_bits(),
        4f32.to_bits(),
        2,
        0,
        0,
        0,
        2,
        4,
        16,
        1,
    ];
    let edge_settings = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&edge_values),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let edge_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("LM tile edge fixture"),
        size: wgpu::Extent3d {
            width: 8,
            height: 8,
            depth_or_array_layers: 2,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut edge_pixels = vec![0u8; 8 * 8 * 2 * 4];
    for layer in 0..2 {
        for y in 0..8 {
            for x in 0..8 {
                let value = match (layer, x) {
                    (0, 0..=1) => 64,
                    (0, 2) => 96,
                    (0, 3) => 192,
                    (1, 0..=1) => 32,
                    (1, 2) => 48,
                    (1, 3) => 64,
                    _ => 255,
                };
                let at = ((layer * 8 + y) * 8 + x) * 4;
                edge_pixels[at..at + 4].copy_from_slice(&[value, value, value, 255]);
            }
        }
    }
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &edge_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &edge_pixels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(32),
            rows_per_image: Some(8),
        },
        wgpu::Extent3d {
            width: 8,
            height: 8,
            depth_or_array_layers: 2,
        },
    );
    let edge_view = edge_texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let edge_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &light_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: edge_settings.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&edge_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: availability.as_entire_binding(),
            },
        ],
    });
    let mut edge_builder = grass_instances::Builder::default();
    edge_builder.mesh((1, 0), vertices.clone(), inds.clone());
    edge_builder.instance_for_area((1, 0), [-0.5, 0., 0.], 1., 0., 0., [2., 0., 2.]);
    let edge_grass = grass_instances::Gpu::new(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        &layout,
        shader,
        edge_builder.finish(),
        Some(&light_layout),
    );
    for unavailable in [false, true] {
        queue.write_buffer(
            &availability,
            0,
            bytemuck::cast_slice(&[3u32, if unavailable { 1 } else { 3 }, 3, 3]),
        );
        for area_mode in [false, true] {
            for daylight in [0.0f32, 0.5, 1.] {
                uni[21] = 0.;
                uni[35] = daylight;
                uni[55] = 0.;
                uni[57] = 0.;
                uni[58] = if area_mode { 1. } else { 0. };
                uni[382] = 0.;
                queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
                let read = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 128 * 128 * 4,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut e = device.create_command_encoder(&Default::default());
                {
                    let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("area center LM clamp"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &dv,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(0.),
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    edge_grass.draw(&mut p, &bind, Some(&edge_bind));
                }
                e.copy_texture_to_buffer(
                    wgpu::ImageCopyTexture {
                        texture: &target,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyBuffer {
                        buffer: &read,
                        layout: wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(512),
                            rows_per_image: Some(128),
                        },
                    },
                    size,
                );
                queue.submit(Some(e.finish()));
                let slice = read.slice(..);
                let (tx, rx) = std::sync::mpsc::channel();
                slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
                device.poll(wgpu::Maintain::Wait);
                rx.recv().unwrap().unwrap();
                let bytes = slice.get_mapped_range();
                let green = bytes[(73 * 128 + 32) * 4 + 1];
                let (day, night) = if area_mode { (96., 48.) } else { (64., 32.) };
                let encoded = if unavailable && area_mode {
                    240. / 255.
                } else {
                    (240. / 255. * 2. * (night * (1. - daylight) + day * daylight) / 255.).min(1.)
                };
                let expected = (((encoded + 0.055) / 1.055).powf(2.4) * 255.).round() as i32;
                assert!(
                (green as i32 - expected).abs() <= 2,
                "area LM clamp mode={area_mode} daylight={daylight}: {green}, expected {expected}"
            );
                println!(
                "area LM clamp mode={area_mode} daylight={daylight}: {green}, expected {expected}"
            );
                drop(bytes);
                read.unmap();
            }
        }
    }
    queue.write_buffer(&availability, 0, bytemuck::cast_slice(&[3u32; 4]));
    uni = edge_saved;
    let hours = [
        (0., 0.),
        (3., 0.),
        (4., 0.25),
        (5., 0.5),
        (6., 0.75),
        (7., 1.),
        (12., 1.),
        (18., 1.),
        (19., 2. / 3.),
        (20., 1. / 3.),
        (21., 0.),
        (24., 0.),
    ];
    for (hour, expected) in hours {
        assert!((environment::daylight(hour) - expected).abs() < 0.00001);
    }
    let cases: Vec<_> = hours
        .iter()
        .map(|&(hour, _)| (hour, 3u32))
        .chain([(-1., 3)])
        .chain((0..3).flat_map(|flags| [0., 5., 12.].map(move |hour| (hour, flags))))
        .collect();
    let mut greens = Vec::new();
    let mut colors = Vec::new();
    for &(hour, flags) in &cases {
        queue.write_buffer(&availability, 0, bytemuck::cast_slice(&[flags; 4]));
        let daylight = if hour < 0. {
            -1.
        } else {
            environment::daylight(hour)
        };
        // No-map fallback must ignore unrelated scene directional lighting.
        if hour < 0. {
            uni[36..44].fill(0.);
        }
        uni[21] = 0.;
        uni[35] = daylight;
        uni[19] = daylight;
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 128 * 128 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut e = device.create_command_encoder(&Default::default());
        {
            let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            light_grass.draw(&mut p, &bind, Some(&light_bind));
        }
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
                    bytes_per_row: Some(512),
                    rows_per_image: Some(128),
                },
            },
            size,
        );
        queue.submit(Some(e.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let pixels = slice.get_mapped_range();
        let g = pixels.chunks_exact(4).map(|p| p[1]).max().unwrap();
        greens.push(g);
        colors.push(
            <[u8; 4]>::try_from(pixels.chunks_exact(4).max_by_key(|p| p[1]).unwrap()).unwrap(),
        );
        drop(pixels);
        buf.unmap();
    }
    for (j, &(hour, flags)) in cases.iter().enumerate() {
        let daylight = if hour < 0. {
            -1.
        } else {
            environment::daylight(hour)
        };
        for ch in 0..3 {
            let mix = (lm[4 + ch] as f32 * (1. - daylight) + lm[ch] as f32 * daylight) / 255.;
            let base = [30., 240., 40.][ch] / 255.;
            let encoded = if daylight < 0. || flags != 3 {
                base
            } else {
                (base * 2. * mix).clamp(0., 1.)
            };
            let linear = if encoded <= 0.04045 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            };
            let expected = (linear * 255.).round() as i32;
            assert!(
                (colors[j][ch] as i32 - expected).abs() <= 2,
                "grass RGB ch={ch} hour={hour} availability={flags}: {:?} expected {expected}",
                colors[j]
            );
        }
        assert_eq!(colors[j][3], 255);
    }
    println!("Grass day/night lightmap test green: {greens:?}");
    let (day_color, night_color) = if let Ok(path) = std::env::var("PW_REAL_BMD_PCK") {
        let mut f = std::fs::File::open(path).expect("open real BMD archive");
        let entries = pck::directory(&mut f).unwrap();
        let entry = entries
            .iter()
            .find(|e| {
                e.name
                    .eq_ignore_ascii_case("litmodels/world/10/litmodel_131.bmd")
            })
            .expect("real model entry");
        let model = bmd::Model::parse(&pck::read_entry(&mut f, entry).unwrap()).unwrap();
        let v = &model.meshes[0].vertices[0];
        println!(
            "Real BMD baked colors: {:#010x} {:#010x}; serialized diffuse {:#010x}",
            v.day_diffuse, v.night_diffuse, v.diffuse
        );
        (v.day_diffuse, v.night_diffuse)
    } else {
        (0xff608040, 0xff203040)
    };
    let bmd_vertices: Vec<_> = expanded
        .iter()
        .map(|v| {
            let mut v = *v;
            v.kind = 2.;
            v.color = [0., 0., 1., 1.];
            v.diffuse = day_color;
            v.night_diffuse = night_color;
            v
        })
        .collect();
    let bmd_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&bmd_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let mut bmd_colors = Vec::new();
    for (hour, _) in hours {
        let daylight = environment::daylight(hour);
        uni[21] = 0.;
        uni[35] = daylight;
        uni[19] = daylight;
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 128 * 128 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut e = device.create_command_encoder(&Default::default());
        {
            let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            p.set_pipeline(&refpipe);
            p.set_bind_group(0, &bind, &[]);
            p.set_vertex_buffer(0, bmd_vb.slice(..));
            p.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
            p.draw_indexed(0..expanded_ix.len() as u32, 0, 0..1);
        }
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
                    bytes_per_row: Some(512),
                    rows_per_image: Some(128),
                },
            },
            size,
        );
        queue.submit(Some(e.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let pixels = slice.get_mapped_range();
        let mut rgb = [0u8; 3];
        for p in pixels.chunks_exact(4) {
            for c in 0..3 {
                rgb[c] = rgb[c].max(p[c]);
            }
        }
        bmd_colors.push(rgb);
        drop(pixels);
        buf.unmap();
    }
    for (j, (_, day)) in hours.into_iter().enumerate() {
        let weight = ((1. - day) * 255.) as u32;
        for (channel, shift) in [16, 8, 0].into_iter().enumerate() {
            let a = (day_color >> shift) & 255;
            let b = (night_color >> shift) & 255;
            let baked = (a * (255 - weight) + b * weight) / 256;
            let tex = [30., 240., 40.][channel];
            let c = (tex / 255. * 2. * baked as f32 / 255.).clamp(0., 1.);
            let linear = if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
            let expected = (linear * 255.).round() as i32;
            assert!(
                (bmd_colors[j][channel] as i32 - expected).abs() <= 3,
                "BMD GPU {:?} channel {} != fixed-function reference {expected}",
                bmd_colors[j],
                channel
            );
        }
    }
    println!("BMD day/night baked vertex RGB test: {bmd_colors:?}");
    // A high-contrast bilinear sample reveals whether filtering happens before
    // or after sRGB decoding. At UV 0.5 a D3D8 UNORM sampler yields 0.5,
    // whereas an sRGB sampler decoded first would encode back to about 0.735.
    let checker: [u8; 16] = [
        0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
    ];
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &checker,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    let checker_vertices: Vec<_> = bmd_vertices
        .iter()
        .map(|v| {
            let mut p = *v;
            p.uv = [0.5, 0.5];
            p.diffuse = 0xff808080;
            p.night_diffuse = 0xff808080;
            p
        })
        .collect();
    let checker_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&checker_vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    uni[19] = 1.;
    uni[21] = 0.;
    uni[22] = 0.;
    uni[23] = 1.;
    queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 128 * 128 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = device.create_command_encoder(&Default::default());
    {
        let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &dv,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        p.set_pipeline(&refpipe);
        p.set_bind_group(0, &bind, &[]);
        p.set_vertex_buffer(0, checker_vb.slice(..));
        p.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
        p.draw_indexed(0..expanded_ix.len() as u32, 0, 0..1);
    }
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
                bytes_per_row: Some(512),
                rows_per_image: Some(128),
            },
        },
        size,
    );
    queue.submit(Some(e.finish()));
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let pixels = slice.get_mapped_range();
    let value = pixels.chunks_exact(4).map(|p| p[0]).max().unwrap();
    drop(pixels);
    buf.unmap();
    let encoded = 0.5_f32 * 2. * 127. / 255.;
    let expected = (((encoded + 0.055) / 1.055).powf(2.4) * 255.).round() as i32;
    assert!(
        (value as i32 - expected).abs() <= 2,
        "encoded-space BMD bilinear {value} != {expected}"
    );
    println!("D3D8-style BMD bilinear encoded sample: linear target {value}, expected {expected}");
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[30, 240, 40, 255].repeat(4),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(2),
        },
        wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
    );
    // BMDs contain many intentionally opposed coincident triangles. The
    // client normally culls the back face; reflection reverses winding.
    // Check both windings independently with the actual scene WGSL.
    let reverse_ix: Vec<u32> = expanded_ix
        .chunks_exact(3)
        .flat_map(|t| [t[0], t[2], t[1]])
        .collect();
    let reverse_ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&reverse_ix),
        usage: wgpu::BufferUsages::INDEX,
    });
    for (reflected, reverse, disabled) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, false),
        (false, true, true),
        (true, false, true),
    ] {
        // Main pass flips X; the reflected pass additionally flips Y.
        uni[0] = -1.;
        uni[5] = if reflected { -1. } else { 1. };
        uni[22] = if reflected { 0. } else { 1. };
        uni[23] = if disabled { 1. } else { 0. };
        uni[19] = 1.;
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 128 * 128 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut e = device.create_command_encoder(&Default::default());
        {
            let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            p.set_pipeline(&refpipe);
            p.set_bind_group(0, &bind, &[]);
            p.set_vertex_buffer(0, bmd_vb.slice(..));
            p.set_index_buffer(
                if reverse {
                    reverse_ib.slice(..)
                } else {
                    ib.slice(..)
                },
                wgpu::IndexFormat::Uint32,
            );
            p.draw_indexed(0..expanded_ix.len() as u32, 0, 0..1);
        }
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
                    bytes_per_row: Some(512),
                    rows_per_image: Some(128),
                },
            },
            size,
        );
        queue.submit(Some(e.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let data = slice.get_mapped_range();
        let visible = data.chunks_exact(4).any(|p| p[1] > 0);
        drop(data);
        buf.unmap();
        assert_eq!(
            visible,
            disabled || !reverse,
            "BMD facing: reflected={reflected} reverse={reverse} disabled={disabled}"
        );
    }
    println!("Mirrored main and reflected BMD front/back winding test passed");
    uni[0] = 1.;
    uni[5] = 1.;
    // Wind is opt-in and approximate; verify it actually deforms the GPU
    // geometry, and that zero restores the exact original coverage.
    uni[21] = 0.;
    uni[22] = 0.;
    uni[23] = 0.;
    uni[55] = 0.;
    // The vertex stage reads `camera.grass_history[extra.z]`, so the fixture
    // needs a history to displace the geometry. `Uniform` is matrix(16) +
    // eleven vec4s + the 80-frame history, so the history spans `uni[60..380]`
    // and `uni[380..384]` is `fog_range`, which the refraction rows above
    // depend on.
    //
    // This test asks only whether the history reaches the vertex stage and
    // whether zeroing the gate restores the image, so it uses a strong,
    // explicit history and measures one thing at a time. Real amplitudes live
    // in `grass_wind` and are covered by its own unit tests.
    // Every frame carries the same non-zero displacement on purpose. Both
    // fixture placements use wind slot 0, and a per-frame curve that happens
    // to start at zero (a plain sine does) would sample an empty entry and the
    // test would silently measure nothing. This test is about the history
    // reaching the vertex stage, not about slot selection.
    for i in 0..80 {
        uni[60 + i * 4..64 + i * 4].copy_from_slice(&[0.3, -0.15, 0., 0.]);
    }
    // Keep the refraction rows above untouched: they read fog_range at 380.
    assert_eq!(uni.len(), 392, "Camera uniform layout changed");
    assert_eq!(uni[60], 0.3, "history[0] carries the displacement");
    let mut wind_frames = Vec::new();
    // `grass_wind.y` is an enable gate, not an amplitude: the viewer writes 1.0
    // when the user turns wind on (gpu.rs uploads
    // `if camera.grass_wind_enabled { 1. } else { 0. }`), because retail scales
    // the effect with authored per-area data rather than a user slider. Drive
    // the uniform the way the renderer does, so a fixture value the viewer
    // never produces cannot fail the check on its own.
    for strength in [0., 1., 0.] {
        uni[56] = 1.1;
        uni[57] = strength;
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 128 * 128 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut e = device.create_command_encoder(&Default::default());
        {
            let mut pass = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            grass.draw(&mut pass, &bind, None);
        }
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
                    bytes_per_row: Some(512),
                    rows_per_image: Some(128),
                },
            },
            size,
        );
        queue.submit(Some(e.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        wind_frames.push(slice.get_mapped_range().to_vec());
        buf.unmap();
    }
    assert_eq!(
        wind_frames[0], wind_frames[2],
        "zero wind should restore image"
    );
    let lit = |f: &[u8]| f.chunks_exact(4).filter(|p| p[1] > 0).count();
    let mean =
        |f: &[u8]| f.iter().skip(1).step_by(4).map(|v| *v as u32).sum::<u32>() as f32 / 16384.;
    let changes = wind_frames[0]
        .chunks_exact(4)
        .zip(wind_frames[1].chunks_exact(4))
        .filter(|(a, b)| (a[1] > 0) != (b[1] > 0))
        .count();
    // Any channel moving at all, which also catches a deformation too small to
    // flip the coverage predicate but large enough to survive rasterisation.
    let touched = wind_frames[0]
        .chunks_exact(4)
        .zip(wind_frames[1].chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    // Carry the measurement in the message: CI only surfaces the panic text
    // and the lines after it, so a bare count cannot say whether the history
    // failed to arrive or the geometry was never on screen.
    assert!(
        changes > 100,
        "wind failed to deform coverage: {changes} ({touched} pixels moved); lit {} -> {} of 16384, mean green {:.3} -> {:.3}",
        lit(&wind_frames[0]),
        lit(&wind_frames[1]),
        mean(&wind_frames[0]),
        mean(&wind_frames[1])
    );
    println!("History-driven grass wind changes {changes} pixels, zero restores exact image");
    grass.update(&device, &queue, grass_instances::Data::default());
    println!("Empty streaming update succeeded");
}
