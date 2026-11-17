//! Validate both real terrain vertex entry points using identical geometry.
use wgpu::util::DeviceExt;
fn main() {
    // Panics abort with status 101 and their message only reaches the job log,
    // which is not always retrievable. Mirror the panic into a GitHub annotation
    // so a CI failure stays diagnosable.
    std::panic::set_hook(Box::new(|info| {
        // Annotations truncate at the first newline, and wgpu validation errors
        // are multi-line, so flatten the message.
        println!(
            "::error::terrain-lod-gpu-check panicked: {}",
            info.to_string().replace('\n', " | ")
        );
    }));
    let inst = wgpu::Instance::default();
    let adapter = pollster::block_on(inst.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        },
        None,
    ))
    .unwrap();
    let source = include_str!("../terrain_gpu.rs")
        .split("const SHADER: &str = r#\"")
        .nth(1)
        .unwrap()
        .split("\"#;")
        .next()
        .unwrap();
    let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("actual terrain vertex shaders"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let fragment=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:None,source:wgpu::ShaderSource::Wgsl("@fragment fn fs(@location(0) world:vec3<f32>,@location(1) normal:vec3<f32>,@location(2) @interpolate(flat) distant:f32,@location(3) refraction:vec2<f32>)->@location(0) vec4<f32>{_ = world+vec3<f32>(refraction,0.);return vec4<f32>(normal*0.4+vec3<f32>(0.5),distant);}".into())});
    const NEAR: [wgpu::VertexAttribute; 5] =
        wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32];
    const FAR: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0=>Float32x3,1=>Snorm8x4];
    let pipeline = |far: bool| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState {
                module: &vertex,
                entry_point: if far { "vs_far" } else { "vs" },
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: if far { 16 } else { 60 },
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: if far { &FAR } else { &NEAR },
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &fragment,
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
        })
    };
    println!("terrain-lod: vertex module and pipelines created");
    let pipes = [pipeline(false), pipeline(true)];
    let points = [
        [-0.8f32, -0.8, 0.5],
        [0.8, -0.8, 0.5],
        [-0.8, 0.8, 0.5],
        [0.8, 0.8, 0.5],
    ];
    let mut near = Vec::<u8>::new();
    let mut far = Vec::<u8>::new();
    for p in points {
        for f in p {
            near.extend_from_slice(&f.to_le_bytes());
            far.extend_from_slice(&f.to_le_bytes());
        }
        for f in [0f32, 1., 0., 0., 0., 0., 0., 0., 1., 0.] {
            near.extend_from_slice(&f.to_le_bytes());
        }
        near.extend_from_slice(&[0u8; 8]);
        far.extend_from_slice(&[0, 127, 0, 0]);
    }
    assert_eq!(near.len(), 4 * 60);
    assert_eq!(far.len(), 4 * 16);
    let vbs = [near, far].map(|x| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &x,
            usage: wgpu::BufferUsages::VERTEX,
        })
    });
    let idx = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&[0u16, 1, 2, 1, 3, 2]),
        usage: wgpu::BufferUsages::INDEX,
    });
    let mut camera = [0f32; 392];
    camera[..16].copy_from_slice(&glam::Mat4::IDENTITY.to_cols_array());
    // `lod`: no bias, and 1.0 leaves the atlas gradient scale untouched.
    camera[388..392].copy_from_slice(&[0., 1., 0., 0.]);
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&camera),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let size = wgpu::Extent3d {
        width: 64,
        height: 64,
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
    let view = target.create_view(&Default::default());
    let mut frames = Vec::new();
    for i in 0..2 {
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipes[i].get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 64 * 64 * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
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
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipes[i]);
            pass.set_bind_group(0, &bind, &[]);
            pass.set_vertex_buffer(0, vbs[i].slice(..));
            pass.set_index_buffer(idx.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..6, 0, 0..1);
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
                    bytes_per_row: Some(256),
                    rows_per_image: Some(64),
                },
            },
            size,
        );
        queue.submit(Some(e.finish()));
        let sl = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        sl.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        frames.push(sl.get_mapped_range().to_vec());
        buf.unmap();
    }
    println!("terrain-lod: both frames read back");
    let count = frames[0].chunks_exact(4).filter(|p| p[0] > 0).count();
    assert!(count > 2000);
    // The check fragment returns vec4(normal*0.4+0.5, distant), so RGB carries
    // the interpolated normal and ALPHA carries the LOD flag: `vs` reports the
    // authored per-vertex kind (0 here) and `vs_far` reports distant = 1. The
    // two frames must agree in RGB and differ in alpha. Comparing whole frames
    // made this check unsatisfiable on real hardware, which went unnoticed
    // because the program was never in the GPU set.
    //
    // Failures are reported as a GitHub `::error::` annotation with the exact
    // counters, because CI job logs are not always retrievable and an annotation
    // is.
    let empty = [0u8; 4];
    let mut rgb_mismatch = 0usize;
    let mut first_rgb = None;
    let mut alpha_bad = 0usize;
    let first_alpha = None::<([u8; 4], [u8; 4], usize)>;
    let mut first_alpha = first_alpha;
    for (i, (a, b)) in frames[0]
        .chunks_exact(4)
        .zip(frames[1].chunks_exact(4))
        .enumerate()
    {
        let (a, b) = (a.try_into().unwrap_or(empty), b.try_into().unwrap_or(empty));
        if a[..3] != b[..3] {
            rgb_mismatch += 1;
            first_rgb.get_or_insert((a, b, i));
        }
        if (a[0] > 0 || b[0] > 0) && (a[3] != 0 || b[3] != 255) {
            alpha_bad += 1;
            first_alpha.get_or_insert((a, b, i));
        }
    }
    if rgb_mismatch > 0 || alpha_bad > 0 {
        println!(
            "::error::terrain-lod-gpu-check: rgb_mismatch={rgb_mismatch} \
             alpha_bad={alpha_bad} covered_pixels={count} \
             first_rgb={first_rgb:?} first_alpha={first_alpha:?}"
        );
        std::process::exit(1);
    }
    println!("terrain actual near/far vertex shaders match for {count} pixels; packed far vertex 16 bytes versus 60 bytes");
}
