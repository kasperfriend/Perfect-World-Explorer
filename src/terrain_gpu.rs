//! GPU terrain: original material layers and masks, plus a persistent world LOD mesh.
use crate::{gpu::Vertex, Terrain};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
const MASK_PAGE: usize = 4096;
const TEX: usize = 512;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Area {
    start: u32,
    count: u32,
    _pad: [u32; 2],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Layer {
    tex: u32,
    axis: u32,
    mask: u32,
    _pad: u32,
    uv: [f32; 2],
    _pad2: [f32; 2],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Settings {
    params: [f32; 4],
    areas: [u32; 4],
    light: [u32; 4],
}
pub struct TerrainGpu {
    pipeline: wgpu::RenderPipeline,
    far_pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    reflect_bind: wgpu::BindGroup,
    far: Option<FarGpu>,
    resources: MaterialBindings,
    tex_map: Vec<u32>,
    mask_bytes: usize,
    pub grass_light_layout: wgpu::BindGroupLayout,
    pub grass_light_bind: wgpu::BindGroup,
    /// One indirect record per visible run, for the near and the far mesh. The
    /// runs change every view, so they are rewritten in `prepare_runs` and then
    /// drawn as a single call each.
    near_args: wgpu::Buffer,
    far_args: wgpu::Buffer,
    near_capacity: u32,
    far_capacity: u32,
    near_records: u32,
    far_records: u32,
    multi_draw: bool,
    scratch_records: Vec<crate::indirect_args::DrawIndexedArgs>,
    last_near_runs: Vec<(u32, u32)>,
    last_far_runs: Vec<(u32, u32)>,
}
/// Owned worker result; dropped immediately after upload, never retained as a CPU mirror.
pub struct TerrainUpload {
    pub materials: crate::material::Materials,
    pub far: Option<FarMesh>,
}
pub type FarMesh = (
    Vec<FarVertex>,
    Vec<u32>,
    Vec<crate::terrain_residency::Span>,
);
struct FarGpu {
    vertex: wgpu::Buffer,
    index: wgpu::Buffer,
    count: u32,
    spans: Vec<crate::terrain_residency::Span>,
}
impl FarGpu {
    fn upload(device: &wgpu::Device, mesh: &FarMesh) -> Self {
        Self {
            vertex: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("full-world terrain LOD vertices"),
                contents: bytemuck::cast_slice(&mesh.0),
                usage: wgpu::BufferUsages::VERTEX,
            }),
            index: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("full-world terrain LOD indices"),
                contents: bytemuck::cast_slice(&mesh.1),
                usage: wgpu::BufferUsages::INDEX,
            }),
            count: mesh.1.len() as u32,
            spans: mesh.2.clone(),
        }
    }
}
// Only immutable shared textures/settings survive a mask-window transition.
// Old bind groups are replaced, so they cannot pin the old whole-world atlas.
struct MaterialBindings {
    layout: wgpu::BindGroupLayout,
    settings: wgpu::Buffer,
    tv: wgpu::TextureView,
    small_view: wgpu::TextureView,
    full_view: wgpu::TextureView,
    native_views: Vec<wgpu::TextureView>,
    light_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    mask_sampler: wgpu::Sampler,
}
struct MaskResources {
    #[cfg(test)]
    texture: wgpu::Texture,
    bytes: usize,
    mv: wgpu::TextureView,
    area_buffer: wgpu::Buffer,
    layer_buffer: wgpu::Buffer,
}
impl MaterialBindings {
    fn bind(
        &self,
        device: &wgpu::Device,
        camera: &wgpu::Buffer,
        masks: &MaskResources,
    ) -> wgpu::BindGroup {
        let Self {
            layout,
            settings,
            tv,
            small_view,
            full_view,
            native_views,
            light_view,
            sampler,
            mask_sampler,
        } = self;
        let MaskResources {
            mv,
            area_buffer,
            layer_buffer,
            ..
        } = masks;
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resident terrain material bindings"),
            layout,
            entries: &[
                entry(0, camera.as_entire_binding()),
                entry(1, settings.as_entire_binding()),
                entry(2, wgpu::BindingResource::TextureView(tv)),
                entry(8, wgpu::BindingResource::TextureView(small_view)),
                entry(9, wgpu::BindingResource::TextureView(full_view)),
                entry(10, wgpu::BindingResource::TextureView(light_view)),
                entry(11, wgpu::BindingResource::TextureView(&native_views[0])),
                entry(12, wgpu::BindingResource::TextureView(&native_views[1])),
                entry(13, wgpu::BindingResource::TextureView(&native_views[2])),
                entry(14, wgpu::BindingResource::TextureView(&native_views[3])),
                entry(15, wgpu::BindingResource::TextureView(&native_views[4])),
                entry(16, wgpu::BindingResource::TextureView(&native_views[5])),
                entry(3, wgpu::BindingResource::Sampler(sampler)),
                entry(4, wgpu::BindingResource::TextureView(mv)),
                entry(5, wgpu::BindingResource::Sampler(mask_sampler)),
                entry(6, area_buffer.as_entire_binding()),
                entry(7, layer_buffer.as_entire_binding()),
            ],
        })
    }
}
const SHADER: &str = r#"
struct Camera { matrix:mat4x4<f32>, direction:vec4<f32>, clip:vec4<f32>, bounds:vec4<f32>, tree_bounds:vec4<f32>, sun:vec4<f32>, light:vec4<f32>, region_ambient:vec4<f32>, region_diffuse:vec4<f32>, region_direction:vec4<f32>, grass_eye:vec4<f32>, grass_wind:vec4<f32>, grass_history:array<vec4<f32>,80>, fog_range:vec4<f32>, fog_color:vec4<f32>, lod:vec4<f32> };
struct Settings { params:vec4<f32>, areas:vec4<u32>, light:vec4<u32> };
struct Area { start:u32,count:u32,pad:vec2<u32> };
struct Layer { tex:u32,axis:u32,mask:u32,pad:u32,uv:vec2<f32>,pad2:vec2<f32> };
@group(0) @binding(0) var<uniform> camera:Camera;
@group(0) @binding(1) var<uniform> settings:Settings;
@group(0) @binding(2) var terrain:texture_2d_array<f32>;
@group(0) @binding(3) var terrain_sampler:sampler;
@group(0) @binding(8) var terrain_small:texture_2d_array<f32>;
@group(0) @binding(9) var terrain_full:texture_2d_array<f32>;
@group(0) @binding(11) var terrain_bc0:texture_2d_array<f32>;
@group(0) @binding(12) var terrain_bc1:texture_2d_array<f32>;
@group(0) @binding(13) var terrain_bc2:texture_2d_array<f32>;
@group(0) @binding(14) var terrain_bc3:texture_2d_array<f32>;
@group(0) @binding(15) var terrain_bc4:texture_2d_array<f32>;
@group(0) @binding(16) var terrain_bc5:texture_2d_array<f32>;
@group(0) @binding(10) var lightmaps:texture_2d_array<f32>;
@group(0) @binding(4) var masks:texture_2d_array<f32>;
@group(0) @binding(5) var mask_sampler:sampler;
@group(0) @binding(6) var<storage,read> area_data:array<Area>;
@group(0) @binding(7) var<storage,read> layers:array<Layer>;
struct In {@location(0) pos:vec3<f32>,@location(1) normal:vec3<f32>,@location(2) uv:vec2<f32>,@location(3) color:vec4<f32>,@location(4) kind:f32};
struct FarIn {@location(0) pos:vec3<f32>, @location(1) normal:vec4<f32>};
struct Out {@builtin(position) pos:vec4<f32>,@location(0) world:vec3<f32>,@location(1) normal:vec3<f32>,@location(2) @interpolate(flat) distant:f32,@location(3) refraction:vec2<f32>};
// terrain_refract_vs: project above-plane vertices down, but use the original
// vertex for UV generation and eye direction. oD0 is independent depth darkness.
fn terrain_vertex(world:vec3<f32>,normal:vec3<f32>,distant:f32)->Out {
 var o:Out;var projected=world;o.refraction=vec2<f32>(1.);
 if camera.clip.y<0. {
  projected.y=min(world.y,camera.clip.x);let depth=camera.clip.x-projected.y;
  let delta=camera.grass_eye.xyz-world;
  var f=select(1.,0.,depth>0.);
  if abs(delta.y)>0.000001 {f=exp2(-0.075*depth*length(delta)/delta.y);}
  o.refraction=vec2<f32>(f,exp2(-0.15*depth));
 }
 o.pos=camera.matrix*vec4<f32>(projected,1.);o.world=world;o.normal=normal;o.distant=distant;return o;
}
@vertex fn vs(i:In)->Out {return terrain_vertex(i.pos,i.normal,i.kind);}
@vertex fn vs_far(i:FarIn)->Out {return terrain_vertex(i.pos,i.normal.xyz,1.);}
// Approximate the original fixed-function/PS 1.1 arithmetic in encoded RGB.
// D3D8 filters authored texture bytes in encoded space. The texture arrays
// use UNORM views; return linear RGB to the viewer's sRGB render target.
fn to_linear(c:vec3<f32>)->vec3<f32>{return select(c/12.92,pow((max(c,vec3<f32>(0.))+0.055)/1.055,vec3<f32>(2.4)),c>vec3<f32>(0.04045));}
fn fogged(linear:vec3<f32>, reciprocal_w:f32)->vec3<f32> {
 if camera.fog_range.z<0.5 {return linear;}
 let view_z=1./max(reciprocal_w,0.0000001);
 let span=camera.fog_range.y-camera.fog_range.x;
 let f=select(clamp((view_z-camera.fog_range.x)/select(span,0.001,abs(span)<0.001),0.,1.),select(0.,1.,view_z>=camera.fog_range.y),abs(span)<0.001);
 let c=max(linear,vec3<f32>(0.));let encoded=select(c*12.92,1.055*pow(c,vec3<f32>(1./2.4))-0.055,c>vec3<f32>(0.0031308));
 let v=mix(encoded,camera.fog_color.rgb,f);
 return select(v/12.92,pow((max(v,vec3<f32>(0.))+0.055)/1.055,vec3<f32>(2.4)),v>vec3<f32>(0.04045));
}
fn terrain_pass_fog(linear:vec3<f32>,reciprocal_w:f32,transmittance:f32)->vec3<f32> {
 if camera.clip.y>=0. || camera.fog_range.z<0.5 {return fogged(linear,reciprocal_w);}
 let c=max(linear,vec3<f32>(0.));let encoded=select(c*12.92,1.055*pow(c,vec3<f32>(1./2.4))-0.055,c>vec3<f32>(0.0031308));
 let v=mix(camera.fog_color.rgb,encoded,clamp(transmittance,0.,1.));return to_linear(v);
}
@fragment fn fs(i:Out)->@location(0) vec4<f32> {
 if camera.clip.y>0. && i.world.y<camera.clip.x {discard;}
 if i.distant>0.5 && i.world.x>=camera.bounds.x && i.world.x<camera.bounds.y && i.world.z>=camera.bounds.z && i.world.z<camera.bounds.w {discard;}
 let gx=clamp(i.world.x/settings.params.z+settings.params.x*0.5,0.,settings.params.x-1.);
 let gy=clamp(settings.params.y*0.5-i.world.z/settings.params.z,0.,settings.params.y-1.);
 let coord=vec2<f32>(gx,gy)/settings.params.w;
 let wx=dpdx(i.world);let wy=dpdy(i.world);
 let ax=u32(floor(coord.x));let ay=u32(floor(coord.y));let a=area_data[ay*settings.areas.x+ax];
 // `camera.sun.w >= 0` is the viewer switch for "the authored day/night pair is
 // bound" (the client's `t2_3layer_batch_14` path). The blend factor is a
 // separate quantity and arrives in `camera.fog_range.w`, which
 // `gpu.rs` fills from `camera.terrain_day_night`; that is the one place the
 // viewer's stand-in for the native `ps c3.a` (`env+0x198`, a lane of the light
 // matrix `0x7b4b00` writes) is produced. Register row: "terrain day/night
 // factor".
 let lightmapped=camera.sun.w>=0.;
 var color=vec3<f32>(0.);
 var baked=vec3<f32>(1.);
 if lightmapped {let id=ay*settings.areas.x+ax;let slot=id%settings.light.z;let page=id/settings.light.z;let side=f32(settings.light.x);let xy=vec2<f32>(f32(slot%settings.light.y),f32(slot/settings.light.y))*side;let loc=clamp(fract(coord)*side,vec2<f32>(0.5),vec2<f32>(side-0.5));let lmuv=(xy+loc)/vec2<f32>(textureDimensions(lightmaps));let day=textureSampleLevel(lightmaps,mask_sampler,lmuv,i32(page),0.).rgb;let night=textureSampleLevel(lightmaps,mask_sampler,lmuv,i32(page+settings.light.w),0.).rgb;
  // t2_3layer_batch_14: `lrp_sat r5, c3.a, r5, r4` with t4 = day (r4), t5 = night
  // (r5): factor 0 selects the day map and 1 the night map, then `mul_x2` doubles
  // the encoded result into the 8-bit target.
  let factor=clamp(camera.fog_range.w,0.,1.);
  baked=mix(day,night,factor)*2.;}
 for(var k=0u;k<a.count;k=k+1u){let l=layers[a.start+k];var v=vec2<f32>(i.world.x,-i.world.z);if l.axis==0u {v=vec2<f32>(i.world.z,-i.world.y);}else if l.axis==2u {v=vec2<f32>(i.world.x,-i.world.y);}
 var dx=vec2<f32>(wx.x,-wx.z);var dy=vec2<f32>(wy.x,-wy.z);
 if l.axis==0u {dx=vec2<f32>(wx.z,-wx.y);dy=vec2<f32>(wy.z,-wy.y);}else if l.axis==2u {dx=vec2<f32>(wx.x,-wx.y);dy=vec2<f32>(wy.x,-wy.y);}
 dx*=l.uv*camera.lod.y;dy*=l.uv*camera.lod.y;let kind=l.tex>>28u;let index=i32(l.tex & 0x0fffffffu);
 // Pass the true screen-space gradient, not a scalar max-footprint LOD.
 // The sampler can then preserve detail along the minor axis at grazing angles.
 var tex:vec3<f32>;
 if kind==3u {tex=textureSampleGrad(terrain_bc0,terrain_sampler,v*l.uv,index,dx,dy).rgb;}
 else if kind==4u {tex=textureSampleGrad(terrain_bc1,terrain_sampler,v*l.uv,index,dx,dy).rgb;}
 else if kind==5u {tex=textureSampleGrad(terrain_bc2,terrain_sampler,v*l.uv,index,dx,dy).rgb;}
 else if kind==6u {tex=textureSampleGrad(terrain_bc3,terrain_sampler,v*l.uv,index,dx,dy).rgb;}
 else if kind==7u {tex=textureSampleGrad(terrain_bc4,terrain_sampler,v*l.uv,index,dx,dy).rgb;}
 else if kind==8u {tex=textureSampleGrad(terrain_bc5,terrain_sampler,v*l.uv,index,dx,dy).rgb;}
 else if kind==2u {tex=textureSampleGrad(terrain_full,terrain_sampler,v*l.uv,index,dx,dy).rgb;}
 else if kind==1u {tex=textureSampleGrad(terrain,terrain_sampler,v*l.uv,index,dx,dy).rgb;}
 else {tex=textureSampleGrad(terrain_small,terrain_sampler,v*l.uv,index,dx,dy).rgb;}
 var alpha=1.;if k>0u {let slot=l.mask%settings.areas.w;let page=l.mask/settings.areas.w;let side=f32(settings.areas.y);let xy=vec2<f32>(f32(slot%settings.areas.z),f32(slot/settings.areas.z))*side;let local=clamp(fract(coord)*side,vec2<f32>(0.5),vec2<f32>(side-0.5));alpha=textureSampleLevel(masks,mask_sampler,(xy+local)/4096.,i32(page),0.).r;}
 if !lightmapped {tex=to_linear(tex);}
 color=mix(color,tex,alpha);}
 // t2_3layer_batch_14 composite: `mul r0, r0, v0` with v0 = c0.x = 1.0 and
 // `mul_x2 r0, r0, r5`. There is no ambient term, no normal term and no scene
 // light: the lightmap is the only lighting, so terrain is never lit twice.
 if lightmapped {return vec4<f32>(terrain_pass_fog(to_linear(clamp(color*baked*i.refraction.y,vec3<f32>(0.),vec3<f32>(1.))),i.pos.w,i.refraction.x),1.);}
 // Viewer-only fallback for a session with no authored pair (the client's
 // fixed-function paths, which this lane has not integrated). Registered gap.
 let light=camera.light.x+camera.light.y*max(dot(normalize(i.normal),normalize(camera.sun.xyz)),0.);
 return vec4<f32>(terrain_pass_fog(color*light*camera.light.zww,i.pos.w,i.refraction.x),1.);
}
"#;
impl TerrainGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        t: &Terrain,
        upload: &TerrainUpload,
        normal: &wgpu::Buffer,
        reflection: &wgpu::Buffer,
    ) -> Result<Self, String> {
        let m = &upload.materials;
        if m.textures.is_empty() {
            return Err("terrain has no textures".into());
        }
        let masks_count = m
            .areas
            .iter()
            .map(|a| a.layers.len().saturating_sub(1))
            .sum::<usize>();
        // Native client masks are 256². A smaller diagnostic setting is useful
        // on software renderers with severely limited memory.
        let mask_side = if std::env::var_os("PW_SMALL_MASK_ATLAS").is_some() {
            128
        } else {
            256
        };
        let per_row = MASK_PAGE / mask_side;
        let per_page = per_row * per_row;
        let pages = ((masks_count + per_page - 1) / per_page).max(1);
        if pages > device.limits().max_texture_array_layers as usize {
            return Err("GPU terrain mask layer limit exceeded".into());
        }
        // Keep BC1/BC2 blocks in their authored GPU representation when the
        // adapter supports it. No recompression, no mip regeneration, no
        // downscaling. Unusual shapes/formats retain the decoded fallback.
        let native_groups: Vec<Option<usize>> = m
            .textures
            .iter()
            .map(|img| {
                crate::terrain_upload::native_group(
                    &img.source,
                    device
                        .features()
                        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
                )
            })
            .collect();
        let mut native_counts = [0usize; 6];
        for group in native_groups.iter().flatten() {
            native_counts[*group] += 1;
        }
        eprintln!("Terrain native BC upload: {} textures; {} decoded fallback textures (original resolution and authored mips retained)",
            native_counts.iter().sum::<usize>(), m.textures.len() - native_counts.iter().sum::<usize>());
        let native_textures: Vec<_> = (0..6)
            .map(|group| {
                let side = if native_counts[group] == 0 {
                    4
                } else {
                    256u32 << (group % 3)
                };
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("authored compressed terrain"),
                    size: wgpu::Extent3d {
                        width: side,
                        height: side,
                        depth_or_array_layers: native_counts[group].max(1) as u32,
                    },
                    mip_level_count: side.ilog2() + 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: if device
                        .features()
                        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
                    {
                        if group < 3 {
                            wgpu::TextureFormat::Bc1RgbaUnorm
                        } else {
                            wgpu::TextureFormat::Bc2RgbaUnorm
                        }
                    } else {
                        wgpu::TextureFormat::Rgba8Unorm
                    },
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            })
            .collect();
        // Avoid upscaling every 256px source into a 512px array: preserve
        // client resolutions in two arrays and save substantial GPU memory.
        let count_small = m
            .textures
            .iter()
            .enumerate()
            .filter(|(i, t)| native_groups[*i].is_none() && t.w <= 256 && t.h <= 256)
            .count();
        // Fit one authored level into a square array slot.
        //
        // The archive census (P2.07) found only 256/512/1024 squares plus a single
        // 256x512 source, and every slot is at least as large as its level, so this
        // only ever enlarges. Nearest-neighbour keeps authored texel centres, which
        // is what the device's bilinear filter then interpolates; it is chosen over
        // a resampling filter so no new detail is invented and no authored texel is
        // averaged away. An off-square source keeps its aspect by mapping through
        // its own dimensions, exactly as the previous code did.
        let fit_level = |lvl: &crate::dxt::Level, slot: usize| -> Vec<u8> {
            let mut out = vec![0u8; slot * slot * 4];
            let (lw, lh) = (lvl.width as usize, lvl.height as usize);
            for y in 0..slot {
                let sy = y * lh / slot;
                for x in 0..slot {
                    let sx = x * lw / slot;
                    let si = (sy * lw + sx) * 4;
                    let di = (y * slot + x) * 4;
                    out[di..di + 4].copy_from_slice(&lvl.rgba[si..si + 4]);
                }
            }
            out
        };
        // Box-reduce a level only when the source has no authored level for that
        // slot (a single-level source, the `generates_mips` case).
        let box_half = |src: &[u8], next: usize| -> Vec<u8> {
            let side = next * 2;
            let mut out = vec![0u8; next * next * 4];
            for y in 0..next {
                for x in 0..next {
                    for k in 0..4 {
                        let a = src[((2 * y) * side + 2 * x) * 4 + k] as u32;
                        let b = src[((2 * y) * side + 2 * x + 1) * 4 + k] as u32;
                        let c = src[((2 * y + 1) * side + 2 * x) * 4 + k] as u32;
                        let d = src[((2 * y + 1) * side + 2 * x + 1) * 4 + k] as u32;
                        out[(y * next + x) * 4 + k] = ((a + b + c + d + 2) / 4) as u8;
                    }
                }
            }
            out
        };
        let count_large = m
            .textures
            .iter()
            .enumerate()
            .filter(|(i, t)| {
                native_groups[*i].is_none() && (t.w > 256 || t.h > 256) && t.w <= 512 && t.h <= 512
            })
            .count();
        let count_full =
            m.textures.len() - native_counts.iter().sum::<usize>() - count_small - count_large;
        if native_counts
            .iter()
            .copied()
            .chain([count_small, count_large, count_full])
            .any(|count| count > device.limits().max_texture_array_layers as usize)
        {
            return Err("GPU terrain texture array layer limit exceeded".into());
        }
        let create_array = |side: usize, count: usize| {
            let side = if count == 0 { 4 } else { side };
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("original terrain texture array"),
                size: wgpu::Extent3d {
                    width: side as u32,
                    height: side as u32,
                    depth_or_array_layers: count.max(1) as u32,
                },
                mip_level_count: side.ilog2() + 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // D3D8 filters texture bytes in their authored encoded space.
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let small_tex = create_array(256, count_small);
        let tex = create_array(TEX, count_large);
        let full_tex = create_array(1024, count_full);
        let mut tex_map = Vec::with_capacity(m.textures.len());
        let mut small_n = 0u32;
        let mut large_n = 0u32;
        let mut full_n = 0u32;
        let mut native_n = [0u32; 6];
        for (image_number, img) in m.textures.iter().enumerate() {
            if let Some(group) = native_groups[image_number] {
                let index = native_n[group];
                native_n[group] += 1;
                tex_map.push(index | ((group as u32 + 3) << 28));
                crate::terrain_upload::upload(queue, &native_textures[group], &img.source, index)?;
                // A submit bounds upload staging; a wait every eight sources
                // also caps in-flight allocations on shared-memory adapters.
                if image_number % 8 == 7 || image_number + 1 == m.textures.len() {
                    queue.submit(std::iter::empty());
                    device.poll(wgpu::Maintain::Wait);
                }
                continue;
            }
            let (image, base_side, index, kind) = if img.w > 512 || img.h > 512 {
                let i = full_n;
                full_n += 1;
                (&full_tex, 1024, i, 2u32)
            } else if img.w > 256 || img.h > 256 {
                let i = large_n;
                large_n += 1;
                (&tex, TEX, i, 1u32)
            } else {
                let i = small_n;
                small_n += 1;
                (&small_tex, 256, i, 0u32)
            };
            tex_map.push(index | (kind << 28));
            // Copy the authored chain level by level. Level 0 is fitted into the
            // array slot; every later level is taken straight from the file, so the
            // client's authored reductions survive. The client uploads what the DDS
            // carries (`0x7f7ef1`) and only generates a chain when the source has a
            // single level, which is the `generates_mips` case below.
            let decoded = img.decode()?;
            let mut level = fit_level(&decoded[0], base_side);
            let mut side = base_side;
            for mip in 0..=base_side.ilog2() {
                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: image,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: index,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &level,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some((side * 4) as u32),
                        rows_per_image: Some(side as u32),
                    },
                    wgpu::Extent3d {
                        width: side as u32,
                        height: side as u32,
                        depth_or_array_layers: 1,
                    },
                );
                if side <= 1 {
                    break;
                }
                let next = side / 2;
                level = match decoded.get(mip as usize + 1) {
                    // Authored level: use it as-is when it matches the slot size.
                    Some(authored)
                        if authored.width as usize == next && authored.height as usize == next =>
                    {
                        authored.rgba.clone()
                    }
                    Some(authored) => {
                        // Authored but non-square or smaller than the slot requests:
                        // fit it rather than silently box-filter it, and keep the
                        // authored reduction where the size already matches.
                        fit_level(authored, next)
                    }
                    None => box_half(&level, next),
                };
                side = next;
            }
            // wgpu keeps write_texture staging alive until submission. Bound
            // peak memory even when maximum-quality terrain uses 1024px images.
            if image_number % 8 == 7 || image_number + 1 == m.textures.len() {
                queue.submit(std::iter::empty());
                device.poll(wgpu::Maintain::Wait);
            }
        }
        let masks = upload_masks(device, queue, m, &tex_map)?;
        let (lighttex, light_side, light_per_row, light_per_page, light_pages, availability) =
            load_lightmaps(device, queue, t, m, mask_side)?;
        let settings = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("terrain world settings"),
            contents: bytemuck::bytes_of(&Settings {
                params: [
                    m.width as f32,
                    m.height as f32,
                    m.spacing,
                    m.mask_grid as f32,
                ],
                areas: [
                    m.areas_x as u32,
                    mask_side as u32,
                    per_row as u32,
                    per_page as u32,
                ],
                light: [
                    light_side as u32,
                    light_per_row as u32,
                    light_per_page as u32,
                    light_pages as u32,
                ],
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let tv = tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let small_view = small_tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let full_view = full_tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let native_views: Vec<_> = native_textures
            .iter()
            .map(|t| {
                t.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                })
            })
            .collect();
        let light_view = lighttex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        // These are repeating colour-layer UVs, not mask-stage coordinates.
        // Mirroring the colour lookup invents a conspicuous 2x2 reflected
        // pattern in rock/grass detail. Keep authored mips but wrap the colour
        // sources, matching the CPU sampler and the colour-layer shader lane.
        let sampler = crate::terrain_upload::color_sampler(device);
        // The mask/lightmap atlas is a packed page grid, not a tiled texture:
        // the fragment shader already clamps each tile's local UV to half a
        // texel inside its slot, so this sampler must never wrap or mirror
        // across slot borders. The client samples the mask in the layer pass
        // with the same point-clamped lookup, and terrain lightmaps are read at
        // exactly one texel per fragment (0x7c1a54: `texld r0, t1, s1` after
        // the atlas address is computed on the CPU).
        let mask_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            anisotropy_clamp: 1,
            ..Default::default()
        });
        // Share the exact terrain day/night lightmap texture with grass.  The
        // client's grassrender.txt modulates grass by these two maps, not a
        // separate procedural directional light.
        let grass_light_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("grass terrain lightmap layout"),
                entries: &[
                    buf(
                        0,
                        wgpu::ShaderStages::FRAGMENT,
                        wgpu::BufferBindingType::Uniform,
                    ),
                    texture(1),
                    sam(2),
                    buf(
                        3,
                        wgpu::ShaderStages::FRAGMENT,
                        wgpu::BufferBindingType::Storage { read_only: true },
                    ),
                ],
            });
        let availability = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("authored terrain lightmap availability for grass"),
            contents: bytemuck::cast_slice(&availability),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let grass_light_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grass terrain day/night lightmaps"),
            layout: &grass_light_layout,
            entries: &[
                entry(0, settings.as_entire_binding()),
                entry(1, wgpu::BindingResource::TextureView(&light_view)),
                entry(2, wgpu::BindingResource::Sampler(&mask_sampler)),
                entry(3, availability.as_entire_binding()),
            ],
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terrain material layout"),
            entries: &[
                buf(
                    0,
                    wgpu::ShaderStages::VERTEX_FRAGMENT,
                    wgpu::BufferBindingType::Uniform,
                ),
                buf(
                    1,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BufferBindingType::Uniform,
                ),
                texture(2),
                texture(8),
                texture(9),
                texture(10),
                texture(11),
                texture(12),
                texture(13),
                texture(14),
                texture(15),
                texture(16),
                sam(3),
                texture(4),
                sam(5),
                buf(
                    6,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
                buf(
                    7,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BufferBindingType::Storage { read_only: true },
                ),
            ],
        });
        let resources = MaterialBindings {
            layout,
            settings,
            tv,
            small_view,
            full_view,
            native_views,
            light_view,
            sampler,
            mask_sampler,
        };
        let normal_bind = resources.bind(device, normal, &masks);
        let reflect_bind = resources.bind(device, reflection, &masks);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("layered terrain GPU shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terrain pipeline"),
            bind_group_layouts: &[&resources.layout],
            push_constant_ranges: &[],
        });
        // Variant selection and pass state come from the recovered native
        // switch (`src/terrain_shading.rs`, 0x7c1450) instead of being restated
        // here. `Caps::default()` is the stock 1.4.2 state: a device with a pixel
        // pipeline and neither terrain-env mode flag set, i.e. `Variant::Batch3Layer`.
        let variant = crate::terrain_shading::select(crate::terrain_shading::Caps::default())
            .map_err(|e| format!("terrain variant {e:?} is unreachable in the client"))?;
        debug_assert_eq!(variant, crate::terrain_shading::Variant::Batch3Layer);
        let state = crate::terrain_shading::pass_state(variant);
        // 0x7cd789 pushes D3DCULL_NONE (1); the state type keeps the restored
        // value explicit so the mapping cannot drift silently.
        debug_assert!(matches!(state.cull, crate::terrain_shading::Cull::None));
        // Both the near and the far pipeline are built from this one state
        // record, so the far path cannot drift back to a restated guess. The
        // match is exhaustive over `Cull` on purpose: adding a second native
        // cull mode has to fail to compile here instead of silently keeping no
        // culling.
        let cull_mode = match state.cull {
            crate::terrain_shading::Cull::None => None,
        };
        let primitive_state = || wgpu::PrimitiveState {
            cull_mode,
            ..Default::default()
        };
        let colour_targets = || {
            [Some(wgpu::ColorTargetState {
                format,
                // ALPHABLENDENABLE is never called for terrain (0x77a290 has no
                // caller in 0x7c1400-0x7d3fff), so terrain is opaque.
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })]
        };
        let depth_state = || wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: state.z_write,
            depth_compare: wgpu::CompareFunction::Greater,
            stencil: Default::default(),
            bias: Default::default(),
        };
        let targets = colour_targets();
        const ATTR: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("layered terrain"),
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
                targets: &targets,
            }),
            primitive: primitive_state(),
            depth_stencil: Some(depth_state()),
            multisample: Default::default(),
            multiview: None,
        });
        const FAR_ATTR: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![0=>Float32x3,1=>Snorm8x4];
        let far_targets = colour_targets();
        let far_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("layered terrain"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_far",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<FarVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &FAR_ATTR,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &far_targets,
            }),
            primitive: primitive_state(),
            depth_stencil: Some(depth_state()),
            multisample: Default::default(),
            multiview: None,
        });
        let far = upload.far.as_ref().map(|mesh| FarGpu::upload(device, mesh));
        eprintln!(
            "Terrain residency: {} masks, {} atlas MiB, {} far indices",
            masks_count,
            pages * MASK_PAGE * MASK_PAGE / (1024 * 1024),
            far.as_ref().map_or(0, |mesh| mesh.count)
        );
        // Start with room for a typical view's runs; `prepare_runs` grows them.
        let run_args = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 256 * crate::indirect_args::DRAW_ARGS,
                usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Ok(Self {
            pipeline,
            far_pipeline,
            bind: normal_bind,
            reflect_bind,
            far,
            resources,
            tex_map,
            mask_bytes: masks.bytes,
            grass_light_layout,
            grass_light_bind,
            near_args: run_args("terrain near run args"),
            far_args: run_args("terrain far run args"),
            near_capacity: 256,
            far_capacity: 256,
            near_records: 0,
            far_records: 0,
            multi_draw: crate::indirect_args::multi_draw(device),
            scratch_records: Vec::new(),
            last_near_runs: Vec::new(),
            last_far_runs: Vec::new(),
        })
    }
    pub fn residency_label(&self) -> String {
        let far_bytes = self
            .far
            .as_ref()
            .map_or(0, |far| far.vertex.size() + far.index.size());
        format!(
            "Terrain: {}; blend masks {:.1} MiB; far geometry {:.1} MiB",
            if self.far.is_none() {
                "native cutoff"
            } else {
                "full extent"
            },
            self.mask_bytes as f64 / 1048576.,
            far_bytes as f64 / 1048576.
        )
    }
    /// The far mesh's visible runs into a caller-owned buffer: the draw path
    /// asks for this once per view on every frame.
    pub fn visible_far_runs_into(&self, matrix: glam::Mat4, out: &mut Vec<(u32, u32)>) {
        match &self.far {
            Some(far) => crate::terrain_residency::visible_runs_into(&far.spans, matrix, out),
            None => out.clear(),
        }
    }
    pub fn edge_mesh(&self) -> Option<(&wgpu::Buffer, &wgpu::Buffer, u32)> {
        self.far
            .as_ref()
            .map(|far| (&far.vertex, &far.index, far.count))
    }
    pub fn update_residency(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        normal: &wgpu::Buffer,
        reflection: &wgpu::Buffer,
        upload: &TerrainUpload,
    ) -> Result<(), String> {
        let masks = upload_masks(device, queue, &upload.materials, &self.tex_map)?;
        self.mask_bytes = masks.bytes;
        self.bind = self.resources.bind(device, normal, &masks);
        self.reflect_bind = self.resources.bind(device, reflection, &masks);
        self.far = upload.far.as_ref().map(|mesh| FarGpu::upload(device, mesh));
        self.last_near_runs.clear();
        self.last_far_runs.clear();
        // Non-blocking poll lets wgpu retire old in-flight atlas/buffer
        // references without stalling the CPU/GPU pipeline.
        device.poll(wgpu::Maintain::Poll);
        Ok(())
    }
    /// `runs` is the per-view visibility result (`block+0x74`): `None` draws the
    /// whole window mesh, `Some` submits only the ranges the view kept.
    /// Upload one indirect record per visible run for this view.
    ///
    /// The run list is what the block walk produces every frame; turning it into
    /// records costs a few hundred bytes of upload and lets the whole terrain go
    /// out as one call per mesh instead of one call per run. Called before the
    /// passes; `draw` falls back to the per-run loop if it was not called with
    /// the same list.
    pub fn prepare_runs(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        runs: Option<&[(u32, u32)]>,
        far_runs: Option<&[(u32, u32)]>,
    ) {
        if let Some(runs) = runs {
            if self.last_near_runs.as_slice() != runs {
                self.last_near_runs.clear();
                self.last_near_runs.extend_from_slice(runs);
                self.near_records = Self::upload_runs(
                    device,
                    queue,
                    &mut self.near_args,
                    &mut self.near_capacity,
                    &mut self.scratch_records,
                    "terrain near run args",
                    runs,
                );
            }
        } else {
            self.last_near_runs.clear();
            self.near_records = 0;
        }
        if let Some(far_runs) = far_runs {
            if self.last_far_runs.as_slice() != far_runs {
                self.last_far_runs.clear();
                self.last_far_runs.extend_from_slice(far_runs);
                self.far_records = Self::upload_runs(
                    device,
                    queue,
                    &mut self.far_args,
                    &mut self.far_capacity,
                    &mut self.scratch_records,
                    "terrain far run args",
                    far_runs,
                );
            }
        } else {
            self.last_far_runs.clear();
            self.far_records = 0;
        }
    }
    fn upload_runs(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        buffer: &mut wgpu::Buffer,
        capacity: &mut u32,
        scratch: &mut Vec<crate::indirect_args::DrawIndexedArgs>,
        label: &str,
        runs: &[(u32, u32)],
    ) -> u32 {
        if runs.is_empty() {
            return 0;
        }
        let records = runs.len() as u32;
        if records > *capacity {
            *capacity = records.next_power_of_two();
            *buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: *capacity as u64 * crate::indirect_args::DRAW_ARGS,
                usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        scratch.clear();
        scratch.extend(
            runs.iter()
                .map(|&(start, count)| crate::indirect_args::record(start..start + count, 0..1)),
        );
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(scratch));
        records
    }
    pub fn draw<'a>(
        &'a self,
        p: &mut wgpu::RenderPass<'a>,
        reflected: bool,
        near_vertex: &'a wgpu::Buffer,
        near_index: &'a wgpu::Buffer,
        near_end: u32,
        runs: Option<&[(u32, u32)]>,
        far_runs: Option<&[(u32, u32)]>,
    ) {
        let bind = if reflected {
            &self.reflect_bind
        } else {
            &self.bind
        };
        if let Some(far) = &self.far {
            if far_runs.is_none() || self.far_records > 0 {
                p.set_pipeline(&self.far_pipeline);
                p.set_bind_group(0, bind, &[]);
                p.set_vertex_buffer(0, far.vertex.slice(..));
                p.set_index_buffer(far.index.slice(..), wgpu::IndexFormat::Uint32);
                match far_runs {
                    Some(runs) => {
                        if can_multi_draw(self.multi_draw, self.far_records, runs) {
                            p.multi_draw_indexed_indirect(&self.far_args, 0, self.far_records);
                        } else {
                            for &(start, count) in runs {
                                p.draw_indexed(start..start + count, 0, 0..1);
                            }
                        }
                    }
                    None => p.draw_indexed(0..far.count, 0, 0..1),
                }
            }
        }
        if (runs.is_none() && near_end > 0) || self.near_records > 0 {
            p.set_pipeline(&self.pipeline);
            p.set_bind_group(0, bind, &[]);
            p.set_vertex_buffer(0, near_vertex.slice(..));
            p.set_index_buffer(near_index.slice(..), wgpu::IndexFormat::Uint32);
            match runs {
                Some(runs) => {
                    if can_multi_draw(self.multi_draw, self.near_records, runs) {
                        p.multi_draw_indexed_indirect(&self.near_args, 0, self.near_records);
                    } else {
                        for &(start, count) in runs {
                            p.draw_indexed(start..start + count, 0, 0..1);
                        }
                    }
                }
                None => p.draw_indexed(0..near_end, 0, 0..1),
            }
        }
    }
}
/// The single-call path is only taken for the exact list that was uploaded;
/// anything else uses the per-run loop, so a caller that forgets
/// `prepare_runs` still draws exactly what it asked for.
fn can_multi_draw(multi_draw: bool, records: u32, runs: &[(u32, u32)]) -> bool {
    multi_draw && records > 0 && records as usize == runs.len()
}
/// One record per run: the same index range and a single instance, which is
/// what `draw_indexed(start..start + count, 0, 0..1)` asks for.
#[allow(dead_code)]
fn run_records(runs: &[(u32, u32)]) -> Vec<crate::indirect_args::DrawIndexedArgs> {
    runs.iter()
        .map(|&(start, count)| crate::indirect_args::record(start..start + count, 0..1))
        .collect()
}
fn upload_masks(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    m: &crate::material::Materials,
    tex_map: &[u32],
) -> Result<MaskResources, String> {
    let masks_count = m
        .areas
        .iter()
        .map(|a| a.layers.len().saturating_sub(1))
        .sum::<usize>();
    let mask_side = if std::env::var_os("PW_SMALL_MASK_ATLAS").is_some() {
        128
    } else {
        256
    };
    let per_row = MASK_PAGE / mask_side;
    let per_page = per_row * per_row;
    let pages = ((masks_count + per_page - 1) / per_page).max(1);
    if pages > device.limits().max_texture_array_layers as usize {
        return Err("GPU terrain mask layer limit exceeded".into());
    }
    let masktex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain layer masks"),
        size: wgpu::Extent3d {
            width: MASK_PAGE as u32,
            height: MASK_PAGE as u32,
            depth_or_array_layers: pages as u32,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | if cfg!(test) {
                wgpu::TextureUsages::COPY_SRC
            } else {
                wgpu::TextureUsages::empty()
            },
        view_formats: &[],
    });
    let mut areas = Vec::new();
    let mut layers = Vec::new();
    let mut page = vec![0u8; MASK_PAGE * MASK_PAGE];
    let mut layer_id = 0usize;
    let mut page_id = 0usize;
    let upload_page = |page: &[u8], id: usize| {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &masktex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: id as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            page,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(MASK_PAGE as u32),
                rows_per_image: Some(MASK_PAGE as u32),
            },
            wgpu::Extent3d {
                width: MASK_PAGE as u32,
                height: MASK_PAGE as u32,
                depth_or_array_layers: 1,
            },
        )
    };
    for a in &m.areas {
        areas.push(Area {
            start: layers.len() as u32,
            count: a.layers.len() as u32,
            _pad: [0; 2],
        });
        for (j, l) in a.layers.iter().enumerate() {
            let id = if j == 0 {
                u32::MAX
            } else {
                if layer_id / per_page != page_id {
                    upload_page(&page, page_id);
                    // Prevent queued 16 MiB mask pages from accumulating
                    // alongside decoded terrain images and lightmaps.
                    if page_id % 2 == 1 {
                        queue.submit(std::iter::empty());
                        device.poll(wgpu::Maintain::Wait);
                    }
                    page.fill(0);
                    page_id += 1;
                }
                let slot = layer_id % per_page;
                let ox = slot % per_row * mask_side;
                let oy = slot / per_row * mask_side;
                let decoded_mask = l.mask.decode(l.mask_size)?;
                for y in 0..mask_side {
                    for x in 0..mask_side {
                        let sx = x * l.mask_size / mask_side;
                        let sy = y * l.mask_size / mask_side;
                        page[(oy + y) * MASK_PAGE + ox + x] = decoded_mask[sy * l.mask_size + sx];
                    }
                }
                let id = layer_id as u32;
                layer_id += 1;
                id
            };
            layers.push(Layer {
                tex: tex_map[l.texture],
                axis: l.axis as u32,
                mask: id,
                _pad: 0,
                uv: [l.u, l.v],
                _pad2: [0.; 2],
            });
        }
    }
    upload_page(&page, page_id);
    queue.submit(std::iter::empty());
    device.poll(wgpu::Maintain::Wait);
    drop(page);
    let area_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("terrain areas"),
        contents: bytemuck::cast_slice(&areas),
        usage: wgpu::BufferUsages::STORAGE
            | if cfg!(test) {
                wgpu::BufferUsages::COPY_SRC
            } else {
                wgpu::BufferUsages::empty()
            },
    });
    // A camera outside the map can have no resident layers. Keep the storage
    // binding valid even though there are no terrain triangles to sample it.
    if layers.is_empty() {
        layers.push(Layer::zeroed());
    }
    let layer_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("terrain layers"),
        contents: bytemuck::cast_slice(&layers),
        usage: wgpu::BufferUsages::STORAGE
            | if cfg!(test) {
                wgpu::BufferUsages::COPY_SRC
            } else {
                wgpu::BufferUsages::empty()
            },
    });
    let mv = masktex.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    Ok(MaskResources {
        #[cfg(test)]
        texture: masktex,
        bytes: pages * MASK_PAGE * MASK_PAGE,
        mv,
        area_buffer,
        layer_buffer,
    })
}
fn buf(
    binding: u32,
    visibility: wgpu::ShaderStages,
    ty: wgpu::BufferBindingType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn texture(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}
fn sam(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}
fn entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}
// T2LM v2: 16 independent DDS lightmaps per 512-grid subterrain.
// t2lm/t2lm1 are day/night color maps with independently indexed DDS records.
fn load_lightmaps(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    t: &Terrain,
    m: &crate::material::Materials,
    side: usize,
) -> Result<(wgpu::Texture, usize, usize, usize, usize, Vec<u32>), String> {
    if std::env::var_os("PW_TEST_NO_LIGHTMAPS").is_some() {
        let per_row = 16usize;
        let per_page = 256usize;
        let pages = ((m.areas.len() + per_page - 1) / per_page).max(1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("diagnostic neutral lightmaps"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: (pages * 2) as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        return Ok((
            texture,
            256,
            per_row,
            per_page,
            pages,
            vec![0; m.areas.len().max(1)],
        ));
    }
    let bc = device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
    let side = if bc { 256 } else { side };
    let per_row = 4096 / side;
    let per_page = per_row * per_row;
    let pages = ((m.areas.len() + per_page - 1) / per_page).max(1);
    if 2 * pages > device.limits().max_texture_array_layers as usize {
        return Err("terrain lightmap page limit exceeded".into());
    }
    // Client DDS payloads are DXT1: use native BC1 rather than quadrupling
    // memory by decoding the 2,816 images to RGBA on compatible GPUs.
    let format = if bc {
        wgpu::TextureFormat::Bc1RgbaUnorm
    } else {
        wgpu::TextureFormat::Rgba8Unorm
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("client terrain day/night lightmaps"),
        size: wgpu::Extent3d {
            width: 4096,
            height: 4096,
            depth_or_array_layers: (2 * pages) as u32,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // Upload each original tile directly into its atlas slot. Building a full
    // CPU copy of all atlas pages duplicates hundreds of MiB during startup.
    // Queue uploads are flushed in bounded groups below; the GPU atlas layout
    // and shader sampling remain exactly the same.
    let layout =
        crate::terrain_lightmaps::Layout::new(m.areas_x, m.areas.len(), t.subterrain_names.len())?;
    // Per-area binding state. A missing day record leaves an area unbound
    // (client `ApplyLightMap` returns without binding), which is not the same
    // as the day/night fallback the night slot uses.
    let mut availability = vec![crate::terrain_lightmaps::Availability::Unbound; m.areas.len()];
    let mut unbound = Vec::new();
    for (si, name) in t.subterrain_names.iter().enumerate() {
        let base = t.element.join("maps/world");
        let pair = match crate::terrain_lightmaps::Pair::read_optional(
            &base.join(format!("{name}.t2lm")),
            &base.join(format!("{name}.t2lm1")),
        )? {
            Some(pair) => pair,
            None => {
                for j in 0..crate::terrain_lightmaps::TILES {
                    unbound.push(layout.area(si, j).unwrap());
                }
                continue;
            }
        };
        for (which, ext) in ["t2lm", "t2lm1"].iter().enumerate() {
            let path = t.element.join("maps/world").join(format!("{name}.{ext}"));
            let tiles = crate::terrain_lightmaps::Tiles::parse(pair.bytes(which == 1))
                .map_err(|e| format!("lightmap {}: {e}", path.display()))?;
            for j in 0..crate::terrain_lightmaps::TILES {
                let raw = tiles.tile(j).unwrap();
                let id = layout.area(si, j).unwrap();
                availability[id] = pair.availability();
                let slot = id % per_page;
                let page = id / per_page + which * pages;
                let x = (slot % per_row) * side;
                let y = (slot / per_row) * side;
                let write_tile = |data: &[u8], bytes_per_row: u32, rows_per_image: u32| {
                    queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: x as u32,
                                y: y as u32,
                                z: page as u32,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        data,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(bytes_per_row),
                            rows_per_image: Some(rows_per_image),
                        },
                        wgpu::Extent3d {
                            width: side as u32,
                            height: side as u32,
                            depth_or_array_layers: 1,
                        },
                    );
                };
                let dds = crate::model_dds::Dds::parse(raw)
                    .map_err(|e| format!("lightmap {}: {e}", path.display()))?;
                if dds.format != crate::model_dds::Format::Dxt1
                    || dds.mips.len() != 1
                    || dds.mips[0].width != 256
                    || dds.mips[0].height != 256
                {
                    return Err(format!("Unsupported lightmap DDS in {}", path.display()));
                }
                // Both upload backends must accept exactly the same records.
                if bc {
                    write_tile(dds.mips[0].bytes, (side / 4 * 8) as u32, (side / 4) as u32);
                } else {
                    let img = image::load_from_memory_with_format(raw, image::ImageFormat::Dds)
                        .map_err(|e| format!("lightmap {}: {e}", path.display()))?
                        .to_rgba8();
                    if img.width() != 256 || img.height() != 256 {
                        return Err("unexpected lightmap dimensions".into());
                    }
                    let img = if side == 256 {
                        img
                    } else {
                        image::imageops::resize(
                            &img,
                            side as u32,
                            side as u32,
                            image::imageops::FilterType::Triangle,
                        )
                    };
                    write_tile(img.as_raw(), (side * 4) as u32, side as u32);
                }
            }
        }
        // write_texture staging belongs to wgpu until the next submission.
        // Wait periodically to cap temporary upload memory on slower GPUs.
        if si % 4 == 3 || si + 1 == t.subterrain_names.len() {
            queue.submit(std::iter::empty());
            device.poll(wgpu::Maintain::Wait);
        }
    }
    // An unbound area still owns an atlas slot, so fill it with a neutral tile
    // rather than the texture's zero-initialised contents. The shader keeps
    // sampling the slot; 0.5 grey reproduces "no lightmap bound" instead of
    // black terrain. Grass never reads these slots (availability is 0).
    if !unbound.is_empty() {
        let neutral = neutral_tile(side, bc);
        for &id in &unbound {
            for which in 0..2usize {
                let slot = id % per_page;
                let page = id / per_page + which * pages;
                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: ((slot % per_row) * side) as u32,
                            y: ((slot / per_row) * side) as u32,
                            z: page as u32,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &neutral,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(if bc {
                            (side / 4 * 8) as u32
                        } else {
                            (side * 4) as u32
                        }),
                        rows_per_image: Some(if bc { (side / 4) as u32 } else { side as u32 }),
                    },
                    wgpu::Extent3d {
                        width: side as u32,
                        height: side as u32,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        queue.submit(std::iter::empty());
        device.poll(wgpu::Maintain::Wait);
    }
    eprintln!(
        "Loaded {} terrain lightmap areas ({} with authored night maps, {} day-only, {} unbound) at {}px ({})",
        m.areas.len(),
        availability.iter().filter(|a| a.bits() == 3).count(),
        availability.iter().filter(|a| a.bits() == 1).count(),
        unbound.len(),
        side,
        if bc { "native BC1" } else { "RGBA" }
    );
    let availability: Vec<u32> = availability.iter().map(|a| a.bits()).collect();
    Ok((texture, side, per_row, per_page, pages, availability))
}

/// Neutral stand-in for an area the client would leave unbound. BC1 blocks
/// select colour0 (`RGB565(0.5,0.5,0.5)`) with every index bit clear.
fn neutral_tile(side: usize, bc: bool) -> Vec<u8> {
    if bc {
        let mut block = [0u8; 8];
        block[..2].copy_from_slice(&0x8410u16.to_le_bytes());
        block.repeat((side / 4) * (side / 4))
    } else {
        [128, 128, 128, 255].repeat(side * side)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct FarVertex {
    pos: [f32; 3],
    normal: [i8; 4],
}
pub fn world_mesh(
    t: &Terrain,
    step: usize,
) -> Result<
    (
        Vec<FarVertex>,
        Vec<u32>,
        Vec<crate::terrain_residency::Span>,
    ),
    String,
> {
    let xs = grid_axis(t.width, step);
    let ys = grid_axis(t.height, step);
    let mut v = Vec::with_capacity(xs.len() * ys.len());
    let mut terrain_reader = t.heights.reader();
    for &y in &ys {
        for &x in &xs {
            let at = y * t.width + x;
            let (height, n) = terrain_reader.cell(at)?;
            v.push(FarVertex {
                pos: [
                    (x as f32 - t.width as f32 * 0.5) * t.grid_spacing,
                    height,
                    (t.height as f32 * 0.5 - y as f32) * t.grid_spacing,
                ],
                normal: [n[0], n[1], n[2], 0],
            });
        }
    }
    let (indices, spans) = far_indices(&v, xs.len(), ys.len());
    Ok((v, indices, spans))
}

fn far_indices(
    vertices: &[FarVertex],
    cols: usize,
    rows: usize,
) -> (Vec<u32>, Vec<crate::terrain_residency::Span>) {
    let mut indices = Vec::with_capacity((cols - 1) * (rows - 1) * 6);
    // Preserve every original triangle, but group index ranges into bounded
    // patches so the main view never transforms the entire off-screen world.
    let mut spans = Vec::new();
    for py in (0..rows - 1).step_by(32) {
        for px in (0..cols - 1).step_by(32) {
            let end_x = (px + 32).min(cols - 1);
            let end_y = (py + 32).min(rows - 1);
            let start = indices.len() as u32;
            let mut low = [f32::INFINITY; 3];
            let mut high = [f32::NEG_INFINITY; 3];
            for y in py..=end_y {
                for x in px..=end_x {
                    for axis in 0..3 {
                        let value = vertices[y * cols + x].pos[axis];
                        low[axis] = low[axis].min(value);
                        high[axis] = high[axis].max(value);
                    }
                    if x < end_x && y < end_y {
                        let a = (y * cols + x) as u32;
                        let b = a + 1;
                        let c = a + cols as u32;
                        indices.extend_from_slice(&[a, b, c, b, c + 1, c]);
                    }
                }
            }
            spans.push(crate::terrain_residency::Span {
                block: (px, py),
                start,
                count: indices.len() as u32 - start,
                bounds: [low[0], low[1], low[2], high[0], high[1], high[2]],
            });
        }
    }
    (indices, spans)
}

fn grid_axis(size: usize, step: usize) -> Vec<usize> {
    assert!(size > 0 && step > 0);
    let mut values: Vec<_> = (0..size).step_by(step).collect();
    if values.last().copied() != Some(size - 1) {
        values.push(size - 1);
    }
    values
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_records_draw_what_the_per_run_loop_draws() {
        // The block walk hands over contiguous index ranges; each record has to
        // reproduce one `draw_indexed(start..start + count, 0, 0..1)`.
        let runs = [(0u32, 96u32), (96, 96), (384, 48)];
        let records = run_records(&runs);
        assert_eq!(records.len(), runs.len());
        for (record, &(start, count)) in records.iter().zip(runs.iter()) {
            assert_eq!(record.index_count, count);
            assert_eq!(record.first_index, start);
            assert_eq!(record.instance_count, 1);
            assert_eq!(record.first_instance, 0);
            assert_eq!(record.base_vertex, 0);
        }
    }

    #[test]
    fn the_single_call_path_only_runs_for_the_list_it_was_given() {
        let runs = [(0u32, 96u32), (96, 96)];
        assert!(can_multi_draw(true, 2, &runs));
        // A stale list (different length) or an empty one must use the loop.
        assert!(!can_multi_draw(true, 1, &runs));
        assert!(!can_multi_draw(true, 3, &runs));
        assert!(!can_multi_draw(true, 0, &[]));
        // Adapters without `MULTI_DRAW_INDIRECT` always use the loop.
        assert!(!can_multi_draw(false, 2, &runs));
    }
    #[test]
    fn far_patches_preserve_every_triangle_and_bound_all_their_vertices() {
        let (cols, rows) = (70, 50);
        let vertices: Vec<_> = (0..rows)
            .flat_map(|y| {
                (0..cols).map(move |x| FarVertex {
                    pos: [x as f32, ((x * 7 + y * 11) % 31) as f32, y as f32],
                    normal: [0, 127, 0, 0],
                })
            })
            .collect();
        let (indices, spans) = far_indices(&vertices, cols, rows);
        let mut expected = std::collections::BTreeSet::new();
        for y in 0..rows - 1 {
            for x in 0..cols - 1 {
                let a = (y * cols + x) as u32;
                let c = a + cols as u32;
                expected.insert([a, a + 1, c]);
                expected.insert([a + 1, c + 1, c]);
            }
        }
        let actual: std::collections::BTreeSet<_> = indices
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(indices.len(), expected.len() * 3, "no duplicate triangles");
        assert_eq!(
            spans.iter().map(|s| s.count as usize).sum::<usize>(),
            indices.len()
        );
        for span in spans {
            for &index in &indices[span.start as usize..(span.start + span.count) as usize] {
                let p = vertices[index as usize].pos;
                for axis in 0..3 {
                    assert!(p[axis] >= span.bounds[axis] && p[axis] <= span.bounds[axis + 3]);
                }
            }
        }
    }

    #[test]
    fn neutral_tiles_match_the_atlas_block_layout() {
        let rgba = super::neutral_tile(256, false);
        assert_eq!(rgba.len(), 256 * 256 * 4);
        assert!(rgba.chunks(4).all(|p| p == [128, 128, 128, 255]));
        let bc1 = super::neutral_tile(256, true);
        assert_eq!(bc1.len(), 256 * 256 / 2);
        // Every 8-byte block selects colour0 = RGB565(0.5,0.5,0.5).
        assert!(bc1.chunks(8).all(|b| b == [0x10, 0x84, 0, 0, 0, 0, 0, 0]));
        assert_eq!(super::neutral_tile(16, true).len(), 16 * 16 / 2);
    }
    #[test]
    fn lod_grid_covers_exact_edges_without_duplicates() {
        assert_eq!(grid_axis(17, 8), vec![0, 8, 16]);
        assert_eq!(grid_axis(18, 8), vec![0, 8, 16, 17]);
        assert_eq!(grid_axis(1, 8), vec![0]);
    }
    #[test]
    #[ignore = "requires a headless GPU; run explicitly after software Vulkan setup"]
    fn cutoff_gpu_residency_repacking() {
        assert!(
            std::env::var_os("PW_SMALL_MASK_ATLAS").is_none(),
            "test native masks, not diagnostic downsampling"
        );
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("headless adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("cutoff residency test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
            },
            None,
        ))
        .unwrap();
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut m = crate::material::cutoff_mask_fixture();
        let full = upload_masks(&device, &queue, &m, &[0]).unwrap();
        assert_eq!(full.bytes, 32 * 1024 * 1024);
        drop(full);
        let near = m.take_window(Some([2, 3, 5, 6]));
        let compact = upload_masks(&device, &queue, &near, &[0]).unwrap();
        assert_eq!(compact.bytes, 16 * 1024 * 1024);
        // Read actual GPU records: cold global IDs must be empty, not aliased
        // to a neighbouring area; retained layers must use compact mask slots.
        fn read_buffer(
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            source: &wgpu::Buffer,
        ) -> Vec<u8> {
            let out = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: source.size(),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut e = device.create_command_encoder(&Default::default());
            e.copy_buffer_to_buffer(source, 0, &out, 0, source.size());
            queue.submit(Some(e.finish()));
            mapped(device, &out)
        }
        fn mapped(device: &wgpu::Device, out: &wgpu::Buffer) -> Vec<u8> {
            let (tx, rx) = std::sync::mpsc::channel();
            out.slice(..)
                .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().unwrap();
            let bytes = out.slice(..).get_mapped_range().to_vec();
            out.unmap();
            bytes
        }
        let areas = read_buffer(&device, &queue, &compact.area_buffer);
        let layers = read_buffer(&device, &queue, &compact.layer_buffer);
        let mut slot = 0;
        for id in 0..400 {
            let a: Area = bytemuck::pod_read_unaligned(&areas[id * 16..id * 16 + 16]);
            if (2..5).contains(&(id % 20)) && (3..6).contains(&(id / 20)) {
                assert_eq!(a.count, 2);
                let base = a.start as usize * std::mem::size_of::<Layer>();
                let layer: Layer = bytemuck::pod_read_unaligned(&layers[base + 32..base + 64]);
                assert_eq!(layer.mask, slot);
                assert_eq!(layer.tex, 0);
                assert_eq!(layer.axis, 1);
                assert_eq!(layer.uv, [1., 1.]);
                // Read every native texel, not just a representative pixel.
                let out = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 256 * 256,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut e = device.create_command_encoder(&Default::default());
                e.copy_texture_to_buffer(
                    wgpu::ImageCopyTexture {
                        texture: &compact.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: slot * 256,
                            y: 0,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyBuffer {
                        buffer: &out,
                        layout: wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(256),
                            rows_per_image: Some(256),
                        },
                    },
                    wgpu::Extent3d {
                        width: 256,
                        height: 256,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit(Some(e.finish()));
                assert!(mapped(&device, &out).iter().all(|&b| b == id as u8));
                slot += 1;
            } else {
                assert_eq!(a.count, 0);
            }
        }
        assert_eq!(slot, 9);
        drop(compact);
        // A camera beyond the map has no layers but still needs a valid binding.
        let mut fixture = crate::material::cutoff_mask_fixture();
        let empty = fixture.take_window(Some([0, 0, 0, 0]));
        let empty_gpu = upload_masks(&device, &queue, &empty, &[0]).unwrap();
        assert!(empty_gpu.layer_buffer.size() >= std::mem::size_of::<Layer>() as u64);
        device.poll(wgpu::Maintain::Wait);
        assert!(pollster::block_on(device.pop_error_scope()).is_none());
    }
}
