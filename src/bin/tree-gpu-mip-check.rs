//! Isolated GPU test using all client SpeedTree types, without world terrain.
#![allow(dead_code)]
#[path = "../dxt.rs"]
mod dxt;
#[path = "../far_trees_gpu.rs"]
mod far_trees_gpu;
#[path = "../foliage_mips.rs"]
mod foliage_mips;
#[path = "../indirect_args.rs"]
mod indirect_args;
#[path = "../model_dds.rs"]
mod model_dds;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../tree_lod.rs"]
mod tree_lod;
#[path = "../trees.rs"]
mod trees;
#[path = "../vegetation_material.rs"]
mod vegetation_material;
#[path = "../vegetation_textures.rs"]
mod vegetation_textures;
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
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("element path"));
    let cache = std::path::PathBuf::from(std::env::args().nth(2).expect("tree cache"));
    let scene = scene::Scene::load(&root.join("maps/world/world.ecwld")).unwrap();
    let trees = trees::Trees::open(&root, &scene, &cache).unwrap();
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .unwrap();
    println!("GPU: {:?}", adapter.get_info());
    let limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: limits,
        },
        None,
    ))
    .unwrap();
    let eye = glam::Vec3::from_array(trees.placements[0].position) + glam::Vec3::new(35., 20., 35.);
    let direction = (glam::Vec3::from_array(trees.placements[0].position) + glam::Vec3::Y * 7.
        - eye)
        .normalize();
    let matrix = glam::Mat4::perspective_rh(60f32.to_radians(), 1., 1000., 0.1)
        * glam::Mat4::look_to_rh(eye, direction, glam::Vec3::Y);
    let mut uni = Vec::<f32>::from(matrix.to_cols_array());
    uni.extend_from_slice(&[
        direction.x,
        direction.y,
        direction.z,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        eye.x,
        eye.z,
        1.,
        0.,
        0.4,
        0.9,
        0.4,
        1.,
        0.5,
        0.5,
        1.,
        1.,
    ]);
    uni.extend_from_slice(&[0.; 12]);
    // Pad to the whole camera uniform and give it the `lod` lane: no bias, and
    // a gradient scale of 1 keeps the LOD this check measures.
    uni.resize(392, 0.);
    uni[389] = 1.;
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&uni),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let mut renderer = far_trees_gpu::FarTrees::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &trees,
        &uniform,
        &uniform,
        false,
    );
    renderer.update_lods(&queue, eye.to_array(), 1.);
    let size = wgpu::Extent3d {
        width: 256,
        height: 256,
        depth_or_array_layers: 1,
    };
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
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
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let depth = depth.create_view(&Default::default());
    let mut frames = Vec::new();
    for (time, strength) in [(0.0f32, 1.0f32), (1.0, 1.0), (0.0, 0.0), (1.0, 0.0)] {
        uni[30] = strength;
        uni[31] = time;
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 256 * 4,
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
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            renderer.draw(&mut pass, false);
        }
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
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
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
        let pixels = slice.get_mapped_range().to_vec();
        buffer.unmap();
        frames.push(pixels);
    }
    let changed = frames[0]
        .chunks_exact(4)
        .zip(frames[1].chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    let lit = frames[0]
        .chunks_exact(4)
        .filter(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
        .count();
    println!(
        "Animated GPU tree test: {lit} visible pixels, {changed} changed between t=0 and 1 second"
    );
    let static_changed = frames[2]
        .chunks_exact(4)
        .zip(frames[3].chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    println!("With wind disabled, changed pixels: {static_changed}");
    assert_eq!(static_changed, 0, "wind=0 must be stationary");
    if trees
        .types
        .iter()
        .flatten()
        .any(|t| t.lods.iter().any(|l| !l.rocking.is_empty()))
    {
        assert!(
            lit > 0 && changed > 0,
            "runtime rocking did not affect rendered pixels"
        );
    }
}
