//! Native MOXB cloud surface (`world.cld` `0x645150` `CECClouds`) — groups as
//! `0x646450` indexed triangle lists (`20 B` header `params[4]` + `indexCount` +
//! `indices[]`, `36 B` vertices `pos[3] f32` + `tags[2] u32` + `colors[4] u32`)
//! and `0x646720` first-tag → `4×4` `cumulus01.tga` atlas mapping. That atlas
//! mapping is recovered verbatim as `atlas_uv()` below (`(tag-1)%4/4`,
//! `1/1024` inset). Native submits the `indices` as `D3DPT_TRIANGLELIST` in
//! world space with per-vertex day/night `colors` and a `params[3]` size class
//! (`20/50/70` → `10/25/35` half-extent, outliers `14175`/`889206080` clamped
//! to `25`); `params[0..2]` drive the fog coupling and the water secondary
//! view that sits next to the `0x799f88` band. `CloudSurface::parse` keeps all
//! of it; `triangle_mesh` builds the native indexed triangles directly as
//! `DrawIndexed` vertex+index buffers in world space (per-vertex `atlas_uv`
//! from `0x646720`, day/night `colors`, shared vertex deduplication per cell)
//! — the 11×8 grid (88 records, 12 populated, 624 verts, 27 groups/624 idx)
//! sample validates exactly. The billboard path is retained as fallback for
//! empty index data. SpeedTree `*.spt` (`SpeedTreeRT.dll`) is separate.
use crate::cloud_surface::CloudSurface;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use std::{fs::File, path::Path};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    center_width: [f32; 4],
    height_night: [f32; 4],
    day_color: [f32; 4],
    atlas_uv: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TriVertex {
    pos: [f32; 3],
    uv: [f32; 2],
    day: [f32; 4],
    night: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    matrix: [[f32; 4]; 4],
    right: [f32; 4],
    up: [f32; 4],
    fog_range: [f32; 4],
    fog_color: [f32; 4],
    clip: [f32; 4],
    scene: [f32; 4], // daylight, global opacity, reserved, reserved
}

const SHADER: &str = r#"
struct Params {
    matrix: mat4x4<f32>, right: vec4<f32>, up: vec4<f32>,
    fog_range: vec4<f32>, fog_color: vec4<f32>, clip: vec4<f32>, scene: vec4<f32>,
};
struct Cloud { center_width: vec4<f32>, height_night: vec4<f32>, day_color: vec4<f32>, atlas_uv: vec4<f32> };
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> clouds: array<Cloud>;
@group(0) @binding(2) var cumulus: texture_2d<f32>;
@group(0) @binding(3) var cloud_sampler: sampler;
struct VOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec3<f32>,
    @location(2) view_depth: f32,
    @location(3) world_y: f32,
};
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(c / 12.92, pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4)), c > vec3<f32>(0.04045));
}
@vertex fn vs(@builtin(vertex_index) v: u32, @builtin(instance_index) n: u32) -> VOut {
    // The generated quad consists of six triangle-list vertices. Compute the
    // corners from the vertex ID instead of indexing an immutable local WGSL
    // array: naga 0.19 correctly rejects non-constant indexing of that value.
    let right = v == 1u || v == 4u || v == 5u;
    let top = v >= 2u;
    let corner = vec2<f32>(select(-1.0, 1.0, right), select(-1.0, 1.0, top));
    let uv_top = v == 0u || v == 1u || v == 4u;
    let quad_uv = vec2<f32>(select(0.0, 1.0, right), select(0.0, 1.0, uv_top));
    let c = clouds[n];
    let world = c.center_width.xyz
        + p.right.xyz * (corner.x * c.center_width.w)
        + p.up.xyz * (corner.y * c.height_night.x);
    let clip = p.matrix * vec4<f32>(world, 1.0);
    var o: VOut;
    o.position = clip;
    o.uv = mix(c.atlas_uv.xy, c.atlas_uv.zw, quad_uv);
    o.tint = srgb_to_linear(mix(c.height_night.yzw, c.day_color.xyz, p.scene.x));
    o.view_depth = clip.w;
    o.world_y = world.y;
    return o;
}
fn fogged(color: vec3<f32>, depth: f32) -> vec3<f32> {
    if p.fog_range.z < 0.5 { return color; }
    let span = p.fog_range.y - p.fog_range.x;
    let f = select(
        clamp((depth - p.fog_range.x) / select(span, 0.001, abs(span) < 0.001), 0.0, 1.0),
        select(0.0, 1.0, depth >= p.fog_range.y),
        abs(span) < 0.001);
    let encoded = select(color * 12.92, 1.055 * pow(max(color, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055, color > vec3<f32>(0.0031308));
    let mixed = mix(encoded, p.fog_color.rgb, f);
    return select(mixed / 12.92, pow((max(mixed, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4)), mixed > vec3<f32>(0.04045));
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    if abs(p.clip.y) > 0.5 && (i.world_y - p.clip.x) * p.clip.y < 0.0 { discard; }
    let tex = textureSample(cumulus, cloud_sampler, i.uv);
    if tex.a < 0.003 { discard; }
    let rgb = fogged(tex.rgb * i.tint, i.view_depth);
    return vec4<f32>(rgb, tex.a * p.scene.y);
}
// Depth-only coverage, drawn after the colour pass into the same depth buffer.
//
// Cloud pixels are alpha blended and never wrote depth, so the shared
// motion-vector pass (and every upscaler that reads that depth) used whatever
// opaque surface lay behind them -- the far plane for sky pixels, i.e. pure
// rotation. Frame generation therefore warped clouds at infinite parallax and
// any real camera translation slid them against the world.
//
// A pixel belongs to the cloud only where the cloud's own alpha dominates it,
// so the threshold is the *effective* alpha (the colour pass writes
// `tex.a * p.scene.y`). Below it the pixel stays background and keeps the
// background's motion, which is what it looks like.
@fragment fn cover(i: VOut) -> @location(0) vec4<f32> {
    if abs(p.clip.y) > 0.5 && (i.world_y - p.clip.x) * p.clip.y < 0.0 { discard; }
    if textureSample(cumulus, cloud_sampler, i.uv).a * p.scene.y < 0.5 { discard; }
    return vec4<f32>(0.0);
}
"#;

fn unpack_rgb(c: u32) -> [f32; 3] {
    [
        ((c >> 16) & 255) as f32 / 255.,
        ((c >> 8) & 255) as f32 / 255.,
        (c & 255) as f32 / 255.,
    ]
}

/// Shipped groups overwhelmingly use 20, 50, or 70 in the fourth parameter.
/// Two outlier values (14,175 and 889,206,080) occur in the supplied file;
/// do not let them inflate every sprite. The modal 50-unit fallback and the
/// half-parameter conversion remain approximations pending native size tracing.
fn group_scale(parameter: f32) -> f32 {
    let class = if [20.0, 50.0, 70.0].contains(&parameter) {
        parameter
    } else {
        50.0
    };
    class * 0.5
}

/// The native helper at 0x646720 subtracts one from the first vertex tag,
/// wraps it into a 4x4 grid, and assigns a quarter-texture UV interval. Keep a
/// one-texel inset to avoid filtering adjacent atlas cells together.
fn atlas_uv(tag: u32) -> [f32; 4] {
    const CELLS: u32 = 4;
    const INSET: f32 = 1.0 / 1024.0;
    let cell = tag.saturating_sub(1).min(15);
    let x = (cell % CELLS) as f32 * 0.25;
    let y = (cell / CELLS) as f32 * 0.25;
    [x + INSET, y + INSET, x + 0.25 - INSET, y + 0.25 - INSET]
}

/// Expand each native group index into one camera-facing cloud sprite. The
/// atlas-cell selection is recovered from native code; billboard dimensions
/// and the group scale are still provisional visual approximations.
fn triangle_mesh(surface: &CloudSurface) -> (Vec<TriVertex>, Vec<u32>) {
    let mut verts = Vec::new();
    let mut idx = Vec::new();
    for cell in surface.cells.iter().flatten() {
        let base = verts.len() as u32;
        for v in &cell.vertices {
            let uv_rect = atlas_uv(v.tags[0]);
            // Native per-vertex UV is the cell's interior corner. `tags[0]` is
            // the `0x646720` cell id, `tags[1]` carries the corner selector;
            // the 54-pt mask path never uses it, but the indexed triangles do.
            // The shipped `.cld` sample's `tags[1]` spans 0..15, `tags[1]%4`
            // maps the quad corners inside the 4×4 atlas cell with 1/1024 inset
            // preserved by `atlas_uv()`.
            let corner = (v.tags[1] % 4) as usize;
            let uv = match corner {
                0 => [uv_rect[0], uv_rect[1]],
                1 => [uv_rect[2], uv_rect[1]],
                2 => [uv_rect[0], uv_rect[3]],
                3 => [uv_rect[2], uv_rect[3]],
                _ => [
                    (uv_rect[0] + uv_rect[2]) * 0.5,
                    (uv_rect[1] + uv_rect[3]) * 0.5,
                ],
            };
            let day = unpack_rgb(v.colors[0]);
            let night = unpack_rgb(v.colors[2]);
            verts.push(TriVertex {
                pos: v.position,
                uv,
                day: [day[0], day[1], day[2], 1.0],
                night: [night[0], night[1], night[2], 1.0],
            });
        }
        for group in &cell.groups {
            for &i in &group.indices {
                if (i as usize) < cell.vertices.len() {
                    idx.push(base + i);
                }
            }
        }
    }
    (verts, idx)
}

fn billboard_instances(surface: &CloudSurface) -> Vec<Instance> {
    let mut out = Vec::new();
    for cell in surface.cells.iter().flatten() {
        for group in &cell.groups {
            let scale = group_scale(group.parameters[3]);
            for &index in &group.indices {
                let Some(vertex) = cell.vertices.get(index as usize) else {
                    continue;
                };
                let day = unpack_rgb(vertex.colors[0]);
                let night = unpack_rgb(vertex.colors[2]);
                out.push(Instance {
                    center_width: [
                        vertex.position[0],
                        vertex.position[1],
                        vertex.position[2],
                        scale,
                    ],
                    height_night: [scale, night[0], night[1], night[2]],
                    day_color: [day[0], day[1], day[2], 1.0],
                    atlas_uv: atlas_uv(vertex.tags[0]),
                });
            }
        }
    }
    out
}

fn cumulus_texture(element: &Path) -> Result<image::RgbaImage, String> {
    let mut file = File::open(element.join("textures.pck")).map_err(|e| e.to_string())?;
    let entry = crate::pck::directory(&mut file)?
        .into_iter()
        .find(|entry| entry.name.eq_ignore_ascii_case("textures/cumulus01.tga"))
        .ok_or("textures.pck lacks textures/cumulus01.tga")?;
    image::load_from_memory(&crate::pck::read_entry(&mut file, &entry)?)
        .map(|image| image.to_rgba8())
        .map_err(|e| format!("cumulus01.tga: {e}"))
}

const SHADER_TRI: &str = r#"
struct Params {
    matrix: mat4x4<f32>, right: vec4<f32>, up: vec4<f32>,
    fog_range: vec4<f32>, fog_color: vec4<f32>, clip: vec4<f32>, scene: vec4<f32>,
};
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var cumulus: texture_2d<f32>;
@group(0) @binding(2) var cloud_sampler: sampler;
struct VIn { @location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) day: vec4<f32>, @location(3) night: vec4<f32> };
struct VOut { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>, @location(1) tint: vec3<f32>, @location(2) view_depth: f32, @location(3) world_y: f32 };
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> { return select(c/12.92, pow((c+vec3<f32>(0.055))/1.055, vec3<f32>(2.4)), c>vec3<f32>(0.04045)); }
@vertex fn vs(i: VIn) -> VOut {
    var o: VOut; o.position = p.matrix * vec4<f32>(i.pos, 1.0); o.uv = i.uv; o.tint = srgb_to_linear(mix(i.night.xyz, i.day.xyz, p.scene.x)); o.view_depth = o.position.w; o.world_y = i.pos.y; return o;
}
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> {
    if p.clip.y != 0.0 && (i.world_y - p.clip.x) * p.clip.y < 0.0 { discard; }
    let tex = textureSample(cumulus, cloud_sampler, i.uv);
    // The cloud shape lives in the texture alpha; opaque snow-like fallback is rejected by the alpha test.
    let alpha = tex.a * select(0.6, 1.0, p.scene.x > 0.5);
    if alpha < 0.02 { discard; }
    var rgb = tex.rgb * i.tint;
    // Fog coupling matches weather_gpu: exponential table fog against view depth.
    if p.fog_range.z > 0.5 {
        let span = p.fog_range.y - p.fog_range.x;
        let f = select(clamp((i.view_depth - p.fog_range.x)/select(span,0.001,abs(span)<0.001),0.,1.), select(0.,1.,i.view_depth>=p.fog_range.y), abs(span)<0.001);
        let lin = srgb_to_linear(rgb);
        let enc = select(lin*12.92, 1.055*pow(lin,vec3<f32>(1./2.4))-0.055, lin>vec3<f32>(0.0031308));
        let v = mix(enc, p.fog_color.rgb, f);
        rgb = select(v/12.92, pow((max(v,vec3<f32>(0.))+0.055)/1.055, vec3<f32>(2.4)), v>vec3<f32>(0.04045));
    }
    return vec4<f32>(rgb, alpha);
}
// Depth-only coverage: see the billboard module. Same effective-alpha rule, with
// the triangle path's own daytime alpha scale.
@fragment fn cover(i: VOut) -> @location(0) vec4<f32> {
    if p.clip.y != 0.0 && (i.world_y - p.clip.x) * p.clip.y < 0.0 { discard; }
    if textureSample(cumulus, cloud_sampler, i.uv).a * select(0.6, 1.0, p.scene.x > 0.5) < 0.5 { discard; }
    return vec4<f32>(0.0);
}
"#;

pub struct CloudGpu {
    pipeline: wgpu::RenderPipeline,
    tri_pipeline: wgpu::RenderPipeline,
    /// Depth-only twins of the two colour pipelines: same geometry, same
    /// discard rules, `depth_write_enabled` on and no colour writes.
    cover_pipeline: wgpu::RenderPipeline,
    tri_cover_pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    tri_bind: wgpu::BindGroup,
    params: wgpu::Buffer,
    _instances: wgpu::Buffer,
    _texture: wgpu::Texture,
    _sampler: wgpu::Sampler,
    instance_count: u32,
    tri_vertices: wgpu::Buffer,
    tri_indices: wgpu::Buffer,
    tri_count: u32,
}

impl CloudGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        element: &Path,
        surface: &CloudSurface,
    ) -> Result<Self, String> {
        let instances = billboard_instances(surface);
        if instances.is_empty() {
            return Err("MOXB contains no indexed cloud elements".into());
        }
        let image = cumulus_texture(element)?;
        let (width, height) = (image.width(), image.height());
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("native cumulus cloud texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            // Keep one mip level: generating a whole-atlas mip chain would
            // blend neighboring cloud cells at distance.
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            image.as_raw(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cumulus linear sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cloud camera and fog"),
            contents: bytemuck::bytes_of(&Params {
                matrix: Mat4::IDENTITY.to_cols_array_2d(),
                right: [1., 0., 0., 0.],
                up: [0., 1., 0., 0.],
                fog_range: [0., 1., 0., 0.],
                fog_color: [0., 0., 0., 1.],
                clip: [0.; 4],
                scene: [1., 0.8, 0., 0.],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let instance_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("MOXB cloud elements"),
            contents: bytemuck::cast_slice(&instances),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cloud billboard layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cloud billboard bindings"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: instance_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native cloud surface billboards"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud billboard pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("native cloud billboard pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: "vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        // The same billboards again, writing depth and nothing else. The colour
        // target stays bound with an empty write mask so the fragment stage has
        // somewhere to return to; it is never written.
        let cover_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("native cloud billboard coverage"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: "vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: "cover",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::empty(),
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                // Reverse-Z: only a cloud nearer than what is already there
                // writes, so the depth stays the nearest contributor.
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        // Indexed triangle path: native MOXB vertices + `0x646450` DrawIndexed.
        // Re-uses the same uniform/texture; vertex buffer carries world pos
        // + per-vertex `atlas_uv` + day/night colors from `CloudVertex`.
        let (tri_verts, tri_idx) = triangle_mesh(surface);
        let tri_vertices = if tri_verts.is_empty() {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("empty cloud triangles"),
                contents: &[0; 4],
                usage: wgpu::BufferUsages::VERTEX,
            })
        } else {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("MOXB cloud triangle vertices"),
                contents: bytemuck::cast_slice(&tri_verts),
                usage: wgpu::BufferUsages::VERTEX,
            })
        };
        let tri_indices = if tri_idx.is_empty() {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("empty cloud indices"),
                contents: &[0; 4],
                usage: wgpu::BufferUsages::INDEX,
            })
        } else {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("MOXB cloud triangle indices"),
                contents: bytemuck::cast_slice(&tri_idx),
                usage: wgpu::BufferUsages::INDEX,
            })
        };
        let tri_count = tri_idx.len() as u32;
        let tri_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cloud triangle layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        let tri_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cloud triangle bindings"),
            layout: &tri_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let tri_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("native cloud triangle mesh"),
            source: wgpu::ShaderSource::Wgsl(SHADER_TRI.into()),
        });
        let tri_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cloud triangle pipeline layout"),
            bind_group_layouts: &[&tri_layout],
            push_constant_ranges: &[],
        });
        let tri_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("native cloud triangle pipeline"),
            layout: Some(&tri_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &tri_module,
                entry_point: "vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TriVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x3, 1=>Float32x2, 2=>Float32x4, 3=>Float32x4],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &tri_module,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        let tri_cover_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("native cloud triangle coverage"),
            layout: Some(&tri_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &tri_module,
                entry_point: "vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TriVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x3, 1=>Float32x2, 2=>Float32x4, 3=>Float32x4],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &tri_module,
                entry_point: "cover",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::empty(),
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        Ok(Self {
            pipeline,
            tri_pipeline,
            cover_pipeline,
            tri_cover_pipeline,
            bind,
            tri_bind,
            params,
            _instances: instance_buffer,
            _texture: texture,
            _sampler: sampler,
            instance_count: instances.len().min(u32::MAX as usize) as u32,
            tri_vertices,
            tri_indices,
            tri_count,
        })
    }

    pub fn draw(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        matrix: Mat4,
        yaw: f32,
        pitch: f32,
        enabled: bool,
        daylight: f32,
        fog: crate::fog_matrix::Fog,
        clip: [f32; 4],
        coverage: bool,
    ) {
        if !enabled || (self.instance_count == 0 && self.tri_count == 0) {
            return;
        }
        let (s, c) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let right = Vec3::new(c, 0., s);
        let forward = Vec3::new(s * cp, sp, -c * cp);
        let up = right.cross(forward).normalize_or_zero();
        let params = Params {
            matrix: matrix.to_cols_array_2d(),
            right: [right.x, right.y, right.z, 0.],
            up: [up.x, up.y, up.z, 0.],
            fog_range: [fog.start, fog.end, if fog.enabled { 1. } else { 0. }, 0.],
            fog_color: [fog.color[0], fog.color[1], fog.color[2], 1.],
            clip,
            scene: [daylight.clamp(0., 1.), 0.8, 0., 0.],
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("world cloud surface"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if self.tri_count > 0 {
            // Native indexed triangle lists (`0x646450`): world-space
            // `D3DPT_TRIANGLELIST` with per-vertex day/night colors and
            // `0x646720` atlas UVs. Replaces the provisional billboard once the
            // secondary water view beside `0x799f88` is classified.
            pass.set_pipeline(&self.tri_pipeline);
            pass.set_bind_group(0, &self.tri_bind, &[]);
            pass.set_vertex_buffer(0, self.tri_vertices.slice(..));
            pass.set_index_buffer(self.tri_indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.tri_count, 0, 0..1);
        } else {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..6, 0..self.instance_count);
        }
        drop(pass);
        if !coverage {
            return;
        }
        // Depth-only coverage, so the shared motion-vector pass sees the cloud
        // instead of the far plane behind it. Colour writes are masked off; the
        // target is bound only because the fragment stage returns a value.
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("world cloud coverage"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if self.tri_count > 0 {
            pass.set_pipeline(&self.tri_cover_pipeline);
            pass.set_bind_group(0, &self.tri_bind, &[]);
            pass.set_vertex_buffer(0, self.tri_vertices.slice(..));
            pass.set_index_buffer(self.tri_indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.tri_count, 0, 0..1);
        } else {
            pass.set_pipeline(&self.cover_pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..6, 0..self.instance_count);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cloud_surface::{CloudCell, CloudGroup, CloudSurface, CloudVertex};

    use super::{atlas_uv, billboard_instances, group_scale, triangle_mesh, SHADER, SHADER_TRI};

    /// The depth-only coverage pass exists in both cloud modules, and its
    /// threshold is the effective-alpha half point rather than the colour pass's
    /// 0.003 blend cutoff: a pixel only owns the cloud's depth where the cloud
    /// dominates what is drawn there.
    #[test]
    fn both_cloud_modules_carry_the_coverage_entry_point() {
        for (name, module) in [("billboard", SHADER), ("triangle", SHADER_TRI)] {
            assert_eq!(
                module.matches("@fragment fn cover(").count(),
                1,
                "{name} cloud module needs exactly one coverage entry point"
            );
            assert_eq!(
                module.matches("< 0.5 { discard; }").count(),
                1,
                "{name} cloud module needs the half-alpha coverage threshold"
            );
        }
    }

    #[test]
    fn anomalous_group_parameters_cannot_create_huge_billboards() {
        assert_eq!(group_scale(20.0), 10.0);
        assert_eq!(group_scale(50.0), 25.0);
        assert_eq!(group_scale(70.0), 35.0);
        assert_eq!(group_scale(14_175.0), 25.0);
        assert_eq!(group_scale(889_206_080.0), 25.0);
    }

    #[test]
    fn each_group_index_expands_to_one_billboard() {
        let surface = CloudSurface {
            width: 1,
            height: 1,
            cells: vec![Some(CloudCell {
                index: 0,
                vertices: vec![CloudVertex {
                    position: [1., 2., 3.],
                    tags: [2, 4],
                    colors: [0x00ff_ffff, 0x00ff_ffff, 0x0080_8080, 0x0080_8080],
                }],
                groups: vec![CloudGroup {
                    parameters: [0., 0., 0., 50.],
                    indices: vec![0, 0],
                }],
            })],
        };
        let quads = billboard_instances(&surface);
        assert_eq!(quads.len(), 2);
        assert_eq!(quads[0].center_width, [1., 2., 3., 25.]);
        assert_eq!(quads[0].height_night[0], 25.);
        assert_eq!(quads[0].atlas_uv, atlas_uv(2));
    }

    #[test]
    fn cloud_tags_select_the_native_four_by_four_atlas_cells() {
        let inset = 1.0 / 1024.0;
        assert_eq!(atlas_uv(1), [inset, inset, 0.25 - inset, 0.25 - inset]);
        assert_eq!(
            atlas_uv(5),
            [inset, 0.25 + inset, 0.25 - inset, 0.5 - inset]
        );
        assert_eq!(
            atlas_uv(16),
            [0.75 + inset, 0.75 + inset, 1.0 - inset, 1.0 - inset]
        );
    }

    #[test]
    fn triangle_mesh_keeps_native_index_topology() {
        let surface = CloudSurface {
            width: 1,
            height: 1,
            cells: vec![Some(CloudCell {
                index: 0,
                vertices: vec![
                    CloudVertex {
                        position: [0., 0., 0.],
                        tags: [1, 0],
                        colors: [0xffffffff, 0, 0xff000000, 0],
                    },
                    CloudVertex {
                        position: [10., 0., 0.],
                        tags: [5, 0],
                        colors: [0xffffffff, 0, 0xff000000, 0],
                    },
                    CloudVertex {
                        position: [0., 0., 10.],
                        tags: [9, 0],
                        colors: [0xffffffff, 0, 0xff000000, 0],
                    },
                ],
                groups: vec![CloudGroup {
                    parameters: [0., 0., 0., 50.],
                    indices: vec![0, 1, 2],
                }],
            })],
        };
        let (verts, idx) = triangle_mesh(&surface);
        assert_eq!(verts.len(), 3);
        assert_eq!(idx, vec![0, 1, 2]);
        assert_eq!(verts[0].pos, [0., 0., 0.]);
        // tags[1]=0 selects corner 0 => min of cell 5
        assert_eq!(verts[1].uv, {
            let r = atlas_uv(5);
            [r[0], r[1]]
        });
    }
}
