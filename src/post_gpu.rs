//! Fullscreen glow based on the client's fullglow1_14/fullglow2_14 shader arithmetic,
//! plus client-region-driven underwater fog, spacewarp and optional display gamma.
//! Which stages run is decided by the shared `post_plan` audit matrix, never by
//! ad-hoc conditions at the call site.
#[path = "gamma_curve.rs"]
mod gamma_curve;
#[path = "glow_modulation.rs"]
pub mod glow_modulation;
#[path = "post_plan.rs"]
pub mod post_plan;
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
#[path = "glow_state.rs"]
pub mod glow_state;
pub use glow_state::WorldGlow;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    screen: [f32; 4],
    effect: [f32; 4],
    fog: [f32; 4],
    fog_color: [f32; 4],
    gamma: [f32; 4],
    mono: [f32; 4],
    modulation: [f32; 4],
    ramp: [[f32; 4]; 64],
}
const SHADER: &str = r#"
struct Params {screen:vec4<f32>,effect:vec4<f32>,fog:vec4<f32>,fog_color:vec4<f32>,gamma:vec4<f32>,mono:vec4<f32>,modulation:vec4<f32>,ramp:array<vec4<f32>,64>};
@group(0) @binding(0) var scene:texture_2d<f32>;
@group(0) @binding(1) var blur:texture_2d<f32>;
@group(0) @binding(2) var smp:sampler;
@group(0) @binding(3) var depth:texture_depth_2d;
@group(0) @binding(4) var<uniform> p:Params;
struct Out {@builtin(position) pos:vec4<f32>, @location(0) uv:vec2<f32>};
@vertex fn vs(@builtin(vertex_index) id:u32)->Out {
 var o:Out; let x=f32((id<<1u)&2u);let y=f32(id&2u);
 o.uv=vec2<f32>(x,y);o.pos=vec4<f32>(x*2.-1.,1.-y*2.,0.,1.);return o;
}
// The client first stretch-copies the full-resolution back target into a
// quarter-size render target, then applies the horizontal five-tap pass.
@fragment fn downsample(i:Out)->@location(0) vec4<f32> {return vec4<f32>(textureSample(scene,smp,i.uv).rgb,1.);}
// fullglow1_14: center*c1.r + two inner*c1.a + two outer*c0.a.
@fragment fn glow_h(i:Out)->@location(0) vec4<f32> {
 // 0x40edd0 uses 4/full_width, not 1/floor(full_width/4).
 // These differ for window sizes that are not divisible by four.
 let dx=vec2<f32>(4.0/p.screen.x,0.);
 let c=textureSample(blur,smp,i.uv).rgb*0.310;
 let a=(textureSample(blur,smp,i.uv-dx).rgb+textureSample(blur,smp,i.uv+dx).rgb)*0.238;
 let b=(textureSample(blur,smp,i.uv-dx*2.).rgb+textureSample(blur,smp,i.uv+dx*2.).rgb)*0.107;
 return vec4<f32>(c+a+b,1.);
}
@fragment fn fog_scene(i:Out)->@location(0) vec4<f32> {
 let uv=i.uv;let underwater=p.effect.x>0.;
 var out=textureSample(scene,smp,uv).rgb;
 {
   let pixel=vec2<i32>(clamp(uv*p.screen.xy,vec2<f32>(0.),p.screen.xy-vec2<f32>(1.)));
   let d=textureLoad(depth,pixel,0);
   let far=p.screen.w;let distance=far/max(1.+d*(far-1.),0.0001);
   // Client defaults to D3DFOG_LINEAR (3); depth is view-space z.
   let fog_span=p.fog.y-p.fog.x;
   var fog=select(clamp((distance-p.fog.x)/select(fog_span,0.001,abs(fog_span)<0.001),0.,1.),select(0.,1.,distance>=p.fog.y),abs(fog_span)<0.001);
   if !underwater && d<=0.0000001 { fog=0.; }
   // Scene and intermediate blur are sampled as encoded UNORM values.
   out=mix(out,p.fog_color.rgb,fog);
 }
 return vec4<f32>(out,1.);
}
// Emulate the 256x256 ARGB procedural bump texture built by 0x410f10.
// The client converts -32*sin/cos to integer before subtracting from 128.
fn bump_texel(p:vec2<f32>)->vec2<f32> {
 let theta=12.56637061436*((p.x+p.y)/256.0-1.0);
 return (vec2<f32>(128.0)-trunc(vec2<f32>(cos(theta),sin(theta))*-32.0))/255.0;
}
fn bump_bilinear(uv:vec2<f32>)->vec2<f32> {
 // Half-texel convention of normalized linear texture sampling.
 let p=fract(uv)*256.0-vec2<f32>(0.5);let base=floor(p);let f=fract(p);
 let a=mix(bump_texel(base),bump_texel(base+vec2<f32>(1.,0.)),f.x);
 let b=mix(bump_texel(base+vec2<f32>(0.,1.)),bump_texel(base+vec2<f32>(1.,1.)),f.x);
 return mix(a,b,f.y)*2.0-vec2<f32>(1.0);
}
// 0x40fd33 executes spacewarp, copies its result back, then 0x40fd64
// starts glow. Blur must therefore consume the quantized warped scene.
@fragment fn warp_scene(i:Out)->@location(0) vec4<f32> {
 let uv=i.uv;
 let bump_uv=vec2<f32>(uv.x*3.,uv.y*3.*p.screen.y/p.screen.x+p.screen.z*0.5);
 let normal=bump_bilinear(bump_uv);
 // spacewarp.txt / spacewarp_tl.txt:
 //   mul r2.rg, r2, c1        ; r2 = vertex texcoord * (-1, 1)
 //   mul r3.rg, r0_bx2, r2    ; normal (-1..1) * that scale
 //   mad r1.rg, r3, r0.a, r1  ; offset scaled by the normal map's alpha
 //   sub r0.r, r0.a, c2.r     ; c2.r = 0.05
 //   texkill r0
 // So c2.r is a kill threshold on the normal-map alpha, NOT the offset size.
 // The viewer has no alpha channel here: `bump_texel` synthesises only the
 // (cos, sin) pair at 0x410f10, so the texkill cannot be reproduced yet and is
 // left out rather than guessed. The 0.005 scale below remains an
 // approximation of the per-vertex texcoord the client builds (0x94ef44).
 return vec4<f32>(textureSample(scene,smp,uv+normal*vec2<f32>(-0.005,0.005)).rgb,1.);
}
fn composite(uv:vec2<f32>)->vec3<f32> {
 let original=textureSample(scene,smp,uv).rgb;
 var out=original;
 if p.effect.y>0.5 {
   // Matching vertical vertex coordinates at 0x40f04b–0x40f053.
   let dy=vec2<f32>(0.,4.0/p.screen.y);
   let c=textureSample(blur,smp,uv).rgb*0.310;
   let a=(textureSample(blur,smp,uv-dy).rgb+textureSample(blur,smp,uv+dy).rgb)*0.238;
   let b=(textureSample(blur,smp,uv-dy*2.).rgb+textureSample(blur,smp,uv+dy*2.).rgb)*0.107;
   let glow=c+a+b;
   // fullglow2_14: lrp(original, blur, glow_level) + glow_power*blur².
   let strength=p.effect.z;
   if p.mono.w>0. {
     // monofullglow_14 uses blur*original (not blur squared) before
     // saturating the luma dot and mixing the monochrome hue.
     let base=mix(original,glow,clamp(p.gamma.z*strength,0.,1.))+p.gamma.w*strength*glow*original;
     let luminance=clamp(dot(base,vec3<f32>(0.35,0.6,0.25)),0.,1.);
     out=mix(base,luminance*p.mono.rgb,p.mono.w);
   } else {
     out=mix(original,glow,clamp(p.gamma.z*strength,0.,1.))+p.gamma.w*strength*glow*glow;
   }
 }
 // PS c3 and D3DBLEND_SRCALPHA / INVSRCALPHA (0x41007b–0x410089).
 // Blend in encoded RGB before display gamma / sRGB presentation.
 if p.effect.y>0.5 {
   let source=clamp(out*p.modulation.rgb,vec3<f32>(0.),vec3<f32>(1.));
   out=mix(original,source,p.modulation.a);
 }
 return out;
}
fn display_color(encoded:vec3<f32>)->vec4<f32> {
 var out=encoded;
 // Emulate the client's 256-entry 16-bit D3D8 display ramp without
 // changing the desktop display. Linear interpolation models LUT lookup.
 if p.gamma.x>0.5 {
   out=client_gamma_ramp(out);
 }
 // Present encoded D3D-style output through an sRGB render target exactly once.
 if p.fog.w>0.5 {out=select(out/12.92,pow((max(out,vec3<f32>(0.))+0.055)/1.055,vec3<f32>(2.4)),out>vec3<f32>(0.04045));}
 return vec4<f32>(out,1.);
}
@fragment fn finish(i:Out)->@location(0) vec4<f32> {return display_color(composite(i.uv));}
@fragment fn finish_encoded(i:Out)->@location(0) vec4<f32> {return vec4<f32>(composite(i.uv),1.);}
@fragment fn sharpen_finish(i:Out)->@location(0) vec4<f32> {
 let size=vec2<i32>(textureDimensions(scene));
 let pixel=clamp(vec2<i32>(i.pos.xy),vec2<i32>(0),size-vec2<i32>(1));
 let center=textureLoad(scene,pixel,0).rgb;
 let below=textureLoad(scene,min(pixel+vec2<i32>(0,1),size-vec2<i32>(1)),0).rgb;
 // Actual fullsharpen_14 overwrites r0 in EACH mad using the original r5.
 // Earlier left/right/up results do NOT accumulate. Preserve that authored
 // behavior, not an invented symmetric five-tap unsharp mask.
 return display_color(clamp(2.*center-below,vec3<f32>(0.),vec3<f32>(1.)));
}
"#;
fn texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    w: u32,
    h: u32,
    label: &str,
) -> wgpu::Texture {
    let alternate = [format.remove_srgb_suffix()];
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: if format.is_srgb() { &alternate } else { &[] },
    })
}
pub struct PostGpu {
    modulation: [f32; 4],
    sharpen: bool,
    composite_target: wgpu::Texture,
    encoded_finish: wgpu::RenderPipeline,
    sharpen_finish: wgpu::RenderPipeline,
    format: wgpu::TextureFormat,
    scene: wgpu::Texture,
    fogged: wgpu::Texture,
    warped: wgpu::Texture,
    downsample: wgpu::Texture,
    blur: wgpu::Texture,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    fog_pipeline: wgpu::RenderPipeline,
    warp_pipeline: wgpu::RenderPipeline,
    downsample_pipeline: wgpu::RenderPipeline,
    horizontal: wgpu::RenderPipeline,
    finish: wgpu::RenderPipeline,
}
impl PostGpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, w: u32, h: u32) -> Self {
        let scene = texture(device, format, w, h, "post scene");
        let fogged = texture(device, format.remove_srgb_suffix(), w, h, "fogged scene");
        let warped = texture(
            device,
            format.remove_srgb_suffix(),
            w,
            h,
            "underwater warped scene",
        );
        let downsample = texture(
            device,
            format.remove_srgb_suffix(),
            (w / 4).max(1),
            (h / 4).max(1),
            "glow quarter-size copy",
        );
        let blur = texture(
            device,
            format.remove_srgb_suffix(),
            (w / 4).max(1),
            (h / 4).max(1),
            "glow blur",
        );
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("post parameters"),
            contents: bytemuck::bytes_of(&Params {
                screen: [1.; 4],
                effect: [0.; 4],
                fog: [0.; 4],
                fog_color: [0.; 4],
                gamma: [0.; 4],
                mono: [0.; 4],
                modulation: [1.; 4],
                ramp: gamma_curve::table(41.),
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post bindings"),
            entries: &[
                tex(0),
                tex(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("client-inspired glow and underwater"),
            source: wgpu::ShaderSource::Wgsl(format!("{}\n{}", gamma_curve::WGSL, SHADER).into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post pipeline"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let make = |entry, target_format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: entry,
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview: None,
            })
        };
        Self {
            modulation: [1.; 4],
            sharpen: false,
            composite_target: texture(
                device,
                format.remove_srgb_suffix(),
                w,
                h,
                "encoded glow before sharpen",
            ),
            encoded_finish: make("finish_encoded", format.remove_srgb_suffix()),
            sharpen_finish: make("sharpen_finish", format),
            format,
            scene,
            fogged,
            warped,
            downsample,
            blur,
            layout,
            sampler,
            uniform,
            fog_pipeline: make("fog_scene", format.remove_srgb_suffix()),
            warp_pipeline: make("warp_scene", format.remove_srgb_suffix()),
            downsample_pipeline: make("downsample", format.remove_srgb_suffix()),
            horizontal: make("glow_h", format.remove_srgb_suffix()),
            finish: make("finish", format),
        }
    }
    /// Bind the caller tint and the effect object's independent fade channels.
    pub fn set_glow_color(&mut self, argb: u32, brightness_fade: f32, opacity_fade: f32) {
        self.modulation = glow_modulation::constant(argb, brightness_fade, opacity_fade);
    }
    /// Native renderer flag +6, 0x40fd69 -> 0x410970. Independent of bloom.
    pub fn set_sharpen(&mut self, enabled: bool) {
        self.sharpen = enabled;
    }
    pub fn resize(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        self.scene = texture(device, self.format, w, h, "post scene");
        self.composite_target = texture(
            device,
            self.format.remove_srgb_suffix(),
            w,
            h,
            "encoded glow before sharpen",
        );
        self.fogged = texture(
            device,
            self.format.remove_srgb_suffix(),
            w,
            h,
            "fogged scene",
        );
        self.warped = texture(
            device,
            self.format.remove_srgb_suffix(),
            w,
            h,
            "underwater warped scene",
        );
        self.downsample = texture(
            device,
            self.format.remove_srgb_suffix(),
            (w / 4).max(1),
            (h / 4).max(1),
            "glow quarter-size copy",
        );
        self.blur = texture(
            device,
            self.format.remove_srgb_suffix(),
            (w / 4).max(1),
            (h / 4).max(1),
            "glow blur",
        );
    }
    pub fn scene_view(&self) -> wgpu::TextureView {
        self.scene.create_view(&Default::default())
    }
    /// Raw view permits legacy encoded-space alpha blending for shoreline waves.
    pub fn scene_raw_view(&self) -> wgpu::TextureView {
        self.scene.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.format.remove_srgb_suffix()),
            ..Default::default()
        })
    }
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        w: u32,
        h: u32,
        time: f32,
        far: f32,
        underwater: f32,
        fog: crate::environment::Fog,
        atmospheric_fog: bool,
        screen_fog: bool,
        bloom: bool,
        bloom_strength: f32,
        glow_parameters: [f32; 6],
        client_gamma: bool,
        gamma_setting: f32,
    ) {
        // Resolve every enable/disable rule in one audited place.
        let plan = post_plan::plan(post_plan::Inputs {
            bloom,
            sharpen: self.sharpen,
            underwater: underwater > 0.,
            screen_fog,
            fog_enabled: atmospheric_fog || underwater > 0.,
            display_gamma: client_gamma,
            srgb_target: self.format.is_srgb(),
        });
        if !plan.active() {
            // Nothing writes the frame: do not copy or quantize it.
            return;
        }
        queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&Params {
                screen: [w as f32, h as f32, time, far],
                effect: [
                    underwater,
                    if bloom { 1. } else { 0. },
                    bloom_strength,
                    if atmospheric_fog { 1. } else { 0. },
                ],
                fog: [
                    fog.start,
                    fog.end,
                    fog.density,
                    if self.format.is_srgb() { 1. } else { 0. },
                ],
                fog_color: [fog.color[0], fog.color[1], fog.color[2], 1.],
                gamma: [
                    if client_gamma { 1. } else { 0. },
                    gamma_curve::setting(gamma_setting),
                    glow_parameters[0],
                    glow_parameters[1],
                ],
                mono: [
                    glow_parameters[2],
                    glow_parameters[3],
                    glow_parameters[4],
                    glow_parameters[5],
                ],
                modulation: self.modulation,
                ramp: gamma_curve::table(gamma_setting),
            }),
        );
        let scene = self.scene.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.format.remove_srgb_suffix()),
            ..Default::default()
        });
        let fogged = self.fogged.create_view(&Default::default());
        let warped = self.warped.create_view(&Default::default());
        let downsample = self.downsample.create_view(&Default::default());
        let blur = self.blur.create_view(&Default::default());
        // Separate horizontal and composite bind groups avoid binding the blur texture
        // as both render target and shader resource in the same pass.
        let bind = |input: &wgpu::TextureView, other: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post textures"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(input),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(other),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(depth),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: self.uniform.as_entire_binding(),
                    },
                ],
            })
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        // Diagnostic depth fog only. Production shading is fogged per material,
        // so this stage is never part of the shipped chain (`post_plan`).
        let apply_fog = plan.fog;
        if apply_fog {
            let group = bind(&scene, &scene);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene fog before glow"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &fogged,
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
            pass.set_pipeline(&self.fog_pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        let before_warp = if apply_fog { &fogged } else { &scene };
        // 0x40fd33: spacewarp consumes the quantized capture, before glow.
        if plan.warp {
            let group = bind(before_warp, before_warp);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("underwater distortion before glow"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &warped,
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
            pass.set_pipeline(&self.warp_pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        let input = if plan.warp { &warped } else { before_warp };
        // 0x40fdb0/0x40f053: quarter-size copy, horizontal blur, then the
        // vertical blur + composite in the finish pass.
        if plan.glow {
            {
                let group = bind(input, input);
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("glow quarter-size stretch copy"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &downsample,
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
                pass.set_pipeline(&self.downsample_pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.draw(0..3, 0..1);
            }
            let group = bind(input, &downsample);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glow horizontal"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &blur,
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
            pass.set_pipeline(&self.horizontal);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        let composite_target = self.composite_target.create_view(&Default::default());
        {
            let group = bind(input, &blur);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("underwater and glow"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: if plan.sharpen {
                        &composite_target
                    } else {
                        target
                    },
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
            pass.set_pipeline(if plan.sharpen {
                &self.encoded_finish
            } else {
                &self.finish
            });
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        // 0x40fd64..0x40fd9c: authored fullsharpen_14 after the encoded glow.
        if plan.sharpen {
            let group = bind(&composite_target, &blur);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("client fullscreen sharpen after encoded glow"),
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
            pass.set_pipeline(&self.sharpen_finish);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
    }
}
