//! Production authored-mip uploads: compare native BC (when available) and the
//! decoded fallback across all levels, including 2x2/1x1 and BC1 transparency.
#![allow(dead_code)]
#[path = "../model_dds.rs"]
mod model_dds;
#[path = "../model_texture_gpu.rs"]
mod model_texture_gpu;
fn fixture(tag: &[u8; 4], transparent: bool) -> Vec<u8> {
    let mut bytes = vec![0; 128];
    bytes[..4].copy_from_slice(b"DDS ");
    for (o, v) in [
        (4, 124u32),
        (8, 0xa1007),
        (12, 8),
        (16, 8),
        (28, 4),
        (76, 32),
        (80, 4),
        (108, 0x401008),
    ] {
        bytes[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes[84..88].copy_from_slice(tag);
    for (m, (color, alpha)) in [
        (0xf800u16, 17u8),
        (0x07e0, 85),
        (0x001f, 170),
        (0xffff, 255),
    ]
    .into_iter()
    .enumerate()
    {
        let blocks = if m == 0 { 4 } else { 1 };
        for _ in 0..blocks {
            if tag == b"DXT3" {
                bytes.extend([alpha; 8]);
            }
            if tag == b"DXT5" {
                bytes.extend([alpha, 0, 0, 0, 0, 0, 0, 0]);
            }
            if transparent {
                bytes.extend([0, 0, 0, 248, 255, 255, 255, 255]);
            } else {
                bytes.extend(color.to_le_bytes());
                bytes.extend([0; 6]);
            }
        }
    }
    bytes
}
// Author different constant blocks per mip so the DX10 header and each
// sub-block mip offset must be correct on both upload routes.
const BC5: [[u8; 4]; 4] = [
    [40, 160, 0, 255],
    [160, 40, 0, 255],
    [20, 80, 0, 255],
    [210, 128, 0, 255],
];
const BC7: [[u8; 4]; 4] = [
    [254, 64, 32, 192],
    [64, 254, 32, 160],
    [32, 64, 254, 128],
    [254, 254, 254, 0],
];
fn dx10_fixture(dxgi: u32, colors: [[u8; 4]; 4]) -> Vec<u8> {
    let mut bytes = fixture(b"DXT1", false);
    bytes.truncate(128);
    bytes[84..88].copy_from_slice(b"DX10");
    bytes.extend([dxgi, 3, 0, 1, 0].into_iter().flat_map(u32::to_le_bytes));
    for (i, color) in colors.into_iter().enumerate() {
        let block = if dxgi == 83 {
            let mut rg = [0u8; 16];
            rg[0] = color[0];
            rg[8] = color[1];
            rg
        } else {
            // BC7 mode 6 with equal seven-bit endpoints, P bits and indices 0.
            let mut data = 64u128;
            for (c, value) in color.into_iter().enumerate() {
                let shift = 7 + c * 14;
                let q = u128::from(value / 2);
                data |= (q << shift) | (q << (shift + 7));
            }
            data.to_le_bytes()
        };
        for _ in 0..if i == 0 { 4 } else { 1 } {
            bytes.extend_from_slice(&block);
        }
    }
    bytes
}

fn main() {
    std::panic::set_hook(Box::new(|p| {
        eprintln!("::error::{}", p.to_string().replace('\n', "%0A"))
    }));
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let features = adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: features,
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .unwrap();
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let shader=device.create_shader_module(wgpu::ShaderModuleDescriptor{label:None,source:wgpu::ShaderSource::Wgsl(r#"
@group(0) @binding(0) var image:texture_2d<f32>;
@group(0) @binding(1) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id:vec3<u32>) {result[id.x]=textureLoad(image,vec2<i32>(0),i32(id.x));}
"#.into())});
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader,
        entry_point: "main",
    });
    let mut comparisons = 0;
    use wgpu::TextureFormat::*;
    let mut cases = Vec::new();
    for (tag, transparent, format) in [
        (b"DXT1", false, Bc1RgbaUnorm),
        (b"DXT3", false, Bc2RgbaUnorm),
        (b"DXT5", false, Bc3RgbaUnorm),
        (b"DXT1", true, Bc1RgbaUnorm),
    ] {
        let mut expected = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255; 4],
        ];
        if transparent {
            expected = [[0; 4]; 4];
        } else if tag != b"DXT1" {
            for (p, a) in expected.iter_mut().zip([17, 85, 170, 255]) {
                p[3] = a;
            }
        }
        cases.push((
            std::str::from_utf8(tag).unwrap(),
            fixture(tag, transparent),
            format,
            expected,
        ));
    }
    cases.push(("DX10 BC5", dx10_fixture(83, BC5), Bc5RgUnorm, BC5));
    cases.push((
        "DX10 BC7 sRGB (encoded values)",
        dx10_fixture(99, BC7),
        Bc7RgbaUnorm,
        BC7,
    ));
    for (tag, bytes, format, expected) in cases {
        for mode in [
            model_texture_gpu::UploadMode::Auto,
            model_texture_gpu::UploadMode::Decoded,
        ] {
            let texture = model_texture_gpu::upload(&device, &queue, &bytes, mode).unwrap();
            assert_eq!(texture.mip_level_count(), 4);
            assert_eq!(
                texture.format(),
                if matches!(mode, model_texture_gpu::UploadMode::Auto) && !features.is_empty() {
                    format
                } else {
                    Rgba8Unorm
                }
            );
            let view = texture.create_view(&Default::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: output.as_entire_binding(),
                    },
                ],
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: None,
                    timestamp_writes: None,
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.dispatch_workgroups(4, 1, 1);
            }
            encoder.copy_buffer_to_buffer(&output, 0, &read, 0, 64);
            queue.submit(Some(encoder.finish()));
            let slice = read.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().unwrap();
            let mapped = slice.get_mapped_range();
            let values: &[f32] = bytemuck::cast_slice(&mapped);
            for (m, color) in expected.into_iter().enumerate() {
                for (i, channel) in color.into_iter().enumerate() {
                    let want = f32::from(channel) / 255.;
                    let got = values[m * 4 + i];
                    assert!(
                        got.is_finite() && (got - want).abs() < 0.00001,
                        "{tag:?}, mip {m}, channel {i}: {got} != {want}"
                    );
                }
                comparisons += 1;
            }
            drop(mapped);
            read.unmap();
        }
    }
    let mut tga = vec![0; 18];
    tga[2] = 2;
    tga[12..14].copy_from_slice(&1u16.to_le_bytes());
    tga[14..16].copy_from_slice(&1u16.to_le_bytes());
    tga[16] = 32;
    tga[17] = 8;
    tga.extend_from_slice(&[3, 2, 1, 4]);
    let (texture, alpha) = model_texture_gpu::upload_with_alpha(
        &device,
        &queue,
        &tga,
        model_texture_gpu::UploadMode::Auto,
    )
    .unwrap();
    assert!(alpha);
    assert_eq!((texture.width(), texture.height()), (1, 1));
    assert_eq!(texture.mip_level_count(), 1);
    assert_eq!(texture.format(), wgpu::TextureFormat::Rgba8Unorm);
    println!(
        "{comparisons} model texture mip RGBA checks plus TGA upload passed; native BC enabled: {}",
        !features.is_empty()
    );
}
