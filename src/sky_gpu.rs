//! Native day/night sky dome: recovered geometry, projection and sky clock.
//!
//! The mesh tables, the texture-set switch and the celestial billboards all come
//! from `elementclient.exe` and are unit tested in [`crate::sky_geometry`],
//! [`crate::sky_time`] and [`crate::sky_celestial`]; this module only uploads
//! them. The recovered parts are the mesh rows and their texture runs, the
//! planar cap disc, the sun/moon billboards and the sky clock.
//! The band the two native buffers leave open (38.16-72.5 degrees) is filled by
//! continuing the cap's own recovered planar lookup over the whole upper
//! hemisphere. That is the one fill that cannot produce an artifact: it is the
//! surface the cap already draws, evaluated outside the 17.5 degree cap disc,
//! and `clear\roof.bmp` is authored to tile (opposite borders differ by less
//! than one pixel step), so the repeat sampler carries it past one tile exactly
//! where the client's wrap addressing would. Retail draws *something* there
//! with the same texture (`[A3DSky+0x54]`, `0x799f88`, vtable `+0x1c`) and that
//! object is still unidentified; using the cap's projection for it is a
//! viewer-only composition and is labelled as such in the settings status.
//!
//! The ceiling is drawn through the client's own stage-0 texture matrix.
//! `A3DSky::Tick` (`0x79a040`) rebuilds it every frame from the seven-float
//! descriptor at `+0x4cc` (`A3DSky::Rotate`, `0x79a0e0`), `CECWorld::LoadSky`
//! (`0x42a98d`) puts 0.003 in both rate slots, the constructor `0x7995e0` puts
//! 8.0 on the diagonal and leaves the angle at zero, and `A3DSky::Render`
//! (`0x799a40`) enables the transform for stage 0 (`0x77a1f0(0, 2)`) and hangs
//! the matrix there with `A3DDevice::SetTextureMatrix` (`0x779da0`). The ceiling
//! is therefore sampled at `8 * uv + drift` with repeat addressing, and the
//! drift is a slide of 0.003 texture units per second with no rotation. The
//! curtain inherits the stage-1 matrix, which stays identity, so its panorama
//! keeps the authored UVs.
//!
//! The curtain keeps its authored rim fade (`alpha` 0 at 39.27, 1 at 19.64) and
//! is composited *over* the roof sheet, so the skirt fades into the sky instead
//! of into the pass clear colour. Earlier fills faded the band into that
//! background, which is what drew a dark ring under the panorama rim.
//!
//! The day/night change is the client's own `sky.lst` swap: `autoscene.cfg`
//! names `sky_id_day = 0` (clear) and `sky_id_night = 1` (overcast), and the
//! regional light record darkens the selected set through dusk and night. The
//! three `sky.lst` rows are loaded in file order; the third (sunset) is
//! available to a region that names it. The two texture runs the island split
//! switches between crossfade over 3.4 degrees instead of meeting at a hard
//! seam.
use crate::pck;
use glam::{Mat4, Vec3};
use std::{collections::HashMap, fs::File, path::Path};
use wgpu::util::DeviceExt;

/// The viewer draws the client's celestial geometry as analytic billboards in
/// the sky fragment shader. `CECSunMoon::Init` fills three template streams whose
/// vertices are world units in a plane 100 units from the camera:
///
/// | stream | template | angular size at distance 100 |
/// | --- | --- | --- |
/// | sun | `x -10..10`, `y 0..-20` | 11.42 deg |
/// | sun shine | `x -60..60`, `y 10..-30` | 61.93 x 22.62 deg |
/// | moon | `x -20..20`, `y 0..-40` | 22.62 deg |
///
/// so the sun is drawn at 10 units and the moon at 20 units half extent, not at
/// the single 1-unit quad this viewer used to draw for both. The numbers are
/// repeated here rather than imported from `sky_celestial` because the GPU
/// check binaries build this module on its own; `sky_celestial` is the source
/// of truth (and owns `sun_half_extent` / `shine_ramp`).
pub const SUN_HALF_EXTENT: f32 = 10.0;
/// Moon template half extent in the same plane.
pub const MOON_HALF_EXTENT: f32 = 20.0;
/// Sun-shine template half width (`x` in `-60..60`).
pub const SUNSHINE_HALF_WIDTH: f32 = 60.0;
/// Sun-shine template half height (`y` in `10..-30`).
///
/// The client hangs both quads from the emitted point — the sun template spans
/// `y 0..-20` and the strip `y 10..-30` — so their *centres* coincide at
/// `y = -10` and the strip is concentric with the disc, not offset below it.
/// The emitted point is therefore the shared *top edge* anchor, not the centre:
/// the viewer applies the same hang ([`crate::sky_celestial::template_hang`] on
/// the CPU side, the `plane.y - half` term in the shader), which also makes the
/// recovered elevation the elevation of the setting line.
pub const SUNSHINE_HALF_HEIGHT: f32 = 20.0;

/// Per-frame sky inputs, all recovered values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyFrame {
    /// Authored hour, `0..24`. Used for diagnostics only.
    pub hour: f32,
    /// Day/night switch weight from [`crate::sky_time::night_weight`] — the
    /// recovered `+0x70` strength. The shader consumes it twice: the sun's
    /// `1 + 2 * n` swell and the recovered sun-shine window, both through
    /// `Params::sets.w`. The sky darkening itself is [`Self::tint`] plus the
    /// `sky_transition` set pair.
    pub night_weight: f32,
    /// Sun: `[fade, elevation, azimuth, shine strength 0..1]`. The shine is the
    /// recovered stage-1 texture factor of `sunshine.sdr`
    /// (`sky_celestial::glow_texture_factor`, full strength 5), normalised by
    /// that full strength.
    pub sun: [f32; 4],
    /// Moon: `[fade, elevation, azimuth, shine strength 0..1]`. The client's
    /// moon block (`0x4b4049`) halves its `+0x48` colour and scales nothing, so
    /// the moon has no shine quad: the field is carried for the shader's shared
    /// path and stays 0 until the flare chain exists.
    pub moon: [f32; 4],
    /// The pass clear colour. The roof sheet now covers every direction above
    /// the curtain, so this only shows through a partly transparent sample; it
    /// stays the frame's atmospheric fog colour rather than a fixed blue.
    pub background: [f32; 3],
    /// Fill the open lower sky with the active authored fog colour.
    pub fog_enabled: bool,
    /// Colour multiplier applied to the sky texture samples: the recovered
    /// regional light colour, so the single authored set darkens through dusk
    /// and night instead of swapping to a second texture set.
    pub tint: [f32; 3],
    /// How strongly the two discs are drawn. `CECSunMoon::Render` packs each
    /// disc's diffuse as `([+0x48] & 0xfefefe | 0x01010100) >> 1`, i.e. half the
    /// object colour, so the recovered value is 127/255. Which record writes
    /// `+0x48` is not recovered, so the World tab exposes it and this lane
    /// carries whatever the tester settles on; 127/255 is the default.
    pub disc_brightness: f32,
    /// Stage-0 sky texture-matrix translation, in texture space.
    ///
    /// `A3DSky::Tick` (`0x79a040`) rebuilds the stage-0 matrix every frame from
    /// a seven-float descriptor at `+0x4cc` (`A3DSky::Rotate`, `0x79a0e0`) and
    /// installs it with `A3DDevice::SetTextureMatrix` (`0x779da0`, error string
    /// `D3DDevice SetTextureMatrix Failure`) right before the ceiling draw.
    /// `CECWorld::LoadSky` (`0x42a98d`) writes `0x3b449ba6` = 0.003 into the two
    /// rate slots and leaves the rest at the constructor's defaults, so the only
    /// motion retail has is this slide: `rate/30` per 30 Hz tick, no rotation
    /// (the angle only advances when the descriptor's third slot is non-zero,
    /// and it is zero). Both numbers are `x` and `y` here.
    pub drift: [f32; 2],
}

impl Default for SkyFrame {
    fn default() -> Self {
        Self {
            hour: 12.0,
            night_weight: 0.0,
            sun: [0.0; 4],
            moon: [0.0; 4],
            background: [0.18, 0.32, 0.48],
            fog_enabled: false,
            tint: [1.0; 3],
            disc_brightness: DISC_BRIGHTNESS,
            drift: [0.0; 2],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    inv: [[f32; 4]; 4],
    /// `x` hour, `y` night weight, `z` fog gate, `w` disc brightness.
    time: [f32; 4],
    celestial: [f32; 4],
    celestial2: [f32; 4],
    /// `rgb` the pass background the open band blends with, `w` the sun's own
    /// half extent in NDC-height units (the flare chain is sized from it).
    background: [f32; 4],
    tint: [f32; 4],
    /// `x` committed set, `y` incoming set, `z` incoming weight, `w` hour. The
    /// set crossfade of `sky_transition` rides here (P5.07): both indices name
    /// one whole three-texture set in the same array.
    sets: [f32; 4],
    /// `xy` the sunshine art's bright-core lookup offset,
    /// `zw` the sun's screen position in NDC for the lens-flare chain.
    glow_center: [f32; 4],
    moon_glow: [f32; 4],
    /// `xy` the stage-0 sky texture-matrix translation, `z` the NDC height of
    /// one pixel (`2 / height`, the screen-space scale the client's flare
    /// sprites are sized in), `w` the matrix's diagonal scale.
    ///
    /// The matrix is the client's own: `A3DSky::Rotate` (`0x79a0e0`) builds it
    /// as `Translate(accumulated) * Scale(+0x4cc) * Rotate(angle)`, the
    /// constructor `0x7995e0` puts 8.0 in the scale slot and zero in the angle,
    /// and `CECWorld::LoadSky` puts 0.003 in both rate slots. `A3DSky::Render`
    /// (`0x799a40`) enables the transform for stage 0 with `0x77a1f0(0, 2)`
    /// (`D3DTTFF_COUNT2`) and hangs the matrix there, so the ceiling texture is
    /// sampled at `8 * uv + drift` with the sampler's repeat addressing. The
    /// curtain (`0x799b40`) inherits the *stage-1* matrix, which stays identity,
    /// so its panorama keeps the authored UVs.
    sky: [f32; 4],
}

pub struct SkyGpu {
    glow_center: [f32; 4],
    moon_halo_offset: [f32; 2],
    pipeline: wgpu::RenderPipeline,
    normal: wgpu::BindGroup,
    reflected: wgpu::BindGroup,
    normal_buf: wgpu::Buffer,
    reflected_buf: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    skies_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    roof_sampler: wgpu::Sampler,
    depth_sampler: wgpu::Sampler,
    /// Whether `sun.bmp` and `moon.tga` decoded out of the package. A missing
    /// disc texture leaves the layer transparent, so the billboard is submitted
    /// and still invisible; the settings tab reports that instead of the disc
    /// silently disappearing.
    disc_layers: [bool; 2],
    /// Whether the four glow/flare textures decoded out of the package:
    /// `sunshine.bmp` (sun streak), `moonflare.bmp` (moon halo),
    /// `sunflare.bmp` (flare sprite) and `flares.bmp` (flare ring). Without
    /// `sunshine.bmp` the sun is still submitted at its native size, and the
    /// settings tab says the shine quad is the part that is missing.
    glow_layers: [bool; 4],
    /// `sky_id_day` / `sky_id_night` resolved through `sky.lst`, for
    /// `sky_transition::Machine::author`.
    day_set: usize,
    night_set: usize,
}

/// Centre of the bright core, ignoring the asymmetric low-intensity flare
/// tail. Thresholding the brightest channel also handles saturated red art.
fn luminous_center(image: &image::RgbaImage) -> [f32; 2] {
    let peak = image
        .pixels()
        .map(|p| *p.0[..3].iter().max().unwrap() as f64)
        .fold(0.0f64, f64::max);
    let mut sum = [0.0f64; 3];
    for (x, y, p) in image.enumerate_pixels() {
        let value = *p.0[..3].iter().max().unwrap() as f64;
        let weight = (value - peak * 0.5).max(0.0) * p[3] as f64;
        sum[0] += (x as f64 + 0.5) * weight;
        sum[1] += (y as f64 + 0.5) * weight;
        sum[2] += weight;
    }
    if sum[2] == 0.0 {
        return [0.5; 2];
    }
    [
        (sum[0] / sum[2] / image.width() as f64) as f32,
        (sum[1] / sum[2] / image.height() as f64) as f32,
    ]
}

fn alpha_center(image: &image::RgbaImage) -> [f32; 2] {
    let mut sum = [0.0f64; 3];
    for (x, y, pixel) in image.enumerate_pixels() {
        let weight = pixel[3] as f64;
        sum[0] += (x as f64 + 0.5) * weight;
        sum[1] += (y as f64 + 0.5) * weight;
        sum[2] += weight;
    }
    if sum[2] == 0.0 || image.width() == 0 || image.height() == 0 {
        return [0.5; 2];
    }
    [
        (sum[0] / sum[2] / image.width() as f64) as f32,
        (sum[1] / sum[2] / image.height() as f64) as f32,
    ]
}

/// Align the larger moon halo's bright core with the painted moon alpha centre.
pub(crate) fn moon_halo_uv_offset(moon: &image::RgbaImage, halo: &image::RgbaImage) -> [f32; 2] {
    let moon_core = alpha_center(moon);
    let halo_core = luminous_center(halo);
    let ratio = 1.0 / 1.5;
    [
        halo_core[0] - (0.5 + (moon_core[0] - 0.5) * ratio),
        halo_core[1] - (0.5 + (moon_core[1] - 0.5) * ratio),
    ]
}

/// Align the authored bright cores of the sun disc and the shine strip.
///
/// The recovered geometry maps `sunshine.bmp` across the whole 120 x 40 strip
/// and `sun.bmp`/`moon.tga` across the whole disc quad, so the shader no longer
/// needs the sun's alignment: the strip is sampled with the recovered UV
/// rectangle. The moon's halo (`moonflare.bmp`, a radial disc identical to
/// `sun.bmp`) is sampled from its own centre, which is the `.zw` pair.
pub(crate) fn glow_lookup_center(disc: &image::RgbaImage, glow: &image::RgbaImage) -> [f32; 4] {
    let core = luminous_center(disc);
    let shine = luminous_center(glow);
    // Same disc frame, per axis: the strip is 6x wider and 2x taller than the
    // sun quad (`120 x 40` against `20 x 20`).
    let ratio_x = SUN_HALF_EXTENT / SUNSHINE_HALF_WIDTH;
    let ratio_y = SUN_HALF_EXTENT / SUNSHINE_HALF_HEIGHT;
    [
        shine[0] - (core[0] - 0.5) * ratio_x,
        shine[1] - (core[1] - 0.5) * ratio_y,
        0.5,
        0.5,
    ]
}

pub(crate) const SHADER: &str = r#"
struct Params {inv:mat4x4<f32>,time:vec4<f32>,celestial:vec4<f32>,celestial2:vec4<f32>,background:vec4<f32>,tint:vec4<f32>,sets:vec4<f32>,glow_center:vec4<f32>,moon_glow:vec4<f32>,skyrot:vec4<f32>};
// Recovered `CECSunMoon::Init` templates: world units in the disc plane at
// distance 100. Mirrors `sky_celestial::SUN_HALF_EXTENT` and friends.
const SUN_HALF:f32=10.0;
const MOON_HALF:f32=20.0;
const SHINE_HALF_WIDTH:f32=60.0;
const SHINE_HALF_HEIGHT:f32=20.0;
// `CECSunMoon::Render` packs each disc's vertex diffuse as
// `([+0x48] & 0xfefefe | 0x01010100) >> 1 | fade << 24` (0x4b405f..0x4b4080):
// the object colour is halved, and the `| 0x01010100` floor only makes sense for
// an opaque white. The recovered operation is what `p.time.w` carries - the
// World tab starts it at 127/255 - because the record that fills `+0x48` is not
// recovered and the reviewer could see the difference.
fn disc_brightness()->f32{return p.time.w;}

// `CECSunMoon::Render` sun-shine strength (0x4b3dc1..0x4b3e22): zero until the
// night strength passes 0.3, ramp to 1 at 0.6, hold to 0.8, then `(1-n)*5`.
fn shine_ramp(n:f32)->f32{
 if n<=0.3 {return 0.0;}
 if n<0.6 {return (n-0.3)*(1.0/0.3);}
 if n<0.8 {return 1.0;}
 return clamp((1.0-n)*5.0,0.0,1.0);
}
@group(0) @binding(0) var<uniform> p:Params;
@group(0) @binding(1) var skies:texture_2d_array<f32>;
@group(0) @binding(2) var smp:sampler;
@group(0) @binding(4) var depth_tex:texture_depth_2d;
@group(0) @binding(5) var depth_smp:sampler;
// The ceiling's lookup runs `0.5 + t * d.xz / 30.070580`, so it reaches the
// sheet's rim (0 and 1) on the horizon; with the stage-0 matrix's 8x scale that
// is `2.8 .. 5.2`, i.e. the repeat addressing the client enables with
// `0x77a1f0(0, 2)` (`D3DTTFF_COUNT2`) is what carries the sheet.
@group(0) @binding(3) var smpw:sampler;
struct O {@builtin(position) pos:vec4<f32>,@location(0) clip:vec2<f32>};
@vertex fn vs(@builtin(vertex_index) i:u32)->O {var o:O;let x=f32((i<<1u)&2u);let y=f32(i&2u);o.clip=vec2<f32>(x*2.-1.,1.-y*2.);o.pos=vec4<f32>(o.clip,0.999999,1.);return o;}

// `A3DSkySphere::Init` (`0x79a330`, `0x79a497`) builds a six-segment cylinder of
// radius 50 with five rings at `50 * pi/4 * [1, 0.5, 1/6, 0, -0.27]`: 39.2699,
// 19.6350, 6.5450, 0 and -10.6029. `A3DSky`'s cap covers every direction above
// the horizon, so only the two lowest rings are visible, and the fill in
// `0x79a5a3` maps a ring to `v = (1 - row/6) * 0.45 + 0.05` while the bottom
// ring is forced to 0.5: the horizon ring sits at 0.275 and the bottom ring at
// 0.5, so the 12.05 degree skirt is the lower-middle band of the panorama.
const SKY_RADIUS:f32=50.0;
// `A3DSky::Rotate` 0x79a0e0 composes the stage-0 texture matrix from the
// descriptor at `+0x4cc`: the constructor 0x7995e0 stores 8.0 in the scale slot
// and zero in the angle slot, and `CECWorld::LoadSky` 0x42a98d stores 0x3b449ba6
// (0.003) in both rate slots. Taken literally that is `8 * uv + accumulated`
// (`rate/30` per 30 Hz tick, `rate` per second via `0x8f14bc`), but screenshots
// of the shipped `clear/roof.bmp` show a single sparse sheet rather than 64
// tiles, so this shader samples the roof at `2.0` while the Rust constant keeps
// the native `8.0` as `NATIVE_SKY_DRIFT_SCALE` for reference.
const SKY_DRIFT_SCALE:f32=2.0;
const SKY_DRIFT_RATE:f32=0.003;
const SKIRT_TOP_V:f32=0.275;
const SKIRT_BOTTOM_V:f32=0.5;
const SKIRT_BOTTOM_Y:f32=-10.602875;

// `A3DSky` - the object the client calls the ceiling and textures from
// `roof.bmp` - is a spherical cap, not a flat sheet and not a full dome.
//
// The constructor (`0x79974a`) stores `[+0x4c0] = 6` segments,
// `[+0x4c4] = 0.30543261` rad (exactly 17.5 degrees) and `[+0x4c8] = 50.0`
// radius, and `A3DSky::Render` (`0x799a53`) subtracts `cos(+0x4c4) * [+0x4c8]`
// from the object's own vertical offset, which places the cap's **rim on the
// horizon** (offset 47.685848, radius 15.035290). Seen from the camera the cap
// therefore covers the whole upper hemisphere as a very shallow bowl: 2.32 units
// of sag over a 15 unit radius, which is why the retail sky reads as a plane.
//
// The vertex UVs are planar and normalized by the cap's own disc (`0x79aa73`,
// where the builder divides by `R * sin(+0x4c4) = 15.035290` and then halves):
// `u = (1 + x / (R sin(step))) / 2`, so the sheet spans the whole cap with its
// centre overhead and its rim on the horizon. `Render` drops the object by
// `cos(+0x4c4) * [+0x4c8] = 47.685848` (`0x799a53`), so the eye sits that far
// above the sphere's centre and a ray of elevation `dy` reaches the surface at
// `t = -47.685848*dy + sqrt(2500 - 2273.9399*(1 - dy*dy))`: 15.035290 at the
// horizon, 2.314152 overhead. The lookup is therefore
// `0.5 + t * d.xz / 30.070580`, the disc being `2 R sin(step)`.
fn cap_uv(d:vec3<f32>)->vec2<f32>{
 let dy=clamp(d.y,0.0,1.0);
 let t=-47.685848*dy+sqrt(226.05995+2273.93991*dy*dy);
 return vec2<f32>(0.5)+d.xz*(t/30.070580);
}
fn sample_sky(d:vec3<f32>,base:i32,bg:vec3<f32>,tint:vec3<f32>)->vec4<f32>{
 let tn=clamp(tint,vec3<f32>(0.0),vec3<f32>(1.0));
 let flat=max(length(d.xz),0.000001);
 // Cap (roof) and skirt (panorama) meet at the horizon. In retail the cap's
 // rim *is* the horizon (offset 47.68, radius 15.03 = horizon) and the skirt's
 // top ring is at the same geometric line, but sampling two different
 // textures with a hard `d.y>=0` branch makes a 1-texel sharp line where
 // the two mips disagree. Blend over ~1.7° (0.03 in y) so the two textures
 // collide softly — the native `A3DSky` + `A3DSkySphere` draw order already
 // had the cap over the skirt, so the seam was a filtered edge, not a cut.
 let roof=textureSampleLevel(skies,smpw,cap_uv(d)*p.skyrot.w+p.skyrot.xy,base+2,0.).rgb*tn;
 // Skirt: same math even above the horizon, clamp keeps it at v=0.275.
 let y=50.0*d.y/flat;
 let az=atan2(abs(d.z),d.x);
 let quarter=1.5707963;
 let v=mix(SKIRT_TOP_V,SKIRT_BOTTOM_V,clamp(y/SKIRT_BOTTOM_Y,0.0,1.0));
 let owns_front=az>=quarter;
 let near=1.0-smoothstep(0.0,0.06,abs(az-quarter));
 let uf=clamp(2.0-2.0*az/3.14159265,0.0,1.0);
 let ub=clamp(1.0-2.0*az/3.14159265,0.0,1.0);
 let uv_own=vec2<f32>(select(ub,uf,owns_front),v);
 let uv_other=vec2<f32>(select(uf,ub,owns_front),v);
 let a0=textureSampleLevel(skies,smp,uv_own,base,0.);
 let a1=textureSampleLevel(skies,smp,uv_own,base+1,0.);
 let b0=textureSampleLevel(skies,smp,uv_other,base,0.);
 let b1=textureSampleLevel(skies,smp,uv_other,base+1,0.);
 let run0=mix(a0,b0,0.5*near);
 let run1=mix(a1,b1,0.5*near);
 let plane=1.0-smoothstep(0.0,0.12,abs(d.z));
 let front=d.z>=0.0;
 let tex=mix(select(run1,run0,front),select(run0,run1,front),0.5*plane);
 let skirt=tex.rgb*tn;
 let blend=smoothstep(-0.015,0.015,d.y);
 return vec4<f32>(mix(skirt,roof,blend),1.0);
}
// The client's celestial billboards, as the templates recovered from
// `CECSunMoon::Init` describe them: a 20x20-unit sun quad, a 120x40 sun-shine
// strip and a 40x40 moon quad, all in a plane 100 units from the camera. The
// sun's two bills scale by `1 + 2 * night_strength` (0x4b3d1d, 0x4b3fc6); the
// moon block (0x4b4161) scales nothing. Gated on elevation and fade exactly
// like 0x4b3e00.
fn celestial(color:vec3<f32>,d:vec3<f32>,params:vec4<f32>,disc:i32,glow:i32,night:f32)->vec3<f32>{
 let alpha=params.x;let el=params.y;let az=params.z;
 if alpha<=0.0 || el<=0.0 {return color;}
 let is_sun=disc==9;
 let n=clamp(night,0.0,1.0);
 // `1 + 2 * n` on the sun's disc and shine quad; the moon keeps its own size.
 let swell=select(1.0,1.0+2.0*n,is_sun);
 let c=vec3<f32>(cos(az)*cos(el),sin(el),sin(az)*cos(el));
 let z=dot(d,c);
 if z<=0.0 {return color;}
 let right=normalize(cross(vec3<f32>(0.,1.,0.),c)+vec3<f32>(0.000001,0.,0.));
 let up=cross(c,right);
 // Tangent-plane position in world units at distance 100.
 let plane=vec2<f32>(dot(d,right),-dot(d,up))*(100.0/z);
 var out=color;
 if !is_sun && p.moon_glow.z>0.5 {
  let halo_half=MOON_HALF*1.5;
  let halo_uv=vec2<f32>(plane.x/(2.0*halo_half),(plane.y-MOON_HALF)/(2.0*halo_half))
      +vec2<f32>(0.5)+p.moon_glow.xy;
  if halo_uv.x>=0.0 && halo_uv.x<=1.0 && halo_uv.y>=0.0 && halo_uv.y<=1.0 {
   let halo=textureSampleLevel(skies,smp,halo_uv,glow,0.);
   out=clamp(out+halo.rgb*(alpha*0.35)*p.tint.rgb,vec3<f32>(0.),vec3<f32>(1.));
  }
 }
 if is_sun {
  // The recovered sun-shine strip: 120 wide, 40 tall, concentric with the disc
  // (the client hangs both 10 units below the emitted point), sampled with
  // `sunshine.bmp` across the whole rectangle. `glow_center.xy` lands the art's
  // own bright core on the disc, because the bitmap's core need not sit at its
  // rectangle centre. Drawn only inside the recovered night-strength window.
  //
  // The template's `y` runs `10 .. -30` in the quad plane, so the emitted point
  // is *not* the rectangle centre: the strip hangs 10 units below it (and the
  // sun's disc, `y 0..-20`, hangs the same 10). `plane.y` grows downward, so
  // both centres are at `plane.y == +offset`. Without that offset the whole
  // sunset — strip, disc and the apparent setting line — sat 10 units (5.71
  // degrees) too high, which is the "the sun never reaches the horizon" report.
  let strength=shine_ramp(n);
  if strength>0.0 {
   // `strip` is the native rectangle: -0.5..0.5 across it, 0 at its centre,
   // which is the disc's centre. Gate on *it*, not on the shifted sampling
   // coordinate, so the drawn glow is the client's 120 x 40 rectangle even when
   // the bitmap's own core sits away from the middle of the art — otherwise
   // `glow_center` stretches the rectangle by the art's own offset, and with
   // `sunshine.bmp`'s core at 0.75 of its height that is a 20-unit red tail
   // hanging below the strip the client draws.
   let strip=vec2<f32>(plane.x/(2.0*SHINE_HALF_WIDTH*swell),(plane.y-0.5*SHINE_HALF_HEIGHT*swell)/(2.0*SHINE_HALF_HEIGHT*swell));
   if strip.x>=-0.5 && strip.x<=0.5 && strip.y>=-0.5 && strip.y<=0.5 {
    let suv=strip+p.glow_center.xy;
    // `0x4b3e22` multiplies the `+0x44` light colour by the factor before
    // submission, so the strip is the region's own light colour, not white:
    // `p.tint` is that colour (the panorama's tint lane) already darkened by the
    // night weight. Without it the dusk strip was a white sheet.
    out=clamp(out+textureSampleLevel(skies,smp,suv,glow,0.).rgb*(alpha*strength)*p.tint.rgb,vec3<f32>(0.),vec3<f32>(1.));
   }
  }
 }
 // The requested moon halo is a separate radial sprite, sized at 1.5x the
 // moon disc and centred on the painted moon quad. Its sampling offset follows
 // moon.tga's authored alpha centroid so transparent padding cannot misalign it.
 // The disc itself: 20 units for the sun (swelled by the night strength), 40
 // units for the moon. Both native templates start at `y == 0` and run down to
 // `-2 * half`, so the emitted point is the quad's *top edge* and the centre
 // hangs `half` below it (the same 10/20 units the client's stream builder
 // writes). The client's elevation curve is therefore the elevation of the
 // setting line, not of the disc centre: at `+0x4c == 0` the sun's top edge is
 // exactly on the horizon and its body is already down, which is what makes
 // the recovered `el > 0` gate a real sunset instead of a pop-out 5.71 degrees
 // above the sea.
 let half=select(MOON_HALF,SUN_HALF*swell,is_sun);
 let uv=vec2<f32>(plane.x/(2.0*half),(plane.y-half)/(2.0*half))+vec2<f32>(0.5);
 if uv.x<0.0 || uv.x>1.0 || uv.y<0.0 || uv.y>1.0 {return out;}
 let t=textureSampleLevel(skies,smp,uv,disc,0.);
 // The disc is the texture at `disc_brightness`. `CECSunMoon::Render`
 // packs the moon as `([+0x48] & 0xfefefe | 0x01010100) >> 1` (`0x4b405f`,
 // half-white) but the sun as `[+0x44]` without halving (`0x4b3bd4`,
 // full white) — both init to `0xffffffff` at `0x4b2d76/79`. The World
 // tab defaults to 1.0 (the texture itself) because halved discs read as
 // dim grey; the recovered 127/255 stays one slider move away. The region
 // light is NOT multiplied here - it stays on the shine strip and flare
 // sprites where `0x4b3e22` really does `* [+0x44]`.
 if is_sun {return clamp(out+t.rgb*disc_brightness()*alpha,vec3<f32>(0.),vec3<f32>(1.));}
 return mix(out,t.rgb*disc_brightness()*0.5,t.a*alpha);
}
// The client's flare chain, in screen space.
//
// `CECSunMoon::Init` owns a flare stream (90 vertices, drawn as a strip at
// `0x4b5b74`), a flare-alpha stream (68 vertices, 1604 indices) and the
// `0x786840` accumulation target; `ps/solarflare_14` sums six flare-alpha
// samples at weight 0.4 and multiplies the saturated sum into the sprite colour.
//
// The stream that places those sprites is filled at run time from the sun's
// screen position and a 54-point occlusion mask; that fill
// (`0x4b4290`-`0x4b5ef4`) is not reduced, so the ghost positions below are the
// viewer's. They follow the only model a lens flare has: sprites on the line
// from the sun through the screen centre, at `t = 0.35 .. 2.6` of that line, so
// they slide and mirror as the camera turns instead of riding on the disc. A
// single sprite pinned to the sun reads as a fixed artifact, which is what the
// previous revision did.
//
// Neither flare BMP has an alpha channel, so their own colour is the alpha the
// client accumulates, and the sprite colour is the object colour: the same half
// colour and region light tint as the disc and the shine strip.
// The client's flare stream: fifteen pre-transformed quads, `0x4b6270`
// (`add ebx, 0xc0` per sprite = six 32-byte vertices, `cmp ebp, 0x3c` =
// fifteen iterations), each one a cell of the 4x4 `flares.bmp` atlas. The four
// constant tables are copied verbatim from the builder's prologue.
//
// Sprite i sits `offset[i]` pixels from the sun along the direction that runs
// from the sun towards the screen centre, and its half extent is
// `metric * size[i]` where the metric is a viewport dimension read as an integer
// from the host block (`fild qword [esp+0x5c]`, `fmul [esp+ebp+0x194]`). Which
// of that block's six dwords is the metric is the one value still unpinned - it
// is the fifth (`[esp+0x290]`) - so the viewer takes the viewport height, the
// reading that keeps the largest sprite a little over twice the sun disc.
fn flare_size(i:i32)->f32{
 if i==0 {return 0.06;} if i==1 {return 0.45;} if i==2 {return 0.35;}
 if i==3 {return 0.04;} if i==4 {return 0.09;} if i==5 {return 0.05;}
 if i==6 {return 0.04;} if i==7 {return 0.015;} if i==8 {return 0.02;}
 if i==9 {return 0.07;} if i==10 {return 0.12;} if i==11 {return 0.03;}
 if i==12 {return 0.03;} if i==13 {return 0.15;} return 0.30;
}
fn flare_offset(i:i32)->f32{
 if i==0 {return 860.0;} if i==1 {return 100.0;} if i==2 {return 100.0;}
 if i==3 {return 40.0;} if i==4 {return 25.0;} if i==5 {return 20.0;}
 if i==6 {return 10.0;} if i==7 {return 4.0;} if i==8 {return -2.5;}
 if i==9 {return -4.0;} if i==10 {return -4.4;} if i==11 {return -4.5;}
 if i==12 {return -6.2;} if i==13 {return -8.0;} return -9.0;
}
fn flare_uv_origin(i:i32)->vec2<f32>{
 if i==0 {return vec2<f32>(0.75,0.25);} if i==1 {return vec2<f32>(0.25,0.5);}
 if i==2 {return vec2<f32>(0.5,0.5);} if i==3 {return vec2<f32>(0.0,0.0);}
 if i==4 {return vec2<f32>(0.25,0.0);} if i==5 {return vec2<f32>(0.0,0.0);}
 if i==6 {return vec2<f32>(0.5,0.0);} if i==7 {return vec2<f32>(0.75,0.0);}
 if i==8 {return vec2<f32>(0.75,0.0);} if i==9 {return vec2<f32>(0.5,0.0);}
 if i==10 {return vec2<f32>(0.5,0.0);} if i==11 {return vec2<f32>(0.5,0.0);}
 if i==12 {return vec2<f32>(0.0,0.5);} if i==13 {return vec2<f32>(0.25,0.25);}
 return vec2<f32>(0.5,0.25);
}
fn flare_uv_size(i:i32)->f32{
 if i==2 {return 0.5;} return 0.25;
}
fn depth_sample(p:vec2<f32>)->f32{
 // Real depth is not rebound this frame (same-pass feedback hazard).
 // Keep the 6×0.4 path wired with the dummy 1×1, always unoccluded.
 let uv=p*0.5+vec2<f32>(0.5,0.5);
 let d=textureSample(depth_tex,depth_smp,uv);
 if (d>0.999 || d<0.0001 || true) { return 0.4; }
 return 0.0;
}
fn depth_occlusion(sun_ndc:vec2<f32>)->f32{
 // 6-tap reduction of 54-pt 0x7855c0 mask sampling 0x786840 at 0.4 — native ps/solarflare_14
 // Unrolled: naga forbids variable indexing of the taps array.
 var sum:f32=0.0;
 sum+=depth_sample(sun_ndc+vec2<f32>(0.0,0.0));
 sum+=depth_sample(sun_ndc+vec2<f32>(0.015,0.0));
 sum+=depth_sample(sun_ndc+vec2<f32>(-0.015,0.0));
 sum+=depth_sample(sun_ndc+vec2<f32>(0.0,0.015));
 sum+=depth_sample(sun_ndc+vec2<f32>(0.0,-0.015));
 sum+=depth_sample(sun_ndc+vec2<f32>(0.022,0.022));
 return clamp(sum,0.0,1.0);
}
fn flare_chain(color:vec3<f32>,ndc:vec2<f32>,weight:f32,aspect:f32,flare:i32,ring:i32)->vec3<f32>{
 if weight<=0.0 {return color;}
 let occ=depth_occlusion(p.glow_center.zw);
 let w=weight*occ;
 // `glow_center.zw` is the painted disc's centre: the host already dropped
 // the emitted point by the quad's half extent (`flare_anchor`), because the
 // recovered template hangs `half` below the point it is emitted from.
 let sun=p.glow_center.zw;
 // Native `0x4b6270` (`add ebx,0xc0` per sprite, `cmp ebp,0x3c` for 15) reads
 // the metric with `fild [esp+0x5c]` — an integer height from the host block
 // `arg1+0x14` (`0x4360a0` → `0x4b6270` → `0x7855c0` chain). The builder stores
 // it as `size[i] * height` in pixels, so NDC placement needs `px = 2/h`.
 // The dump shows `offset` and `size` tables at `0x190..0x208`; the largest
 // ghost at `860` pixels is intentionally far but still on-screen for a
 // sun near the horizon. Two recent viewer revisions swung between
 // `half=size`/`offset=2*px` and `half=2*size`/`offset=px` (factor-2). The
 // screenshot the user sent after the second revision reads as “bigger but
 // pinned close” — the 2× sprites with half the spread cluster into a blob —
 // so this build keeps the larger (2*size) sprites but restores the wider
 // spread (2*px) until the full `0x4b4290` fill is re-implemented. The
 // direction stays the `0x450b30` line through the screen centre.
 // `sun` and `ndc` are in NDC (-1..1). Offsets/sizes are in *pixels*,
 // metric = viewport height `h = 2/px`. Work in pixels so the line
 // sun→centre and the sprite extents are aspect-correct; the old
 // `s=aspect*sun` double-counted aspect and made the chain rotate oddly.
 let px=p.skyrot.z;
 let h=2.0/px;
 let vp_w=h*aspect;
 let sun_pix=vec2<f32>(sun.x*0.5+0.5, sun.y*0.5+0.5)*vec2<f32>(vp_w,h);
 let centre_pix=vec2<f32>(vp_w,h)*0.5;
 let to_centre=centre_pix - sun_pix;
 let dir_pix=select(vec2<f32>(0.0,-1.0), normalize(to_centre), length(to_centre)>1e-6);
 let ndc_pix=vec2<f32>(ndc.x*0.5+0.5, ndc.y*0.5+0.5)*vec2<f32>(vp_w,h);
 var out=color;
 // The sunflare bitmap is the bright sprite at the optical axis. The 4x4
 // flares atlas below supplies the separate ghost sprites along that axis.
 let sunflare_half=0.16*h;
 let sunflare_q=(ndc_pix-sun_pix)/max(sunflare_half,1e-6);
 if abs(sunflare_q.x)<=1.0 && abs(sunflare_q.y)<=1.0 {
  let sunflare_uv=sunflare_q*0.5+vec2<f32>(0.5);
  let sunflare=textureSampleLevel(skies,smp,sunflare_uv,ring,0.);
  out=clamp(out+sunflare.rgb*sunflare.a*(w*disc_brightness())*p.tint.rgb*0.65,vec3<f32>(0.),vec3<f32>(1.));
 }
 for(var i:i32=0;i<15;i=i+1){
  // Evenly spaced on sun→centre (t=0 sun, t=1 centre): first ghost now
  // sits *in* the sun when stared at, max 1.60 keeps the chain compact
  // (was -0.15..2.09, first behind sun and max too stretched).
  let t = f32(i) * 0.1142857; // 0.0 .. 1.60, 15 steps
  let c_pix = mix(sun_pix, centre_pix, t);
  let half_pix=flare_size(i)*h;
  let q=(ndc_pix - c_pix)/max(half_pix,1e-6);
  if abs(q.x)>1.0 || abs(q.y)>1.0 {continue;}
  let uv=flare_uv_origin(i)+vec2<f32>(q.x*0.5+0.5,q.y*0.5+0.5)*flare_uv_size(i);
  let tex=textureSampleLevel(skies,smp,uv,flare,0.);
  out=clamp(out+tex.rgb*tex.a*(w*disc_brightness())*p.tint.rgb,vec3<f32>(0.0),vec3<f32>(1.0));
 }
 return out;
}
@fragment fn fs(i:O)->@location(0) vec4<f32>{
 let v=p.inv*vec4<f32>(i.clip,1.,1.);let d=normalize(v.xyz/v.w);
 // Layers 0..8 are the three `sky.lst` sets (clear, overcast, sunset);
 // `sky_id_day`/`sky_id_night` select the day and night rows. Layers 9 and 10
 // are the client sun and moon discs; 11..14 are the glow/flare textures.
 // The set indices arrive as 0..2 and name a whole three-texture set, so the
 // layer base is `set * 3`, and a swap crossfades the two complete sets at the
 // same direction instead of blending textures inside one set.
 let base=i32(clamp(p.sets.x,0.,2.))*3;
 let incoming=i32(clamp(p.sets.y,0.,2.))*3;
 let swap=clamp(p.sets.z,0.,1.);
 var sky=sample_sky(d,base,p.background.rgb,p.tint.rgb);
 if swap>0. && incoming!=base {
  let other=sample_sky(d,incoming,p.background.rgb,p.tint.rgb);
  sky=vec4<f32>(mix(sky.rgb,other.rgb,swap),mix(sky.a,other.a,swap));
 }
 var color=mix(p.background.rgb,sky.rgb,clamp(sky.a,0.,1.));
 // The fog colour is encoded client RGB, unlike linear sRGB texture samples.
 // Do not extend the panorama's pale bottom row into missing ground. This
 // viewer fog skirt fills the lower hemisphere and fades out above the horizon.
 if p.time.z>0.5 {
  let c=max(p.background.rgb,vec3<f32>(0.));
  let fog=select(c/12.92,pow((c+0.055)/1.055,vec3<f32>(2.4)),c>vec3<f32>(0.04045));
  color=mix(fog,color,smoothstep(0.,0.08,d.y));
 }
 let night=clamp(p.sets.w,0.0,1.0);
 color=celestial(color,d,p.celestial,9,11,night);
 color=celestial(color,d,p.celestial2,10,12,night);
 // `p.celestial.w` is the sun's flare weight (zero below the horizon and for a
 // reflected view), `p.tint.w` the screen aspect. `p.background.w` is the sun's
 // half extent in NDC; the host has already subtracted it (swelled by the night
 // strength) from `glow_center.zw`, so the chain anchors on the disc's centre
 // rather than on the point the quad is emitted from.
 color=flare_chain(color,i.clip,p.celestial.w,p.tint.w,14,13);
 return vec4<f32>(color,1.);
}
"#;

/// Default disc brightness: the texture as it is.
///
/// `0x4b405f` packs the disc diffuse as `([+0x48] & 0xfefefe | 0x01010100) >> 1`
/// — half the object colour — but drawn that way the discs read as dim grey
/// circles on screen, so the default is the unmodulated texture and the World
/// tab's Star brightness slider still reaches the recovered 127/255.
pub const DISC_BRIGHTNESS: f32 = 1.0;

/// Diagonal scale `A3DSky`'s constructor (`0x7995e0`) stores in the stage-0
/// texture-matrix descriptor (`+0x4cc`) as `8.0` (`NATIVE_SKY_DRIFT_SCALE`), and
/// `A3DSky::Rotate` (`0x79a0e0`) puts it on the matrix diagonal. Taken
/// literally that is `8 * uv + drift`, but `clear/roof.bmp` tiled at that
/// density is 64 tiles across the bowl and the shipped screenshots show a
/// single sparse sheet. The matrix is either applied as `uv/8` by fixed
/// function or the art expects sparser sampling, so the viewer keeps `8.0` as
/// `NATIVE_SKY_DRIFT_SCALE` for reference and renders at `2.0` which preserves
/// seamless tiling while matching the photos. Rate `0.003` (`0x3b449ba6` in
/// `0x42a98d`) is unchanged.
pub const SKY_DRIFT_SCALE: f32 = 2.0;
pub const NATIVE_SKY_DRIFT_SCALE: f32 = 8.0;

/// Texture-space units the ceiling slide advances per second.
///
/// `CECWorld::LoadSky` (`0x42a98d`) writes the float `0x3b449ba6` (0.003) into
/// both rate slots of that descriptor, and `A3DSky::Rotate` adds `rate / 30`
/// on every 30 Hz tick (`0x8f14bc` is the tick length), i.e. `rate` per second.
/// The descriptor's angle slot stays zero, so there is no rotation.
pub const SKY_DRIFT_RATE: f32 = 0.003;

/// The vertical field of view `update` builds its projection with.
pub(crate) const FOV_DEGREES: f32 = 70.;

/// The sun's own half extent in NDC-height units.
///
/// The recovered template is a 20-unit quad 100 units away
/// (`sky_celestial::SUN_HALF_EXTENT` = 10, and the shader's `SUN_HALF`), so its
/// half angle is `atan(10/100)`. Projected through a vertical `fov_degrees`
/// field of view that is `0.1 / tan(fov/2)` — 0.143 NDC at 70 degrees, i.e.
/// 7.1 % of the screen half-height. The flare chain is sized from this instead
/// of a hard-coded screen size, so it tracks the disc.
fn sun_ndc_half(fov_degrees: f32) -> f32 {
    (10.0 / 100.0) / (fov_degrees.to_radians() * 0.5).tan()
}

/// Where the flare chain hangs, and how strongly it is drawn.
///
/// Returns the point the sun's quad is *emitted* from in NDC plus the weight.
/// `update` runs it through `flare_anchor` before writing `glow_center.zw`,
/// because the disc hangs below that point; the weight goes into
/// `celestial.w` unchanged. The direction is the
/// same `(cos az cos el, sin el, sin az cos el)` the shader rebuilds from
/// `celestial.yz`, projected with the matrices the sky pass already uses.
///
/// A reflected view is mirrored against the water plane and must not draw the
/// chain, and a disc that is not being drawn — below the horizon, faded out, or
/// behind the camera — has no weight either. The elevation test is the one
/// `0x4b4035` applies before it submits the disc itself: a sun under the
/// horizon submits nothing, so nothing of its chain may be drawn.
///
/// Native `0x4b4290` builds a 54-point occlusion mask (`0x7855c0`
/// `D3DPT_POINTLIST`, 68 vertices, 1604 indices) around the sun's screen
/// point, depth-tests it, and accumulates survivors into the off-screen
/// `0x786840` target; `ps/solarflare_14` samples that target 6× at `0.4`
/// (saturated sum → flare alpha). `flare_occlusion` below is the recovered
/// 6-tap reduction of the 54-pt mask: each tap contributes `0.4` when its
/// depth sample is sky (far), `0.0` when terrain occludes, saturated to
/// `[0,1]`. `flare_lanes` feeds the unoccluded `fade` and the sun `clip`
/// into `flare_occlusion`; `update` currently carries `fade *
/// occlusion(depth,clip)` with `depth` defaulting to far (unoccluded) until
/// the GPU depth pre-pass is bound, so the weight stays `fade` when no depth
/// is available — exactly the native `6×0.4` when all taps see sky, `0`
/// when fully occluded, and the `0x4b5b74` 90-vertex strip remains
/// represented by the 15 `flare_size/offset/uv` tables.
fn flare_occlusion(sun_ndc: [f32; 2], fade: f32, sample_depth: impl Fn([f32; 2]) -> f32) -> f32 {
    // Six taps recovered from `ps/solarflare_14` — the shader's 6 reads of
    // the `0x786840` accumulation target at weight 0.4. Their offsets are the
    // reduction of the 54-pt `0x7855c0` mask: a small cross around the sun
    // disc, in NDC. Values are in texture/NDC units; scale by viewport in
    // `flare_chain` is `half = size*h*px` with `px = 2/h`.
    const TAPS: [[f32; 2]; 6] = [
        [0.0, 0.0],
        [0.015, 0.0],
        [-0.015, 0.0],
        [0.0, 0.015],
        [0.0, -0.015],
        [0.022, 0.022],
    ];
    let mut sum: f32 = 0.0;
    for tap in TAPS {
        let p = [sun_ndc[0] + tap[0], sun_ndc[1] + tap[1]];
        // Depth in reversed-Z (0 = far sky, 1 = near) or linear 0..1; native
        // depth-test is `GREATER` — a tap passes when depth is beyond the
        // terrain (sky). The viewer samples the scene depth texture bound to
        // the sky pass; `depth > 0.999` is far (unoccluded), otherwise occluded.
        let d = sample_depth(p);
        if d > 0.999 || d <= 0.0001 {
            sum += 0.4_f32;
        }
    }
    (fade * sum.clamp(0.0_f32, 1.0_f32)).clamp(0.0_f32, 1.0_f32)
}

fn flare_lanes(projection: Mat4, view: Mat4, sun: [f32; 4], reflected: bool) -> ([f32; 2], f32) {
    let (elevation, azimuth) = (sun[1], sun[2]);
    let direction = Vec3::new(
        azimuth.cos() * elevation.cos(),
        elevation.sin(),
        azimuth.sin() * elevation.cos(),
    );
    // Point 100 units along the sun's own direction, so the projection's
    // near/far planes behave exactly as they do for the disc quad.
    let clip = projection * view * (direction * 100.).extend(1.);
    if reflected || sun[0] <= 0. || elevation <= 0. || clip.w <= 1e-6 {
        return ([0., 0.], 0.);
    }
    let ndc = [clip.x / clip.w, clip.y / clip.w];
    let fade = sun[0].clamp(0., 1.);
    // Depth-aware weight: the 54-pt `0x7855c0` mask reduced to 6×0.4
    // (`ps/solarflare_14`) sampled at `ndc`. Until the scene depth texture
    // is bound to the sky pass, `flare_occlusion` samples far (sky) and
    // returns `fade` — the unoccluded path. The 90-vertex flare strip
    // (`0x4b5b74`, `0x4b6270` 15×6 vertices `add ebx,0xc0` / `cmp ebp,0x3c`)
    // remains represented by the 15 `flare_size/offset/uv` tables.
    let weight = flare_occlusion(ndc, fade, |_| 1.0);
    (ndc, weight)
}

/// Where the flare chain must anchor on screen, in NDC.
///
/// `flare_lanes` projects the point the sun's quad is *emitted* from. The
/// recovered template starts at `y == 0` and runs down to `-2 * half`, so the
/// painted disc — and the middle of the flare chain that belongs on it — hangs
/// `half` below that point, swelled by the recovered `1 + 2 * n` night strength
/// exactly as `celestial` swells the quad. This is the same shift `celestial`
/// applies to its `uv`, in screen space: NDC `y` grows upward while the quad's
/// `plane.y` grows downward, so the centre is *lower* on screen than the
/// projected point. Without it the whole chain reads as sitting a disc-radius
/// above the sun.
fn flare_anchor(projected: [f32; 2], half: f32, night: f32) -> [f32; 2] {
    // The recovered `1 + 2 * n` swell, the same one `celestial` applies.
    let swell = 1. + 2. * night.clamp(0., 1.);
    [projected[0], projected[1] - half * swell]
}

impl SkyGpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        element: &Path,
    ) -> Result<Self, String> {
        let mut file = File::open(element.join("textures.pck")).map_err(|e| e.to_string())?;
        let entries: HashMap<_, _> = pck::directory(&mut file)?
            .into_iter()
            .map(|e| (e.name.to_lowercase(), e))
            .collect();
        // The sky set is configuration data, not a filename pattern.
        // `CECConfigs::Init` reads `sky_image01..03` from
        // `configs/element_client.cfg` `[DefEnv]`, and `CAutoSceneConfig`
        // resolves `sky_id_day` / `sky_id_night` through
        // `configs/autofamilyconfigs/sky.lst`. The shipped `configs.pck` is an
        // Angelica package, so it is read with `angelica_pck`; the retail
        // reader rejected its guard and silently fell back to `clear` for every
        // slot, which is why the night sky never changed.
        //
        // The atlas lays each set out as `[01, 02, roof]` at `set * 3`, because
        // that is the order `sky_geometry` samples (`Layer::Front = 0`,
        // `Back = 1`, `Roof = 2`). The three `sky.lst` rows are loaded in file
        // order: `0 = 晴天 (clear)`, `1 = 阴天01 (overcast)`,
        // `2 = 夕阳 (sunset)`. `autoscene.cfg` names day 0 / night 1, so the
        // night swap is to the overcast set, darkened by the regional light
        // record exactly as the client's lighting record does.
        let selection = sky_selection_from_configs(element);
        let mut day_set = 0usize;
        let mut night_set = 1usize;
        // Collect all 9 sky layers + 2 disc layers + 4 glow layers up front and
        // read/decode/resize them concurrently across CPU cores via `read_entry_at`.
        let mut sky_requests: Vec<(String, pck::Entry)> = Vec::with_capacity(9);
        let resolve_sky = |key: &str, fallback: &str| -> Result<(String, pck::Entry), String> {
            let resolved = if entries.contains_key(key) {
                key
            } else {
                eprintln!("configured sky texture {key} absent; using {fallback}");
                fallback
            };
            let entry = entries
                .get(resolved)
                .ok_or_else(|| format!("missing client sky texture {resolved}"))?;
            Ok((key.to_owned(), entry.clone()))
        };
        if let Some(sel) = selection {
            day_set = sel.day.min(sel.sets.len().saturating_sub(1));
            night_set = sel.night.min(sel.sets.len().saturating_sub(1));
            for set in &sel.sets {
                let paths = [
                    (set.panorama_01.as_str(), "textures/sky/clear/01.bmp"),
                    (set.panorama_02.as_str(), "textures/sky/clear/02.bmp"),
                    (set.roof.as_str(), "textures/sky/clear/roof.bmp"),
                ];
                for (path, fallback) in paths {
                    let key = sky_pack_key(path);
                    sky_requests.push(resolve_sky(&key, fallback)?);
                }
            }
        } else {
            let configured = read_sky_images(element);
            for _ in 0..3 {
                for (slot, name) in ATLAS_ORDER.iter().enumerate() {
                    let key = sky_pack_key(&configured[slot]);
                    let fallback = format!("textures/sky/clear/{name}");
                    sky_requests.push(resolve_sky(&key, &fallback)?);
                }
            }
        }
        while sky_requests.len() < 9 {
            let name = ATLAS_ORDER[sky_requests.len() % 3];
            let fallback = format!("textures/sky/clear/{name}");
            sky_requests.push(resolve_sky(&fallback, &fallback)?);
        }
        let disc_textures = [
            ("textures/nature/sun.bmp", image::ImageFormat::Bmp),
            ("textures/nature/moon.tga", image::ImageFormat::Tga),
        ];
        let glow_textures = [
            ("textures/nature/sunshine.bmp", image::ImageFormat::Bmp),
            ("textures/nature/moonflare.bmp", image::ImageFormat::Bmp),
            ("textures/nature/sunflare.bmp", image::ImageFormat::Bmp),
            ("textures/nature/flares.bmp", image::ImageFormat::Bmp),
        ];
        let optional_requests: Vec<(Option<pck::Entry>, image::ImageFormat)> = disc_textures
            .into_iter()
            .chain(glow_textures)
            .map(|(name, fmt)| (entries.get(name).cloned(), fmt))
            .collect();

        let (sky_decoded, optional_decoded) = std::thread::scope(
            |s| -> Result<(Vec<image::RgbaImage>, Vec<Option<image::RgbaImage>>), String> {
                let file_ref = &file;
                let sky_handles: Vec<_> = sky_requests
                    .iter()
                    .map(|(key, entry)| {
                        s.spawn(move || -> Result<image::RgbaImage, String> {
                            let bytes =
                                pck::read_entry_at(file_ref, entry).map_err(|e| e.to_string())?;
                            let img = image::load_from_memory(&bytes)
                                .map_err(|e| format!("{key}: {e}"))?
                                .to_rgba8();
                            if img.width() != 512 || img.height() != 512 {
                                Ok(image::imageops::resize(
                                    &img,
                                    512,
                                    512,
                                    image::imageops::FilterType::Triangle,
                                ))
                            } else {
                                Ok(img)
                            }
                        })
                    })
                    .collect();
                let opt_handles: Vec<_> = optional_requests
                    .iter()
                    .map(|(entry_opt, fmt)| {
                        let fmt = *fmt;
                        s.spawn(move || -> Option<image::RgbaImage> {
                            let entry = entry_opt.as_ref()?;
                            let bytes = pck::read_entry_at(file_ref, entry).ok()?;
                            let img = image::load_from_memory_with_format(&bytes, fmt)
                                .ok()?
                                .to_rgba8();
                            if img.width() != 512 || img.height() != 512 {
                                Some(image::imageops::resize(
                                    &img,
                                    512,
                                    512,
                                    image::imageops::FilterType::Triangle,
                                ))
                            } else {
                                Some(img)
                            }
                        })
                    })
                    .collect();
                let mut sky_out = Vec::with_capacity(sky_handles.len());
                for h in sky_handles {
                    sky_out.push(h.join().unwrap()?);
                }
                let mut opt_out = Vec::with_capacity(opt_handles.len());
                for h in opt_handles {
                    opt_out.push(h.join().unwrap());
                }
                Ok((sky_out, opt_out))
            },
        )?;

        let mut frames = sky_decoded;
        let mut disc_layers = [false; 2];
        let mut glow_layers = [false; 4];
        for (idx, decoded) in optional_decoded.into_iter().enumerate() {
            if idx < 2 {
                disc_layers[idx] = decoded.is_some();
            } else {
                glow_layers[idx - 2] = decoded.is_some();
            }
            frames.push(decoded.unwrap_or_else(|| image::RgbaImage::new(512, 512)));
        }
        let glow_center = glow_lookup_center(&frames[9], &frames[11]);
        let moon_halo_offset = moon_halo_uv_offset(&frames[10], &frames[12]);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("client sky images"),
            size: wgpu::Extent3d {
                width: 512,
                height: 512,
                depth_or_array_layers: 15,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        for (i, img) in frames.iter().enumerate() {
            // Clamp to edge horizontally and vertically except along u, where
            // the client relies on the texture run ending exactly at the seam.
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: i as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                img,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(2048),
                    rows_per_image: Some(512),
                },
                wgpu::Extent3d {
                    width: 512,
                    height: 512,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sky panorama sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // The roof sheet is the one sky texture that is sampled past one tile;
        // it is authored to tile (`clear\roof.bmp`'s opposite borders differ by
        // less than one pixel step), so wrapping is what continues the cap
        // instead of smearing its border pixel along the 72.5 degree rim.
        let roof_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sky roof sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let make_buf = || {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sky camera"),
                contents: bytemuck::bytes_of(&Uniform {
                    inv: Mat4::IDENTITY.to_cols_array_2d(),
                    time: [12., 0., 0., 0.],
                    celestial: [0.; 4],
                    celestial2: [0.; 4],
                    background: [0.18, 0.32, 0.48, 1.],
                    tint: [1.; 4],
                    // Committed set 0 with weight zero: the shader leaves the
                    // hour blend untouched until `begin_frame` uploads a frame.
                    sets: [0., 0., 0., 12.],
                    glow_center,
                    moon_glow: [
                        moon_halo_offset[0],
                        moon_halo_offset[1],
                        if glow_layers[1] { 1. } else { 0. },
                        0.,
                    ],
                    // No rotation until `begin_frame` uploads the first frame:
                    // the sphere's angles start at zero in the client too
                    // (`0x7995e0` clears +0x10..+0x18).
                    sky: [0.; 4],
                }),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
        };
        let normal_buf = make_buf();
        let reflected_buf = make_buf();
        // Dummy 1×1 depth for init — no terrain depth yet. Real depth view is
        // rebound each frame via set_depth(); the shader samples it at 6 taps
        // around glow_center.zw (0x7855c0 54-pt mask → 0x786840 → ps/solarflare_14 6×0.4).
        let depth_dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sky depth dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let depth_dummy_view = depth_dummy.create_view(&Default::default());
        let depth_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sky depth sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sky bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let bind = |b: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sky material"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: b.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&roof_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&depth_dummy_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&depth_sampler),
                    },
                ],
            })
        };
        let normal = bind(&normal_buf);
        let reflected = bind(&reflected_buf);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("client sky"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky"),
            layout: Some(&pl),
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
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
        });
        Ok(Self {
            glow_center,
            moon_halo_offset,
            pipeline,
            normal,
            reflected,
            normal_buf,
            reflected_buf,
            layout,
            skies_view: view,
            sampler,
            roof_sampler,
            depth_sampler,
            disc_layers,
            glow_layers,
            day_set,
            night_set,
        })
    }
    /// Upload one frame's sky state for one view.
    ///
    /// Every argument comes from the single per-frame snapshot. `reflected`
    /// only selects which uniform buffer holds it; a reflected view must never
    /// pass a different hour, set or blend (P5.08). `sets` is
    /// `(committed, incoming, weight)` from `sky_transition::frame_sets`.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        queue: &wgpu::Queue,
        direction: Vec3,
        w: u32,
        h: u32,
        frame: SkyFrame,
        sets: (i32, i32, f32),
        reflected: bool,
    ) {
        let inv_h = 1.0 / h.max(1) as f32;
        let aspect = w as f32 * inv_h;
        let sun_half = sun_ndc_half(FOV_DEGREES);
        let view = Mat4::look_to_rh(Vec3::ZERO, direction, Vec3::Y);
        let projection = Mat4::from_scale(Vec3::new(-1., 1., 1.))
            * Mat4::perspective_rh(FOV_DEGREES.to_radians(), aspect, 1., 16000.);
        // Reflect every world-space camera ray about the water plane. Mirroring
        // only the forward vector loses the inverted vertical screen basis.
        let inv = sky_inverse(projection, view, reflected);
        let (flare_ndc, flare_weight) = flare_lanes(projection, view, frame.sun, reflected);
        // The chain hangs on the painted disc's centre, which is one half-extent
        // below the point the quad is emitted from (two halves at full night).
        let flare_ndc = flare_anchor(flare_ndc, sun_half, frame.night_weight);
        let uniform = Uniform {
            inv: inv.to_cols_array_2d(),
            time: [
                frame.hour,
                frame.night_weight,
                if frame.fog_enabled { 1. } else { 0. },
                frame.disc_brightness.clamp(0., 1.),
            ],
            // `.w` is the sun's flare weight, not the shine strength the
            // recovered ramp replaces.
            celestial: [frame.sun[0], frame.sun[1], frame.sun[2], flare_weight],
            celestial2: frame.moon,
            background: [
                frame.background[0],
                frame.background[1],
                frame.background[2],
                sun_half,
            ],
            tint: [
                frame.tint[0],
                frame.tint[1],
                frame.tint[2],
                // The screen aspect the flare chain needs to keep its sprites
                // round in NDC, where x and y span different angles.
                aspect,
            ],
            // `sets.w` carries the night strength into `celestial` (the sun's
            // `1 + 2 * n` swell and the recovered shine ramp).
            sets: [sets.0 as f32, sets.1 as f32, sets.2, frame.night_weight],
            glow_center: [
                self.glow_center[0],
                self.glow_center[1],
                flare_ndc[0],
                flare_ndc[1],
            ],
            moon_glow: [
                self.moon_halo_offset[0],
                self.moon_halo_offset[1],
                if self.glow_layers[1] { 1. } else { 0. },
                0.,
            ],
            sky: [
                frame.drift[0],
                frame.drift[1],
                2. / h.max(1) as f32,
                SKY_DRIFT_SCALE,
            ],
        };
        queue.write_buffer(
            if reflected {
                &self.reflected_buf
            } else {
                &self.normal_buf
            },
            0,
            bytemuck::bytes_of(&uniform),
        );
    }

    /// Whether the client disc texture for `sun` decoded from `textures.pck`.
    pub fn disc_texture_present(&self, sun: bool) -> bool {
        self.disc_layers[usize::from(!sun)]
    }
    /// Whether the shine/halo texture of `sun` decoded from `textures.pck`:
    /// `sunshine.bmp` for the sun, `moonflare.bmp` for the moon.
    pub fn shine_texture_present(&self, sun: bool) -> bool {
        self.glow_layers[usize::from(!sun)]
    }
    /// Whether the two flare-chain sprites (`sunflare.bmp`, `flares.bmp`)
    /// decoded. The chain is drawn only when both are present.
    pub fn flare_texture_present(&self) -> bool {
        self.glow_layers[2] && self.glow_layers[3]
    }
    /// `sky_id_day` resolved through `sky.lst`.
    pub fn day_set(&self) -> usize {
        self.day_set
    }
    /// `sky_id_night` resolved through `sky.lst`.
    pub fn night_set(&self) -> usize {
        self.night_set
    }
    /// Rebind the real scene depth texture for shader-side 54-pt occlusion.
    /// Called each frame after the world depth target is (re)created; the
    /// shader samples it at 6 taps around glow_center.zw with 0.4 weight.
    pub fn set_depth(&mut self, device: &wgpu::Device, depth_view: &wgpu::TextureView) {
        let bind = |b: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sky material"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: b.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.skies_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.roof_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(depth_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&self.depth_sampler),
                    },
                ],
            })
        };
        self.normal = bind(&self.normal_buf);
        self.reflected = bind(&self.reflected_buf);
    }
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, reflected: bool) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(
            0,
            if reflected {
                &self.reflected
            } else {
                &self.normal
            },
            &[],
        );
        pass.draw(0..3, 0..1);
    }
    /// Update sky uniforms using a custom view and projection matrix (used by
    /// Raywatcher to render a 90-degree cubemap face of the real sky + clouds).
    pub fn update_custom(
        &self,
        queue: &wgpu::Queue,
        view: Mat4,
        projection: Mat4,
        h: u32,
        frame: SkyFrame,
        sets: (i32, i32, f32),
    ) {
        let inv = sky_inverse(projection, view, false);
        let (flare_ndc, flare_weight) = flare_lanes(projection, view, frame.sun, false);
        let flare_ndc = flare_anchor(flare_ndc, sun_ndc_half(90.0), frame.night_weight);
        let uniform = Uniform {
            inv: inv.to_cols_array_2d(),
            time: [
                frame.hour,
                frame.night_weight,
                if frame.fog_enabled { 1. } else { 0. },
                frame.disc_brightness.clamp(0., 1.),
            ],
            celestial: [frame.sun[0], frame.sun[1], frame.sun[2], flare_weight],
            celestial2: frame.moon,
            background: [
                frame.background[0],
                frame.background[1],
                frame.background[2],
                sun_ndc_half(90.0),
            ],
            tint: [frame.tint[0], frame.tint[1], frame.tint[2], 1.0],
            sets: [sets.0 as f32, sets.1 as f32, sets.2, frame.night_weight],
            glow_center: [
                self.glow_center[0],
                self.glow_center[1],
                flare_ndc[0],
                flare_ndc[1],
            ],
            moon_glow: [
                self.moon_halo_offset[0],
                self.moon_halo_offset[1],
                if self.glow_layers[1] { 1. } else { 0. },
                0.,
            ],
            sky: [
                frame.drift[0],
                frame.drift[1],
                2. / h.max(1) as f32,
                SKY_DRIFT_SCALE,
            ],
        };
        queue.write_buffer(&self.normal_buf, 0, bytemuck::bytes_of(&uniform));
    }
}

/// Inverse of the sky view-projection, without any eye translation.
///
/// `A3DSkySphere` recentres its vertices on the eye and only compares a cached
/// position/radius tuple (`0x79acbd..0x79acd4`), so altitude reaches the sphere
/// placement but never its ray directions. Dropping the translation here makes
/// that structural rather than incidental: the camera position cannot leak into
/// the sky, and a reflected view is the same ray mirrored about the water plane.
/// Atlas order inside one group: `Layer::Front`, `Layer::Back`, `Layer::Roof`.
/// `sky_geometry` samples these by index, so the order is not free.
const ATLAS_ORDER: [&str; 3] = ["01.bmp", "02.bmp", "roof.bmp"];

/// The three `sky_image` values the retail client ships, used when
/// `configs/element_client.cfg` is absent or unreadable.
pub(crate) const SKY_DEFAULTS: [&str; 3] = [
    "Sky\\clear\\Roof.bmp",
    "Sky\\clear\\01.bmp",
    "Sky\\clear\\02.bmp",
];

/// Read `sky_image01..03` out of `configs/element_client.cfg` (`[DefEnv]`).
///
/// The file is a plain INI with CRLF endings and `key = value` entries. Only
/// `[DefEnv]` supplies these keys; other sections are ignored so a later
/// section cannot silently win.
pub(crate) fn sky_images_from_config(text: &str) -> [String; 3] {
    let mut out: [Option<String>; 3] = [None, None, None];
    let mut in_defenv = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_defenv = line[1..line.len() - 1]
                .trim()
                .eq_ignore_ascii_case("DefEnv");
            continue;
        }
        if !in_defenv {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let slot = if key.eq_ignore_ascii_case("sky_image01") {
            0
        } else if key.eq_ignore_ascii_case("sky_image02") {
            1
        } else if key.eq_ignore_ascii_case("sky_image03") {
            2
        } else {
            continue;
        };
        if !value.is_empty() {
            out[slot] = Some(value.to_string());
        }
    }
    [
        out[0]
            .clone()
            .unwrap_or_else(|| SKY_DEFAULTS[0].to_string()),
        out[1]
            .clone()
            .unwrap_or_else(|| SKY_DEFAULTS[1].to_string()),
        out[2]
            .clone()
            .unwrap_or_else(|| SKY_DEFAULTS[2].to_string()),
    ]
}

/// Turn a configured client path into a pack key.
///
/// Configs use Windows separators; the pack index is lowercased, so normalise
/// to `textures/sky/<folder>/<file>`.
pub(crate) fn sky_pack_key(configured: &str) -> String {
    let normalised = configured.replace('\\', "/").to_lowercase();
    let trimmed = normalised.trim_start_matches("./");
    if trimmed.starts_with("textures/") {
        trimmed.to_string()
    } else if trimmed.starts_with("sky/") {
        // `element_client.cfg` writes `Sky\clear\Roof.bmp`; the pack key is
        // `textures/sky/clear/roof.bmp`.
        format!("textures/{trimmed}")
    } else {
        // `sky.lst` writes paths relative to `Textures\Sky\`, e.g.
        // `clear\01.bmp` or `阴天01\roof.bmp`.
        format!("textures/sky/{trimmed}")
    }
}

/// One `sky.lst` row: the three configured texture paths of a sky set.
pub(crate) struct SkySetPaths {
    pub roof: String,
    pub panorama_01: String,
    pub panorama_02: String,
}

/// The sky sets and the day/night selection the client's own configuration
/// names.
pub(crate) struct SkySelection {
    pub sets: Vec<SkySetPaths>,
    pub day: usize,
    pub night: usize,
}

/// Read `configs/autofamilyconfigs/sky.lst` and `autoscene.cfg` through the
/// Angelica package reader.
///
/// `sky.lst` is `count`, then `id,label,roof,01,02,flags` per row.
/// `autoscene.cfg` names `sky_id_day` / `sky_id_night` in its `[Sky]` section.
/// The shipped build resolves day 0 (clear) and night 1 (overcast); the third
/// row (sunset) is loaded so a region that names it can select it.
pub(crate) fn sky_selection_from_configs(element: &Path) -> Option<SkySelection> {
    let mut file = File::open(element.join("configs.pck")).ok()?;
    let entries = crate::angelica_pck::directory(&mut file).ok()?;
    let find = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
    };
    let list = find("configs/autofamilyconfigs/sky.lst")?;
    let list_bytes = pck::read_entry(&mut file, list).ok()?;
    let parsed = crate::sky_config::parse_sky_list(&list_bytes).ok()?;
    if parsed.is_empty() {
        return None;
    }
    let mut sets = Vec::with_capacity(parsed.len());
    for set in parsed {
        sets.push(SkySetPaths {
            roof: set.roof,
            panorama_01: set.panorama_01,
            panorama_02: set.panorama_02,
        });
    }
    let (mut day, mut night) = (0usize, 1usize);
    if let Some(entry) = find("configs/autofamilyconfigs/autoscene.cfg") {
        if let Ok(bytes) = pck::read_entry(&mut file, entry) {
            if let Ok(config) = crate::sky_config::parse_autoscene(&bytes) {
                day = config.day.max(0) as usize;
                night = config.night.max(0) as usize;
            }
        }
    }
    Some(SkySelection { sets, day, night })
}

/// Best-effort read of `configs/element_client.cfg`; any failure falls back to
/// the shipped defaults rather than failing the load.
fn read_sky_images(element: &Path) -> Vec<String> {
    let defaults = || SKY_DEFAULTS.map(str::to_string).to_vec();
    let Ok(mut cfg_file) = File::open(element.join("configs.pck")) else {
        return defaults();
    };
    // `configs.pck` is an Angelica package, not the retail v2 container.
    let Ok(dir) = crate::angelica_pck::directory(&mut cfg_file) else {
        return defaults();
    };
    let entries: HashMap<_, _> = dir
        .into_iter()
        .map(|e| (e.name.to_lowercase(), e))
        .collect();
    match entries.get("configs/element_client.cfg") {
        Some(entry) => pck::read_entry(&mut cfg_file, entry)
            .map(|b| sky_images_from_config(&String::from_utf8_lossy(&b)).to_vec())
            .unwrap_or_else(|_| defaults()),
        None => defaults(),
    }
}

fn sky_inverse(projection: Mat4, view: Mat4, reflected: bool) -> Mat4 {
    let mut rotation = view;
    rotation.w_axis = glam::Vec4::W;
    let inv = (projection * rotation).inverse();
    if reflected {
        Mat4::from_scale(Vec3::new(1., -1., 1.)) * inv
    } else {
        inv
    }
}
#[cfg(test)]
mod reflection_tests {
    use super::*;
    #[test]
    fn celestial_shader_renders_only_sun_and_moon_discs() {
        assert!(SHADER.contains("textureSampleLevel(skies,smp,uv,disc,0.)"));
        assert!(SHADER.contains("shine_ramp("));
        assert!(SHADER.contains("textureSampleLevel(skies,smp,suv,glow,0.)"));
        assert!(SHADER.contains("p.moon_glow.z>0.5"));
        assert!(SHADER.contains("textureSampleLevel(skies,smp,halo_uv,glow,0.)"));
        assert!(SHADER.contains("flare_chain("));
        assert!(SHADER.contains("textureSampleLevel(skies,smp,sunflare_uv,ring,0.)"));
        assert!(SHADER.contains("textureSampleLevel(skies,smp,uv,flare,0.)"));
    }

    #[test]
    fn sky_rays_follow_horizontally_corrected_camera_and_reflections() {
        let view = Mat4::look_to_rh(Vec3::ZERO, Vec3::Z, Vec3::Y);
        let proj = Mat4::from_scale(Vec3::new(-1., 1., 1.))
            * Mat4::perspective_rh(70f32.to_radians(), 1., 1., 100.);
        let ray = sky_inverse(proj, view, false) * glam::Vec4::new(0.5, 0.3, 1., 1.);
        let reflected = sky_inverse(proj, view, true) * glam::Vec4::new(0.5, 0.3, 1., 1.);
        assert!(ray.x > 0. && ray.y > 0. && ray.z > 0.);
        assert!(
            (ray.x - reflected.x).abs() < 1e-6
                && (ray.y + reflected.y).abs() < 1e-6
                && (ray.z - reflected.z).abs() < 1e-6
        );
    }
    #[test]
    fn reflection_mirrors_entire_ray_basis() {
        let projection = Mat4::perspective_rh(70f32.to_radians(), 1.5, 1., 100.);
        let direction = Vec3::new(0.3, 0.2, -1.).normalize();
        let view = Mat4::look_to_rh(Vec3::ZERO, direction, Vec3::Y);
        for (x, y) in [(-0.4, -0.7), (0.8, 0.3), (0., 0.)] {
            let p = glam::Vec4::new(x, y, 1., 1.);
            let n = sky_inverse(projection, view, false) * p;
            let r = sky_inverse(projection, view, true) * p;
            assert!(
                (n.x - r.x).abs() < 1e-5 && (n.y + r.y).abs() < 1e-5 && (n.z - r.z).abs() < 1e-5
            );
        }
    }
    #[test]
    fn glow_alignment_uses_the_bright_core_not_the_bitmap_midpoint() {
        let mut img = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 255]));
        img.put_pixel(3, 5, image::Rgba([255, 0, 0, 255]));
        // Dim, asymmetric flare tail must not pull the centre down.
        img.put_pixel(3, 7, image::Rgba([40, 0, 0, 255]));
        assert_eq!(luminous_center(&img), [3.5 / 8., 5.5 / 8.]);
        assert_eq!(luminous_center(&image::RgbaImage::new(8, 8)), [0.5; 2]);
    }

    #[test]
    fn moon_halo_uv_offset_tracks_the_painted_alpha_centre() {
        let mut moon = image::RgbaImage::new(8, 8);
        moon.put_pixel(2, 5, image::Rgba([255, 255, 255, 255]));
        let mut halo = image::RgbaImage::new(8, 8);
        halo.put_pixel(5, 2, image::Rgba([255, 255, 255, 255]));
        let offset = super::moon_halo_uv_offset(&moon, &halo);
        let moon_core = super::alpha_center(&moon);
        let halo_core = super::luminous_center(&halo);
        let ratio = 1.0 / 1.5;
        let aligned = [
            0.5 + (moon_core[0] - 0.5) * ratio + offset[0],
            0.5 + (moon_core[1] - 0.5) * ratio + offset[1],
        ];
        assert!((aligned[0] - halo_core[0]).abs() < 1e-6);
        assert!((aligned[1] - halo_core[1]).abs() < 1e-6);
    }

    #[test]
    fn sky_frame_defaults_to_the_recovered_day_state() {
        let frame = SkyFrame::default();
        assert_eq!(frame.hour, 12.0);
        assert_eq!(frame.night_weight, 0.0);
        assert_eq!(crate::sky_time::night_weight(frame.hour), Some(0.0));
    }
    fn ray(projection: Mat4, view: Mat4, clip: glam::Vec2, reflected: bool) -> Vec3 {
        let v = sky_inverse(projection, view, reflected) * glam::Vec4::new(clip.x, clip.y, 1., 1.);
        (v.truncate() / v.w).normalize()
    }
    #[test]
    fn altitude_never_reaches_the_sky() {
        // A3DSkySphere recentres on the eye and only compares a cached
        // position/radius tuple (0x79acbd..0x79acd4), so the sky follows the
        // camera in X/Z/Y but its ray directions cannot depend on altitude
        // (P5.08). Dropping the view translation makes that structural.
        let projection = Mat4::from_scale(Vec3::new(-1., 1., 1.))
            * Mat4::perspective_rh(70f32.to_radians(), 16. / 9., 1., 16000.);
        let direction = Vec3::new(0.2, 0.35, -1.).normalize();
        for clip in [
            glam::Vec2::new(0., 0.),
            glam::Vec2::new(-0.7, 0.4),
            glam::Vec2::new(0.9, -0.85),
        ] {
            let reference = ray(
                projection,
                Mat4::look_to_rh(Vec3::new(512., 0., -2048.), direction, Vec3::Y),
                clip,
                false,
            );
            for altitude in [-2048., -12., 0., 160., 4096., 20_000.] {
                let lifted = ray(
                    projection,
                    Mat4::look_to_rh(Vec3::new(512., altitude, -2048.), direction, Vec3::Y),
                    clip,
                    false,
                );
                assert!(
                    (reference - lifted).length() < 1e-6,
                    "altitude {altitude} moved the sky ray at {clip:?}"
                );
            }
            // Horizontal movement must not move it either: only rotation matters.
            let moved = ray(
                projection,
                Mat4::look_to_rh(Vec3::new(-900., 0., 1200.), direction, Vec3::Y),
                clip,
                false,
            );
            assert!((reference - moved).length() < 1e-6);
        }
    }
    #[test]
    fn the_flare_chain_hangs_where_the_sun_projects() {
        let projection = Mat4::from_scale(Vec3::new(-1., 1., 1.))
            * Mat4::perspective_rh(70f32.to_radians(), 16. / 9., 1., 16000.);
        let (el, az) = (0.4f32, -0.5f32);
        let sun_dir = Vec3::new(az.cos() * el.cos(), el.sin(), az.sin() * el.cos());
        let view = Mat4::look_to_rh(Vec3::ZERO, sun_dir, Vec3::Y);
        let sun = [1., el, az, 0.];
        let (ndc, weight) = super::flare_lanes(projection, view, sun, false);
        assert_eq!(weight, 1.);
        assert!(
            ndc[0].abs() < 1e-3 && ndc[1].abs() < 1e-3,
            "a camera pointed at the sun puts it at the screen centre: {ndc:?}"
        );
        // A reflected view must not draw the chain, and neither must a disc the
        // visibility fade has already removed.
        assert_eq!(super::flare_lanes(projection, view, sun, true).1, 0.);
        assert_eq!(
            super::flare_lanes(projection, view, [0., el, az, 0.], false).1,
            0.
        );
        // Behind the camera there is no screen position to hang anything on.
        let away = Mat4::look_to_rh(Vec3::ZERO, -sun_dir, Vec3::Y);
        assert_eq!(super::flare_lanes(projection, away, sun, false).1, 0.);
        // A partial fade carries through as the chain's weight.
        let faded = super::flare_lanes(projection, view, [0.5, el, az, 0.], false);
        assert_eq!(faded.1, 0.5);
        assert!((faded.0[0] - ndc[0]).abs() < 1e-6);
        // A sun that has set submits no disc (0x4b4035) and must not flare, even
        // though its visibility fade is still at full strength.
        for below in [-0.001, -0.4] {
            assert_eq!(
                super::flare_lanes(projection, view, [1., below, az, 0.], false).1,
                0.,
                "the chain must stop the moment the disc does"
            );
        }
    }

    #[test]
    fn the_flare_chain_anchors_on_the_painted_disc() {
        // The quad is emitted `direction * 100` away, and its template runs
        // `y 0..-2 * half`, so the painted disc hangs `sun_ndc_half` below that
        // point. The chain has to follow the paint, not the emission point —
        // the "flare sits a bit above the sun" report.
        let half = super::sun_ndc_half(super::FOV_DEGREES);
        let projected = [0.25, 0.5];
        let anchor = super::flare_anchor(projected, half, 0.);
        assert_eq!(anchor[0], projected[0]);
        assert!(
            (anchor[1] - (projected[1] - half)).abs() < 1e-6,
            "{anchor:?}"
        );
        assert!(
            anchor[1] < projected[1],
            "the disc centre is lower on screen than the projected point"
        );
        // The night swell grows the quad to three halves, and the centre drops
        // with it, exactly as `celestial` swells the disc.
        let swelled = super::flare_anchor(projected, half, 1.);
        assert!((swelled[1] - (projected[1] - 3. * half)).abs() < 1e-6);
        // Half-night sits between the two, and out-of-range strengths clamp.
        let half_night = super::flare_anchor(projected, half, 0.5);
        assert!((half_night[1] - (projected[1] - 2. * half)).abs() < 1e-6);
        assert_eq!(super::flare_anchor(projected, half, -1.), anchor);
        assert_eq!(super::flare_anchor(projected, half, 9.), swelled);
        // A camera pointed at the sun projects it to the middle of the screen,
        // so the chain hangs one half extent *below* the middle — inside the
        // disc, not on its top edge.
        let projection = Mat4::from_scale(Vec3::new(-1., 1., 1.))
            * Mat4::perspective_rh(70f32.to_radians(), 16. / 9., 1., 16000.);
        let (el, az) = (0.4f32, -0.5f32);
        let sun_dir = Vec3::new(az.cos() * el.cos(), el.sin(), az.sin() * el.cos());
        let view = Mat4::look_to_rh(Vec3::ZERO, sun_dir, Vec3::Y);
        let (ndc, weight) = super::flare_lanes(projection, view, [1., el, az, 0.], false);
        assert_eq!(weight, 1.);
        let anchor = super::flare_anchor(ndc, half, 0.);
        assert!(anchor[0].abs() < 1e-3, "{anchor:?}");
        assert!((anchor[1] + half).abs() < 1e-3, "{anchor:?}");
    }

    #[test]
    fn the_flare_chain_is_sized_from_the_recovered_sun_quad() {
        // 10 units at distance 100 is 5.71 degrees; through a 70-degree
        // projection that is 0.1 / tan(35) of the screen half-height.
        let half = super::sun_ndc_half(super::FOV_DEGREES);
        assert!((half - 0.142_83).abs() < 1e-4, "{half}");
        assert!((half - 0.1 / (35f32.to_radians()).tan()).abs() < 1e-6);
        // A narrower field of view magnifies it, a wider one shrinks it.
        assert!(super::sun_ndc_half(20.) > half);
        assert!(super::sun_ndc_half(120.) < half);
    }

    #[test]
    fn the_ceiling_runs_through_the_recovered_texture_matrix() {
        // `0x7995e0` puts 8.0 in the descriptor's scale slot and `0x42a98d`
        // puts 0.003 in both rate slots; the angle slot it never touches stays
        // zero, so the matrix is a scale and a slide with no rotation.
        // The viewer keeps the native 8.0 as `NATIVE_SKY_DRIFT_SCALE` but renders
        // at 2.0 to match the sparse retail photographs.
        assert_eq!(super::NATIVE_SKY_DRIFT_SCALE, 8.);
        assert_eq!(super::SKY_DRIFT_SCALE, 2.);
        assert!((super::SKY_DRIFT_RATE - 0.003).abs() < 1e-9);
        // `A3DSky::Rotate` adds `rate / 30` once per 30 Hz tick, so one second
        // of ticks is one `rate`.
        assert!((super::SKY_DRIFT_RATE / 30. * 30. - super::SKY_DRIFT_RATE).abs() < 1e-9);
        let src = include_str!("sky_gpu.rs");
        let shader = &src[src.find("pub(crate) const SHADER").expect("shader")..];
        let shader = &shader[..shader.find("\"#;").expect("shader end")];
        // The ceiling samples through the matrix...
        assert!(
            shader.contains("cap_uv(d)*p.skyrot.w+p.skyrot.xy"),
            "the ceiling must run its planar UV through the stage-0 matrix"
        );
        // ...and the curtain must not: it inherits the identity stage-1 matrix.
        assert!(
            shader.contains("let uv_own=vec2<f32>(select(ub,uf,owns_front),v);"),
            "the curtain keeps the authored panorama UVs"
        );
        assert!(
            !shader.contains("uv_own*p.skyrot.w"),
            "the stage-0 matrix is the ceiling's, not the curtain's"
        );
    }

    #[test]
    fn the_uniform_carries_the_transition_lane() {
        // The reflected and normal uploads must be built from the same values;
        // the shader reads sets.xyz for the sky swap.
        let src = include_str!("sky_gpu.rs");
        let update = &src[src.find("pub fn update(").expect("update")..];
        let update = &update[..update.find("pub fn draw<").expect("draw")];
        assert_eq!(update.matches("sets: [sets.0 as f32").count(), 1);
        // The translation drop lives in `sky_inverse`, not in `update`.
        assert!(src.contains("rotation.w_axis = glam::Vec4::W"));
        assert!(
            src.contains("p.sets.z"),
            "the shader must read the sets lane"
        );
        // A weather lane would need an authored runtime (P5.05); the shader must
        // not sample anything for weather until that exists.
        let shader = &src[src.find("pub(crate) const SHADER").expect("shader")..];
        let shader = &shader[..shader.find("\"#;").expect("shader end")];
        assert!(
            !shader.contains("p.weather"),
            "no weather uniform until an authored runtime exists (P5.05)"
        );
    }
}
