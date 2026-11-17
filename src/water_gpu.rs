//! Experimental multi-pass water, informed by client water_expensive.txt.
//! Reflection is rendered per distinct visible water elevation.
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct WaterUniform {
    pub matrix: [[f32; 4]; 4],
    pub inverse_matrix: [[f32; 4]; 4],
    pub eye_time: [f32; 4],
    pub size: [f32; 4],
    /// x: fractional frame; y: regional underwater fog end distance;
    /// z: output target is sRGB (filled in by WaterGpu::update);
    /// w: bounded angular phase for client normal-map offset.
    pub animation: [f32; 4],
    /// x: original-style rasterized edge alpha is available in refraction.
    pub edge_mode: [f32; 4],
    /// x: water-surface fog start; y: fog end; z: 1 when fog applies;
    /// w: unused. Appended after `edge_mode`, so earlier offsets are unchanged.
    pub fog: [f32; 4],
    /// The water-surface fog colour bound right before the water draw.
    pub fog_color: [f32; 4],
}

/// Retail increments the normal index every 33 milliseconds. Fractional
/// position is retained for the viewer's smooth frame interpolation.
pub fn normal_phase(elapsed_seconds: f64) -> f32 {
    if !elapsed_seconds.is_finite() || elapsed_seconds < 0. {
        return 0.;
    }
    ((elapsed_seconds * (1000.0 / 33.0)).rem_euclid(29.0)) as f32
}
/// The default client water mode rotates its 0.05 texture offset by 25
/// degrees per second (elapsed milliseconds * 0.025 degrees).
pub fn normal_offset_angle(elapsed_seconds: f64) -> f32 {
    if !elapsed_seconds.is_finite() || elapsed_seconds < 0. {
        return 0.;
    }
    (elapsed_seconds * (25.0_f64.to_radians())).rem_euclid(std::f64::consts::TAU) as f32
}

/// Fog range set for the per-plane clipped refraction render at 0x7cc6e0.
pub fn refraction_fog_range(
    eye_height: f32,
    plane_height: f32,
    underwater_fog_end: f32,
) -> [f32; 2] {
    let height = eye_height - plane_height;
    [height + 0.01, height + 0.55 * underwater_fog_end]
}

/// The client adds an integer cell-dependent UV base and interpolates one
/// unit across each grid cell. With repeat addressing the integer base can be
/// omitted; layer 1 doubles these coordinates in the shader.
pub fn cell_base_uv() -> [[f32; 2]; 4] {
    [[0., 0.], [1., 0.], [0., 1.], [1., 1.]]
}
const WATER_SHADER: &str = r#"
struct W {matrix:mat4x4<f32>,inverse_matrix:mat4x4<f32>,eye_time:vec4<f32>,size:vec4<f32>,animation:vec4<f32>,edge_mode:vec4<f32>,fog:vec4<f32>,fog_color:vec4<f32>};
@group(0) @binding(0) var<uniform> w:W;
@group(0) @binding(1) var reflection:texture_2d<f32>;
@group(0) @binding(2) var refraction:texture_2d<f32>;
@group(0) @binding(3) var normals:texture_2d_array<f32>;
@group(0) @binding(4) var screen_sampler:sampler;
@group(0) @binding(5) var tile_sampler:sampler;
@group(0) @binding(6) var scene_depth:texture_depth_2d;
@group(0) @binding(7) var terrain_depth:texture_depth_2d;
@group(0) @binding(8) var scene_color:texture_2d<f32>;
struct In { @location(0) pos:vec3<f32>, @location(1) normal:vec3<f32>, @location(2) uv:vec2<f32>, @location(3) color:vec4<f32>, @location(4) kind:f32 };
struct Out { @builtin(position) pos:vec4<f32>, @location(0) world:vec3<f32>, @location(1) uv:vec2<f32> };
@vertex fn water_vs(i:In)->Out {var o:Out;o.pos=w.matrix*vec4<f32>(i.pos,1.);o.world=i.pos;o.uv=i.uv;return o;}
fn present_encoded(c:vec3<f32>)->vec3<f32>{
 if w.animation.z<0.5 {return c;}
 let v=max(c,vec3<f32>(0.));
 return select(v/12.92,pow((v+0.055)/1.055,vec3<f32>(2.4)),v>vec3<f32>(0.04045));
}
// The client binds its water-surface fog triple with D3DRS_FOGSTART/END/COLOR
// right before the water draw (0x7cb444, 0x7cb502, 0x7cb94e) and D3D applies
// the fixed-function fog stage to the source colour. The water shaders carry
// no fog arithmetic of their own, so this is that stage, not a shader mix.
fn surface_fog(c:vec3<f32>, reciprocal_w:f32)->vec3<f32>{
 // Fragment position.w is 1/clip.w, not view depth. The viewer's
 // perspective matrix has clip.w = positive view Z (also in reversed Z).
 let depth=1.0/max(reciprocal_w,0.0000001);
 if w.fog.z<0.5 {return c;}
 let span=w.fog.y-w.fog.x;
 let f=select(clamp((w.fog.y-depth)/select(span,0.001,abs(span)<0.001),0.0,1.0),select(1.,0.,depth>=w.fog.y),abs(span)<0.001);
 return mix(w.fog_color.rgb,c,f);
}
@fragment fn water_fs(i:Out)->@location(0) vec4<f32> {
 // Default client texture layer 1: 2x base-cell UV with a rotating
 // radius-.05 translation (0x7c95a8, 0x7ca47b).
 let angle=w.animation.w;
 let uv=i.uv*2.0+0.05*vec2<f32>(cos(angle),sin(angle));
 // The retail update routine at 0x7ca5ea advances the 29 normal frames
 // once per 33 ms. Interpolation is a viewer extension that preserves the
 // requested smooth playback while retaining the recovered cycle timing.
 let phase=w.animation.x; let frame=i32(floor(phase))%29;
 let n0=textureSample(normals,tile_sampler,uv,frame).xyz*2.0-vec3<f32>(1.0);
 let n1=textureSample(normals,tile_sampler,uv,(frame+1)%29).xyz*2.0-vec3<f32>(1.0);
 // The retail pixel shader uses the decoded normal-map vector directly;
 // normalizing it changes both the bump displacement and Fresnel response.
 let n=mix(n0,n1,fract(phase));
 let screen=i.pos.xy/w.size.xy;
 if w.size.w>0.5 && w.eye_time.y<=i.world.y {
   // 0x7ca9af selects waterrefract.txt below the surface, not the
   // expensive reflected-water shader. t3 is eye minus vertex in the
   // original water_vs; ps.1.4 takes its dot with the negated normal.
   let uv_under=clamp(screen+n.xz*0.25,vec2<f32>(0.001),vec2<f32>(0.999));
   let refr=textureSample(refraction,screen_sampler,uv_under).rgb;
   let facing=clamp(dot(w.eye_time.xyz-i.world,-n),0.0,1.0);
   let fresnel=pow(1.0-facing,5.0);
   let tint=mix(vec3<f32>(1.0),vec3<f32>(0.0,0.3,0.3),fresnel);
   return vec4<f32>(present_encoded(surface_fog(refr*0.85*tint,i.pos.w)),1.0);
 }
 let z=textureLoad(terrain_depth,vec2<i32>(i.pos.xy),0);
 // The client edge pass uses saturate((waterY - terrainY)/0.66).
 // The terrain-only depth copy excludes trees and other scenery. The
 // original draws a separate edge mask; this reconstructs its height.
 var edge=1.0;
 if z>0.0000001 && w.eye_time.y>i.world.y {
   let clip=vec4<f32>(screen.x*2.0-1.0,1.0-screen.y*2.0,z,1.0);
   let hit=w.inverse_matrix*clip;
   if abs(hit.w)>0.0000001 { edge=clamp((i.world.y-hit.y/hit.w)/0.66,0.0,1.0); }
 }
 // water_expensive.txt: c1=(0.25,-0.15,...), with distortion
 // attenuated by the squared edge alpha. The depth-derived edge remains provisional.
 if w.edge_mode.x>0.5 && w.eye_time.y>i.world.y { edge=textureSample(refraction,screen_sampler,screen).a; }
 let bump=n.xz*edge*edge;
 let mirrored=textureSample(reflection,screen_sampler,clamp(screen+bump*0.25,vec2<f32>(0.001),vec2<f32>(0.999))).rgb;
 let fallback=textureSample(scene_color,screen_sampler,screen).rgb*vec3<f32>(0.75,0.85,0.95);
 let reflect_color=select(fallback,mirrored,w.size.w>0.5);
 // Reject displaced samples in front of the water plane. Without this,
 // shore objects smear across the water even when they should occlude it.
 let refr_uv=clamp(screen-bump*0.15,vec2<f32>(0.001),vec2<f32>(0.999));
 let refr_px=vec2<i32>(clamp(refr_uv*w.size.xy,vec2<f32>(0.),w.size.xy-vec2<f32>(1.)));
 let refr_depth=textureLoad(scene_depth,refr_px,0);
 let use_guard=w.size.w<0.5 || w.eye_time.y<=i.world.y;
 let safe_uv=select(refr_uv,screen,use_guard && refr_depth>i.pos.z+0.000001);
 let refract_color=textureSample(refraction,screen_sampler,safe_uv).rgb;
 let view=normalize(w.eye_time.xyz-i.world);
 let facing=clamp(dot(view,n),0.0,1.0);
 // Expensive water path sets PS c0.a from camera height above this
 // water plane and the current regional underwater fog end (0x7ca8d0).
 let fog_end=max(w.animation.y,0.000001);
 let min_reflect=clamp(0.23*(w.eye_time.y-i.world.y)/fog_end,0.11,0.23);
 // water_expensive.txt: the quintic is `(1-cos)^5`, taken from `1-r1.a` where
 // r1.a is the saturating dp3_x2 of the packed eye vector and the bias normal.
 // The lerp weight is `lrp r0.a, r0.a, c2.a, c0.a`, i.e. with c2.a = 1.0 and
 // c0.a = the program's minimum reflect alpha:
 //     weight = c0.a + (1 - c0.a) * quintic
 // The simple and cheap variants instead use their own literal `c1.a` (0.8 and
 // 0.6) as that identity term, so this camera-derived value is the expensive
 // path only.
 let quintic=pow(1.0-facing,5.0);
 let fresnel=clamp(min_reflect+(1.0-min_reflect)*quintic,0.0,1.0);
 // The paired water_vs shader writes a zero secondary diffuse/specular color.
 let color=mix(refract_color*0.95,reflect_color,fresnel);
 let background=textureSample(scene_color,screen_sampler,screen).rgb;
 // Fog is the draw's own state and covers the whole surface, including the
 // edge-alpha blend with the already-fogged scene behind it.
 return vec4<f32>(present_encoded(surface_fog(mix(background,color,edge),i.pos.w)),1.0);
}
struct FSOut { @builtin(position) pos:vec4<f32>, @location(0) uv:vec2<f32> };
@vertex fn blit_vs(@builtin(vertex_index) id:u32)->FSOut { var o:FSOut;let x=f32((id<<1u)&2u);let y=f32(id&2u);o.uv=vec2<f32>(x,y);o.pos=vec4<f32>(x*2.0-1.0,1.0-y*2.0,0.0,1.0);return o;}
@fragment fn blit_fs(i:FSOut)->@location(0) vec4<f32>{return vec4<f32>(present_encoded(textureSample(scene_color,screen_sampler,i.uv).rgb),1.);}
"#;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EdgeUniform {
    matrix: [[f32; 4]; 4],
    bounds: [f32; 4],
    params: [f32; 4],
}
const EDGE_SHADER: &str = r#"
struct E { matrix:mat4x4<f32>,bounds:vec4<f32>,params:vec4<f32> };
@group(0) @binding(0) var<uniform> e:E;
struct O { @builtin(position) pos:vec4<f32>, @location(0) world:vec3<f32>, @location(1) @interpolate(flat) far:u32 };
@vertex fn near_vs(@location(0) pos:vec3<f32>)->O { var o:O;o.pos=e.matrix*vec4<f32>(pos,1.);o.world=pos;o.far=0u;return o; }
@vertex fn far_vs(@location(0) pos:vec3<f32>)->O { var o:O;o.pos=e.matrix*vec4<f32>(pos,1.);o.world=pos;o.far=1u;return o; }
@fragment fn fs(i:O)->@location(0) vec4<f32> {
 if i.far==1u && i.world.x>=e.bounds.x && i.world.x<e.bounds.y && i.world.z>=e.bounds.z && i.world.z<e.bounds.w { discard; }
 return vec4<f32>(1.,1.,1.,clamp((e.params.x-i.world.y)*e.params.y,0.,1.));
}
"#;
struct Targets {
    reflection: wgpu::Texture,
    refraction: wgpu::Texture,
    scene: wgpu::Texture,
    reflect_depth: wgpu::Texture,
    sample_depth: wgpu::Texture,
    terrain_depth: wgpu::Texture,
    width: u32,
    height: u32,
    bind: wgpu::BindGroup,
}
/// The largest `|x|` or `|z|` a decoded texel of the water normal map holds,
/// where a decoded component is `texel * 2 - 1`.
///
/// The surface displaces its screen-space samples by `n.xz * edge * edge * 0.25`.
/// `edge` is in `0..1`, so `|n.xz|` is the only unknown, and it is a property of
/// the shipped map rather than of the frame: measuring it once at load is what
/// lets a reflection or refraction render be scissored to the part of the screen
/// the surface can actually read, instead of to a quarter of the screen on every
/// side. Every mip level is scanned, and a filtered sample or the cross-frame
/// blend of two of them is an average of these values, so neither can exceed it.
fn normal_extent(frames: &[Vec<image::RgbaImage>]) -> f32 {
    let mut extent: f32 = 0.;
    for levels in frames {
        for image in levels {
            for px in image.pixels() {
                // `n.xz` is the decoded red and blue channels.
                for channel in [0, 2] {
                    let decoded = px.0[channel] as f32 / 255. * 2. - 1.;
                    extent = extent.max(decoded.abs());
                }
            }
        }
    }
    extent.clamp(0., 1.)
}

pub struct WaterGpu {
    layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    normal: wgpu::Texture,
    screen_sampler: wgpu::Sampler,
    tile_sampler: wgpu::Sampler,
    targets: Targets,
    water_pipeline: wgpu::RenderPipeline,
    blit_pipeline: wgpu::RenderPipeline,
    edge_uniform: wgpu::Buffer,
    edge_bind: wgpu::BindGroup,
    edge_near: wgpu::RenderPipeline,
    edge_far: wgpu::RenderPipeline,
    format: wgpu::TextureFormat,
    /// Largest `|x|` or `|z|` in the loaded normal map, decoded as the shader
    /// decodes it. The scissor for the reflection and refraction renders is sized
    /// from this instead of from the worst case a normal map could hold.
    normal_extent: f32,
}
fn color(device: &wgpu::Device, format: wgpu::TextureFormat, w: u32, h: u32) -> wgpu::Texture {
    let alternate = [format.remove_srgb_suffix()];
    let views = if format.is_srgb() {
        &alternate[..]
    } else {
        &[]
    };
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("water scene map"),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: views,
    })
}
impl WaterGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        w: u32,
        h: u32,
        frames: &[Vec<image::RgbaImage>],
    ) -> Self {
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("water uniforms"),
            contents: bytemuck::bytes_of(&WaterUniform {
                matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
                inverse_matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
                eye_time: [0.; 4],
                size: [w as f32, h as f32, 0., 0.],
                animation: [0.; 4],
                edge_mode: [0.; 4],
                fog: [0., 1., 0., 0.],
                fog_color: [1., 1., 1., 1.],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let (nw, nh) = frames[0][0].dimensions();
        let normal = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("client water normals"),
            size: wgpu::Extent3d {
                width: nw,
                height: nh,
                depth_or_array_layers: frames.len() as u32,
            },
            mip_level_count: frames[0].len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (i, levels) in frames.iter().enumerate() {
            for (level, img) in levels.iter().enumerate() {
                let (mw, mh) = img.dimensions();
                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &normal,
                        mip_level: level as u32,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: i as u32,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    img.as_raw(),
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
        }
        let screen_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let tile_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("water bindings"),
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
                    binding: 8,
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
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("experimental water"),
            source: wgpu::ShaderSource::Wgsl(WATER_SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("water pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        const ATTR: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32];
        let water_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("water shading"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "water_vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<crate::gpu::Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTR,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "water_fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
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
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("water background"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "blit_vs",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "blit_fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
        });
        // The client renders water_edge_vs/ps into only the refraction
        // target's alpha channel, preserving the previously rendered RGB.
        let edge_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("water edge uniforms"),
            contents: bytemuck::bytes_of(&EdgeUniform {
                matrix: glam::Mat4::IDENTITY.to_cols_array_2d(),
                bounds: [0.; 4],
                params: [0.; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let edge_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("water edge layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let edge_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water edge bind"),
            layout: &edge_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: edge_uniform.as_entire_binding(),
            }],
        });
        let edge_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("client water edge shader"),
            source: wgpu::ShaderSource::Wgsl(EDGE_SHADER.into()),
        });
        let edge_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("water edge pipeline layout"),
            bind_group_layouts: &[&edge_layout],
            push_constant_ranges: &[],
        });
        let make_edge = |stride, entry| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("terrain water edge alpha"),
                layout: Some(&edge_pl),
                vertex: wgpu::VertexState {
                    module: &edge_shader,
                    entry_point: entry,
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: stride,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        }],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &edge_shader,
                    entry_point: "fs",
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALPHA,
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
            })
        };
        let edge_near = make_edge(std::mem::size_of::<crate::gpu::Vertex>() as u64, "near_vs");
        let edge_far = make_edge(16, "far_vs");
        let targets = Self::targets(
            device,
            format,
            w,
            h,
            &layout,
            &uniform,
            &normal,
            &screen_sampler,
            &tile_sampler,
        );
        Self {
            layout,
            uniform,
            normal,
            screen_sampler,
            tile_sampler,
            targets,
            water_pipeline,
            blit_pipeline,
            edge_uniform,
            edge_bind,
            edge_near,
            edge_far,
            format,
            normal_extent: normal_extent(frames),
        }
    }
    fn targets(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        w: u32,
        h: u32,
        layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        normal: &wgpu::Texture,
        screen: &wgpu::Sampler,
        tile: &wgpu::Sampler,
    ) -> Targets {
        let reflection = color(device, format, w, h);
        let refraction = color(device, format, w, h);
        let scene = color(device, format, w, h);
        let reflect_depth = super::gpu::depth(device, w, h);
        let sample_depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("water scene depth copy"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let terrain_depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("water terrain-only depth"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let terrain_depth_view = terrain_depth.create_view(&Default::default());
        let sample_depth_view = sample_depth.create_view(&Default::default());
        let normal_view = normal.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        // Retail water PS filters and combines encoded A8R8G8B8 colors.
        // Reinterpret the rendered sRGB targets as UNORM for those operations.
        let raw_view = wgpu::TextureViewDescriptor {
            format: Some(format.remove_srgb_suffix()),
            ..Default::default()
        };
        let reflect_view = reflection.create_view(&raw_view);
        let refract_view = refraction.create_view(&raw_view);
        let scene_view = scene.create_view(&raw_view);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water maps"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&reflect_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&refract_view),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&normal_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(screen),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(tile),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&sample_depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&terrain_depth_view),
                },
            ],
        });
        Targets {
            reflection,
            refraction,
            scene,
            reflect_depth,
            sample_depth,
            terrain_depth,
            width: w.max(1),
            height: h.max(1),
            bind,
        }
    }
    /// See [`normal_extent`]. Bounded to `0..1`.
    pub fn normal_extent(&self) -> f32 {
        self.normal_extent
    }

    pub fn resize(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        self.targets = Self::targets(
            device,
            self.format,
            w,
            h,
            &self.layout,
            &self.uniform,
            &self.normal,
            &self.screen_sampler,
            &self.tile_sampler,
        );
    }
    pub fn sample_depth(&self) -> wgpu::TextureView {
        self.targets.sample_depth.create_view(&Default::default())
    }
    pub fn is_srgb(&self) -> bool {
        self.format.is_srgb()
    }
    pub fn scene(&self) -> wgpu::TextureView {
        self.targets.scene.create_view(&Default::default())
    }
    pub fn reflection_raw(&self) -> wgpu::TextureView {
        self.targets
            .reflection
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.format.remove_srgb_suffix()),
                ..Default::default()
            })
    }
    pub fn refraction_raw(&self) -> wgpu::TextureView {
        self.targets
            .refraction
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.format.remove_srgb_suffix()),
                ..Default::default()
            })
    }
    pub fn scene_raw(&self) -> wgpu::TextureView {
        self.targets
            .scene
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.format.remove_srgb_suffix()),
                ..Default::default()
            })
    }
    pub fn copy_scene_to_refraction(&self, e: &mut wgpu::CommandEncoder) {
        e.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: &self.targets.scene,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &self.targets.refraction,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.targets.width,
                height: self.targets.height,
                depth_or_array_layers: 1,
            },
        );
    }
    pub fn reflection(&self) -> wgpu::TextureView {
        self.targets.reflection.create_view(&Default::default())
    }
    #[allow(dead_code)]
    pub fn refraction_texture(&self) -> &wgpu::Texture {
        &self.targets.refraction
    }
    pub fn refraction(&self) -> wgpu::TextureView {
        self.targets.refraction.create_view(&Default::default())
    }
    pub fn depth(&self) -> wgpu::TextureView {
        self.targets.reflect_depth.create_view(&Default::default())
    }
    pub fn update(&self, queue: &wgpu::Queue, u: &WaterUniform) {
        let mut value = *u;
        value.animation[2] = if self.format.is_srgb() { 1. } else { 0. };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&value));
    }
    pub fn copy_depth(&self, e: &mut wgpu::CommandEncoder, source: &wgpu::Texture) {
        e.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: source,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::ImageCopyTexture {
                texture: &self.targets.sample_depth,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::Extent3d {
                width: self.targets.width,
                height: self.targets.height,
                depth_or_array_layers: 1,
            },
        );
    }
    pub fn copy_terrain_depth(&self, e: &mut wgpu::CommandEncoder, source: &wgpu::Texture) {
        e.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: source,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::ImageCopyTexture {
                texture: &self.targets.terrain_depth,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::Extent3d {
                width: self.targets.width,
                height: self.targets.height,
                depth_or_array_layers: 1,
            },
        );
    }
    pub fn blit(&self, e: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        {
            let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present refraction"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            p.set_pipeline(&self.blit_pipeline);
            p.set_bind_group(0, &self.targets.bind, &[]);
            p.draw(0..3, 0..1);
        }
    }
    /// Rasterize the client's terrain-height edge shader to the refraction
    /// alpha after the clipped scene, preserving color and scene depth.
    pub fn render_edge(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        matrix: glam::Mat4,
        plane: f32,
        bounds: [f32; 4],
        depth: &wgpu::TextureView,
        far: Option<(&wgpu::Buffer, &wgpu::Buffer, u32)>,
        near: (&wgpu::Buffer, &wgpu::Buffer, u32),
    ) {
        let params = EdgeUniform {
            matrix: matrix.to_cols_array_2d(),
            bounds,
            params: [plane, 1. / 0.66, 0., 0.],
        };
        queue.write_buffer(&self.edge_uniform, 0, bytemuck::bytes_of(&params));
        let mut e = device.create_command_encoder(&Default::default());
        let target = self.refraction();
        {
            let mut pass = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("water edge alpha"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
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
            pass.set_bind_group(0, &self.edge_bind, &[]);
            if let Some(far) = far {
                pass.set_pipeline(&self.edge_far);
                pass.set_vertex_buffer(0, far.0.slice(..));
                pass.set_index_buffer(far.1.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..far.2, 0, 0..1);
            }
            pass.set_pipeline(&self.edge_near);
            pass.set_vertex_buffer(0, near.0.slice(..));
            pass.set_index_buffer(near.1.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..near.2, 0, 0..1);
        }
        queue.submit(Some(e.finish()));
    }
    pub fn draw_water(
        &self,
        e: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        vertex: &wgpu::Buffer,
        index: &wgpu::Buffer,
        start: u32,
        end: u32,
    ) {
        {
            let mut p = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("water surface"),
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
            p.set_pipeline(&self.water_pipeline);
            p.set_bind_group(0, &self.targets.bind, &[]);
            p.set_vertex_buffer(0, vertex.slice(..));
            p.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
            p.draw_indexed(start..end, 0, 0..1);
        }
    }
}

#[cfg(test)]
mod animation_tests {
    use super::{cell_base_uv, normal_offset_angle, normal_phase, refraction_fog_range};
    /// The scissor that makes a reflection or refraction render cheap is sized
    /// from this number, so it has to be the decoded-component maximum the shader
    /// can actually sample: `texel * 2 - 1` of the red and blue channels, over
    /// every frame and every mip.
    #[test]
    fn the_normal_extent_is_the_widest_component_the_shader_can_sample() {
        use image::RgbaImage;
        let flat = RgbaImage::from_pixel(4, 4, image::Rgba([128, 128, 128, 255]));
        // 64 decodes to -0.498, and 128 to +0.004: the widest is the 64.
        let mut tilted = flat.clone();
        tilted.put_pixel(1, 1, image::Rgba([128, 128, 64, 255]));
        assert!(super::normal_extent(&[vec![flat.clone()]]) < 0.01);
        let extent = super::normal_extent(&[vec![flat.clone()], vec![tilted]]);
        assert!((extent - 0.498).abs() < 1e-3, "got {extent}");
        // A component at either end of the byte range is a full unit.
        let extreme = RgbaImage::from_pixel(2, 2, image::Rgba([0, 128, 255, 255]));
        assert!((super::normal_extent(&[vec![extreme]]) - 1.).abs() < 1e-6);
    }

    #[test]
    fn client_refraction_fog_tracks_plane_and_region() {
        let a = refraction_fog_range(30., 20., 100.);
        assert!((a[0] - 10.01).abs() < 1e-5);
        assert!((a[1] - 65.).abs() < 1e-5);
        let b = refraction_fog_range(130., 120., 200.);
        assert!((b[0] - a[0]).abs() < 1e-5);
        assert!((b[1] - 120.).abs() < 1e-5);
    }
    #[test]
    fn client_cell_uv_and_offset_loop() {
        assert_eq!(cell_base_uv(), [[0., 0.], [1., 0.], [0., 1.], [1., 1.]]);
        assert_eq!(normal_offset_angle(0.), 0.);
        assert!((normal_offset_angle(3.6) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!(normal_offset_angle(14.4) < 1e-5);
        assert_eq!(normal_offset_angle(f64::NAN), 0.);
        assert!((normal_offset_angle(1_000_000.0) - normal_offset_angle(1_000_014.4)).abs() < 1e-4);
    }
    #[test]
    fn client_frame_period_and_smooth_loop() {
        assert_eq!(normal_phase(0.), 0.);
        assert!((normal_phase(0.0165) - 0.5).abs() < 1e-5);
        assert!((normal_phase(0.033) - 1.).abs() < 1e-5);
        assert!((normal_phase(0.9405) - 28.5).abs() < 1e-5);
        assert!(normal_phase(0.957) < 1e-4);
        assert!((normal_phase(3600000.0 + 0.0165) - normal_phase(3600000.0) - 0.5).abs() < 1e-3);
        assert_eq!(normal_phase(f64::NAN), 0.);
    }
}
