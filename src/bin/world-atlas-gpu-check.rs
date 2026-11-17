//! Bit-exact native atlas sharing through production upload and draw.
#![allow(dead_code)]
#[path = "../bmd.rs"]
mod bmd;
// The baked draw-state table is shared with the production scenery path.
#[path = "../bmd_baked.rs"]
mod bmd_baked;
#[path = "../objects.rs"]
mod objects;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../scenery_visibility.rs"]
mod scenery_visibility;
#[path = "../texture_atlas.rs"]
mod texture_atlas;
#[path = "../texture_resolution.rs"]
mod texture_resolution;
#[path = "../world_objects.rs"]
mod world_objects;
#[path = "../world_page_cache.rs"]
mod world_page_cache;
mod gpu {
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    pub(crate) struct Vertex {
        pub(crate) pos: [f32; 3],
        pub(crate) normal: [f32; 3],
        pub(crate) uv: [f32; 2],
        pub(crate) color: [f32; 4],
        pub(crate) kind: f32,
        pub(crate) diffuse: u32,
        pub(crate) night_diffuse: u32,
    }
}

use wgpu::util::DeviceExt;
use world_objects::{GpuPage, Page};
const SIDE: usize = 4096;
fn page(right: bool, shared: bool) -> Page {
    // Coincident triangles deliberately make submission order observable.
    let positions = [[-1., -1., 0.], [1., -1., 0.], [-1., 1., 0.]];
    let vertices = positions
        .into_iter()
        .map(|pos| gpu::Vertex {
            pos,
            normal: [0., 1., 0.],
            uv: [64. / SIDE as f32, 64. / SIDE as f32],
            color: [0., 0., 128. / SIDE as f32, 128. / SIDE as f32],
            kind: 2.,
            diffuse: if right { 0xffff0000 } else { 0xffffffff },
            night_diffuse: 0xffffffff,
        })
        .collect();
    let atlas = if shared {
        Vec::new()
    } else {
        [37, 119, 201, 255].repeat(SIDE * 128)
    };
    Page {
        vertices,
        indices: vec![0, 1, 2],
        atlas,
        shared_atlas: shared.then_some(0),
        processed: 1,
        used_height: 128,
        mips: Vec::new(),
        bounds: Vec::new(),
        page_min: [f32::INFINITY; 3],
        page_max: [f32::NEG_INFINITY; 3],
        page_has_sdr: false,
        placements: vec![world_objects::PlacementRange {
            start: 0,
            end: 3,
            export_id: 1,
            order: u64::from(right),
            visibility: scenery_visibility::Visibility {
                reflect: true,
                refract: true,
            },
        }],
    }
    .finish()
}
fn render(
    d: &wgpu::Device,
    q: &wgpu::Queue,
    pipeline: &wgpu::RenderPipeline,
    pages: &[GpuPage],
    reflected: bool,
) -> Vec<u8> {
    let target = d.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 16,
            height: 16,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let read = d.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16 * 256,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut e = d.create_command_encoder(&Default::default());
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
        pass.set_pipeline(pipeline);
        let order = GpuPage::draw_order(pages);
        GpuPage::draw_ordered(
            pages,
            &order,
            &mut pass,
            reflected,
            scenery_visibility::Pass::Main,
        );
    }
    e.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::ImageCopyBuffer {
            buffer: &read,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(16),
            },
        },
        wgpu::Extent3d {
            width: 16,
            height: 16,
            depth_or_array_layers: 1,
        },
    );
    q.submit(Some(e.finish()));
    let slice = read.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    d.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let map = slice.get_mapped_range();
    let pixels = map
        .chunks(256)
        .flat_map(|row| row[..64].iter().copied())
        .collect();
    drop(map);
    read.unmap();
    pixels
}
fn main() {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&Default::default())).expect("adapter");
    let (d, q) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
        },
        None,
    ))
    .unwrap();
    let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
    let uniform = |gain: f32| {
        d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&[0f32, gain, 0., 0.]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        })
    };
    let normal = uniform(1.);
    let reflected = uniform(0.5);
    let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let mut shared = vec![GpuPage::upload(
        &d,
        &q,
        &layout,
        &sampler,
        &normal,
        &reflected,
        page(false, false),
    )];
    let second = GpuPage::upload_world(
        &d,
        &q,
        &layout,
        &sampler,
        &normal,
        &reflected,
        &shared,
        page(true, true),
    );
    assert!(second.shares_atlas_with(&shared[0]));
    shared.push(second);
    let reference = vec![
        GpuPage::upload(
            &d,
            &q,
            &layout,
            &sampler,
            &normal,
            &reflected,
            page(true, false),
        ),
        GpuPage::upload(
            &d,
            &q,
            &layout,
            &sampler,
            &normal,
            &reflected,
            page(false, false),
        ),
    ];
    assert!(!reference[0].shares_atlas_with(&reference[1]));
    assert_eq!(GpuPage::draw_order(&reference), vec![(1, 0), (0, 0)]);
    assert_eq!(GpuPage::draw_order(&shared), vec![(0, 0), (1, 0)]);
    let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(
            r#"
struct Out { @builtin(position) pos:vec4<f32>, @location(0) uv:vec2<f32>, @location(1) tint:vec3<f32> };
@group(0) @binding(0) var<uniform> params:vec4<f32>;
@group(0) @binding(1) var atlas:texture_2d<f32>;
@group(0) @binding(2) var smp:sampler;
@vertex fn vs(@location(0) pos:vec3<f32>,@location(1) uv:vec2<f32>,@location(2) diffuse:u32)->Out {
    var o:Out; o.pos=vec4<f32>(pos,1.); o.uv=uv;
    o.tint=vec3<f32>(f32((diffuse>>16u)&255u),f32((diffuse>>8u)&255u),f32(diffuse&255u))/255.; return o;
}
@fragment fn fs(o:Out)->@location(0) vec4<f32> {
    return vec4<f32>(textureSampleLevel(atlas,smp,o.uv,params.x).rgb * params.y * o.tint,1.);
}
"#
            .into(),
        ),
    });
    let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let pipeline = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs",
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<gpu::Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Uint32,
                        offset: 52,
                        shader_location: 2,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 24,
                        shader_location: 1,
                    },
                ],
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
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
    for level in [0f32, 3., 7.] {
        q.write_buffer(&normal, 0, bytemuck::cast_slice(&[level, 1f32, 0., 0.]));
        q.write_buffer(
            &reflected,
            0,
            bytemuck::cast_slice(&[level, 0.5f32, 0., 0.]),
        );
        for reflected in [false, true] {
            let got = render(&d, &q, &pipeline, &shared, reflected);
            let expected = render(&d, &q, &pipeline, &reference, reflected);
            assert!(
                got.chunks(4).filter(|p| p[0] > 0).count() > 64,
                "nonempty geometry"
            );
            assert!(
                got.chunks_exact(4).all(|p| p[1] == 0 && p[2] == 0),
                "original last-drawn red triangle must win"
            );
            assert_eq!(
                got, expected,
                "shared atlas changes pixels: mip {level}, reflected {reflected}"
            );
        }
    }
    println!("Production whole-world atlas sharing: 6/6 pixel comparisons passed, including final mip and reflected binding; 2 geometry buffers share 1 unchanged texture allocation");
}
