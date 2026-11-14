//! Retained asset -> texture retry/cache -> material -> raster/update regression.
//! Synthetic asset, not a claim of world action scheduling or client screenshots.
#![allow(dead_code)]
#[path = "../angelica_pck.rs"]
mod angelica_pck;
#[path = "../animation.rs"]
mod animation;
#[path = "../client_regional_light.rs"]
mod client_regional_light;
// `gfx_owner` and `gfx_player` log through the shared channel registry, so this
// target needs the same module the main binary declares.
#[path = "../diagnostics.rs"]
mod diagnostics;
#[path = "../ecmodel.rs"]
mod ecmodel;
#[path = "../gfx_catalog.rs"]
mod gfx_catalog;
#[path = "../gfx_color_track.rs"]
mod gfx_color_track;
#[path = "../gfx_decal.rs"]
mod gfx_decal;
#[path = "../gfx_effect_gpu.rs"]
mod gfx_effect_gpu;
#[path = "../gfx_file.rs"]
mod gfx_file;
#[path = "../gfx_light.rs"]
mod gfx_light;
#[path = "../gfx_owner.rs"]
mod gfx_owner;
#[path = "../gfx_player.rs"]
mod gfx_player;
#[path = "../model_asset.rs"]
mod model_asset;
#[path = "../model_attachment.rs"]
mod model_attachment;
#[path = "../model_dds.rs"]
mod model_dds;
#[path = "../model_draw.rs"]
mod model_draw;
#[path = "../../tools/fixtures/model_draw.rs"]
mod model_draw_fixture;
#[path = "../model_draw_gpu.rs"]
mod model_draw_gpu;
#[path = "../model_light.rs"]
mod model_light;
#[path = "../model_material.rs"]
mod model_material;
#[path = "../model_playback.rs"]
mod model_playback;
#[path = "../model_queue.rs"]
mod model_queue;
#[path = "../model_receiver_class.rs"]
mod model_receiver_class;
#[path = "../model_receiver_gpu.rs"]
mod model_receiver_gpu;
#[path = "../model_texture_gpu.rs"]
mod model_texture_gpu;
#[path = "../pck.rs"]
mod pck;
#[path = "../scene.rs"]
mod scene;
#[path = "../scene_lights.rs"]
mod scene_lights;
#[path = "../skeleton.rs"]
mod skeleton;
#[path = "../skeleton_pose.rs"]
mod skeleton_pose;
#[path = "../skin.rs"]
mod skin;
#[path = "../skin_pose.rs"]
mod skin_pose;
#[path = "../sky_config.rs"]
mod sky_config;
#[path = "../sky_gpu.rs"]
mod sky_gpu;
#[path = "../world_models.rs"]
mod world_models;
#[path = "../world_models_gpu.rs"]
mod world_models_gpu;
trait TestDraw {
    fn needs_depth(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>);
}
impl TestDraw for model_draw_gpu::Draw {
    fn submit<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw(pass);
    }
}
impl TestDraw for world_models_gpu::World {
    fn needs_depth(&self) -> bool {
        true
    }
    fn submit<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw(pass);
    }
}
fn white_dds() -> Vec<u8> {
    let mut b = vec![0; 128];
    b[..4].copy_from_slice(b"DDS ");
    for (o, v) in [
        (4, 124u32),
        (8, 0xa1007),
        (12, 4),
        (16, 4),
        (28, 1),
        (76, 32),
        (80, 4),
        (108, 0x1000),
    ] {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    b[84..88].copy_from_slice(b"DXT1");
    b.extend([255, 255, 0, 0, 0, 0, 0, 0]);
    b
}
fn poses(asset: &model_asset::ModelAsset) -> Vec<model_asset::PosedMesh> {
    asset.pose_bind(glam::Mat4::IDENTITY).unwrap()
}
fn check(device: &wgpu::Device, queue: &wgpu::Queue, draw: &dyn TestDraw, expected: [[u8; 4]; 2]) {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 2,
            height: 1,
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
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let depth = draw.needs_depth().then(|| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: target.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
    });
    let depth_view = depth.as_ref().map(|d| d.create_view(&Default::default()));
    let mut e = device.create_command_encoder(&Default::default());
    {
        let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: depth_view.as_ref().map(|view| {
                wgpu::RenderPassDepthStencilAttachment {
                    view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        draw.submit(&mut p);
    }
    e.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::ImageCopyBuffer {
            buffer: &read,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
        },
        target.size(),
    );
    queue.submit(Some(e.finish()));
    let slice = read.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let bytes = slice.get_mapped_range();
    for i in 0..2 {
        for c in 0..4 {
            assert!(
                (bytes[i * 4 + c] as i32 - expected[i][c] as i32).abs() <= 1,
                "pixel {i} channel {c}: {} != {}",
                bytes[i * 4 + c],
                expected[i][c]
            );
        }
    }
}
fn main() {
    std::panic::set_hook(Box::new(|p| {
        eprintln!("::error::{}", p.to_string().replace('\n', "%0A"))
    }));
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
    // Validate the actual experimental sky shader after changing its visibility
    // uniform from a boolean to quantized alpha. This is compilation coverage,
    // not a native celestial material/image parity assertion.
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("production sky visibility shader validation"),
        source: wgpu::ShaderSource::Wgsl(sky_gpu::SHADER.into()),
    });
    let (mut asset, _) = model_draw_fixture::fixture();
    let mut material = asset.skins[0].data.materials[0].clone();
    material.two_sided = false;
    material.ambient = [0.25, 0.5, 0.75, 0.];
    material.diffuse = [0., 0., 0., 1.];
    asset.skins[0].data.materials[0] = material.clone();
    material.ambient = [0.75, 0.25, 0.5, 0.];
    asset.skins[0].data.materials.push(material);
    let source = asset.skins[0].data.meshes[0].clone();
    asset.skins[0].data.meshes.clear();
    for i in 0..2 {
        let mut m = source.clone();
        m.material = i;
        if i == 1 {
            m.rigid_bone = Some(0);
        } // final model slot with no bones
        m.indices = vec![0, 1, 2, 2, 1, 3];
        let left = i as f32 - 1.;
        let right = i as f32;
        m.vertices = [
            [left, -1., 0.5],
            [right, -1., 0.5],
            [left, 1., 0.5],
            [right, 1., 0.5],
        ]
        .into_iter()
        .map(|position| skin::Vertex {
            position,
            normal: [0., 0., 1.],
            uv: [0.5; 2],
            weights: [1., 0., 0., 0.],
            joints: [0; 4],
        })
        .collect();
        asset.skins[0].data.meshes.push(m);
    }
    let frame = model_draw_gpu::Frame {
        blend_enabled: true,
        matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
        lighting: model_material::Lighting {
            environment_ambient: [1.; 4],
            additional_ambient: [0.; 4],
            directional_diffuse: [0., 0., 0., 1.],
            point_ambient: [0.; 4],
            point_diffuse: [0.; 4],
            transparency: 0.,
            point_enabled: false,
        },
        direction: [0., 0., -1.],
        point_position: [0., 0., 10.],
        attenuation: [0., 0.4, 0.],
        eye_direction: [0., 0., -1.],
        directional_specular: [1.; 4],
        specular_variant: true,
        vertex_fog: 1.,
        fog_color: [0.; 4],
        fog_range: [0.; 4],
        clip_plane: [0.; 4],
        reverse_front: false,
        alpha_compare: 0,
        alpha_reference: 84. / 255.,
    };
    let sampler = device.create_sampler(&Default::default());
    let mut reads = 0;
    let draw = model_draw_gpu::Draw::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        None,
        false,
        &sampler,
        &asset,
        &poses(&asset),
        frame,
        |path| {
            reads += 1;
            Ok(if path.contains("tex_a/") {
                vec![0; 128]
            } else {
                white_dds()
            })
        },
    )
    .unwrap();
    assert_eq!(reads, 2);
    assert_eq!(draw.mesh_count(), 2);
    assert_eq!(draw.texture_count(), 1);
    // A BC1 DDS decoded into RGBA8 must retain its source queue category.
    assert_eq!(draw.alpha_texture(0), Some(false));
    assert_eq!(draw.alpha_texture(1), Some(false));
    check(
        &device,
        &queue,
        &draw,
        [[64, 128, 191, 255], [191, 64, 128, 255]],
    );
    // Independent instances must share immutable GPU resources, but changing
    // one instance's constants must never change its neighbor's lighting.
    let mut receivers = model_draw_gpu::ReceiverCache::default();
    let mut shared = std::collections::HashMap::new();
    let mut uploads = 0;
    let mut copies = Vec::new();
    let mut first_pipeline_count = 0;
    for instance in 0..2 {
        copies.push(
            model_draw_gpu::Draw::with_textures(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8Unorm,
                None,
                false,
                &sampler,
                &asset,
                &poses(&asset),
                frame,
                &mut receivers,
                |path| {
                    if let Some(texture) = shared.get(path) {
                        return Ok((std::sync::Arc::clone(texture), false));
                    }
                    uploads += 1;
                    let texture = std::sync::Arc::new(model_texture_gpu::upload(
                        &device,
                        &queue,
                        &white_dds(),
                        model_texture_gpu::UploadMode::Auto,
                    )?);
                    shared.insert(path.to_string(), texture.clone());
                    Ok((texture, false))
                },
            )
            .unwrap(),
        );
        if instance == 0 {
            first_pipeline_count = receivers.len();
        } else {
            assert_eq!(
                receivers.len(),
                first_pipeline_count,
                "second placement reuses every pipeline variant"
            );
        }
    }
    assert_eq!(uploads, 1, "one texture allocation across two placements");
    assert_eq!(
        first_pipeline_count, 2,
        "fixture exercises both base and alpha pipelines"
    );
    let mut half = frame;
    half.lighting.environment_ambient = [0.5; 4];
    copies[0].update_constants(&queue, &asset, half).unwrap();
    check(
        &device,
        &queue,
        &copies[0],
        [[32, 64, 96, 255], [96, 32, 64, 255]],
    );
    check(
        &device,
        &queue,
        &copies[1],
        [[64, 128, 191, 255], [191, 64, 128, 255]],
    );
    println!(
        "::notice::shared model textures/pipelines and independent constants-only updates passed"
    );
    let mut dim = frame;
    dim.lighting.environment_ambient = [0.5; 4];
    draw.update(&queue, &asset, &poses(&asset), dim).unwrap();
    let expected = [[32, 64, 96, 255], [96, 32, 64, 255]];
    check(&device, &queue, &draw, expected);
    let mut bad = poses(&asset);
    bad[1].vertices[0].normal[0] = f32::NAN;
    assert!(draw.update(&queue, &asset, &bad, frame).is_err());
    check(&device, &queue, &draw, expected);
    let mut moved = poses(&asset);
    for v in &mut moved[0].vertices {
        v.position[0] -= 4.;
    }
    draw.update(&queue, &asset, &moved, dim).unwrap();
    check(&device, &queue, &draw, [[0; 4], expected[1]]);
    // Actual pose -> retained GPU upload: the rigid ID remains a direct
    // skeleton index while the weighted mesh keeps its v9 virtual slot.
    let t = |x| glam::Mat4::from_translation(glam::Vec3::X * x);
    asset.skeleton.bones.push(skeleton::Bone {
        name: "rigid-root".into(),
        flags: 0,
        parent: -1,
        first_joint: -1,
        relative: t(5.).to_cols_array(),
        initial: t(-1.).to_cols_array(),
        children: vec![],
    });
    draw.update(&queue, &asset, &poses(&asset), dim).unwrap();
    check(&device, &queue, &draw, [expected[0], [0; 4]]);
    asset.skins[0].data.meshes[1].rigid_bone = Some(1);
    draw.update(&queue, &asset, &poses(&asset), dim).unwrap();
    check(&device, &queue, &draw, expected);
    asset.skeleton.bones.clear();
    asset.skins[0].data.meshes[1].rigid_bone = Some(0);
    // Descriptor RGB selects the clamped scaled-material method; alpha remains
    // separately controlled. Restore original method before the world fixture.
    asset.descriptor.original_color = Some(0xff804020);
    draw.update(&queue, &asset, &poses(&asset), dim).unwrap();
    check(&device, &queue, &draw, [[16, 16, 12, 255], [48, 8, 8, 255]]);
    asset.descriptor.original_color = None;
    // Queue routing: a non-default authored blend pair reaches the retail alpha
    // list instead of failing the instance, and the alpha receiver is built from
    // the authored factors with the initial-AABB bounds the sort needs.
    asset.descriptor.destination_blend = Some(2);
    let alpha_frame = model_draw_gpu::Frame {
        blend_enabled: false,
        lighting: model_material::Lighting {
            transparency: -1.,
            ..frame.lighting
        },
        ..frame
    };
    let alpha_draw = model_draw_gpu::Draw::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        None,
        false,
        &sampler,
        &asset,
        &poses(&asset),
        alpha_frame,
        |_| Ok(white_dds()),
    )
    .unwrap();
    assert_eq!(alpha_draw.alpha_meshes(), &[0, 1]);
    assert_eq!(alpha_draw.mesh_count(), 2);
    assert!(alpha_draw.mesh_bounds(0).is_some());
    // An unsupported authored factor stays an explicit error, not a substitute.
    asset.descriptor.destination_blend = Some(11);
    assert!(model_draw_gpu::Draw::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        None,
        false,
        &sampler,
        &asset,
        &poses(&asset),
        alpha_frame,
        |_| Ok(white_dds()),
    )
    .is_err());
    asset.descriptor.destination_blend = None;
    // World materials with authored specular are no longer rejected now
    // that actual regional directional specular reaches the frame binding.
    for material in &mut asset.skins[0].data.materials {
        material.specular = [0.2, 0.1, 0.05, 1.];
    }
    let playback = model_playback::Playback::new(&asset, None).unwrap();
    let scene_assets = world_models::Assets {
        assets: vec![asset],
        instances: vec![world_models::Instance {
            placement: scene::SceneModel {
                export_id: 1,
                position: [0.; 3],
                direction: [0., 0., 1.],
                up: [0., 1., 0.],
                path: "models/a/a.ecm".into(),
                action: "absent".into(),
            },
            asset: 0,
            action: None,
            playback: Ok(playback),
        }],
        failures: vec![],
        unloaded: vec![],
    };
    let mut opaque = frame;
    opaque.blend_enabled = false;
    opaque.directional_specular = [0., 0., 0., 1.];
    opaque.lighting.transparency = -1.;
    opaque.alpha_compare = 5;
    opaque.alpha_reference = 84. / 255.;
    let mut opaque_dim = dim;
    opaque_dim.blend_enabled = false;
    opaque_dim.directional_specular = [0., 0., 0., 1.];
    opaque_dim.lighting.transparency = -1.;
    opaque_dim.alpha_compare = 5;
    opaque_dim.alpha_reference = 84. / 255.;
    let mut world = world_models_gpu::World::with_reader(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        scene_assets,
        |_| {
            let mut b = white_dds();
            b[84..88].copy_from_slice(b"DXT3");
            b.splice(128..128, [0x88; 8]);
            Ok(b)
        },
    )
    .unwrap();
    assert_eq!(world.status(), (0, 0, 1));
    world.update(
        &device,
        &queue,
        opaque,
        [0.; 3],
        &scene_lights::SceneLights::default(),
        0.,
        0,
    );
    // The ordinary opaque ECM must not put anything in the retail alpha list.
    assert!(world.alpha_order().is_empty());
    check(
        &device,
        &queue,
        &world,
        [[64, 128, 191, 136], [191, 64, 128, 136]],
    );
    // Clock -> native packed regional specular -> material constants ->
    // ordinary weighted AND rigid world draws -> rasterized RGBA pixels.
    for (hour, expected) in [
        (12., [25, 13, 6, 136]),
        (0., [8, 4, 2, 136]),
        (21., [0, 0, 0, 136]),
    ] {
        let mut specular = opaque;
        specular.lighting.environment_ambient = [0., 0., 0., 1.];
        let regional = client_regional_light::evaluate(hour, [0.5; 3], [0.2; 3]);
        specular.directional_specular = [
            regional.specular[0],
            regional.specular[1],
            regional.specular[2],
            1.,
        ];
        world.update(
            &device,
            &queue,
            specular,
            [0.; 3],
            &Default::default(),
            0.,
            0,
        );
        check(&device, &queue, &world, [expected; 2]);
    }
    world.update(
        &device,
        &queue,
        opaque_dim,
        [0.; 3],
        &scene_lights::SceneLights::default(),
        0.,
        17,
    );
    check(
        &device,
        &queue,
        &world,
        [[32, 64, 96, 136], [96, 32, 64, 136]],
    );
    // Main projection mirrors X. Correct front interpretation is mandatory for
    // one-sided authored materials, not just the synthetic identity camera.
    let mut mirrored = opaque_dim;
    mirrored.matrix = glam::Mat4::from_scale(glam::Vec3::new(-1., 1., 1.)).to_cols_array_2d();
    mirrored.reverse_front = true;
    world.update(
        &device,
        &queue,
        mirrored,
        [0.; 3],
        &Default::default(),
        0.,
        0,
    );
    check(
        &device,
        &queue,
        &world,
        [[96, 32, 64, 136], [32, 64, 96, 136]],
    );
    mirrored.reverse_front = false;
    world.update(
        &device,
        &queue,
        mirrored,
        [0.; 3],
        &Default::default(),
        0.,
        0,
    );
    check(&device, &queue, &world, [[0; 4]; 2]);
    // Reflection also mirrors Y: the two flips restore the original winding.
    mirrored.matrix = glam::Mat4::from_scale(glam::Vec3::new(-1., -1., 1.)).to_cols_array_2d();
    world.update(
        &device,
        &queue,
        mirrored,
        [0.; 3],
        &Default::default(),
        0.,
        0,
    );
    check(
        &device,
        &queue,
        &world,
        [[96, 32, 64, 136], [32, 64, 96, 136]],
    );
    // Clip in original world space, even when the camera is mirrored.
    mirrored.clip_plane = [1., 0., 0., 0.];
    world.update(
        &device,
        &queue,
        mirrored,
        [0.; 3],
        &Default::default(),
        0.,
        0,
    );
    check(&device, &queue, &world, [[96, 32, 64, 136], [0; 4]]);
    mirrored.clip_plane = [0.; 4];
    mirrored.fog_range = [0., 2., 1., 0.];
    mirrored.fog_color = [0.; 4];
    world.update(
        &device,
        &queue,
        mirrored,
        [0.; 3],
        &Default::default(),
        0.,
        0,
    );
    check(
        &device,
        &queue,
        &world,
        [[48, 16, 32, 136], [16, 32, 48, 136]],
    );
    let lights = scene_lights::SceneLights {
        lights: vec![scene_lights::SceneLight {
            position: [0., 0., 0.5],
            range: 2.,
            stored_attenuation: [0., 0.4, 0.],
            diffuse: [0., 0., 0., 1.],
            ambient: [0.5, 0.5, 0.5, 1.],
            bounds_center: [0., 0., 0.5],
            bounds_half_extent: [2.; 3],
            bounds_min: [-2., -2., -1.5],
            bounds_max: [2., 2., 2.5],
            flags: 1,
        }],
    };
    world.update(&device, &queue, opaque_dim, [13., 0., 0.], &lights, 0., 17);
    check(
        &device,
        &queue,
        &world,
        [[32, 64, 96, 136], [96, 32, 64, 136]],
    );
    world.update(&device, &queue, opaque_dim, [13., 0., 0.], &lights, 1., 17);
    check(
        &device,
        &queue,
        &world,
        [[64, 128, 191, 136], [191, 64, 128, 136]],
    );
    // Publication -> scene query -> weighted/rigid receiver GPU draw. Match
    // the independently specified static .5 ambient case above: .2*2.5=.5;
    // material diffuse RGB is zero, so the dynamic diffuse is irrelevant.
    let published = scene_lights::DynamicLight {
        position: [0., 0., 0.5],
        radius: 2.,
        color: [2.5, 2.5, 2.5, 1.],
    };
    world.publish_dynamic_light(
        7,
        scene_lights::DynamicLight {
            color: [0.; 4],
            ..published
        },
    );
    world.publish_dynamic_light(7, published);
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        0,
    );
    check(
        &device,
        &queue,
        &world,
        [[64, 128, 191, 136], [191, 64, 128, 136]],
    );
    // A reflected/clipped pass in the SAME frame retains the captured light.
    let mut published_reflection = opaque_dim;
    published_reflection.matrix =
        glam::Mat4::from_scale(glam::Vec3::new(-1., -1., 1.)).to_cols_array_2d();
    published_reflection.clip_plane = [1., 0., 0., 0.];
    world.update_pass(
        &device,
        &queue,
        published_reflection,
        [13., 0., 0.],
        &Default::default(),
        0.,
        999,
        false,
    );
    check(&device, &queue, &world, [[191, 64, 128, 136], [0; 4]]);
    // Consumed membership is reset, not a persistent-active-light list.
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        0,
    );
    check(
        &device,
        &queue,
        &world,
        [[32, 64, 96, 136], [96, 32, 64, 136]],
    );
    world.publish_dynamic_light(7, published);
    world.unregister_dynamic_light(7);
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        0,
    );
    check(
        &device,
        &queue,
        &world,
        [[32, 64, 96, 136], [96, 32, 64, 136]],
    );
    // Decoded text element -> real lifetime/visibility/priority execution ->
    // publication -> selection -> weighted AND rigid receiver rendering.
    let source = "MOXTVersion: 37\nGFXELEMENTCOUNT: 1\nGFXELEMENTID: 130\nRepeatCount: 1\nRepeatDelay: 0\nBindEle: \nDummyEle: \nIsDummy: 0\nMatchGrnd: 0\nLightType: 1\nPriority: 4\nRange: 2\nStartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: 1000\nPosition: 0, 0, 0.5\nColor: -1\nScale: 1\nDirection: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 0\n";
    let mut emitter = gfx_light::Emitter::new(gfx_light::parse(source).unwrap().pop().unwrap());
    let parent = glam::Mat4::IDENTITY.to_cols_array();
    assert!(world
        .tick_gfx_light(48, &mut emitter, 0, true, 4, parent, None)
        .unwrap());
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        0,
    );
    // Native packed white -> point ambient .2; material ambient*(.5+.2).
    // Specular variant clamps attenuation below 1, and diffuse RGB is zero.
    check(
        &device,
        &queue,
        &world,
        [[45, 89, 134, 136], [134, 45, 89, 136]],
    );
    assert!(!world
        .tick_gfx_light(48, &mut emitter, 200, false, 4, parent, None)
        .unwrap());
    assert!(!world
        .tick_gfx_light(48, &mut emitter, 200, true, 3, parent, None)
        .unwrap());
    assert!(!world
        .tick_gfx_light(48, &mut emitter, 600, true, 4, parent, None)
        .unwrap());
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        0,
    );
    check(
        &device,
        &queue,
        &world,
        [[32, 64, 96, 136], [96, 32, 64, 136]],
    );
    // Production producer path: actual ECWLD scene effect -> owner lifecycle ->
    // lazy GFX root load -> publication -> scene query -> receiving pixels.
    let owner_source = "MOXTVersion: 37\nGFXELEMENTCOUNT: 1\nGFXELEMENTID: 130\nRepeatCount: 1\nRepeatDelay: 0\nBindEle: \nDummyEle: \nIsDummy: 0\nMatchGrnd: 0\nLightType: 1\nPriority: 4\nRange: 2\nStartTime: 0\nKEYPOINTCOUNT: 1\nInterpolateMode: 0\nTimeSpan: -1\nPosition: 0, 0, 0.5\nColor: -1\nScale: 1\nDirection: 0, 0, 0, 1\nRad_2D: 0\nCtrlMethodCount: 0\n";
    world.attach_gfx_reader(move |path: &str| {
        assert_eq!(path, "gfx/scene/lamp.gfx");
        Ok(owner_source.to_string())
    });
    let scene_fixture = scene::Scene {
        width: 64.,
        length: 64.,
        block_size: 32.,
        rows: 1,
        cols: 1,
        tree_types: vec![],
        grass_types: vec![],
        blocks: vec![scene::Block {
            row: 0,
            col: 0,
            trees: vec![],
            water: vec![],
            grass: vec![],
            ornaments: vec![],
            boxes: vec![],
            effects: vec![scene::Effect {
                index: 0,
                // The record's own id word; the shipped map gives every one of
                // its 2,275 placements a distinct value.
                export_id: 1,
                scale: 1.,
                position: [0., 0., 0.5],
                direction: [0., 0., 1.],
                up: [0., 1., 0.],
                speed: 1.,
                valid_time: 2,
                alpha: 1.,
                attenuation: false,
                path: "scene\\lamp.gfx".into(),
            }],
            models: vec![],
            critters: vec![],
            beziers: vec![],
            sounds: vec![],
        }],
    };
    assert_eq!(world.install_scene_effects(&scene_fixture), 1);
    world.set_view_center(0., 0., 128.);
    // One root loads per update and the load frame publishes nothing.
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        0,
    );
    assert_eq!(world.gfx_status().live, 1);
    check(
        &device,
        &queue,
        &world,
        [[32, 64, 96, 136], [96, 32, 64, 136]],
    );
    // The next advancing update ticks the owner, publishes and consumes.
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        17,
    );
    check(
        &device,
        &queue,
        &world,
        [[45, 89, 134, 136], [134, 45, 89, 136]],
    );
    // Region unload removes the producer, so its light cannot survive.
    world.set_view_center(4096., 4096., 16.);
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        17,
    );
    check(
        &device,
        &queue,
        &world,
        [[32, 64, 96, 136], [96, 32, 64, 136]],
    );
    println!("::notice::scene effect owner passed: ECWLD placement -> one-root-per-update load -> publication -> scene query -> receiving pixels, and region unload removes the light");
    assert_eq!(world.status(), (1, 0, 1));
    // Terrain residency must release model geometry, not only deactivate FX.
    assert!(world.residency_cache_counts().0 > 0);
    assert!(world.residency_cache_counts().1 > 0);
    world.set_resident_bounds(Some([4000., 4100., 4000., 4100.]));
    assert_eq!(
        world.residency_cache_counts(),
        (0, 0),
        "cold textures/pipelines were retained"
    );
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        0,
    );
    assert_eq!(world.status(), (0, 0, 1));
    check(&device, &queue, &world, [[0; 4]; 2]);
    world.set_resident_bounds(Some([-100., 100., -100., 100.]));
    world.update(
        &device,
        &queue,
        opaque_dim,
        [13., 0., 0.],
        &Default::default(),
        0.,
        0,
    );
    assert_eq!(world.status(), (1, 0, 1));
    check(
        &device,
        &queue,
        &world,
        [[32, 64, 96, 136], [96, 32, 64, 136]],
    );
    assert!(world.residency_cache_counts().0 > 0);
    let retained = world.residency_cache_counts();
    world.set_resident_bounds(Some([-101., 101., -101., 101.]));
    assert_eq!(
        world.residency_cache_counts(),
        retained,
        "live draw's shared resources evicted"
    );
    world.set_resident_bounds(None);
    let mut unsupported = opaque_dim;
    unsupported.blend_enabled = true;
    world.update(
        &device,
        &queue,
        unsupported,
        [0.; 3],
        &Default::default(),
        0.,
        0,
    );
    assert_eq!(world.status(), (0, 1, 1));
    world.update(
        &device,
        &queue,
        opaque_dim,
        [0.; 3],
        &Default::default(),
        0.,
        0,
    );
    assert_eq!(world.status(), (0, 1, 1)); // rejected instances are not retried as stand-ins
    println!("::notice::world instance pipeline passed: source placement basis, no-action bind pose, authored texture/material, reversed-depth attachment, and updated regional lighting");
    println!("::notice::retained model draw passed: two material meshes, shared authored DDS with decode-failure retry, frame update, rejected-update atomicity, and posed geometry movement");
}
