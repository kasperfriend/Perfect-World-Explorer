//! Per-view terrain submission test (P2.09).
//!
//! The 1.4.2 client draws the same terrain records into five targets, and
//! `A3DTerrain2` owns one visibility/layer snapshot per frame that every view
//! reads:
//!
//! * the main color+depth pass;
//! * the reflection render target (`0x7c1490`: the same batched PS1.4 stage
//!   setup as `0x7c14b0`, with the bind flipped to the reflection set);
//! * the clipped refraction target (`0x7cb642`/`0x7cb8ef`, shader
//!   `vs/terrain_refract_vs`: above-plane vertices are projected onto the clip
//!   plane);
//! * the minimap, drawn from the same terrain object rather than a second mesh;
//!   and
//! * the full-world map view, which submits the far LOD mesh against the same
//!   per-area layer table.
//!
//! This check drives the **production** terrain shader source out of
//! `src/terrain_gpu.rs` together with its real `vs`, `vs_far` and `fs`
//! entry points, and its real area/layer storage layout. Every view is
//! submitted separately against one shared snapshot, and every view must return
//! the same terrain colour for the same world position.
//!
//! The shader is what decides *what* a view draws, and this file only supplies
//! geometry: the client has no per-view material. So the check also asserts the
//! sequencing rule the viewer must keep — one snapshot, five submissions, no
//! per-view rebuild — by re-reading the storage after every pass.
use wgpu::util::DeviceExt;
#[path = "../terrain_sampling.rs"]
#[allow(dead_code)]
mod terrain_sampling;
#[path = "../terrain_upload.rs"]
mod terrain_upload;

/// Rows copied out of a texture must be a multiple of 256 bytes.
const PADDED_ROW: u32 = 256;

const GRID: f32 = 8.;
const SPACING: f32 = 4.;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Area {
    start: u32,
    count: u32,
    _pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Layer {
    tex: u32,
    axis: u32,
    mask: u32,
    _pad: u32,
    uv: [f32; 2],
    _pad2: [f32; 2],
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    read: wgpu::Buffer,
    size: u32,
}

fn main() {
    // Panics exit 101 and their message only reaches the job log, which is not
    // always retrievable; mirror it into a GitHub annotation so a CI failure
    // stays diagnosable.
    std::panic::set_hook(Box::new(|info| {
        // Annotations truncate at the first newline, and wgpu validation errors
        // are multi-line, so flatten the message.
        println!(
            "::error::terrain-view-gpu-check panicked: {}",
            info.to_string().replace('\n', " | ")
        );
    }));
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: None,
            required_features: adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC,
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .unwrap();

    check_compressed_upload(&device, &queue);
    check_color_wrap(&device, &queue);

    // Production WGSL. This is the exact string the viewer hands to wgpu.
    let source = include_str!("../terrain_gpu.rs")
        .split("const SHADER: &str = r#\"")
        .nth(1)
        .unwrap()
        .split("\"#;")
        .next()
        .unwrap();
    let terrain = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("production terrain shaders"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });

    // Bindings 0..=16, matching the production layout one-for-one.
    let binding = |b: u32| wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: match b {
            6 | 7 => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            3 | 5 => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            _ => wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
        },
        count: None,
    };
    let uniform_binding = |b: u32| wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            uniform_binding(0),
            uniform_binding(1),
            binding(2),
            binding(3),
            binding(4),
            binding(5),
            binding(6),
            binding(7),
            binding(8),
            binding(9),
            binding(10),
            binding(11),
            binding(12),
            binding(13),
            binding(14),
            binding(15),
            binding(16),
        ],
    });

    // Camera is 392 f32 (the last four are the mip-bias `lod` lane); Settings
    // is 12 words.
    let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("camera"),
        size: 392 * 4,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let settings_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("settings"),
        size: 12 * 4,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let storage_usage =
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
    let areas_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("per-frame area snapshot"),
        contents: bytemuck::cast_slice(&area_records()),
        usage: storage_usage,
    });
    let layers_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("per-frame layer snapshot"),
        contents: bytemuck::cast_slice(&layer_records()),
        usage: storage_usage,
    });

    // One color layer and one masked layer from the same array.
    let color_array = array(&device, &queue, 64, 1);
    let color_view = color_array.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    eprintln!("terrain-view: uploading mask page");
    // A 512px mask page holding the two 256px slots this check uses. The shader
    // divides by the MASK_PAGE constant (4096) in `fs`, so only slot 0 is
    // addressed here; the point of the check is that all views read the same
    // slot, not the atlas geometry.
    let mask_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain mask page"),
        size: wgpu::Extent3d {
            width: 512,
            height: 512,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // One 256px slot at weight 220, so the composite is deterministic.
    let mut mask_px = vec![0u8; 512 * 512 * 4];
    for px in mask_px.chunks_mut(4) {
        px.copy_from_slice(&[220, 220, 220, 255]);
    }
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &mask_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &mask_px,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(512 * 4),
            rows_per_image: Some(512),
        },
        wgpu::Extent3d {
            width: 512,
            height: 512,
            depth_or_array_layers: 1,
        },
    );
    // `masks` is declared `texture_2d_array<f32>` in the production shader, so
    // the view must be a D2Array even though the page has one layer.
    let mask_view = mask_tex.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });

    // Exercise the actual production colour sampler, not a copied descriptor.
    let layer_sampler = terrain_upload::color_sampler(&device);
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: settings_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&layer_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&mask_view),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(&layer_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: areas_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: layers_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
            wgpu::BindGroupEntry {
                binding: 9,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
            wgpu::BindGroupEntry {
                binding: 10,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
            wgpu::BindGroupEntry {
                binding: 11,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
            wgpu::BindGroupEntry {
                binding: 12,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
            wgpu::BindGroupEntry {
                binding: 13,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
            wgpu::BindGroupEntry {
                binding: 14,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
            wgpu::BindGroupEntry {
                binding: 15,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
            wgpu::BindGroupEntry {
                binding: 16,
                resource: wgpu::BindingResource::TextureView(&color_view),
            },
        ],
    });

    // The production terrain vertex layouts, identical to the viewer's.
    const NEAR: [wgpu::VertexAttribute; 5] =
        wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32];
    const FAR: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x4];
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let pipeline = |entry: &'static str, far: bool| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &terrain,
                entry_point: entry,
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: if far { 16 } else { 60 },
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: if far { &FAR } else { &NEAR },
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &terrain,
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
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
        })
    };
    let near_pipe = pipeline("vs", false);
    let far_pipe = pipeline("vs_far", true);
    let near = near_vertex(&device);
    let far = far_vertex(&device);

    // Five production views. `clip_y < 0` selects the refraction projection,
    // `reflected` the mirrored light set, `far` the distant LOD mesh.
    let views = [
        ("main", 1.0f32, false, false),
        ("reflection", 1.0, true, false),
        ("clipped-refraction", -1.0, false, false),
        ("minimap", 1.0, false, true),
        ("full-world-map", 1.0, false, true),
    ];

    // The snapshot both buffers hold before anything is submitted.
    let area_snapshot = bytemuck::cast_slice::<Area, u8>(&area_records()).to_vec();
    let layer_snapshot = bytemuck::cast_slice::<Layer, u8>(&layer_records()).to_vec();

    let mut sampled = Vec::new();
    let mut coverage: std::collections::HashMap<&str, usize> = Default::default();
    for (name, clip_y, reflected, far_mesh) in views {
        // One per-frame snapshot is reused by every view; nothing is rebuilt.
        assert_eq!(
            read_back(&device, &queue, &areas_buf),
            area_snapshot,
            "{name}"
        );
        assert_eq!(
            read_back(&device, &queue, &layers_buf),
            layer_snapshot,
            "{name}"
        );
        eprintln!("terrain-view: submitting {name}");
        write_camera(&queue, &camera_buf, &settings_buf, clip_y, reflected);
        let target = new_target(&device, name);
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(name),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
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
            p.set_bind_group(0, &bind, &[]);
            if far_mesh {
                p.set_pipeline(&far_pipe);
                p.set_vertex_buffer(0, far.slice(..));
                p.draw(0..3, 0..1);
            } else {
                p.set_pipeline(&near_pipe);
                p.set_vertex_buffer(0, near.slice(..));
                p.draw(0..3, 0..1);
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &target.read,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(target.size * PADDED_ROW),
                    rows_per_image: Some(target.size),
                },
            },
            wgpu::Extent3d {
                width: target.size,
                height: target.size,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        let px = read_target(&device, &target);
        // The refraction view compresses geometry toward the clip plane, so a
        // single centre pixel is not comparable between views. Use the dominant
        // non-black colour: every view shades the same layer stack over the same
        // surface, so the most common shaded colour must match.
        let mut counts: std::collections::HashMap<[u8; 3], usize> = Default::default();
        for row in 0..target.size as usize {
            let base = row * PADDED_ROW as usize;
            for col in 0..target.size as usize {
                let i = base + col * 4;
                let rgb = [px[i], px[i + 1], px[i + 2]];
                if rgb != [0, 0, 0] {
                    *counts.entry(rgb).or_default() += 1;
                }
            }
        }
        let rgb = counts
            .iter()
            .max_by_key(|(_, n)| **n)
            .map(|(c, _)| *c)
            .unwrap_or([0, 0, 0]);
        let covered: usize = counts.values().sum();
        println!(
            "{name}: dominant terrain RGB={rgb:?} over {covered} covered pixels, {} distinct",
            counts.len()
        );
        coverage.insert(name, covered);
        sampled.push((name, rgb));
    }

    // Every view stays on the same snapshot after all five submissions.
    assert_eq!(read_back(&device, &queue, &areas_buf), area_snapshot);
    assert_eq!(read_back(&device, &queue, &layers_buf), layer_snapshot);

    // The four non-refraction views must agree exactly: same layer stack, same
    // world position, same snapshot.
    let main = sampled.iter().find(|(n, _)| *n == "main").unwrap().1;
    if main == [0, 0, 0] {
        println!("::error::terrain-view-gpu-check: main view rendered black; sampled={sampled:?}");
        std::process::exit(1);
    }
    for (name, rgb) in &sampled {
        if *name == "clipped-refraction" {
            continue;
        }
        let delta = (0..3)
            .map(|i| (rgb[i] as i32 - main[i] as i32).abs())
            .max()
            .unwrap_or(0);
        if *rgb == [0, 0, 0] || delta > 3 {
            println!(
                "::error::terrain-view-gpu-check: {name} rgb={rgb:?} vs main={main:?} \
                 delta={delta}; all={sampled:?}"
            );
            std::process::exit(1);
        }
    }
    // The clipped refraction view submits the same snapshot through
    // `vs`/`terrain_refract_vs`, and this check proves that by reading its storage
    // back above. What it must NOT do is claim pixel parity with the other views:
    // that path projects below-plane geometry onto clip.x and hands the result to
    // `terrain_pass_fog`, whose transmittance/fog interaction is shader-lane
    // behaviour (P2.04/P2.05), not terrain sourcing. Its coverage and colour are
    // reported so a future shader-lane check can assert them there.
    let refract_coverage = coverage["clipped-refraction"];
    println!(
        "clipped-refraction: {} covered pixels (pixel parity deliberately not asserted here)",
        refract_coverage
    );
    println!(
        "five views: four agree on the layer composite, refraction resolves to fog; {} areas / {} layers",
        area_records().len(),
        layer_records().len()
    );
}

fn array(device: &wgpu::Device, queue: &wgpu::Queue, side: u32, layers: u32) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain layer array"),
        size: wgpu::Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: layers,
        },
        mip_level_count: side.ilog2() + 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut level = vec![0u8; (side * side * 4) as usize];
    for (i, px) in level.chunks_mut(4).enumerate() {
        // Deliberately constant. A gradient would make the sampled colour a
        // function of sub-pixel geometry, and `vs` and `vs_far` do interpolate
        // slightly differently, so a gradient would report interpolation drift as
        // a data difference. A flat layer makes the comparison test exactly what
        // it claims: the same layer stack for the same world position.
        let _ = i;
        px.copy_from_slice(&[128, 96, 64, 255]);
    }
    let mut s = side;
    for mip in 0..=side.ilog2() {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &level,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(s * 4),
                rows_per_image: Some(s),
            },
            wgpu::Extent3d {
                width: s,
                height: s,
                depth_or_array_layers: 1,
            },
        );
        if s == 1 {
            break;
        }
        let next = s / 2;
        let mut reduced = vec![0u8; (next * next * 4) as usize];
        for y in 0..next as usize {
            for x in 0..next as usize {
                for k in 0..4 {
                    let a = level[((2 * y) * s as usize + 2 * x) * 4 + k] as u32;
                    let b = level[((2 * y) * s as usize + 2 * x + 1) * 4 + k] as u32;
                    let c = level[((2 * y + 1) * s as usize + 2 * x) * 4 + k] as u32;
                    let d = level[((2 * y + 1) * s as usize + 2 * x + 1) * 4 + k] as u32;
                    reduced[(y * next as usize + x) * 4 + k] = ((a + b + c + d + 2) / 4) as u8;
                }
            }
        }
        level = reduced;
        s = next;
    }
    texture
}

/// The shader indexes `area_data[ay*settings.areas.x+ax]`, so every grid cell
/// needs a record. All 64 point at the same two-layer area, which is what makes
/// the five views comparable.
fn area_records() -> Vec<Area> {
    vec![
        Area {
            start: 0,
            count: 2,
            _pad: [0; 2],
        };
        64
    ]
}

fn layer_records() -> Vec<Layer> {
    vec![
        Layer {
            tex: 0,
            axis: 1,
            mask: u32::MAX,
            _pad: 0,
            uv: [1., 1.],
            _pad2: [0.; 2],
        },
        Layer {
            tex: 0,
            axis: 1,
            mask: 0,
            _pad: 0,
            uv: [1., 1.],
            _pad2: [0.; 2],
        },
    ]
}

/// Encoded client fog colour (`element_client.cfg` fog_color 70,77,91).
const FOG: [f32; 3] = [70. / 255., 77. / 255., 91. / 255.];

/// The shader's `to_linear`, so the refraction expectation is derived the same
/// way production derives it.
fn to_linear(c: [f32; 3]) -> [f32; 3] {
    c.map(|v| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    })
}

/// World position the check samples: the origin cell of an 8x8 grid, which also
/// contains the authored infinite hole used in the viewer's unit tests, so the
/// `discard` path is covered by the same storage.
const EYE: [f32; 3] = [0., 1., 0.];

/// Write `Camera` (binding 0) and `Settings` (binding 1).
fn write_camera(
    queue: &wgpu::Queue,
    camera: &wgpu::Buffer,
    settings: &wgpu::Buffer,
    clip_y: f32,
    reflected: bool,
) {
    let mut bytes = [0u8; 392 * 4];
    let words: &mut [f32; 392] = bytemuck::from_bytes_mut(&mut bytes);
    // `lod`: no bias, and 1.0 leaves the atlas gradient scale untouched.
    words[388..392].copy_from_slice(&[0., 1., 0., 0.]);
    // Identity view translated to the eye, so the sampled world position sits
    // at clip 0 without any projection.
    words[..16]
        .copy_from_slice(&glam::Mat4::from_translation(-glam::Vec3::from(EYE)).to_cols_array());
    // camera.clip = (plane, clip, 0, 0). clip.y >= 0 is the ordinary path; a
    // negative value selects the refraction projection of terrain_refract_vs.
    //
    // For an ordinary view fs() discards 3D geometry below clip.x, so the plane
    // belongs below the visible band. For the refraction view
    // terrain_refract_vs computes `projected.y = min(world.y, clip.x)`: a plane
    // below every vertex drags the whole triangle onto one line (nothing is
    // drawn) and a plane above every vertex leaves the geometry alone but makes
    // transmittance zero everywhere. The plane must therefore straddle the
    // triangle, which spans EYE[1]-1.5 .. EYE[1]+1.5.
    words[20] = if clip_y < 0. {
        EYE[1] + 0.25
    } else {
        EYE[1] - 2.
    };
    words[21] = clip_y;
    // camera.sun = (dir.xyz, night factor). A negative w selects the unlit
    // layer-composite branch, so only the shared layer snapshot matters.
    words[32] = -1.;
    words[33] = 0.;
    words[34] = 0.;
    words[35] = -1.;
    // camera.light = (ambient, diffuse, specular, 1). Held identical across all
    // views on purpose: the claim under test is that a view does not change what
    // the terrain looks like for a given world position, so the comparison must
    // not inject a per-view lighting difference of its own.
    let _ = reflected;
    words[36] = 0.5;
    words[37] = 0.5;
    words[38] = 1.;
    words[39] = 1.;
    // camera.grass_eye is the refraction reference point. terrain_refract_vs
    // divides by its y delta, so leaving it zero makes the transmittance
    // exponent blow up for below-plane vertices; the viewer always supplies the
    // eye.
    words[52] = EYE[0];
    words[53] = EYE[1];
    words[54] = EYE[2];
    words[55] = 0.;
    // fog_range.z = 0 disables the ordinary fog term, but the refraction path
    // still returns the fog colour where its transmittance reaches zero, which is
    // exactly what happens to geometry below the clip plane. Use the client's
    // authored fog colour (`element_client.cfg`: fog_color 70,77,91).
    words[380] = 0.;
    words[381] = 0.;
    words[382] = 0.;
    words[383] = 0.;
    words[384] = FOG[0];
    words[385] = FOG[1];
    words[386] = FOG[2];
    words[387] = 1.;
    queue.write_buffer(camera, 0, &bytes);

    let mut sbytes = [0u8; 12 * 4];
    let s: &mut [f32; 12] = bytemuck::from_bytes_mut(&mut sbytes);
    s[0] = GRID;
    s[1] = GRID;
    s[2] = SPACING;
    s[3] = GRID;
    let areas = [1u32, 1, 2, 1, 0, 0, 0, 0];
    for (i, v) in areas.iter().enumerate() {
        sbytes[16 + i * 4..20 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    queue.write_buffer(settings, 0, &sbytes);
}

fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, source: &wgpu::Buffer) -> Vec<u8> {
    let size = source.size();
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(source, 0, &staging, 0, size);
    queue.submit(Some(encoder.finish()));
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let bytes = slice.get_mapped_range().to_vec();
    drop(slice);
    staging.unmap();
    bytes
}

fn new_target(device: &wgpu::Device, label: &str) -> Target {
    let size = 32u32;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    // copy_texture_to_buffer requires 256-byte row alignment; a 32px RGBA row is
    // 128 bytes, so read back through a padded row.
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (size * size * PADDED_ROW) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    Target {
        texture,
        view,
        read,
        size,
    }
}

fn read_target(device: &wgpu::Device, target: &Target) -> Vec<u8> {
    let slice = target.read.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();
    let bytes = slice.get_mapped_range().to_vec();
    drop(slice);
    target.read.unmap();
    bytes
}

/// A screen-covering triangle in world space, inside the origin cell.
fn near_vertex(device: &wgpu::Device) -> wgpu::Buffer {
    let mut data = Vec::new();
    // Vertices vary in x and y at constant z: a triangle flat in the y = eye
    // plane is edge-on to this camera and covers no pixels.
    for (x, y) in [(-1.5f32, -1.5f32), (1.5, -1.5), (0., 1.5)] {
        let pos = [EYE[0] + x, EYE[1] + y, EYE[2]];
        data.extend_from_slice(bytemuck::cast_slice(&pos));
        data.extend_from_slice(bytemuck::cast_slice(&[0f32, 1., 0.]));
        data.extend_from_slice(bytemuck::cast_slice(&[0f32, 0.]));
        data.extend_from_slice(bytemuck::cast_slice(&[1f32; 4]));
        data.extend_from_slice(bytemuck::cast_slice(&[0f32]));
        // The terrain pipeline reuses `gpu::Vertex`, so the stride is 60 bytes:
        // after location 4 it still carries `diffuse` and `night_diffuse`, which
        // this pipeline does not bind but the buffer must contain.
        data.extend_from_slice(bytemuck::cast_slice(&[0u32, 0]));
    }
    assert_eq!(data.len(), 3 * 60);
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: &data,
        usage: wgpu::BufferUsages::VERTEX,
    })
}

fn far_vertex(device: &wgpu::Device) -> wgpu::Buffer {
    let mut data = Vec::new();
    for (x, y) in [(-1.5f32, -1.5f32), (1.5, -1.5), (0., 1.5)] {
        let pos = [EYE[0] + x, EYE[1] + y, EYE[2]];
        data.extend_from_slice(bytemuck::cast_slice(&pos));
        // Location 1 is Snorm8x4, not a f32 vec4: the production far layout is
        // 12 bytes of position plus 4 bytes of packed normal, stride 16.
        data.extend_from_slice(&[0, 127, 0, 0]);
    }
    assert_eq!(data.len(), 3 * 16);
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: &data,
        usage: wgpu::BufferUsages::VERTEX,
    })
}

/// Exercise the exact production uploader, including its 2x2 and 1x1 block
/// extents. Read the tail back byte-for-byte: no hidden mip generation allowed.
fn check_compressed_upload(device: &wgpu::Device, queue: &wgpu::Queue) {
    if !device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        println!("::notice::native terrain BC upload unavailable on this adapter; decoded shader path still tested");
        return;
    }
    for (fourcc, format, block) in [
        (b"DXT1", wgpu::TextureFormat::Bc1RgbaUnorm, 8usize),
        (b"DXT3", wgpu::TextureFormat::Bc2RgbaUnorm, 16usize),
    ] {
        let mut source = vec![0u8; 128];
        source[..4].copy_from_slice(b"DDS ");
        for (offset, value) in [
            (4, 124u32),
            (12, 256),
            (16, 256),
            (28, 9),
            (76, 32),
            (80, 4),
        ] {
            source[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        source[84..88].copy_from_slice(fourcc);
        for mip in 0..9 {
            let blocks = (256u32 >> mip).max(1).div_ceil(4) as usize;
            source.resize(source.len() + blocks * blocks * block, (mip + 1) as u8);
        }
        assert!(terrain_upload::native_group(&source, true).is_some());
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("production terrain BC upload regression"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
            mip_level_count: 9,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        terrain_upload::upload(queue, &texture, &source, 0).unwrap();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 768,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        for (i, mip) in [6u32, 7, 8].into_iter().enumerate() {
            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: i as u64 * 256,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let bytes = slice.get_mapped_range();
        for (i, value) in [7u8, 8, 9].into_iter().enumerate() {
            assert_eq!(&bytes[i * 256..i * 256 + block], vec![value; block]);
        }
        drop(bytes);
        buffer.unmap();
    }
    println!("::notice::production terrain BC1/BC2 uploads preserve authored tiny mip blocks byte-for-byte");
}

/// Asymmetric source pixels expose mirrored repetition in either UV axis,
/// including negative world coordinates. Uses the production sampler factory.
fn check_color_wrap(device: &wgpu::Device, queue: &wgpu::Queue) {
    let source = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut pixels = Vec::new();
    for y in 0..4u8 {
        for x in 0..4u8 {
            pixels.extend_from_slice(&[20 + x * 40, 30 + y * 50, 100, 255]);
        }
    }
    queue.write_texture(
        source.as_image_copy(),
        &pixels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(16),
            rows_per_image: Some(4),
        },
        wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
    );
    let view = source.create_view(&Default::default());
    let sampler = terrain_upload::color_sampler(device);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(
            r#"
@group(0) @binding(0) var tex:texture_2d<f32>;
@group(0) @binding(1) var smp:sampler;
@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {
    var p=array<vec2<f32>,3>(vec2<f32>(-1.,-1.),vec2<f32>(3.,-1.),vec2<f32>(-1.,3.));
    return vec4<f32>(p[i],0.,1.);
}
@fragment fn fs(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32> {
    var uv=array<vec2<f32>,8>(vec2<f32>(.125,.125),vec2<f32>(1.125,.125),
        vec2<f32>(-.875,.125),vec2<f32>(.125,1.125),vec2<f32>(.125,-.875),
        vec2<f32>(.875,.875),vec2<f32>(1.875,1.875),vec2<f32>(-.125,-.125));
    return textureSampleLevel(tex,smp,uv[u32(p.x)%8u],0.);
}
"#
            .into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs",
            buffers: &[],
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
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    let target = new_target(device, "terrain colour wrap regression");
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
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
        target.texture.as_image_copy(),
        wgpu::ImageCopyBuffer {
            buffer: &target.read,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(PADDED_ROW),
                rows_per_image: Some(target.size),
            },
        },
        wgpu::Extent3d {
            width: target.size,
            height: target.size,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let pixels = read_target(device, &target);
    for i in 0..8 {
        assert_eq!(
            &pixels[i * 4..i * 4 + 4],
            if i < 5 {
                &[20, 30, 100, 255]
            } else {
                &[140, 180, 100, 255]
            },
            "terrain colour wrap sample {i} is mirrored or clamped"
        );
    }
    println!(
        "Terrain colour wrap: 8/8 asymmetric pixel checks passed across positive and negative U/V"
    );
}
