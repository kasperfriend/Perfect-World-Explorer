//! Synthetic GPU regression: authored vegetation mip levels upload through FarTrees.
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
fn lod() -> trees::TreeLod {
    let v = |pos, uv| trees::Vert {
        pos,
        uv,
        normal: [0., -1., 0.],
    };
    let corners = [[0., -1., -1.], [0., 1., -1.], [0., 1., 1.], [0., -1., 1.]];
    let mut rocking = Vec::new();
    for c in corners {
        for frame in 0..64 {
            let shift = (frame as f32 * 0.25 * std::f32::consts::PI / 2.).sin();
            rocking.push([c[0], c[1] + shift, c[2]]);
        }
    }
    trees::TreeLod {
        branch: trees::Part {
            vertices: vec![
                v([-5., 0., 0.], [0., 0.]),
                v([5., 0., 0.], [1., 0.]),
                v([-5., 0., 5.], [0., 1.]),
                v([5., 0., 5.], [1., 1.]),
            ],
            indices: vec![0, 1, 2, 1, 3, 2],
        },
        frond: Default::default(),
        branch_alpha_ref: Some(84.),
        frond_alpha_ref: Some(255.),
        leaves: vec![trees::Leaf {
            center: [0., 2., 2.5],
            normal: None,
            rho: 0.,
            corners,
            uv: [[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
            cluster: 0,
        }],
        rocking,
        rocking_clusters: 1,
    }
}
fn main() {
    std::env::set_var("PW_SMALL_TREE_ATLAS", "1");
    let tex = image::RgbaImage::from_pixel(128, 128, image::Rgba([240, 190, 70, 255]));
    let trees = trees::Trees {
        types: vec![Some(trees::TreeMesh {
            lods: vec![lod(), lod(), lod()],
            bark: tex.clone(),
            composite: image::RgbaImage::from_pixel(128, 128, image::Rgba([20, 240, 30, 255])),
            bark_mips: (1..=7)
                .map(|level| {
                    image::RgbaImage::from_pixel(
                        128 >> level,
                        128 >> level,
                        image::Rgba([240, 190, 70, 255]),
                    )
                })
                .collect(),
            composite_mips: (1..=7)
                .map(|level| {
                    image::RgbaImage::from_pixel(
                        128 >> level,
                        128 >> level,
                        image::Rgba([20, 240, 30, 255]),
                    )
                })
                .collect(),
        })],
        placements: vec![scene::Tree {
            tree_type: 0,
            position: [0., 0., 0.],
        }],
    };
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
    let matrix = glam::Mat4::orthographic_rh(-6., 6., -1., 7., 100., 0.1)
        * glam::Mat4::look_at_rh(
            glam::Vec3::new(0., 2.5, 20.),
            glam::Vec3::new(0., 2.5, 0.),
            glam::Vec3::Y,
        );
    let mut uni = Vec::<f32>::from(matrix.to_cols_array());
    uni.extend_from_slice(&[
        0., 0., -1., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 1., 0., 1., 1., 0.,
        1., 1.,
    ]);
    uni.extend_from_slice(&[0.; 348]);
    // The camera uniform ends with the `lod` lane: no bias here, and a gradient
    // scale of 1 keeps the LOD these checks measure.
    uni.extend_from_slice(&[0., 1., 0., 0.]);
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&uni),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let mut renderer = far_trees_gpu::FarTrees::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        &trees,
        &uniform,
        &uniform,
        true,
    );
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
    let dv = depth.create_view(&Default::default());
    let mut frames = Vec::new();
    for (frame_index, (dist, strength, time)) in [
        (20., 0., 0.),
        (25., 0., 0.),
        (30., 0., 0.),
        (80., 0., 0.),
        (90., 0., 0.),
        (20., 1., 0.),
        (20., 1., 1.),
        (20., 0., 0.),
        (20., 0., 1.),
        (20., 0., 0.),
        (20., 0., 0.),
        (20., 0., 0.),
        (20., 0., 0.),
        (20., 0., 0.),
        (20., 0., 0.),
    ]
    .into_iter()
    .enumerate()
    {
        if frame_index == 9 || frame_index == 10 {
            let (a, d) = if frame_index == 9 {
                ([89., 115., 142.], [75., 88., 107.])
            } else {
                ([108., 133., 142.], [56., 61., 82.])
            };
            uni[40..44].copy_from_slice(&[a[0] / 255., a[1] / 255., a[2] / 255., 1.]);
            uni[44..48].copy_from_slice(&[d[0] / 255., d[1] / 255., d[2] / 255., 0.]);
            uni[48..52].copy_from_slice(&[-0.1524305, 0.619779, -0.76983047, 0.]);
        } else if frame_index == 12 || frame_index == 13 {
            uni[40..44].copy_from_slice(if frame_index == 12 {
                &[1., 1., 1., 1.]
            } else {
                &[4., 0., 0., 1.]
            });
            uni[44..48].copy_from_slice(&[0., 0., 0., 0.]);
            uni[48..52].copy_from_slice(&[0., 1., 0., 0.]);
        } else {
            uni[43] = 0.;
        }
        if frame_index == 14 {
            uni[380..384].copy_from_slice(&[0., 0.5, 1., 0.]);
            uni[384..388].copy_from_slice(&[0.5, 0.25, 0.75, 1.]);
        }
        uni[30] = strength;
        uni[31] = time;
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&uni));
        renderer.update_lods(&queue, [dist, 0., 0.], 1.);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
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
            renderer.draw(&mut pass, false);
        }
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
                    bytes_per_row: Some(512),
                    rows_per_image: Some(128),
                },
            },
            size,
        );
        queue.submit(Some(e.finish()));
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        frames.push(slice.get_mapped_range().to_vec());
        buffer.unmap();
    }
    let lit = frames[0].chunks_exact(4).filter(|p| p[0] > 0).count();
    assert!(lit > 1000, "tree missing: {lit}");
    for f in &frames[1..5] {
        assert_eq!(f, &frames[0], "LOD transition altered identical geometry");
    }
    assert_ne!(frames[5], frames[6], "animated leaf did not move");
    assert_eq!(frames[7], frames[8], "disabled wind still moves");
    assert_ne!(frames[9], frames[10], "region day/night RGB not applied");
    assert_eq!(
        frames[0], frames[11],
        "turning experimental lighting off changed the original tree output"
    );
    for i in [9, 10, 13] {
        for (p, white) in frames[i].chunks_exact(4).zip(frames[12].chunks_exact(4)) {
            for c in 0..3 {
                assert!(
                    p[c] <= white[c].saturating_add(1),
                    "regional mode over-brightened texture channel"
                );
            }
        }
    }
    assert_ne!(frames[12], frames[13], "regional hue was lost");
    let fogged = frames[14]
        .chunks_exact(4)
        .zip(frames[0].chunks_exact(4))
        .filter(|(_, old)| old[0] > 0)
        .filter(|(p, _)| {
            (p[0] as i32 - 55).abs() <= 1
                && (p[1] as i32 - 13).abs() <= 1
                && (p[2] as i32 - 133).abs() <= 1
        })
        .count();
    assert_eq!(fogged, lit, "tree per-pass fog");
    println!("Tree per-pass fog: {fogged} pixels match encoded RGB reference");
    for (index, a, d) in [
        (9, [89., 115., 142.], [75., 88., 107.]),
        (10, [108., 133., 142.], [56., 61., 82.]),
    ] {
        let expected: Vec<u8> = [20., 240., 30.]
            .into_iter()
            .enumerate()
            .map(|(c, tex)| {
                let rgb: [f32; 3] = std::array::from_fn(|i| {
                    (a[i] / 255. + d[i] / 255. * 0.619779f32).clamp(0., 1.)
                });
                let lum = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
                let illumination = lum * 0.75 + rgb[c] * 0.25;
                let encoded = tex / 255.0f32;
                let linear = if encoded <= 0.04045 {
                    encoded / 12.92
                } else {
                    ((encoded + 0.055) / 1.055).powf(2.4)
                };
                (linear * illumination * 255.).round() as u8
            })
            .collect();
        assert!(
            expected[1] < 200,
            "bright leaf clipped under region lighting"
        );
        let matched = frames[index]
            .chunks_exact(4)
            .filter(|p| (0..3).all(|c| (p[c] as i32 - expected[c] as i32).abs() <= 2))
            .count();
        assert!(
            matched > 10,
            "regional tree lighting did not match reference {expected:?}: {matched} pixels"
        );
        println!("regional tree frame {index}, expected {expected:?}, matched {matched} pixels");
    }
    println!("Tree LOD GPU test: {lit} pixels, same across boundaries; sampled leaf motion changes with time and stops at wind=0");
}
