//! Camera-relative world-space precipitation driven by the native 33 ms tick
//! (`CECWorld::Tick` `0x429378` `>0x21` → `CECWeather::Update` vtable `+0x48`,
//! `0x9b3eec` accumulator) and the `EC-BSD` area flags (`0x421520` bits 1+2
//! → `WeatherSelection::RAIN/SNOW`). The two objects' own stores are consumed
//! verbatim: `rain.tga` `Speed 0.5 Size 0.04 ±30% ±50%` (`0x42901c` `RAIN`) and
//! `snow.bmp` `Size 0.3` / `Speed 0.5→0.06` (`0x42919f` `SNOW`), with the
//! `ps/snow` luminance→alpha mask and the 33 ms `WeatherClock` exactly as
//! shipped. Generation is still a deterministic camera-centred volume, not the
//! client's RNG/lifetimes/collisions, and the `0x786840`-style off-screen
//! accumulation is not used — the particles are drawn directly with the
//! `Speed`/`Size` lanes `particle[0..3]`/`motion[0..3]` (see `profile_lanes`).
use crate::client_weather::{
    PrecipitationProfile, WeatherClock, WeatherSelection, RAIN, SNOW, SNOW_SPEED_SCALE,
};
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use std::{collections::HashMap, fs::File, path::Path};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    matrix: [[f32; 4]; 4],
    camera: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    forward: [f32; 4],
    fog_range: [f32; 4],
    fog_color: [f32; 4],
    clip: [f32; 4],
    clock_weather: [f32; 4], // wrapped ticks, reserved, rain, snow
    /// `[rain half width, rain size variation, snow half width, snow variation]`
    /// from the two constructors' `Size`/`SizeVariation` stores.
    particle: [f32; 4],
    /// `[rain speed, rain speed variation, snow speed, snow speed variation]`
    /// from the same objects' `Speed`/`SpeedVariation` stores.
    motion: [f32; 4],
}

/// The two uniform lanes the shader reads, built from the objects' stores.
///
/// `particle` is `[rain size, rain size variation, snow size, snow variation]`
/// and `motion` is `[rain speed, rain speed variation, snow speed, snow speed
/// variation]`. A store a constructor never writes contributes `0.0`, which the
/// shader reads as "no spread" rather than as a zero particle.
fn active_instance_range(selection: WeatherSelection) -> (u32, u32) {
    match (selection.rain, selection.snow) {
        (true, true) => (0, 480),
        (true, false) => (0, 320),
        (false, true) => (320, 480),
        (false, false) => (0, 0),
    }
}

fn profile_lanes(
    rain: PrecipitationProfile,
    snow: PrecipitationProfile,
    snow_speed_scale: f32,
) -> ([f32; 4], [f32; 4]) {
    (
        [
            rain.size,
            rain.size_variation.unwrap_or_default(),
            snow.size,
            snow.size_variation.unwrap_or_default(),
        ],
        [
            rain.speed,
            rain.speed_variation.unwrap_or_default(),
            snow.speed * snow_speed_scale,
            snow.speed_variation.unwrap_or_default(),
        ],
    )
}

const SHADER: &str = r#"
struct Params {
    matrix: mat4x4<f32>, camera: vec4<f32>, right: vec4<f32>, up: vec4<f32>,
    forward: vec4<f32>, fog_range: vec4<f32>, fog_color: vec4<f32>,
    clip: vec4<f32>, clock_weather: vec4<f32>,
    particle: vec4<f32>, motion: vec4<f32>,
};
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var images: texture_2d_array<f32>;
@group(0) @binding(2) var smp: sampler;
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec3<f32>,
    @location(2) @interpolate(flat) kind: u32,
    @location(3) view_depth: f32,
    @location(4) world_y: f32,
};
fn hash(n: f32) -> f32 { return fract(sin(n * 127.1 + 311.7) * 43758.5453); }
@vertex fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> Out {
    let snow = instance >= 320u;
    let local_id = f32(select(instance, instance - 320u, snow));
    let kind = select(0u, 1u, snow);
    let enabled = select(p.clock_weather.z, p.clock_weather.w, snow);
    let seed = local_id + f32(kind) * 8192.0;
    let phase = hash(seed + 7.0);
    let x = (hash(seed + 1.0) * 2.0 - 1.0) * 28.0;
    let z = 4.0 + hash(seed + 3.0) * 72.0;
    // Speed/SpeedVariation: the object stores 0.5 for both effects and a
    // 50 % variation for rain only, so the viewer reads the stored percentage
    // literally as +- that fraction of Speed.
    let speed_base = select(p.motion.x, p.motion.z, snow);
    let speed_spread = select(p.motion.y, p.motion.w, snow);
    let speed = speed_base * (1.0 + (hash(seed + 11.0) * 2.0 - 1.0) * speed_spread);
    let seconds = p.clock_weather.x * 0.033;
    // Y must *decrease* with time. `fract` grows, so the particle has to walk
    // from +55 down to -55; the earlier expression added the growing phase,
    // which made every drop and flake climb - the reported "rain goes up".
    let fall = 55.0 - fract(phase + seconds * speed) * 110.0;
    let center = p.camera.xyz + p.right.xyz * x + p.forward.xyz * z + vec3<f32>(0.0, fall, 0.0);
    // The renderer already tested this camera against the same active-cell
    // water mask and zeroes both weather lanes when submerged.
    let visible = enabled > 0.5;
    // The generated quad consists of six triangle-list vertices. Compute the
    // corners from the vertex ID instead of indexing an immutable local WGSL
    // array: naga 0.19 correctly rejects non-constant indexing of that value.
    let right = vertex == 1u || vertex == 4u || vertex == 5u;
    let top = vertex >= 2u;
    let corner = vec2<f32>(select(-1.0, 1.0, right), select(-1.0, 1.0, top));
    let uv_top = vertex == 0u || vertex == 1u || vertex == 4u;
    let uv = vec2<f32>(select(0.0, 1.0, right), select(0.0, 1.0, uv_top));
    // Size/SizeVariation: the quad is the stored Size across, so the recovered
    // 0.04 drop and 0.3 flake replace the viewer's 0.07/0.24 guesses. Rain
    // stores a 30 % size spread; the flake keeps its square shape and has no
    // stored spread. The streak length stays a viewer value: it is the visible
    // consequence of a shutter the client does not have.
    let size_base = select(p.particle.x, p.particle.z, snow) * 0.5;
    let size_spread = select(p.particle.y, p.particle.w, snow);
    let half_width = size_base * (1.0 + (hash(seed + 5.0) * 2.0 - 1.0) * size_spread);
    let half_height = select(1.1 + hash(seed + 17.0) * 1.3, half_width, snow);
    let vertical = select(vec3<f32>(0.0, corner.y * half_height, 0.0), p.up.xyz * (corner.y * half_height), snow);
    let world = center + p.right.xyz * (corner.x * half_width) + vertical;
    let clip = p.matrix * vec4<f32>(world, 1.0);
    var o: Out;
    o.position = select(clip, vec4<f32>(2.0, 2.0, 2.0, 1.0), !visible);
    o.uv = uv;
    o.color = select(vec3<f32>(0.66, 0.78, 0.91), vec3<f32>(0.91, 0.95, 1.0), snow);
    o.kind = kind;
    o.view_depth = clip.w;
    o.world_y = world.y;
    return o;
}
fn fogged(color: vec3<f32>, depth: f32) -> vec3<f32> {
    if p.fog_range.z < 0.5 { return color; }
    let span = p.fog_range.y - p.fog_range.x;
    let f = select(clamp((depth - p.fog_range.x) / select(span, 0.001, abs(span) < 0.001), 0.0, 1.0),
                   select(0.0, 1.0, depth >= p.fog_range.y), abs(span) < 0.001);
    let encoded = select(color * 12.92, 1.055 * pow(max(color, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055, color > vec3<f32>(0.0031308));
    let mixed = mix(encoded, p.fog_color.rgb, f);
    return select(mixed / 12.92, pow((max(mixed, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4)), mixed > vec3<f32>(0.04045));
}
@fragment fn fs(i: Out) -> @location(0) vec4<f32> {
    if abs(p.clip.y) > 0.5 && (i.world_y - p.clip.x) * p.clip.y < 0.0 { discard; }
    let tex = textureSample(images, smp, i.uv, i32(i.kind));
    let alpha = tex.a * select(0.52, 0.82, i.kind == 1u);
    if alpha < 0.003 { discard; }
    return vec4<f32>(fogged(tex.rgb * i.color, i.view_depth), alpha);
}
"#;

/// The client's snow BMP is grayscale on black without alpha. Convert the
/// luminance to an alpha mask before filtering to avoid opaque square cards.
fn snow_luminance_mask(mut pixels: image::RgbaImage) -> image::RgbaImage {
    for pixel in pixels.pixels_mut() {
        let [r, g, b, _] = pixel.0;
        let alpha = ((77 * r as u16 + 150 * g as u16 + 29 * b as u16 + 128) >> 8) as u8;
        *pixel = image::Rgba([255, 255, 255, alpha]);
    }
    pixels
}

fn weather_images(element: &Path) -> [image::RgbaImage; 2] {
    const SIZE: u32 = 64;
    let mut file = File::open(element.join("textures.pck")).ok();
    let entries = file
        .as_mut()
        .and_then(|file| crate::pck::directory(file).ok())
        .unwrap_or_default();
    let load = |name: &str, kind: usize, file: &mut Option<File>| {
        let bytes = entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
            .and_then(|entry| {
                file.as_mut()
                    .and_then(|file| crate::pck::read_entry(file, entry).ok())
            });
        let decoded = bytes
            .and_then(|bytes| image::load_from_memory(&bytes).ok())
            .map(|img| img.to_rgba8());
        if let Some(mut img) = decoded {
            if kind == 1 {
                img = snow_luminance_mask(img);
            }
            if img.width() == SIZE && img.height() == SIZE {
                return img;
            }
            return image::imageops::resize(
                &img,
                SIZE,
                SIZE,
                image::imageops::FilterType::Triangle,
            );
        }
        eprintln!("Native {name} weather texture unavailable; using a simple fallback mask");
        image::RgbaImage::from_fn(SIZE, SIZE, |x, y| {
            let dx = x as f32 - 31.5;
            let dy = y as f32 - 31.5;
            let alpha = if kind == 0 {
                if x >= 29 && x <= 34 {
                    220
                } else {
                    0
                }
            } else if dx * dx + dy * dy <= 220. {
                220
            } else {
                0
            };
            image::Rgba([255, 255, 255, alpha])
        })
    };
    [
        load("textures/rain.tga", 0, &mut file),
        load("textures/snow.bmp", 1, &mut file),
    ]
}

pub struct WeatherGpu {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    _texture: wgpu::Texture,
    _sampler: wgpu::Sampler,
    clock: WeatherClock,
    ticks: u32,
}
impl WeatherGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        element: &Path,
        format: wgpu::TextureFormat,
    ) -> Self {
        let images = weather_images(element);
        let mut pixels = Vec::with_capacity(64 * 64 * 4 * 2);
        for image in images {
            pixels.extend_from_slice(image.as_raw());
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("client rain and snow textures"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 2,
            },
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
            &pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(64 * 4),
                rows_per_image: Some(64),
            },
            wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 2,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("client rain and snow texture array"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("weather mask sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world weather particle layout"),
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
                        view_dimension: wgpu::TextureViewDimension::D2Array,
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
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("world weather parameters"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world weather particle bindings"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
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
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world weather particle shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world weather particle pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world weather particle pipeline"),
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
                // The world camera uses a reversed-Z projection and clears
                // depth to 0, so translucent world particles must pass when
                // their depth is GREATER than the opaque depth in front.
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        Self {
            pipeline,
            bind,
            uniform,
            _texture: texture,
            _sampler: sampler,
            clock: WeatherClock::default(),
            ticks: 0,
        }
    }

    /// Advance the native fixed-step weather clock exactly once per world frame.
    pub fn advance(&mut self, delta_ms: u32) {
        self.ticks = self.ticks.wrapping_add(self.clock.advance(delta_ms));
    }

    /// Submit a depth-tested world-space precipitation view. The generated
    /// particles fall through a camera-centred volume. The renderer suppresses
    /// the pass when its exact CPU water-mask query marks the camera submerged,
    /// avoiding a repeated water-area scan in every particle vertex. Native
    /// RNG/lifetimes and terrain collision remain outstanding.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        matrix: Mat4,
        camera: Vec3,
        yaw: f32,
        pitch: f32,
        fog: crate::fog_matrix::Fog,
        clip: [f32; 4],
        selection: WeatherSelection,
    ) {
        if !selection.rain && !selection.snow {
            return;
        }
        let (s, c) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let right = Vec3::new(c, 0., s);
        let forward = Vec3::new(s * cp, sp, -c * cp);
        let up = right.cross(forward).normalize_or_zero();
        let lanes = profile_lanes(RAIN, SNOW, SNOW_SPEED_SCALE);
        let params = Params {
            matrix: matrix.to_cols_array_2d(),
            camera: [camera.x, camera.y, camera.z, 1.],
            right: [right.x, right.y, right.z, 0.],
            up: [up.x, up.y, up.z, 0.],
            forward: [forward.x, forward.y, forward.z, 0.],
            fog_range: fog.range(),
            fog_color: fog.color4(),
            clip,
            clock_weather: [
                (self.ticks % 1_000_000) as f32,
                0.,
                selection.rain as u8 as f32,
                selection.snow as u8 as f32,
            ],
            particle: lanes.0,
            motion: lanes.1,
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&params));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("regional world-space precipitation"),
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
        let (first_instance, end_instance) = active_instance_range(selection);
        pass.draw(0..6, first_instance..end_instance);
    }
}

#[cfg(test)]
mod tests {
    use super::{active_instance_range, profile_lanes, snow_luminance_mask};
    use crate::client_weather::{RAIN, SNOW, SNOW_SPEED_SCALE};

    #[test]
    fn the_shader_lanes_carry_the_two_constructors_stores() {
        let (particle, motion) = profile_lanes(RAIN, SNOW, SNOW_SPEED_SCALE);
        assert_eq!(particle[0], 0.04, "rain Size");
        assert_eq!(particle[1], 0.3, "rain SizeVariation");
        assert_eq!(particle[2], 0.3, "snow Size");
        assert_eq!(particle[3], 0.0, "snow stores no SizeVariation");
        assert_eq!(motion[0], 0.5, "rain Speed");
        assert_eq!(motion[1], 0.5, "rain SpeedVariation");
        assert_eq!(motion[2], 0.5 * SNOW_SPEED_SCALE, "snow descent");
        assert_eq!(motion[3], 0.0, "snow stores no SpeedVariation");
        // Rain is visibly the faster of the two, which is the only reason the
        // viewer keeps a flake scale at all.
        assert!(motion[2] < motion[0]);
        // A profile with no variation must not read as a zero particle.
        assert!(particle[2] > 0. && motion[2] > 0.);
    }

    #[test]
    fn only_enabled_weather_particle_instances_are_submitted() {
        assert_eq!(
            active_instance_range(crate::client_weather::WeatherSelection {
                rain: true,
                snow: false,
            }),
            (0, 320)
        );
        assert_eq!(
            active_instance_range(crate::client_weather::WeatherSelection {
                rain: false,
                snow: true,
            }),
            (320, 480)
        );
        assert_eq!(
            active_instance_range(crate::client_weather::WeatherSelection {
                rain: true,
                snow: true,
            }),
            (0, 480)
        );
    }

    #[test]
    fn particle_vertices_do_not_rewalk_water_masks() {
        assert!(!super::SHADER.contains("camera_in_mapped_water"));
        assert!(!super::SHADER.contains("water_areas"));
        assert!(!super::SHADER.contains("water_mask"));
    }

    #[test]
    fn snow_bmp_luminance_becomes_antialiased_transparency() {
        let image = image::RgbaImage::from_raw(
            3,
            1,
            vec![0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255],
        )
        .unwrap();
        let masked = snow_luminance_mask(image);
        assert_eq!(masked.get_pixel(0, 0).0, [255, 255, 255, 0]);
        assert_eq!(masked.get_pixel(1, 0).0, [255, 255, 255, 128]);
        assert_eq!(masked.get_pixel(2, 0).0, [255, 255, 255, 255]);
    }
}
