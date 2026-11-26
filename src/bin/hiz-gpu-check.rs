//! Headless GPU regression for the production reversed-Z Hi-Z reduction shader.
use std::sync::mpsc;
use wgpu::util::DeviceExt;

#[path = "../hiz.rs"]
mod hiz;

const WIDTH: u32 = 13;
const HEIGHT: u32 = 7;

const DEPTH_TEST_WGSL: &str = r#"
struct FragmentOut { @builtin(frag_depth) depth: f32 };
@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let vertices = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0)
    );
    return vec4<f32>(vertices[index], 0.5, 1.0);
}
@fragment
fn fs(@builtin(position) pixel: vec4<f32>) -> FragmentOut {
    var output: FragmentOut;
    // Keep a clear-depth hole at the odd bottom-right edge. Every other pixel
    // writes a closer reversed-Z surface and must survive in every mip that
    // does not cover that hole.
    if u32(pixel.x) == 12u && u32(pixel.y) == 6u {
        output.depth = 0.0;
    } else {
        output.depth = 0.8;
    }
    return output;
}
"#;

fn main() {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: true,
        compatible_surface: None,
    }))
    .expect("no fallback GPU adapter");
    println!("Hi-Z GPU check adapter: {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("Hi-Z regression device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .expect("failed to create Hi-Z regression device");

    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("synthetic reversed-Z depth"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&Default::default());
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("synthetic depth writer"),
        source: wgpu::ShaderSource::Wgsl(DEPTH_TEST_WGSL.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("synthetic reversed-Z depth writer"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: "vs",
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: "fs",
            targets: &[],
        }),
        primitive: wgpu::PrimitiveState::default(),
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

    let pyramid = hiz::Pyramid::new(&device, WIDTH, HEIGHT).expect("Hi-Z allocation failed");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Hi-Z GPU regression encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("write synthetic reversed-Z depth"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    pyramid.encode(&device, &mut encoder, &depth_view);
    let mut copy_layouts = Vec::with_capacity(pyramid.levels().len());
    let mut readback_size = 0u64;
    for level in pyramid.levels() {
        readback_size = readback_size.div_ceil(256) * 256;
        let row_pitch = (u64::from(level.width) * 4).div_ceil(256) * 256;
        copy_layouts.push((readback_size, row_pitch));
        readback_size += row_pitch * u64::from(level.height);
    }
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Hi-Z regression readback"),
        size: readback_size.max(4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    for (index, (level, (offset, row_pitch))) in
        pyramid.levels().iter().zip(copy_layouts.iter()).enumerate()
    {
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: pyramid.texture(),
                mip_level: index as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout {
                    offset: *offset,
                    bytes_per_row: Some(*row_pitch as u32),
                    rows_per_image: Some(level.height),
                },
            },
            wgpu::Extent3d {
                width: level.width,
                height: level.height,
                depth_or_array_layers: 1,
            },
        );
    }
    queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .expect("readback callback dropped")
        .expect("Hi-Z readback mapping failed");
    let bytes = slice.get_mapped_range().to_vec();
    let expected = [(13u32, 7u32), (6, 3), (3, 1), (1, 1)];
    assert_eq!(pyramid.levels().len(), expected.len());
    for (index, (level, (width, height))) in pyramid.levels().iter().zip(expected).enumerate() {
        assert_eq!((level.width, level.height), (width, height), "mip {index}");
        let (offset, row_pitch) = copy_layouts[index];
        for y in 0..height as usize {
            let row_start = offset as usize + y * row_pitch as usize;
            let row = &bytes[row_start..row_start + width as usize * 4];
            let mip = bytemuck::cast_slice::<u8, f32>(row);
            assert!(
                mip.iter().all(|v| (*v - 0.8).abs() < 1.0e-5 || *v == 0.0),
                "mip {index} contains unexpected depth values: {mip:?}"
            );
            if y == height as usize - 1 {
                assert_eq!(
                    mip[width as usize - 1],
                    0.0,
                    "clear hole must propagate through mip {index}"
                );
            }
            if index == 0 && y == 0 {
                assert_eq!(mip[0], 0.8);
                assert_eq!(mip[WIDTH as usize - 2], 0.8);
            }
        }
    }
    drop(bytes);
    readback.unmap();
    println!("::notice::Hi-Z GPU check passed: {}x{} depth, {} min-reduction levels, odd-edge clear propagated",
        WIDTH, HEIGHT, pyramid.levels().len());
}
