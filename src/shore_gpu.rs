//! Shoreline wave pass recovered from the client's wave loader, mesh builder,
//! wavevs.txt and waveblend.txt. Geometry random phase is not yet client-global RNG identical.
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use std::path::Path;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    p: [f32; 3],
    uv: [f32; 2],
    day: u32,
    night: u32,
    alpha: f32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    matrix: [[f32; 4]; 4],
    eye_night: [f32; 4],
    offsets: [f32; 4],
    flags: [f32; 4],
    bounds: [f32; 4],
    fog_range: [f32; 4],
    fog_color: [f32; 4],
}

// Client 0x766320/0x766910: seeded Park-Miller table, wrapped
// neighbour smoothing, five octaves with normalized amplitudes.
fn noise() -> (Vec<f32>, Vec<(usize, usize, f32)>) {
    let mut state = 6036u64;
    let mut random = || {
        state = state * 16807 % 2147483647;
        state as u32
    };
    let mut table = vec![0.; 64];
    for channel in 0..3 {
        let values: Vec<f32> = (0..64)
            .map(|_| ((random() % 2001) as f32 - 1000.) * 0.001)
            .collect();
        if channel == 0 {
            for i in 0..64 {
                table[i] = values[i] * 0.5 + (values[(i + 63) % 64] + values[(i + 1) % 64]) * 0.25;
            }
        }
    }
    let mut octave = Vec::new();
    let mut scale = 1.;
    let mut sum = 0.;
    let mut period = 23;
    for _ in 0..5 {
        let offset = (random() % 1023) as usize;
        octave.push((period, offset, scale));
        sum += scale;
        scale *= 0.995;
        period /= 2;
        if period == 0 {
            break;
        }
    }
    for x in &mut octave {
        x.2 /= sum;
    }
    (table, octave)
}
fn sample_noise(table: &[f32], oct: &[(usize, usize, f32)], x: f32) -> f32 {
    oct.iter()
        .map(|&(period, off, amp)| {
            let x = x / period as f32 + off as f32;
            let k = x.floor() as usize;
            let f = x - x.floor();
            (table[k % 64] * (1. - f) + table[(k + 1) % 64] * f).abs() * amp
        })
        .sum()
}
fn mesh(waves: &[crate::water::WaterArea]) -> (Vec<Vertex>, Vec<u32>) {
    let (noise_table, oct) = noise();
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    // rand() phase belongs to the client's global RNG. Its state depends on
    // prior game activity; use its standard recurrence pending integration.
    let mut rand = 1u32;
    for area in waves {
        for w in &area.shore_waves {
            if w.points.len() < 2 {
                continue;
            }
            let base = vertices.len() as u32;
            let mut previous = Vec3::ZERO;
            let mut distance_along = 0.0f32;
            for i in 0..w.points.len() {
                let p = Vec3::from_array(w.points[i]);
                let next = if i + 1 < w.points.len() {
                    let d = Vec3::from_array(w.points[i + 1]) - p;
                    Vec3::new(d.z, 0., -d.x).normalize_or_zero()
                } else {
                    previous
                };
                let miter = if i == 0 {
                    next
                } else if i + 1 == w.points.len() {
                    previous
                } else {
                    (previous + next).normalize_or_zero()
                };
                let tangent = Vec3::new(-next.z, 0., next.x);
                let sine = (1. - miter.dot(tangent).clamp(-1., 1.).powi(2)).sqrt();
                let distance = if sine > 1e-4 {
                    w.width.min(4.) / sine
                } else {
                    w.width.min(4.)
                };
                let delta = miter * distance;
                rand = rand.wrapping_mul(214013).wrapping_add(2531011);
                let r = ((rand >> 16) & 0x7fff) % 11;
                let jitter = r as f32 * 0.01;
                let u = distance_along / 15.;
                let v = (sample_noise(&noise_table, &oct, i as f32) - 0.5) * 0.1;
                let positions = [
                    p + Vec3::Y * jitter,
                    p - delta * 0.35,
                    p - delta - Vec3::Y * jitter,
                ];
                let vs = [v + 1., v + 0.75, v + 0.5];
                for row in 0..3 {
                    vertices.push(Vertex {
                        p: positions[row].to_array(),
                        uv: [u, vs[row]],
                        day: w.day_colors[i * 3 + row],
                        night: w.night_colors[i * 3 + row],
                        alpha: if row == 1 { 1. } else { 0. },
                    });
                }
                if i + 1 < w.points.len() {
                    let j = base + (i as u32) * 3;
                    indices.extend_from_slice(&[
                        j,
                        j + 1,
                        j + 3,
                        j + 3,
                        j + 1,
                        j + 4,
                        j + 4,
                        j + 1,
                        j + 2,
                        j + 4,
                        j + 2,
                        j + 5,
                    ]);
                }
                if i + 1 < w.points.len() {
                    distance_along += (Vec3::from_array(w.points[i + 1]) - p).length();
                }
                previous = next;
            }
        }
    }
    (vertices, indices)
}
fn bezier(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    let u = 1. - t;
    u * u * u * a + 3. * u * u * t * b + 3. * u * t * t * c + t * t * t * d
}
/// `wavevs.txt` takes its fade radius from `c0.w = 1/(vSightRange*vSightRange)`
/// and the recovered client value is a sight range of 400, i.e. 1/160000.
pub(crate) const WAVE_SIGHT_RANGE: f32 = 400.;

/// Shoreline wave distance fade, transcribed from `shaders/vs/wavevs.txt`:
///
/// ```text
/// sub r1, v0, c21        ; to camera
/// mov r1.y, c0.y         ; height difference deliberately ignored
/// dp3 r0, r1, r1
/// mul r1.w, r0.w, c0.w   ; 0~1 out to sight range
/// sub r1.w, c0.x, r1.w   ; 1~0 out to sight range
/// max r0.w, r1.w, c0.y   ; clamp below zero
/// ```
///
/// The vertical term is zeroed before the dot product, so this is a
/// horizontal-only falloff: a tall waterfall fades exactly as fast as a
/// puddle at the same xz offset. Using a full 3D distance here would fade
/// the tops of waterfalls out early.
pub(crate) fn wave_fade(position: [f32; 3], eye: [f32; 3]) -> f32 {
    let dx = position[0] - eye[0];
    let dz = position[2] - eye[2];
    (1. - (dx * dx + dz * dz) / (WAVE_SIGHT_RANGE * WAVE_SIGHT_RANGE)).max(0.)
}

fn wave_offset(seconds: f32, phase: f32, half: bool) -> f32 {
    let t = (seconds + phase).rem_euclid(10.);
    let p = if half {
        [-0.3, -0.175, 0., 0.175, 0.25]
    } else {
        [-0.6, -0.35, 0., 0.35, 0.5]
    };
    let (a, b, c, d, t) = if t < 1. {
        (p[0], p[0], p[1], p[2], t)
    } else if t < 3. {
        (p[2], 2. * p[2] - p[1], p[3], p[4], (t - 1.) / 2.)
    } else if t < 6. {
        (p[4], p[4], p[3], p[2], (t - 3.) / 3.)
    } else {
        (p[2], 2. * p[2] - p[3], p[1], p[0], (t - 6.) / 4.)
    };
    -0.5 * bezier(a, b, c, d, t)
}
const SHADER: &str = r#"
struct Uniform { matrix:mat4x4<f32>, eye_night:vec4<f32>, offsets:vec4<f32>, flags:vec4<f32>, bounds:vec4<f32>, fog_range:vec4<f32>, fog_color:vec4<f32> };
@group(0) @binding(0) var<uniform> u:Uniform;
@group(0) @binding(1) var tex:texture_2d<f32>;
@group(0) @binding(2) var samp:sampler;
struct In { @location(0) p:vec3<f32>, @location(1) uv:vec2<f32>, @location(2) day:u32, @location(3) night:u32, @location(4) alpha:f32 };
struct Out { @builtin(position) p:vec4<f32>, @location(0) uv:vec2<f32>, @location(1) color:vec4<f32>, @location(2) world:vec3<f32> };
@vertex fn vs(i:In)->Out {
 var o:Out; o.world=i.p; o.p=u.matrix*vec4<f32>(i.p,1.); o.uv=i.uv;
 let n=u32(clamp(u.eye_night.w,0.,1.)*255.); let d=unpack4x8unorm(i.day).bgr*255.; let q=unpack4x8unorm(i.night).bgr*255.;
 let color=floor((d*f32(255u-n)+q*f32(n))/256.)/255.;
 let dist=i.p.xz-u.eye_night.xz; let fade=max(1.-dot(dist,dist)/(u.flags.y*u.flags.y),0.);
 o.color=vec4<f32>(color,i.alpha*fade); return o;
}
fn linear(c:vec3<f32>)->vec3<f32> {let v=max(c,vec3<f32>(0.));return select(v/12.92,pow((v+0.055)/1.055,vec3<f32>(2.4)),v>vec3<f32>(0.04045));}
@fragment fn fs(i:Out)->@location(0) vec4<f32> {
 if u.flags.z>0.5 && (i.world.x<u.bounds.x || i.world.x>=u.bounds.y || i.world.z<u.bounds.z || i.world.z>=u.bounds.w) {discard;}

 let a=textureSample(tex,samp,i.uv+vec2<f32>(0.,u.offsets.x));
 let b=textureSample(tex,samp,i.uv+vec2<f32>(0.5,u.offsets.y));
 let c=textureSample(tex,samp,i.uv*0.5+vec2<f32>(0.,u.offsets.z));
 var r=vec4<f32>(c.rgb,c.a*0.5); r=mix(r,b,b.a); r=mix(r,a,a.a); r.a*=0.6;
 var color=clamp(i.color.rgb*r.rgb*2.,vec3<f32>(0.),vec3<f32>(1.));
 if u.fog_range.z>0.5 {
  let view_z=max((u.matrix*vec4<f32>(i.world,1.)).w,0.); let span=u.fog_range.y-u.fog_range.x;
  let f=select(clamp((view_z-u.fog_range.x)/select(span,0.001,abs(span)<0.001),0.,1.),select(0.,1.,view_z>=u.fog_range.y),abs(span)<0.001);
  color=mix(color,u.fog_color.rgb,f);
 }
 return vec4<f32>(select(color,linear(color),u.flags.x>0.5),i.color.a*r.a);
}
"#;
#[cfg(test)]
mod fog_contract_tests {
    use super::SHADER;

    #[test]
    fn shoreline_waves_use_the_same_world_view_depth_as_terrain() {
        assert!(SHADER.contains("u.matrix*vec4<f32>(i.world,1.)"));
        assert!(SHADER.contains("i.world.z<u.bounds.z"));
    }
}

pub struct ShoreGpu {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}
impl ShoreGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        element: &Path,
        areas: &[crate::water::WaterArea],
    ) -> Result<Option<Self>, String> {
        let (verts, idx) = mesh(areas);
        if idx.is_empty() {
            return Ok(None);
        }
        let mut file =
            std::fs::File::open(element.join("textures.pck")).map_err(|e| e.to_string())?;
        let entries = crate::pck::directory(&mut file)?;
        let e = entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case("textures/water/wave1.dds"))
            .ok_or("missing textures/water/wave1.dds")?;
        let raw = crate::pck::read_entry(&mut file, e)?;
        let images = crate::water::decode_texture_mips(&raw)?;
        Ok(Some(Self::from_images(
            device, queue, format, images, verts, idx,
        )))
    }
    fn from_images(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        images: Vec<image::RgbaImage>,
        verts: Vec<Vertex>,
        idx: Vec<u32>,
    ) -> Self {
        let (w, h) = images[0].dimensions();
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("client shoreline wave texture"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: images.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, image) in images.iter().enumerate() {
            let (mw, mh) = image.dimensions();
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &tex,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                image,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(mw * 4),
                    rows_per_image: Some(mh),
                },
                wgpu::Extent3d {
                    width: mw,
                    height: mh,
                    depth_or_array_layers: 1,
                },
            );
        }
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shoreline uniforms"),
            contents: bytemuck::bytes_of(&Uniform {
                matrix: Mat4::IDENTITY.to_cols_array_2d(),
                eye_night: [0.; 4],
                offsets: [0.; 4],
                flags: [0.; 4],
                bounds: [0.; 4],
                fog_range: [0.; 4],
                fog_color: [0.; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            // Client 0x7cb0ca sets U=WRAP (1), V=CLAMP (3)
            // for all three shoreline texture stages.
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shoreline layout"),
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
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shoreline bindings"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &tex.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("client shoreline shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shoreline pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        const ATTR: [wgpu::VertexAttribute; 5] =
            wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x2,2=>Uint32,3=>Uint32,4=>Float32];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shoreline wave"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTR,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: format.remove_srgb_suffix(),
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
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
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shoreline vertices"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shoreline indices"),
            contents: bytemuck::cast_slice(&idx),
            usage: wgpu::BufferUsages::INDEX,
        });
        Self {
            pipeline,
            bind,
            uniform,
            vertices,
            indices,
            count: idx.len() as u32,
        }
    }
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        matrix: Mat4,
        eye: Vec3,
        night: f32,
        seconds: f32,
        bounds: Option<[f32; 4]>,
        fog_range: [f32; 4],
        fog_color: [f32; 4],
    ) {
        let u = Uniform {
            matrix: matrix.to_cols_array_2d(),
            eye_night: [eye.x, eye.y, eye.z, night],
            offsets: [
                wave_offset(seconds, 0., false),
                wave_offset(seconds, 2.5, false),
                wave_offset(seconds, 0.4, true),
                0.,
            ],
            // flags.y is the wave sight range: wavevs takes it as c0.w =
            // 1/(range*range) rather than hard-coding the radius in the shader.
            flags: [
                0.,
                WAVE_SIGHT_RANGE,
                if bounds.is_some() { 1. } else { 0. },
                0.,
            ],
            bounds: bounds.unwrap_or([0.; 4]),
            fog_range,
            fog_color,
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));
        let mut e = device.create_command_encoder(&Default::default());
        {
            let mut pass = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shoreline waves"),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.count, 0, 0..1);
        }
        queue.submit(Some(e.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wave_fade_matches_the_retail_vertex_shader() {
        let eye = [0., 100., 0.];
        // Full strength at the eye, zero at and beyond the sight range.
        assert_eq!(wave_fade(eye, eye), 1.);
        assert_eq!(wave_fade([400., 100., 0.], eye), 0.);
        assert_eq!(wave_fade([900., 100., 0.], eye), 0.);
        // Half way out the squared term is a quarter.
        assert!((wave_fade([200., 100., 0.], eye) - 0.75).abs() < 1e-6);
        // `mov r1.y, c0.y` zeroes the vertical term, so height cannot fade it.
        assert_eq!(
            wave_fade([120., -900., 0.], eye),
            wave_fade([120., 100., 0.], eye)
        );
        assert!((wave_fade([120., -900., 0.], eye) - (1. - 14400. / 160000.)).abs() < 1e-6);
        // A 3D distance would have faded this one to nothing; the shader does not.
        assert!(wave_fade([120., -900., 0.], eye) > 0.5);
    }
    #[test]
    fn curve_uses_client_phases_and_repeats() {
        assert!((wave_offset(0., 0., false) - 0.3).abs() < 0.00001);
        assert!(wave_offset(1., 0., false).abs() < 0.00001);
        assert!((wave_offset(3., 0., false) + 0.25).abs() < 0.00001);
        assert!(wave_offset(6., 0., false).abs() < 0.00001);
        assert!((wave_offset(10., 0., false) - 0.3).abs() < 0.00001);
    }
    #[test]
    fn compressed_wave_dds_preserves_mip_levels() {
        let mut b = vec![0u8; 128 + 8 + 8];
        b[..4].copy_from_slice(b"DDS ");
        for (p, v) in [
            (4, 124u32),
            (8, 0x81007 | 0x20000),
            (12, 4),
            (16, 4),
            (20, 8),
            (28, 2),
            (76, 32),
            (80, 4),
            (84, 0x31545844),
            (108, 0x401008),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[128..130].copy_from_slice(&0xf800u16.to_le_bytes());
        b[136..138].copy_from_slice(&0x07e0u16.to_le_bytes());
        let levels = crate::water::decode_texture_mips(&b).unwrap();
        assert_eq!(
            levels.iter().map(|x| x.dimensions()).collect::<Vec<_>>(),
            [(4, 4), (2, 2)]
        );
        assert!(levels[0].get_pixel(0, 0)[0] > 200);
        assert!(levels[1].get_pixel(0, 0)[1] > 200);
        let truncated = crate::water::decode_texture_mips(&b[..b.len() - 1]).unwrap();
        assert_eq!(truncated.len(), 1);
    }
    #[test]
    fn real_shoreline_mesh_if_available() {
        let Ok(path) = std::env::var("PW_WATER_SAMPLE_69") else {
            return;
        };
        let Ok(b) = std::fs::read(path) else {
            return;
        };
        let area = crate::water::WaterArea::parse(&b[..0x9df4], 0x3d17, 0).unwrap();
        let (v, i) = mesh(&[area]);
        assert_eq!(v.len(), 3 * (52 + 45 + 41 + 37 + 40 + 132 + 286 + 34));
        assert_eq!(i.len(), 12 * (52 + 45 + 41 + 37 + 40 + 132 + 286 + 34 - 8));
        assert!(v.iter().all(|v| v.p.iter().all(|v| v.is_finite())));
        assert!(v
            .chunks_exact(3)
            .all(|c| c[0].alpha == 0. && c[1].alpha == 1. && c[2].alpha == 0.));
    }
    #[test]
    fn gpu_shoreline_shader_draws() {
        let instance = wgpu::Instance::default();
        let Some(adapter) = pollster::block_on(instance.request_adapter(&Default::default()))
        else {
            return;
        };
        let (dev, q) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            },
            None,
        ))
        .unwrap();
        for case in 0..3 {
            for format in [
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ] {
                let alternate = [format.remove_srgb_suffix()];
                let mk = |x, y| Vertex {
                    p: [x, y, 0.5],
                    uv: [0., 0.],
                    day: 0xffffffff,
                    night: 0xffffffff,
                    alpha: 1.,
                };
                let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 255, 255, 255]));
                let shore = ShoreGpu::from_images(
                    &dev,
                    &q,
                    format,
                    vec![img],
                    vec![mk(-1., -1.), mk(1., -1.), mk(0., 1.)],
                    vec![0, 1, 2],
                );
                let target = dev.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: 32,
                        height: 32,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: if format.is_srgb() { &alternate } else { &[] },
                });
                let depth = crate::gpu::depth(&dev, 32, 32);
                let view = target.create_view(&Default::default());
                let dv = depth.create_view(&Default::default());
                let mut enc = dev.create_command_encoder(&Default::default());
                {
                    let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
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
                }
                q.submit(Some(enc.finish()));
                let raw = target.create_view(&wgpu::TextureViewDescriptor {
                    format: Some(format.remove_srgb_suffix()),
                    ..Default::default()
                });
                shore.draw(
                    &dev,
                    &q,
                    &raw,
                    &dv,
                    Mat4::IDENTITY,
                    Vec3::ZERO,
                    0.,
                    0.,
                    if case == 1 {
                        Some([100., 200., 100., 200.])
                    } else {
                        None
                    },
                    if case == 2 {
                        [0., 0.5, 1., 0.]
                    } else {
                        [0.; 4]
                    },
                    [0.2, 0.4, 0.6, 1.],
                );
                let buf = dev.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 256 * 32,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut enc = dev.create_command_encoder(&Default::default());
                enc.copy_texture_to_buffer(
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
                            rows_per_image: Some(32),
                        },
                    },
                    wgpu::Extent3d {
                        width: 32,
                        height: 32,
                        depth_or_array_layers: 1,
                    },
                );
                q.submit(Some(enc.finish()));
                let slice = buf.slice(..);
                let (tx, rx) = std::sync::mpsc::channel();
                slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
                dev.poll(wgpu::Maintain::Wait);
                rx.recv().unwrap().unwrap();
                let data = slice.get_mapped_range();
                assert!(
                    ((data[16 * 256 + 16 * 4] as i32)
                        - (match case {
                            0 => 153,
                            1 => 0,
                            _ => 31,
                        }))
                    .abs()
                        <= 2,
                    "shoreline encoded blend incorrect: {:?}",
                    &data[16 * 256 + 16 * 4..16 * 256 + 16 * 4 + 4]
                );
            }
        }
    }
}
