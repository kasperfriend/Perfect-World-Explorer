//! Exercise production terrain refraction vertex projection, depth darkness,
//! interpolated fog and encoded blending, plus ordinary/reflection fog controls.
use wgpu::util::DeviceExt;
fn main() {
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
    let mut uniform = [0f32; 392];
    uniform[..16].copy_from_slice(&glam::Mat4::IDENTITY.to_cols_array());
    // `lod`: no bias, and 1.0 leaves the atlas gradient scale untouched.
    uniform[388..392].copy_from_slice(&[0., 1., 0., 0.]);
    uniform[52..56].copy_from_slice(&[0., 10., 0., 0.]);
    uniform[380..384].copy_from_slice(&[0., 2., 1., 0.]);
    uniform[384..388].copy_from_slice(&[0.1, 0.8, 0.2, 1.]);
    let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let size = wgpu::Extent3d {
        width: 64,
        height: 64,
        depth_or_array_layers: 1,
    };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    for (name, clip, plane, enabled) in [
        ("main", 0.0f32, 0.0f32, true),
        ("reflection", 1., 0., true),
        ("refraction-0", -1., 0., true),
        ("refraction-quarter", -1., 0.25, true),
        ("refraction-half", -1., 0.5, true),
        ("fog-disabled", 0., 0., false),
    ] {
        uniform[20] = plane;
        uniform[21] = clip;
        uniform[382] = if enabled { 1. } else { 0. };
        queue.write_buffer(&buf, 0, bytemuck::cast_slice(&uniform));
        let file = include_str!("../terrain_gpu.rs");
        let source = file
            .split("const SHADER: &str = r#\"")
            .nth(1)
            .unwrap()
            .split("\"#;")
            .next()
            .unwrap();
        let source = format!(
            "{source}\n{}",
            r#"
@vertex fn test_vs(@builtin(vertex_index) id:u32)->Out {
 var points=array<vec3<f32>,6>(vec3<f32>(-1.,-1.,0.5),vec3<f32>(1.,-1.,0.5),vec3<f32>(-1.,1.,0.5),vec3<f32>(1.,-1.,0.5),vec3<f32>(1.,1.,0.5),vec3<f32>(-1.,1.,0.5));
 return terrain_vertex(points[id],vec3<f32>(0.,1.,0.),0.);
}
@fragment fn test_fs(i:Out)->@location(0) vec4<f32> {
 let rgb=to_linear(vec3<f32>(0.5,0.25,0.75)*i.refraction.y);
 return vec4<f32>(terrain_pass_fog(rgb,i.pos.w,i.refraction.x),1.);
}

"#
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(name),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: "test_vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: "test_fs",
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
        let layout = pipeline.get_bind_group_layout(0);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 64 * 64 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..6, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &tex,
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
            size,
        );
        queue.submit(Some(encoder.finish()));
        let slice = read.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let bytes = slice.get_mapped_range();
        let p = &bytes[(48 * 64 + 16) * 4..][..3];
        let (transmittance, darkness) = if clip < 0. {
            let y = 1. - (48.5f32 / 32.);
            let v = (y + 1.) / (plane + 1.);
            let bottom_depth = plane + 1.;
            let bottom_ray = (122.25f32).sqrt() / 11.;
            (
                (-0.075 * bottom_depth * bottom_ray).exp2() * (1. - v) + v,
                (-0.15 * bottom_depth).exp2() * (1. - v) + v,
            )
        } else {
            (if enabled { 0.5 } else { 1. }, 1.)
        };
        let expected: Vec<i32> = [(0.5f32, 0.1f32), (0.25, 0.8), (0.75, 0.2)]
            .into_iter()
            .map(|(base, fog)| {
                let encoded = base * darkness * transmittance + fog * (1. - transmittance);
                let linear = if encoded <= 0.04045 {
                    encoded / 12.92
                } else {
                    ((encoded + 0.055) / 1.055).powf(2.4)
                };
                (linear * 255.).round() as i32
            })
            .collect();
        assert!(
            (0..3).all(|i| (p[i] as i32 - expected[i]).abs() <= 2),
            "{name}: {p:?}, expected {expected:?}; factors {transmittance},{darkness}"
        );
        println!("{name} terrain GPU RGB={p:?}, expected {expected:?}");
        drop(bytes);
        read.unmap();
    }
}
