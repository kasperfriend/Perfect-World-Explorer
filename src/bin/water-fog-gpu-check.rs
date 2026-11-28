//! Exercise the water-surface fog stage from `src/water_gpu.rs`.
//!
//! The client does not fog water in a shader: it binds the region's own
//! `D3DRS_FOGSTART/END/COLOR` triple right before the water draw
//! (`0x7cb444`, `0x7cb502`, `0x7cb94e`) and `0x7ca9d9`/`0x7ca9e8`/`0x7ca9f7`
//! inside the surface renderer, so the fixed-function fog stage covers the
//! whole surface. This check runs the production `surface_fog` helper for both
//! recovered triples: white 100..400 above water, blue 10..60 below it.
#![allow(dead_code)]
use wgpu::util::DeviceExt;
#[path = "../fog_matrix.rs"]
mod fog_matrix;
#[path = "../water_gpu.rs"]
mod water_gpu;
/// Minimal stand-in: the water module only needs the vertex stride and layout.
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
const WIDTH: u32 = 512;
const HEIGHT: u32 = 8;

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
    let shader = include_str!("../water_gpu.rs");
    let source = shader
        .split("const WATER_SHADER: &str = r#\"")
        .nth(1)
        .unwrap()
        .split("\"#;")
        .next()
        .unwrap();
    // The fragment's view depth is its own pixel column, so each case can pick
    // an exact depth without rebuilding the shader per case.
    let full = source.to_owned()
        + r#"
@vertex fn test_vs(@builtin(vertex_index) id:u32)->@builtin(position) vec4<f32> {
 let x=f32((id<<1u)&2u);let y=f32(id&2u);return vec4<f32>(x*2.-1.,1.-y*2.,0.5,1.);
}
@fragment fn test_fs(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32> {
 let c=vec3<f32>(0.21404114,0.05087609,0.52252155);
 return vec4<f32>(surface_fog(c,vec3<f32>(p.x,0.,0.)),1.);
}
"#;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("water surface fog"),
        source: wgpu::ShaderSource::Wgsl(full.into()),
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
    let size = wgpu::Extent3d {
        width: WIDTH,
        height: HEIGHT,
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
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (WIDTH * HEIGHT * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let air = fog_matrix::Fog {
        start: 0.,
        end: 400.,
        color: [0.2, 0.4, 0.6],
        enabled: true,
    };
    let under = fog_matrix::Fog {
        start: 12.,
        end: 64.,
        color: [0.1, 0.2, 0.4],
        enabled: true,
    };
    // name, bound triple, fog enabled, pixel column (view depth = col + 0.5)
    let cases: [(&str, fog_matrix::Fog, bool, u32); 8] = [
        ("air near", air, true, 0),
        ("air mid", air, true, 199),
        ("air end", air, true, 399),
        ("air past end", air, true, 460),
        ("under mid", under, true, 37),
        ("under end", under, true, 63),
        ("under past end", under, true, 80),
        ("air disabled", air, false, 199),
    ];
    let shade = [0.21404114f32, 0.05087609, 0.52252155];
    // Matrix (16) + inverse (16) + eye_time (4) + size (4) + animation (4) +
    // edge_mode (4) = 48 floats, then fog (4) and fog_color (4).
    let mut uniform = [0f32; 56];
    // The production helper derives view depth from the projected world
    // position's W component. Map world.x to that value for this 1D test.
    uniform[3] = 1.0;
    for (name, bound, fog_on, column) in cases {
        let fog = bound.range();
        let color = bound.color4();
        uniform[48..52].copy_from_slice(&fog);
        uniform[52..56].copy_from_slice(&color);
        if !fog_on {
            uniform[50] = 0.0;
        }
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
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
            pass.draw(0..3, 0..1);
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
                    bytes_per_row: Some(WIDTH * 4),
                    rows_per_image: Some(HEIGHT),
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
        let at = ((HEIGHT / 2) * WIDTH + column) * 4;
        let p = &bytes[at as usize..][..3];
        let depth = column as f32 + 0.5;
        let mut f = 1.0f32;
        if fog_on {
            let span = fog[1] - fog[0];
            f = ((fog[1] - depth) / span).clamp(0.0, 1.0);
        }
        let mut expected = [0u8; 3];
        for i in 0..3 {
            let v = color[i] + (shade[i] - color[i]) * f;
            expected[i] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        let ok = (0..3).all(|i| (p[i] as i32 - expected[i] as i32).abs() <= 1);
        assert!(ok, "{name}: water fog {p:?} expected {expected:?}");
        println!("{name}: fog {fog_on} depth {depth:.1} water fog RGB={p:?}");
        drop(bytes);
        read.unmap();
    }
    println!("Water surface fog: 8 cases passed (bound region fog over view depth)");
}
