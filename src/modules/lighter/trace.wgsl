// Shared radiance, texture and ray-query interface for both Lighter backends.
// All colors written to the image are linear; the compositor encodes once.
struct Frame {
    vp: mat4x4<f32>, inv_vp: mat4x4<f32>,
    eye: vec4<f32>, // xyz camera, w maximum length of a secondary ray
    sun: vec4<f32>, // xyz direction to light, w day fraction
    ambient: vec4<f32>, sun_color: vec4<f32>,
    air: vec4<f32>, air_color: vec4<f32>,
    water: vec4<f32>, water_color: vec4<f32>,
    terrain: vec4<f32>, // width, height, spacing, mask grid
    extent: vec4<u32>, // ray image (xy), target (zw)
    options: vec4<u32>, // flags, bounces, spp, terrain area table
    waves: vec4<u32>, // first/next texture addresses, blend, phase angle
    sky0: vec4<u32>, sky1: vec4<u32>, // panorama/roof tiles, fade in sky0.w
    sky_tint: vec4<f32>, sky_uv: vec4<f32>,
    sun_disc: vec4<f32>, moon_disc: vec4<f32>,
    sampling: vec4<u32>, // transport sequence (x); camera rays keep fixed strata
    material_time: vec4<f32>, // terrain night blend, grass fade, static light night factor, reserved
    grass_history: array<vec4<f32>,80>,
    shore_offsets: vec4<f32>,
    resident_bounds: vec4<f32>,
    water_effects: vec4<u32>,
    sky_cdf: vec4<u32>,
    previous_screen_vp: mat4x4<f32>,
    ray_effects: vec4<f32>,
}
@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage,read> world: array<u32>;
@group(0) @binding(2) var<storage,read> moving: array<u32>;
@group(0) @binding(3) var<storage,read> texels: array<u32>;
@group(0) @binding(4) var<storage,read_write> image: array<Pixel>;
var<private> dependencies: u32;
var<private> motion_dependent: bool;
var<private> sun_cache_read: u32;
var<private> sun_cache_write: u32;

const NIL: u32 = 0xffffffffu;
const DYNAMIC: u32 = 0x800000u;
const TERRAIN: u32 = 1u;
const BAKED: u32 = 4u;
const CUTOUT: u32 = 8u;
const BLEND: u32 = 16u;
const EMISSIVE: u32 = 32u;
const ADDITIVE: u32 = 64u;
const WATER: u32 = 128u;
const CLAMP_UV: u32 = 256u;
const NO_REFLECT: u32 = 512u;
const NO_REFRACT: u32 = 1024u;
const SIMPLE_WATER: u32 = 2048u;
const CULL_BACK: u32 = 8192u;
const WRITE_DEPTH: u32 = 16384u;
const IGNORE_TEXTURE_ALPHA: u32 = 32768u;
const STRICT_ALPHA: u32 = 65536u;
const HIDE_PRIMARY: u32 = 131072u;
const SINGLE_SIDED: u32 = 262144u;
const GRASS_MATERIAL: u32 = 524288u;
const UV_TRANSFORM: u32 = 1048576u;
const BAKED_COLOR_ONLY: u32 = 2097152u;
const FOLIAGE: u32 = 4194304u;
const PRIMARY_ONLY: u32 = 8388608u;
const TREE_MATERIAL: u32 = 16777216u;
const SHORE: u32 = 33554432u;
const OVERLAY: u32 = 67108864u;
const CLOUD: u32 = 134217728u;
const PRECIPITATION: u32 = 268435456u;
const ORDERED_GFX: u32 = 536870912u;
const GLASS: u32 = 1073741824u;
const SOLID_MASK: u32 = 1u;
const BLEND_MASK: u32 = 2u;
const BLOCKER_MASK: u32 = 4u;
const GFX_MASK: u32 = 8u;
const SECONDARY_MASK: u32 = 16u;
const TRANSMISSIVE_MASK: u32 = 32u;
const GFX_OVERLAY_MASK: u32 = 64u;
const GFX_MASKS: u32 = GFX_MASK | GFX_OVERLAY_MASK;
const LIGHTING: u32 = 1u;
const SHADOWS: u32 = 2u;
const REFLECTIONS: u32 = 4u;
const RAY_LIGHTING_OVERLAY: u32 = 512u;
const WATER_MIRROR: u32 = 8u;
const CULL_BUILDINGS: u32 = 16u;
const UNDERWATER: u32 = 32u;
const SOFT_SUN: u32 = 64u;
const ANISO_WIDE: u32 = 128u;
const ANISO_ISOTROPIC: u32 = 256u;
const BAKED_LIGHTMAPS: u32 = 512u;
const GRASS_AREA_LIGHTMAPS: u32 = 1024u;
const RELIGHT_BAKED: u32 = 2048u;
const CLOUD_DETAIL: u32 = 4096u;
const PHYSICAL_BSDF: u32 = 8192u;
const REFERENCE_MODE: u32 = 16384u;
const SINGLE_PATH: u32 = 32768u;
const VIEW: u32 = 0u;
const SHADOW: u32 = 1u;
const WATER_REFLECT: u32 = 2u;
const WATER_TRANSMIT: u32 = 3u;
const DIFFUSE: u32 = 4u;
const SPECULAR: u32 = 5u;

// The instance index (including the dynamic bit) maps directly from Vulkan's
// 24-bit instanceCustomIndex. Primitive IDs refer to the packed index array.
struct Hit { t: f32, instance: u32, primitive: u32, bary: vec2<f32> }
struct Ray {
    origin: vec3<f32>, dir: vec3<f32>, low: f32, high: f32,
    width: f32, spread: f32, coverage: f32, pixel_seed: u32,
}
struct Surface {
    n: vec3<f32>, face: vec3<f32>, // shading and incident-facing geometric normals
    uv: vec2<f32>, color: vec4<f32>, flags: u32, reflectance: f32, power: f32,
    specular: vec3<f32>, ambient: vec3<f32>, emission: vec3<f32>, glaze: vec4<f32>, blend: u32,
    glass: vec4<f32>, pbr: vec2<f32>, entering: bool, body: u32,
}
// Order is nonzero only while enumerating the narrow distance band of
// coplanar sheets. A cursor excludes exactly the layers already composited,
// not all geometry at that depth. NIL sorts after all real keys.
struct Query { purpose: u32, mask: u32, ordered: bool, before: vec2<u32> }
fn solid_mask(purpose: u32) -> u32 { return select(SECONDARY_MASK,SOLID_MASK,purpose==VIEW); }
struct Path { ray: Ray, weight: vec3<f32>, depth: u32, purpose: u32, medium: u32, previous: vec4<f32> }

fn word(dynamic: bool, i: u32) -> u32 {
    if dynamic { return moving[i]; }
    return world[i];
}
fn floats(dynamic: bool, i: u32) -> vec3<f32> {
    return bitcast<vec3<f32>>(vec3<u32>(word(dynamic,i),word(dynamic,i+1u),word(dynamic,i+2u)));
}
fn inst_addr(instance: u32) -> u32 {
    return word((instance & DYNAMIC) != 0u, 4u) + (instance & (DYNAMIC-1u)) * 14u;
}
fn vertex_addresses(dynamic: bool, instance_addr: u32, primitive: u32) -> vec3<u32> {
    let first = word(dynamic,0u) + word(dynamic,instance_addr+6u)*8u;
    let t = word(dynamic,1u) + primitive*3u;
    return first + 8u*vec3<u32>(word(dynamic,t),word(dynamic,t+1u),word(dynamic,t+2u));
}
fn rotate(v: vec3<f32>, c: f32, s: f32) -> vec3<f32> {
    return vec3<f32>(c*v.x-s*v.z, v.y, s*v.x+c*v.z);
}
// Usual scale/yaw instances do not pay for a full matrix in their record.
// A zero scale points to forward and inverse column-major affine bases.
fn instance_basis(m: bool, at: u32) -> mat3x3<f32> {
    return mat3x3<f32>(floats(m,at),floats(m,at+3u),floats(m,at+6u));
}
fn instance_vector(m: bool, inst: u32, v: vec3<f32>) -> vec3<f32> {
    let scale = bitcast<f32>(word(m,inst+3u));
    let at = word(m,inst+4u);
    if scale==0. { return instance_basis(m,at)*v; }
    return rotate(v,bitcast<f32>(at),bitcast<f32>(word(m,inst+5u)))*scale;
}
fn norm(v: vec3<f32>) -> vec3<f32> {
    return v * inverseSqrt(max(dot(v,v), 1.e-20));
}
fn strongest(v: vec3<f32>) -> f32 { return max(v.x,max(v.y,v.z)); }
fn rgba(x: u32) -> vec4<f32> {
    return unpack4x8unorm(x);
}
fn linear(c: vec3<f32>) -> vec3<f32> {
    return select(c/12.92, pow((max(c,vec3<f32>(0.))+0.055)/1.055, vec3<f32>(2.4)), c>vec3<f32>(0.04045));
}
fn encoded(c: vec3<f32>) -> vec3<f32> {
    return select(c*12.92,1.055*pow(max(c,vec3<f32>(0.)),vec3<f32>(1./2.4))-0.055,c>vec3<f32>(0.0031308));
}
const AREA_MIPS: u32 = 1u;
const AREA_TOP: u32 = 2u;
const LINEAR_DATA: u32 = 4u;
const ALPHA_IS_DATA: u32 = 8u;
const PACKED_MASK: u32 = 16u;
const NORMAL_RG: u32 = 128u;

fn linear_texels(c: vec4<f32>) -> vec4<f32> {
    return select(c/12.92,pow((c+0.055)/1.055,vec4<f32>(2.4)),c>vec4<f32>(0.04045));
}
fn sample_word(word_value: u32, fmt: u32) -> vec4<f32> {
    let c = rgba(word_value);
    if (fmt & LINEAR_DATA)!=0u { return c; }
    return vec4<f32>(linear(c.rgb)*select(c.a,1.,(fmt & ALPHA_IS_DATA)!=0u),c.a);
}
struct QuadTap { index: vec4<u32>, weight: vec4<f32> }
fn bilinear(base: u32, dims: vec2<i32>, uv: vec2<f32>, edge: bool) -> QuadTap {
    let xy = select(fract(uv),clamp(uv,vec2<f32>(0.),vec2<f32>(1.)),edge)*vec2<f32>(dims)-0.5;
    let p = vec2<i32>(floor(xy));
    let q = p+1;
    let f = fract(xy);
    // With the UV normalized above, the only wrapped addresses are -1
    // and dims. Compute the four weights and addresses together, without
    // four copies of the addressing/format branches at every sampler site.
    let a = select(select(p,dims-1,p<vec2<i32>(0)),max(p,vec2<i32>(0)),edge);
    let b = select(select(q,vec2<i32>(0),q>=dims),min(q,dims-1),edge);
    let row = vec2<u32>(vec2<i32>(a.y,b.y))*u32(dims.x)+base;
    return QuadTap(row.xxyy+vec2<u32>(u32(a.x),u32(b.x)).xyxy,
        vec4<f32>((1.-f.x)*(1.-f.y),f.x*(1.-f.y),(1.-f.x)*f.y,f.x*f.y));
}
fn rgba_mip_dims(addr: u32, mip: u32) -> vec2<i32> {
    return vec2<i32>(max(vec2<u32>(1u),vec2<u32>(texels[addr],texels[addr+1u])>>vec2<u32>(mip)));
}
fn buffer_level(addr: u32, uv: vec2<f32>, edge: bool, mip: u32, fmt: u32) -> vec4<f32> {
    let base = addr + texels[addr+4u+mip];
    let dims = rgba_mip_dims(addr,mip);
    if all(dims==vec2<i32>(1)) { return sample_word(texels[base],fmt); }
    let tap = bilinear(base,dims,uv,edge);
    let c0 = rgba(texels[tap.index.x]);
    let c1 = rgba(texels[tap.index.y]);
    let c2 = rgba(texels[tap.index.z]);
    let c3 = rgba(texels[tap.index.w]);
    let weights = tap.weight;
    let alpha = vec4<f32>(c0.a,c1.a,c2.a,c3.a);
    var r = vec4<f32>(c0.r,c1.r,c2.r,c3.r);
    var g = vec4<f32>(c0.g,c1.g,c2.g,c3.g);
    var b_chan = vec4<f32>(c0.b,c1.b,c2.b,c3.b);
    var rgb_weights = weights;
    if (fmt & LINEAR_DATA)==0u {
        r = linear_texels(r);
        g = linear_texels(g);
        b_chan = linear_texels(b_chan);
        if (fmt & ALPHA_IS_DATA)==0u { rgb_weights *= alpha; }
    }
    return vec4<f32>(dot(r,rgb_weights),dot(g,rgb_weights),dot(b_chan,rgb_weights),dot(alpha,weights));
}
fn unassociate(c: vec4<f32>, fmt: u32) -> vec4<f32> {
    if (fmt & (LINEAR_DATA|ALPHA_IS_DATA))!=0u { return c; }
    return vec4<f32>(c.rgb/max(c.a,1.e-7),c.a);
}
fn footprint(ray: Ray, t: f32) -> f32 { return ray.width+ray.spread*t; }
struct Taps { step: vec2<f32>, lod: f32, count: u32 }
fn one_tap() -> Taps { return Taps(vec2<f32>(0.),0.,1u); }

// Project the round ray cone onto the hit plane and through the UV gradients.
// The 2x2 Gram matrix in texel space gives major/minor footprint axes. Use
// minor-axis LOD and integrate along the major axis, clamping anisotropy by
// widening the minor filter rather than dropping coverage at grazing angles.
struct UvFootprint { u: vec3<f32>, v: vec3<f32> }
fn project_footprint(dir: vec3<f32>, n: vec3<f32>, gu: vec3<f32>, gv: vec3<f32>, width: f32) -> UvFootprint {
    let nd = dot(n,dir);
    let inv = select(-1.,1.,nd>=0.)/max(abs(nd),0.002);
    return UvFootprint((gu-n*(dot(gu,dir)*inv))*width,(gv-n*(dot(gv,dir)*inv))*width);
}
fn uv_taps(addr: u32, ellipse: UvFootprint) -> Taps {
    let dims = vec2<f32>(f32(texels[addr]),f32(texels[addr+1u]));
    if all(dims==vec2<f32>(1.)) { return one_tap(); }
    let u = ellipse.u*dims.x;
    let v = ellipse.v*dims.y;
    let uu = dot(u,u);
    let vv = dot(v,v);
    let uv = dot(u,v);
    let major2 = 0.5*(uu+vv+length(vec2<f32>(uu-vv,2.*uv)));
    if !(major2>1.) { return one_tap(); }
    let minor = length(cross(u,v))*inverseSqrt(major2);
    let major = sqrt(major2);
    var cap = select(8.,16.,(frame.options.x & ANISO_WIDE)!=0u);
    if (frame.options.x & ANISO_ISOTROPIC)!=0u { cap = 1.; }
    let across = max(1.,max(minor,major/cap));
    let count = u32(clamp(ceil(major/across),1.,cap));
    let lod = clamp(log2(across),0.,f32(texels[addr+2u]-1u));
    var axis = vec2<f32>(0.);
    if count>1u {
        // Use the better-conditioned of the two expressions for the major
        // eigenvector. Either can collapse to zero for an axis-aligned ellipse.
        let p = vec2<f32>(major2-vv,uv);
        let q = vec2<f32>(uv,major2-uu);
        axis = normalize(select(p,q,dot(q,q)>dot(p,p)));
    }
    return Taps(axis*(major/f32(count))/dims,lod,count);
}
fn footprint_taps(addr: u32, dir: vec3<f32>, n: vec3<f32>, gu: vec3<f32>, gv: vec3<f32>, width: f32) -> Taps {
    if texels[addr]==1u && texels[addr+1u]==1u { return one_tap(); }
    return uv_taps(addr,project_footprint(dir,n,gu,gv,width));
}
fn mesh_footprint(ray: Ray, hit: Hit, m: bool, inst: u32, v: vec3<u32>, uv_scale: vec2<f32>) -> UvFootprint {
    let a = floats(m,v.x);
    let e = instance_vector(m,inst,floats(m,v.y)-a);
    let f = instance_vector(m,inst,floats(m,v.z)-a);
    let n = cross(e,f);
    let n2 = dot(n,n);
    if n2<1.e-20 { return UvFootprint(vec3<f32>(0.),vec3<f32>(0.)); }
    let b = vertex_uv(m,v.x);
    let du = vertex_uv(m,v.y)-b;
    let dv = vertex_uv(m,v.z)-b;
    let g0 = cross(f,n)/n2;
    let g1 = cross(n,e)/n2;
    return project_footprint(ray.dir,n*inverseSqrt(n2),(du.x*g0+dv.x*g1)*uv_scale.x,(du.y*g0+dv.y*g1)*uv_scale.y,footprint(ray,hit.t));
}
fn mesh_taps(addr: u32, ray: Ray, hit: Hit, m: bool, inst: u32, v: vec3<u32>, scale: vec2<f32>) -> Taps {
    if texels[addr]==1u && texels[addr+1u]==1u { return one_tap(); }
    return uv_taps(addr,mesh_footprint(ray,hit,m,inst,v,scale));
}
fn tap_average(addr: u32, uv: vec2<f32>, edge: bool, taps: Taps, fmt: u32) -> vec4<f32> {
    let mip = u32(taps.lod);
    let part = fract(taps.lod);
    var sum = vec4<f32>(0.);
    for (var i=0u; i<taps.count; i++) {
        let p = uv+(f32(i)+0.5-0.5*f32(taps.count))*taps.step;
        let levels = select(1u,2u,part>0.);
        for (var l=0u; l<levels; l++) {
            sum += level(addr,p,edge,mip+l,fmt)*select(1.-part,part,l!=0u);
        }
    }
    return sum/f32(taps.count);
}
fn filtered_tile(addr: u32, uv: vec2<f32>, edge: bool, taps: Taps) -> vec4<f32> {
    let fmt = texels[addr+3u];
    return unassociate(tap_average(addr,uv,edge,taps,fmt),fmt);
}
fn alpha_level(addr: u32, uv: vec2<f32>, edge: bool, mip: u32) -> f32 {
    let base = addr+texels[addr+4u+mip];
    let dims = rgba_mip_dims(addr,mip);
    if all(dims==vec2<i32>(1)) { return f32(texels[base]>>24u)/255.; }
    let tap = bilinear(base,dims,uv,edge);
    let packed = vec4<u32>(texels[tap.index.x],texels[tap.index.y],texels[tap.index.z],texels[tap.index.w]);
    return dot(vec4<f32>(packed>>vec4<u32>(24u))/255.,tap.weight);
}
fn alpha_average(addr: u32, uv: vec2<f32>, edge: bool, taps: Taps,
                 threshold_top: bool, cutoff: f32, flags: u32) -> f32 {
    let mip = u32(taps.lod);
    let part = fract(taps.lod);
    let levels = select(1u,2u,part>0.);
    var sum = 0.;
    for (var i=0u; i<taps.count; i++) {
        let p = uv+(f32(i)+0.5-0.5*f32(taps.count))*taps.step;
        for (var l=0u; l<levels; l++) {
            let j = mip+l;
            var a = alpha_level(addr,p,edge,j);
            if threshold_top && j==0u { a = select(0.,1.,alpha_test(a,cutoff,flags)); }
            sum += a*select(1.-part,part,l!=0u);
        }
    }
    return sum/f32(taps.count);
}
// Keep packed scalar fetches out of the general material sampler. Mip starts
// are word aligned, with four consecutive R8 weights per word.
fn mask_level(addr: u32, uv: vec2<f32>, mip: u32) -> f32 {
    let base = addr + texels[addr+4u+mip];
    let dims = max(vec2<u32>(1u),vec2<u32>(texels[addr],texels[addr+1u])>>vec2<u32>(mip));
    if all(dims==vec2<u32>(1u)) { return f32(texels[base]&255u)/255.; }
    let xy = clamp(uv,vec2<f32>(0.),vec2<f32>(1.))*vec2<f32>(dims)-0.5;
    let lo = vec2<i32>(floor(xy));
    let a = vec2<u32>(max(lo,vec2<i32>(0)));
    let b = vec2<u32>(min(lo+1,vec2<i32>(dims)-1));
    let i = vec2<u32>(a.y,b.y).xxyy*dims.x+vec2<u32>(a.x,b.x).xyxy;
    let at = i/vec4<u32>(4u)+base;
    let words = vec4<u32>(texels[at.x],texels[at.y],texels[at.z],texels[at.w]);
    let r = vec4<f32>((words>>((i&vec4<u32>(3u))*8u))&vec4<u32>(255u))/255.;
    let f = fract(xy);
    return dot(r,vec4<f32>((1.-f.x)*(1.-f.y),f.x*(1.-f.y),(1.-f.x)*f.y,f.x*f.y));
}
fn terrain_mask(addr: u32, uv: vec2<f32>, taps: Taps) -> f32 {
    if (texels[addr+3u] & PACKED_MASK)==0u { return filtered_tile(addr,uv,true,taps).r; }
    let mip = u32(taps.lod);
    let part = fract(taps.lod);
    var sum = 0.;
    for (var i=0u; i<taps.count; i++) {
        let p = uv+(f32(i)+0.5-0.5*f32(taps.count))*taps.step;
        let levels = select(1u,2u,part>0.);
        for (var l=0u; l<levels; l++) {
            sum += mask_level(addr,p,mip+l)*select(1.-part,part,l!=0u);
        }
    }
    return sum/f32(taps.count);
}
fn terrain_coord(xz: vec2<f32>) -> vec2<f32> {
    return clamp(vec2<f32>(xz.x,-xz.y)/frame.terrain.z + frame.terrain.xy*0.5,
                 vec2<f32>(0.),frame.terrain.xy-vec2<f32>(1.))/frame.terrain.w;
}
fn terrain_area(area: vec2<f32>) -> u32 {
    let nx = u32(ceil(frame.terrain.x/frame.terrain.w));
    return frame.options.w + 4u*(u32(area.y)*nx+u32(area.x));
}
fn bake_enabled() -> bool {
    return (frame.options.x & (BAKED_LIGHTMAPS|RELIGHT_BAKED))==BAKED_LIGHTMAPS && frame.terrain.w>0.;
}
fn bake_light(area: u32, uv: vec2<f32>, night: f32, grass: bool) -> vec4<f32> {
    let day_addr = texels[area+2u];
    let night_addr = texels[area+3u];
    if day_addr==0u || (grass && day_addr==night_addr) { return vec4<f32>(1.,1.,1.,0.); }
    // This is a legacy multiplier in encoded RGB, not albedo. A day-only
    // fallback copies the day address, but never claims to be a grass pair.
    var rgb = vec3<f32>(0.);
    let f = select(clamp(night,0.,1.),0.,day_addr==night_addr);
    let first = select(0u,1u,f==1.);
    let end = select(2u,1u,f==0.);
    for (var i=first; i<end; i++) {
        let day = i==0u;
        rgb += select(f,1.-f,day)*level(select(night_addr,day_addr,day),uv,true,0u,LINEAR_DATA).rgb;
    }
    return vec4<f32>(2.*rgb,1.);
}
fn terrain_color(p: vec3<f32>, face: vec3<f32>, ray: Ray, t: f32, baked: vec4<f32>) -> vec3<f32> {
    let coord = terrain_coord(p.xz);
    let area = terrain_area(floor(coord));
    let start = texels[area];
    let count = texels[area+1u];
    let width = footprint(ray,t);
    let fallback = vec3<f32>(0.30,0.33,0.24);
    var rgb = vec3<f32>(0.);
    if count==0u { rgb = select(linear(fallback),fallback,baked.a>0.); }
    var left = 1.;
    let s = 1./(frame.terrain.z*frame.terrain.w);
    let mask_uv = project_footprint(ray.dir,face,vec3<f32>(s,0.,0.),vec3<f32>(0.,0.,-s),width);
    // The front-to-back form of the same stack of lerps. A transparent
    // layer costs only its R8 mask; a fully covering layer hides all below.
    for (var j=count; j>0u && left>0.; j--) {
        let at = start+(j-1u)*5u;
        var weight = 1.;
        if j>1u {
            let mask = texels[at+1u];
            weight = terrain_mask(mask,fract(coord),uv_taps(mask,mask_uv));
            if weight<=0. { continue; }
        }
        var gu = vec3<f32>(1.,0.,0.);
        var gv = vec3<f32>(0.,0.,-1.);
        switch texels[at+2u] {
            case 0u: { gu = vec3<f32>(0.,0.,1.); gv = vec3<f32>(0.,-1.,0.); }
            case 2u: { gv = vec3<f32>(0.,-1.,0.); }
            default: {}
        }
        let scale = bitcast<vec2<f32>>(vec2<u32>(texels[at+3u],texels[at+4u]));
        gu *= scale.x;
        gv *= scale.y;
        let addr = texels[at];
        let c = terrain_tile(addr,vec2<f32>(dot(gu,p),dot(gv,p)),
            footprint_taps(addr,ray.dir,face,gu,gv,width));
        rgb += (left*weight)*select(c,encoded(c),baked.a>0.);
        left *= 1.-weight;
    }
    if baked.a>0. { rgb = linear(min(vec3<f32>(1.), rgb*baked.rgb)); }
    return rgb;
}
fn vertex_uv(m: bool, v: u32) -> vec2<f32> {
    return bitcast<vec2<f32>>(vec2<u32>(word(m,v+4u),word(m,v+5u)));
}
fn vertex_tint(m: bool, v: u32) -> vec4<f32> {
    return mix(rgba(word(m,v+7u)),rgba(word(m,v+6u)),frame.sun.w);
}
fn material_addr(m: bool, primitive: u32) -> u32 {
    // Same word order as scene::Surface.
    return word(m,3u) + 27u*word(m,word(m,2u)+2u*primitive);
}
fn layer_key(hit: Hit) -> vec2<u32> {
    let m = (hit.instance & DYNAMIC)!=0u;
    return vec2<u32>(hit.instance,word(m,word(m,2u)+2u*hit.primitive+1u));
}
fn key_less(a: vec2<u32>, b: vec2<u32>) -> bool {
    return a.x<b.x || (a.x==b.x && a.y<b.y);
}
fn closer(hit: Hit, best: Hit, q: Query) -> bool {
    if q.ordered {
        let key = layer_key(hit);
        // GFX follows the native forward draw order (including write-Z).
        // Other blend sheets reverse only the current coplanar band.
        let forward = (q.mask & GFX_MASKS)!=0u;
        if !select(key_less(key,q.before),key_less(q.before,key),forward) { return false; }
        if best.instance==NIL { return true; }
        let other = layer_key(best);
        return select(key_less(other,key),key_less(key,other),forward)
            || (all(key==other) && (hit.t<best.t || (hit.t==best.t && hit.primitive<best.primitive)));
    }
    return hit.t<best.t || (hit.t==best.t && (hit.instance<best.instance ||
           (hit.instance==best.instance && hit.primitive<best.primitive)));
}
fn weights_of(hit: Hit) -> vec3<f32> {
    return vec3<f32>(1.-hit.bary.x-hit.bary.y,hit.bary);
}
fn words2(m: bool, at: u32) -> vec2<f32> {
    return bitcast<vec2<f32>>(vec2<u32>(word(m,at),word(m,at+1u)));
}
fn surface_uv(m: bool, v: vec3<u32>, w: vec3<f32>, mat: u32, flags: u32) -> vec2<f32> {
    var uv = vertex_uv(m,v.x)*w.x + vertex_uv(m,v.y)*w.y + vertex_uv(m,v.z)*w.z;
    if (flags & UV_TRANSFORM)!=0u {
        let scroll = words2(m,mat+13u);
        if any(scroll!=vec2<f32>(0.)) { dependencies |= ANIM_SCROLL; }
        uv = uv * words2(m,mat+11u) + scroll * frame.air.w;
    }
    return uv;
}
fn tint_of(m: bool, v: vec3<u32>, w: vec3<f32>) -> vec4<f32> {
    return vertex_tint(m,v.x)*w.x + vertex_tint(m,v.y)*w.y + vertex_tint(m,v.z)*w.z;
}
fn geometric(m: bool, inst: u32, v: vec3<u32>) -> vec3<f32> {
    let a = floats(m,v.x);
    let e = floats(m,v.y)-a;
    let f = floats(m,v.z)-a;
    let at = word(m,inst+4u);
    if word(m,inst+3u)==0u {
        let basis = instance_basis(m,at);
        return cross(basis*e,basis*f);
    }
    return rotate(cross(e,f),bitcast<f32>(at),bitcast<f32>(word(m,inst+5u)));
}
fn alpha_test(a: f32, reference: f32, flags: u32) -> bool {
    // Allow for the one-ULP difference between CPU normalization of a byte
    // reference and GPU unpack/bilinear interpolation of the same byte.
    let strict = (flags & STRICT_ALPHA)!=0u;
    return a-reference>=select(-0.00000012,0.00000012,strict);
}
fn hash(seed: u32) -> u32 {
    let s = seed*747796405u + 2891336453u;
    let h = ((s >> ((s >> 28u)+4u)) ^ s)*277803737u;
    return (h >> 22u) ^ h;
}
fn coverage_sample(ray: Ray, m: bool, inst: u32, v: vec3<u32>) -> f32 {
    // Scramble distinct masked planes, not individual triangles. Otherwise
    // two 50%-covered leaves both reveal the *same* 50% of the background.
    // Quantize plane parameters rather than hit distance, which moves with
    // the camera and can differ for the two halves of a quad.
    var n = norm(geometric(m,inst,v));
    let a = abs(n);
    var axis = select(1u,0u,a.x>=a.y);
    if a.z>a[axis] { axis = 2u; }
    n *= select(1.,-1.,n[axis]<0.);
    let p = floats(m,inst) + instance_vector(m,inst,floats(m,v.x));
    let key = bitcast<u32>(i32(round(dot(n,p)*64.)));
    let shape = bitcast<vec3<u32>>(vec3<i32>(round(n*16.)));
    let salt = hash(key ^ (shape.x*0x9e3779b9u) ^ (shape.y*0x85ebca6bu) ^ (shape.z*0xc2b2ae35u) ^ ray.pixel_seed);
    // Rotate the strata independently per pixel and plane, not per frame.
    // A fixed plane-only rotation would correlate every pixel at low SPP.
    return fract(ray.coverage + f32(salt & 0xffffffu)/16777216.);
}
fn tree_lod_visible(m: bool, inst: u32, pixel: u32) -> bool {
    let side = word(m,inst+12u);
    if side==0u { return true; }
    // Same complementary four-by-four thresholds as native tree LODs.
    let x = (pixel-1u)%frame.extent.x%4u;
    let y = (pixel-1u)/frame.extent.x%4u;
    let b = (((x&1u)^(y&1u))*8u) + (y&1u)*4u + (((x>>1u)^(y>>1u))*2u) + (y>>1u);
    let d = (f32(b)+0.5)/16.;
    let has_far = d<bitcast<f32>(word(m,inst+11u));
    return has_far==(side==2u);
}
fn grass_fade(m: bool, inst: u32, v: vec3<u32>, w: vec3<f32>) -> f32 {
    let sight = bitcast<f32>(word(m,v.x+6u));
    if !(sight>0. && sight<10000.) { return 1.; }
    let slot = word(m,inst+13u);
    var wind = vec3<f32>(0.);
    if slot<80u { wind = frame.grass_history[slot].xyz; }
    var fade = 0.;
    let from_eye = floats(m,inst)-frame.eye.xyz;
    // Native calculates the fade at *unbent* vertices and interpolates it.
    for (var j=0u; j<3u; j++) {
        let vertex = v[j];
        let weight = bitcast<f32>(word(m,vertex+7u));
        var d = from_eye + instance_vector(m,inst,floats(m,vertex));
        if weight>=0. && weight<100. { d -= wind*weight; }
        fade += w[j]*max(1.-dot(d,d)/(sight*sight),0.);
    }
    return fade;
}
// Called before committing the hit. No albedo, lighting, normals, or depth
// writes for a rejected candidate; an alpha hole never advances ray.low.
fn accepts(hit: Hit, ray: Ray, q: Query) -> bool {
    let m = (hit.instance & DYNAMIC)!=0u;
    let mat = material_addr(m,hit.primitive);
    let flags = word(m,mat+1u);
    let blend = (flags & BLEND)!=0u;
    let gfx = (flags & ORDERED_GFX)!=0u;
    if ((q.mask & GFX_MASKS)!=0u && (!blend || !gfx || ((flags & OVERLAY)!=0u && (q.mask & GFX_OVERLAY_MASK)==0u)
            || ((flags & OVERLAY)==0u && (q.mask & GFX_MASK)==0u)))
        || ((q.mask & (SOLID_MASK|SECONDARY_MASK))!=0u && blend) || (q.mask==BLEND_MASK && (!blend || gfx))
        || ((flags & CLOUD)!=0u && (frame.options.x & CLOUD_DETAIL)==0u)
        || (q.purpose==VIEW && (flags & HIDE_PRIMARY)!=0u)
        || (q.purpose!=VIEW && (flags & PRIMARY_ONLY)!=0u)
        || (q.purpose==SHADOW && (flags & (BLEND|WATER))!=0u && (flags & (WRITE_DEPTH|CLOUD))==0u)
        || (q.purpose==WATER_REFLECT && (flags & NO_REFLECT)!=0u)
        || (q.purpose==WATER_TRANSMIT && (flags & NO_REFRACT)!=0u) { return false; }
    if q.purpose==SHADOW {
        let thin = (flags & (FOLIAGE|CLOUD))!=0u
            || ((flags & GLASS)!=0u && (word(m,mat+23u) & 0x80000000u)==0u);
        if thin!=(q.mask==TRANSMISSIVE_MASK) { return false; }
    }
    let cull = (flags & SINGLE_SIDED)!=0u ||
        (q.purpose==VIEW && (frame.options.x & CULL_BUILDINGS)!=0u && (flags & CULL_BACK)!=0u);
    if (flags & SHORE)!=0u {
        let xz = (ray.origin+ray.dir*hit.t).xz;
        let bounds = frame.resident_bounds;
        if any(xz<bounds.xz) || any(xz>=bounds.yw) { return false; }
    }
    let inst = inst_addr(hit.instance);
    if (flags & TREE_MATERIAL)!=0u && !tree_lod_visible(m,inst,ray.pixel_seed) { return false; }
    if !cull && (flags & (CUTOUT|BLEND|WRITE_DEPTH))==0u { return true; }
    let v = vertex_addresses(m,inst,hit.primitive);
    if cull && dot(geometric(m,inst,v),ray.dir)>=0. { return false; }
    if (flags & (CUTOUT|BLEND|WRITE_DEPTH))==0u { return true; }
    let w = weights_of(hit);
    var a = 1.;
    if (flags & IGNORE_TEXTURE_ALPHA)==0u {
        let addr = word(m,mat);
        let uv = surface_uv(m,v,w,mat,flags);
        let edge = (flags & CLAMP_UV)!=0u;
        let tile_flags = texels[addr+3u];
        let cutout = (flags & CUTOUT)!=0u;
        let area = cutout && (tile_flags & AREA_MIPS)!=0u;
        let native_cut = bitcast<f32>(word(m,mat+2u));
        var cut = native_cut;
        if (flags & GRASS_MATERIAL)!=0u && q.purpose==VIEW && frame.material_time.y>0.5 {
            let fade = grass_fade(m,inst,v,w);
            if fade<=native_cut { return false; }
            cut = native_cut/fade;
        }
        let taps = mesh_taps(addr,ray,hit,m,inst,v,words2(m,mat+11u));
        // No RGB unpack, weighting or transfer function in the ray-query loop.
        a = alpha_average(addr,uv,edge,taps,area && (tile_flags & AREA_TOP)==0u,cut,flags);
        if area {
            // Fixed per-pixel/per-sample stratification, never seeded by time.
            if a<=0. || (a<1. && a<=coverage_sample(ray,m,inst,v)) { return false; }
            a = 1.;
        } else if cutout && !alpha_test(a,cut,flags) { return false; }
    }
    if blend || (q.purpose==SHADOW && (flags & WRITE_DEPTH)!=0u) {
        a *= tint_of(m,v,w).a;
        if blend && a<=0. && word(m,mat+21u)==0u { return false; }
        if q.purpose==SHADOW && a<0.5 { return false; }
    }
    return true;
}
fn tangent_normal(m: bool, inst: u32, v: vec3<u32>, scale: vec2<f32>, n: vec3<f32>, sample: vec3<f32>) -> vec3<f32> {
    let p = floats(m,v.x);
    let e = instance_vector(m,inst,floats(m,v.y)-p);
    let f = instance_vector(m,inst,floats(m,v.z)-p);
    let uv = vertex_uv(m,v.x);
    let du = (vertex_uv(m,v.y)-uv)*scale;
    let dv = (vertex_uv(m,v.z)-uv)*scale;
    let det = du.x*dv.y-du.y*dv.x;
    if abs(det)<1.e-10 { return n; }
    let handed = sign(det);
    let raw = handed*(e*dv.y-f*du.y);
    let tangent = norm(raw-n*dot(n,raw));
    let bitangent = cross(n,tangent);
    let orientation = select(-1.,1.,dot(bitangent,handed*(f*du.x-e*dv.x))>=0.);
    return norm(tangent*sample.x+bitangent*(orientation*sample.y)+n*sample.z);
}
fn shore_vertex(m: bool, v: u32) -> vec4<f32> {
    let day = rgba(word(m,v+6u));
    let night = rgba(word(m,v+7u));
    let n = floor(clamp(1.-frame.sun.w,0.,1.)*255.);
    let color = floor((day.rgb*(255.-n)+night.rgb*n)*(255./256.))/255.;
    // Shore mesh vertices are already in world space. Fade in the vertex
    // stage's xz plane (not at the intersection or in 3D).
    let d = floats(m,v).xz-frame.eye.xz;
    return vec4<f32>(color,day.a*max(1.-dot(d,d)/(400.*400.),0.));
}
fn shore_art(addr: u32, uv: vec2<f32>, taps: Taps, tint: vec4<f32>) -> vec4<f32> {
    dependencies |= ANIM_WAVES;
    // Recovered encoded texture-stage combiner. Authored shore strips are
    // overlaid; they aren't water boundaries for reflected/transmitted rays.
    var rgba_encoded = vec4<f32>(0.);
    for (var stage=0u; stage<3u; stage++) {
        let scale = select(1.,0.5,stage==0u);
        let shift = vec2<f32>(select(0.,0.5,stage==1u),frame.shore_offsets[2u-stage]);
        let t = filtered_tile(addr,uv*scale+shift,false,
            Taps(taps.step*scale,max(0.,taps.lod+log2(scale)),taps.count));
        let c = vec4<f32>(encoded(t.rgb),t.a);
        rgba_encoded = select(mix(rgba_encoded,c,c.a),c*vec4<f32>(1.,1.,1.,0.5),stage==0u);
    }
    return vec4<f32>(linear(min(tint.rgb*rgba_encoded.rgb*2.,vec3<f32>(1.))),
                     tint.a*rgba_encoded.a*0.6);
}
fn simple_water_art(uv: vec2<f32>, taps: Taps, angle: f32, addr: u32) -> vec4<f32> {
    dependencies |= ANIM_WAVES;
    let t = filtered_tile(addr,uv*2.5+0.0125*vec2<f32>(cos(angle),sin(angle)),false,taps);
    let color = mix(vec3<f32>(37.,79.,85.)/255.,encoded(t.rgb),t.a);
    return vec4<f32>(linear(color),min(t.a+160./255.,1.));
}
fn surface(hit: Hit, ray: Ray) -> Surface {
    let m = (hit.instance & DYNAMIC)!=0u;
    let inst = inst_addr(hit.instance);
    let v = vertex_addresses(m,inst,hit.primitive);
    let mat = material_addr(m,hit.primitive);
    var flags = word(m,mat+1u);
    let p = ray.origin + ray.dir*hit.t;
    let w = weights_of(hit);
    let uv = surface_uv(m,v,w,mat,flags);
    let raw = rgba(word(m,v.x+3u)).xyz*w.x + rgba(word(m,v.y+3u)).xyz*w.y + rgba(word(m,v.z+3u)).xyz*w.z;
    let gn = norm(geometric(m,inst,v));
    let face = select(gn,-gn,dot(gn,ray.dir)>0.);
    // Match the native skin stage: transform posed normals as vectors, then
    // align to the actual affine triangle plane on back/grazing faces.
    let at = word(m,inst+4u);
    var n = raw*2.-1.;
    if word(m,inst+3u)==0u {
        if (flags & GRASS_MATERIAL)!=0u {
            // Native wind bends positions, not the authored normals. The
            // shear leaves the original scaled X basis intact.
            let x = norm(floats(m,at));
            n = norm(rotate(n,x.x,x.z));
        } else { n = norm(instance_basis(m,at)*n); }
    } else { n = norm(rotate(n,bitcast<f32>(at),bitcast<f32>(word(m,inst+5u)))); }
    if dot(n,face)<0. { n = -n; }
    // Bad/sufficiently grazing interpolated normals must not face away from
    // the incident ray. They never decide which side of the triangle we enter.
    if dot(n,face)<1.e-4 || dot(n,ray.dir)>=0. { n = face; }
    if (flags & (SIMPLE_WATER|SHORE))!=0u {
        var art = vec4<f32>(linear(tint_of(m,v,w).rgb),1.);
        if (flags & SHORE)!=0u {
            let addr = word(m,mat);
            let tint = shore_vertex(m,v.x)*w.x+shore_vertex(m,v.y)*w.y+shore_vertex(m,v.z)*w.z;
            art = shore_art(addr,uv,mesh_taps(addr,ray,hit,m,inst,v,vec2<f32>(1.)),tint);
        } else if frame.water_effects.x!=0u {
            let addr = frame.water_effects.x;
            art = simple_water_art(uv,mesh_taps(addr,ray,hit,m,inst,v,vec2<f32>(2.5)),bitcast<f32>(frame.waves.w),addr);
        }
        return Surface(n,face,uv,art,flags,0.,-1.,vec3<f32>(0.),vec3<f32>(0.),vec3<f32>(0.),vec4<f32>(0.),0u,vec4<f32>(0.),vec2<f32>(0.,-1.),false,word(m,mat+26u));
    }
    let terrain = (flags & TERRAIN)!=0u;
    let grass = (flags & GRASS_MATERIAL)!=0u;
    let use_bake = (terrain || grass) && bake_enabled();
    var baked = vec4<f32>(1.,1.,1.,0.);
    if use_bake {
        let coord = terrain_coord(p.xz);
        var selected = floor(coord);
        if grass && (frame.options.x & GRASS_AREA_LIGHTMAPS)!=0u {
            selected = floor(terrain_coord(words2(m,inst+11u)));
        }
        baked = bake_light(terrain_area(selected),coord-selected,select(frame.material_time.x,1.-frame.sun.w,grass),grass);
    }
    if terrain {
        let ground = terrain_color(p,face,ray,hit.t,baked);
        flags |= select(0u,BAKED,baked.a>0.);
        return Surface(n,face,uv,vec4<f32>(ground,1.),flags,0.,-1.,
                       vec3<f32>(1.),ground,vec3<f32>(0.),vec4<f32>(0.),0u,vec4<f32>(0.),vec2<f32>(0.,-1.),false,word(m,mat+26u));
    }
    let relight = (frame.options.x & RELIGHT_BAKED)!=0u;
    if relight { flags &= ~BAKED; }
    let scale = words2(m,mat+11u);
    let edge = (flags & CLAMP_UV)!=0u;
    var tex = vec4<f32>(1.);
    var hi = vec4<f32>(0.);
    var glaze = vec4<f32>(0.);
    // One filtering site for all four stages. Most surfaces only issue the
    // diffuse sample, with neither extra texture nor tangent-frame cost.
    let tiles = vec4<u32>(word(m,mat),select(word(m,mat+6u),0u,(flags & BAKED)!=0u),
                          word(m,mat+18u),word(m,mat+19u));
    let present = select(vec4<u32>(0u),vec4<u32>(1u,2u,4u,8u),tiles!=vec4<u32>(0u));
    var stages = present.x | present.y | present.z | present.w;
    var projected: UvFootprint;
    var ready = false;
    while stages!=0u {
        let stage = firstTrailingBit(stages);
        stages &= stages-1u;
        let addr = tiles[stage];
        var taps = one_tap();
        if (texels[addr]!=1u || texels[addr+1u]!=1u) &&
            (stage!=0u || !reference() || !reference_emitter(word(m,mat+1u),word(m,mat+15u),word(m,mat+19u),floats(m,mat+7u))) {
            if !ready {
                projected = mesh_footprint(ray,hit,m,inst,v,scale);
                ready = true;
            }
            taps = uv_taps(addr,projected);
        }
        let value = filtered_tile(addr,uv,edge,taps);
        switch stage {
            case 0u: { tex = value; }
            case 1u: {
                var sample = value.xyz*2.-1.;
                if (texels[addr+3u] & NORMAL_RG)!=0u {
                    sample.z = sqrt(max(0.,1.-dot(sample.xy,sample.xy)));
                }
                let bump = tangent_normal(m,inst,v,scale,n,sample);
                if dot(bump,face)>1.e-4 && dot(bump,ray.dir)<0. { n = bump; }
            }
            case 2u: { hi = value; }
            default: { glaze = value; }
        }
    }
    var tint = vec4<f32>(1.);
    if !grass { tint = tint_of(m,v,w); }
    let alpha = select(tex.a,1.,(flags & IGNORE_TEXTURE_ALPHA)!=0u)*tint.a;
    var albedo = tex.rgb;
    let mode = word(m,mat+15u);
    if mode!=0u {
        let tone = rgba(word(m,mat+16u)).rgb;
        let skin = rgba(word(m,mat+17u)).rgb;
        // Retain the fixed-function face/eye/body texture stages in their
        // authored (encoded) arithmetic, before Lighter's incident lighting.
        let base = encoded(tex.rgb);
        let feature = encoded(hi.rgb);
        switch mode {
            case 1u: { albedo = linear(base*tone); }
            case 2u: { albedo = linear(mix(base, base*tone, tex.a)); }
            case 3u: { albedo = linear(mix(base*tone,feature,hi.a)); }
            default: { albedo = linear(mix(base*skin,feature*tone,hi.a)); }
        }
    }
    var base = albedo*linear(tint.rgb);
    var color = base*rgba(word(m,mat+20u)).rgb;
    if (flags & EMISSIVE)!=0u { color = albedo*tint.rgb; }
    if (flags & BAKED)!=0u {
        let gain = select(2.,1.,(flags & BAKED_COLOR_ONLY)!=0u);
        color = linear(min(encoded(tex.rgb)*tint.rgb*gain,vec3<f32>(1.)));
    } else if relight && (word(m,mat+1u) & BAKED)!=0u {
        // Baked vertex light isn't a reflectance. BMDs use the actual material
        // diffuse coefficient and the texture, not the day/night vertex RGB.
        base = tex.rgb;
        color = base*rgba(word(m,mat+20u)).rgb;
    } else if grass && use_bake {
        // For an incomplete area pair native grass displays the unlit base.
        if baked.a>0. { color = linear(min(vec3<f32>(1.),encoded(color)*baked.rgb)); }
        flags |= BAKED;
    }
    var emission = vec3<f32>(0.);
    if (flags & BAKED)==0u { emission = floats(m,mat+7u)*base; }
    return Surface(n,face,uv,vec4<f32>(color,alpha),flags,bitcast<f32>(word(m,mat+3u)),bitcast<f32>(word(m,mat+4u)),
                   rgba(word(m,mat+5u)).rgb,base*rgba(word(m,mat+10u)).rgb,emission,glaze,word(m,mat+21u),
                   vec4<f32>(rgba(word(m,mat+22u)).rgb,bitcast<f32>(word(m,mat+23u))),
                   words2(m,mat+24u),dot(gn,ray.dir)<0.,word(m,mat+26u));
}
fn none(ray: Ray) -> Hit { return Hit(ray.high,NIL,0u,vec2<f32>(0.)); }
fn trace(ray: Ray, q: Query) -> Hit {
    if ray.low>ray.high || ((world[11u]|moving[11u]) & q.mask)==0u { return none(ray); }
    let hit = intersect(ray,q);
    if hit.instance!=NIL && (hit.instance & DYNAMIC)!=0u { motion_dependent = true; }
    return hit;
}
fn bias(p: vec3<f32>) -> f32 {
    return max(0.0005,strongest(abs(p))*4.77e-7);
}
fn offset_origin(p: vec3<f32>, d: vec3<f32>, face: vec3<f32>) -> vec3<f32> {
    // Offset across the actual surface, not along a grazing ray or a bump
    // normal. Don't also advance t-min and skip nearby geometry twice.
    return p + face*select(-bias(p),bias(p),dot(d,face)>=0.);
}
fn secondary(p: vec3<f32>, d: vec3<f32>, face: vec3<f32>, ray: Ray, t: f32) -> Ray {
    return Ray(offset_origin(p,d,face),d,0.,frame.eye.w,footprint(ray,t),ray.spread,ray.coverage,ray.pixel_seed);
}
// Unsupplied by SpeedTree: a conservative thin-leaf RGB transmission
// approximation tied to the authored leaf pigment, never the cutout alpha.
fn leaf_transmission(albedo: vec3<f32>) -> vec3<f32> {
    return 0.12+0.5*clamp(albedo,vec3<f32>(0.),vec3<f32>(1.));
}
fn visibility(p: vec3<f32>, face: vec3<f32>, light: LightRay, width: f32, source: Ray) -> vec3<f32> {
    if (frame.options.x & SHADOWS)==0u { return vec3<f32>(1.); }
    let origin = offset_origin(p,light.dir,face);
    let entry = p+light.dir*light.distance;
    let to_light = light.dir*light.distance - (origin-p);
    let distance = length(to_light);
    if distance<=1.e-5 { return vec3<f32>(1.); }
    var r = Ray(origin,to_light/distance,0.,distance,width,0.,source.coverage,source.pixel_seed);
    var throughput = vec3<f32>(1.);
    var tail = dot(light.tail,light.tail)>0.;
    let sample = light.purpose!=VIEW;
    // An accepted opaque blocker anywhere on the leg makes transmittance
    // zero. Test it with an any-hit query before walking colored layers;
    // NEE still requires the closest hit to verify the sampled endpoint.
    var blocker = !sample;
    var layer = 0u;
    while layer<17u {
        let mask = select(select(TRANSMISSIVE_MASK,BLOCKER_MASK,blocker),solid_mask(light.purpose),sample);
        let hit = trace(r,Query(select(SHADOW,light.purpose,sample),mask,false,vec2<u32>(NIL)));
        if sample {
            // Delta interfaces aren't connected by a straight NEE ray. Their
            // continuations get full weight once their direction has changed.
            return vec3<f32>(select(0.,1.,hit.instance==NIL || (hit.instance==light.key.x && hit.primitive==light.key.y)));
        }
        if blocker {
            if hit.instance!=NIL { return vec3<f32>(0.); }
            blocker = false;
            continue;
        }
        if hit.instance==NIL {
            if tail {
                tail = false;
                blocker = true;
                layer++;
                let up = vec3<f32>(0.,1.,0.);
                r = Ray(offset_origin(entry,light.tail,up),light.tail,0.,frame.eye.w,width,0.,source.coverage,source.pixel_seed);
                continue;
            }
            return throughput;
        }
        if layer==16u { return vec3<f32>(0.); }
        let m = (hit.instance & DYNAMIC)!=0u;
        let mat = material_addr(m,hit.primitive);
        let flags = word(m,mat+1u);
        let inst = inst_addr(hit.instance);
        let v = vertex_addresses(m,inst,hit.primitive);
        if (flags & GLASS)!=0u {
            // A straight connection through a thin, parallel-sided pane.
            // The two-interface Fresnel loss includes internal reflections;
            // refracted light connections through thick solids are separate.
            let gn = norm(geometric(m,inst,v));
            let local_n = select(gn,-gn,dot(gn,r.dir)>0.);
            let ior = bitcast<f32>(word(m,mat+23u));
            if ior<0. { return vec3<f32>(0.); } // thick glass needs a bent connection
            let optics = dielectric(r.dir,local_n,1./ior);
            throughput *= rgba(word(m,mat+22u)).rgb*((1.-optics.fresnel)/(1.+optics.fresnel));
        } else {
            let w = weights_of(hit);
            let uv = surface_uv(m,v,w,mat,flags);
            let addr = word(m,mat);
            let sample = filtered_tile(addr,uv,(flags & CLAMP_UV)!=0u,mesh_taps(addr,r,hit,m,inst,v,vec2<f32>(1.)));
            throughput *= select(leaf_transmission(sample.rgb),vec3<f32>(1.-0.72*sample.a*tint_of(m,v,w).a),(flags & CLOUD)!=0u);
        }
        if strongest(throughput)<0.002 { break; }
        r.low = advance(hit.t);
        layer++;
    }
    return vec3<f32>(0.);
}
fn band(t: f32) -> f32 {
    // Group only floating-point ties, not surfaces separated by a ray bias.
    return max(0.000001,abs(t)*0.000002);
}
fn advance(t: f32) -> f32 {
    // All ray distances here are positive. Keep the inclusive end of the band
    // from being visited twice, without leaving a gap before the next one.
    return bitcast<f32>(bitcast<u32>(t)+1u);
}
fn sky_triple(d: vec3<f32>, spread: f32, layer: vec3<u32>) -> vec3<f32> {
    // Native camera-centred roof cap and panorama skirt mapping. At infinity
    // the cone's angle, not its world-space width, controls the lookup.
    let roof_weight = smoothstep(-0.015,0.015,d.y);
    var roof = vec3<f32>(0.);
    if roof_weight>0. {
        let y = max(d.y,0.);
        let root = sqrt(226.05995+2273.93991*y*y);
        let cap = (-47.685848*y + root)/30.070580;
        let dy = select(0.,(-47.685848 + 2273.93991*y/root)/30.070580,d.y>=0.);
        let scale = frame.sky_uv.z;
        let uv = (vec2<f32>(0.5) + d.xz*cap)*scale + frame.sky_uv.xy;
        let taps = footprint_taps(layer.z,d,d,vec3<f32>(cap,d.x*dy,0.)*scale,
                                  vec3<f32>(0.,d.z*dy,cap)*scale,spread);
        roof = filtered_tile(layer.z,uv,false,taps).rgb;
        dependencies |= ANIM_SKY;
    }
    if roof_weight>=1. { return roof; }
    let horizon = max(length(d.xz),1.e-6);
    let az = atan2(max(abs(d.z),1.e-7),d.x);
    let row = (50./-10.602875)*d.y/horizon;
    let v = mix(0.275,0.5,clamp(row,0.,1.));
    let mirrored = az>=1.5707963;
    let u = clamp(select(1.,2.,mirrored) - 2.*az/3.14159265,0.,1.);
    let other = clamp(select(2.,1.,mirrored) - 2.*az/3.14159265,0.,1.);
    let primary = select(layer.y,layer.x,d.z>=0.);
    let side = select(layer.x,layer.y,d.z>=0.);
    let gu = (2./3.14159265)*vec3<f32>(abs(d.z),0.,-d.x*sign(d.z))/(horizon*horizon);
    let gv = select(vec3<f32>(0.), (0.225*50./-10.602875)/horizon
                  *vec3<f32>(-d.x*d.y/(horizon*horizon),1.,-d.z*d.y/(horizon*horizon)),
                   row>0. && row<1.);
    let primary_taps = footprint_taps(primary,d,d,gu,gv,spread);
    let a = filtered_tile(primary,vec2<f32>(u,v),true,primary_taps).rgb;
    var skirt = a;
    let seam = (1.-smoothstep(0.,0.06,abs(az-1.5707963)))*0.5;
    if seam>0. {
        skirt = mix(skirt,filtered_tile(primary,vec2<f32>(other,v),true,primary_taps).rgb,seam);
    }
    let side_weight = (1.-smoothstep(0.,0.12,abs(d.z)))*0.5;
    if side_weight>0. {
        let side_taps = footprint_taps(side,d,d,gu,gv,spread);
        skirt = mix(skirt,filtered_tile(side,vec2<f32>(u,v),true,side_taps).rgb,side_weight);
    }
    return mix(skirt,roof,roof_weight);
}
fn analytic_sky(dir: vec3<f32>) -> vec3<f32> {
    let h = clamp(dir.y*1.5,0.,1.);
    let horizon = mix(vec3<f32>(0.022,0.030,0.064),vec3<f32>(0.60,0.72,0.85),frame.sun.w);
    let dome = mix(vec3<f32>(0.004,0.009,0.026),vec3<f32>(0.11,0.31,0.64),frame.sun.w);
    return mix(horizon,dome,h);
}
fn environment(ray: Ray, seen_sun: bool) -> vec3<f32> {
    let dir = ray.dir;
    var sky = vec3<f32>(0.);
    if !seen_sun && (frame.options.x & SINGLE_PATH)!=0u && frame.sky_cdf.x!=0u {
        // Cache only the broad diffuse environment term. The authored view,
        // mirrors, and refraction keep their filtered sky art and roof drift.
        return diffuse_sky(dir);
    }
    if frame.sky0.x!=0u {
        var radiance = sky_triple(dir,ray.spread,frame.sky0.xyz);
        let f = bitcast<f32>(frame.sky0.w);
        if f>0. && frame.sky1.x!=0u {
            radiance = mix(radiance,sky_triple(dir,ray.spread,frame.sky1.xyz),f);
        }
        sky = linear(encoded(radiance)*clamp(frame.sky_tint.rgb,vec3<f32>(0.),vec3<f32>(1.)));
    } else {
        sky = analytic_sky(dir);
    }
    // Diffuse paths already sample the analytic sun in direct(). The
    // decorative disc is visible to the eye, in mirrors and through water.
    // Gate sun and moon behind time of day so moon never bleeds into day
    // (dark blue noon) and sun never bleeds into night.
    let is_day = frame.sun.w > 0.3;
    let sun_disc = select(0.,smoothstep(0.9994,0.99985,dot(dir,frame.sun_disc.xyz))*frame.sun_disc.w,seen_sun);
    let moon_disc = smoothstep(0.9985,0.9992,dot(dir,frame.moon_disc.xyz))*frame.moon_disc.w;
    return sky
        + select(vec3<f32>(0.),frame.sun_color.rgb*sun_disc*2.,is_day)
        + select(vec3<f32>(0.),vec3<f32>(0.45,0.5,0.6)*moon_disc,!is_day);
}
struct LightRay {
    dir: vec3<f32>, distance: f32, energy: vec3<f32>, tail: vec3<f32>,
    purpose: u32, key: vec2<u32>,
}
struct LightCursor { feed: u32, node: u32, item: u32, end_item: u32 }
const LIGHT_MOVING: u32 = 0x80000000u;
fn empty_light() -> LightRay {
    return LightRay(vec3<f32>(0.),0.,vec3<f32>(0.),vec3<f32>(0.),VIEW,vec2<u32>(NIL));
}
fn local_power(light: LightRay) -> f32 {
    return max(luminance(max(light.energy,vec3<f32>(0.))),0.);
}
fn local_light(p: vec3<f32>, n: vec3<f32>, face: vec3<f32>, key: u32, seed: ptr<function,u32>) -> LightRay {
    let m = (key & LIGHT_MOVING)!=0u;
    let index = key & ~LIGHT_MOVING;
    let a = word(m,9u) + index*12u;
    let to = floats(m,a)-p;
    let r = bitcast<f32>(word(m,a+3u));
    let d2 = dot(to,to);
    if d2>=r*r || d2<=1.e-8 { return empty_light(); }
    var rgb = floats(m,a+4u);
    if (word(m,a+10u) & 1u)!=0u {
        if frame.material_time.z<=0. { return empty_light(); }
        // The client scales its encoded diffuse before the color stage.
        rgb = linear(clamp(rgb*frame.material_time.z,vec3<f32>(0.),vec3<f32>(1.)));
    }
    let d = sqrt(d2);
    var sample = to/d;
    var end = d;
    let kind = word(m,a+8u);
    if (frame.options.x & SHADOWS)!=0u && kind!=0u {
        let radius = min(bitcast<f32>(word(m,a+9u)),d*0.99);
        let u = random(seed);
        let phi = random(seed)*6.2831853;
        if kind==1u {
            // Uniform solid-angle cone subtended by the sphere. The
            // endpoint is its near surface, not the emitter center.
            let t2 = radius*radius/d2;
            let delta = u*t2/(1.+sqrt(1.-t2));
            let sin2 = delta*(2.-delta);
            sample = around(sample,1.-delta,sqrt(sin2),phi);
            end = d*(1.-delta)-sqrt(max(0.,radius*radius-d2*sin2));
        } else {
            // Authored direction is unavailable for runtime lights;
            // use a finite, receiver-facing disk of equivalent size.
            let delta = around(sample,0.,1.,phi)*(radius*sqrt(u));
            end = length(to+delta);
            sample = (to+delta)/end;
        }
    }
    let cosine = dot(sample,n);
    if cosine<=0. || dot(sample,face)<=0. { return empty_light(); }
    let falloff = 1.-d/r;
    return LightRay(sample,end,rgb*(cosine*falloff*falloff),vec3<f32>(0.),VIEW,vec2<u32>(NIL));
}
fn next_light(p: vec3<f32>, s: Surface, cursor: ptr<function,LightCursor>, seed: ptr<function,u32>, identity: ptr<function,vec2<u32>>) -> LightRay {
    loop {
        let m = (*cursor).feed!=0u;
        if (*cursor).item<(*cursor).end_item {
            let index = word(m,word(m,6u)+(*cursor).item);
            (*cursor).item++;
            let tag = vec2<u32>(index | select(0u,LIGHT_MOVING,m),*seed);
            let light = local_light(p,s.n,s.face,tag.x,seed);
            if light.distance<=0. { continue; }
            *identity = tag;
            return light;
        }
        if (*cursor).node>=word(m,13u) {
            if m { break; }
            (*cursor) = LightCursor(1u,moving[12u],0u,0u);
            continue;
        }
        let at = word(m,5u)+(*cursor).node*9u;
        if any(p<floats(m,at)) || any(p>floats(m,at+3u)) {
            (*cursor).node = word(m,at+6u);
        } else {
            (*cursor).node++;
            (*cursor).item = word(m,at+7u);
            (*cursor).end_item = (*cursor).item + word(m,at+8u);
        }
    }
    return LightRay(vec3<f32>(0.),0.,vec3<f32>(0.),vec3<f32>(0.),VIEW,vec2<u32>(NIL));
}
// One unshadowed realization per eligible local source, then exactly one
// shadow ray. The optional screen cache combines only fresh reservoirs.
fn resample_lights(p: vec3<f32>, s: Surface, cursor: ptr<function,LightCursor>, reuse: bool, seed: ptr<function,u32>) -> LightRay {
    var selected = empty_light();
    var sum = 0.;
    var chosen = 0.;
    var identity = vec2<u32>(NIL);
    var selected_tag = identity;
    loop {
        let candidate = next_light(p,s,cursor,seed,&identity);
        if candidate.distance<=0. { break; }
        let power = local_power(candidate);
        if power<=0. { continue; }
        sum += power;
        if random(seed)*sum<power { selected = candidate; chosen = power; selected_tag = identity; }
    }
    var inv_pmf = 0.;
    if chosen>0. { inv_pmf = sum/chosen; }
    if reuse { return screen_lights(p,s,selected_tag,inv_pmf,selected,seed); }
    if chosen>0. { selected.energy *= inv_pmf; }
    return selected;
}
fn random(seed: ptr<function,u32>) -> f32 {
    let value = hash(*seed);
    *seed = *seed*747796405u + 2891336453u;
    return f32(value & 0xffffffu)/16777216.;
}
fn around(axis: vec3<f32>, axial: f32, radial: f32, angle: f32) -> vec3<f32> {
    let u = norm(cross(axis,select(vec3<f32>(0.,1.,0.),vec3<f32>(1.,0.,0.),abs(axis.y)>0.95)));
    let v = cross(axis,u);
    return norm((u*cos(angle)+v*sin(angle))*radial+axis*axial);
}
// Two digitally shifted Sobol dimensions. Gray code indexes the direction
// numbers; the second dimension uses the x+1 recurrence. The scramble is
// fixed in screen space so a stationary pixel sees stratified disk samples.
fn sun_sobol(sequence: u32, pixel: u32) -> vec2<f32> {
    var gray = sequence ^ (sequence >> 1u);
    var direction = 0x80000000u;
    var y = 0u;
    let x = reverseBits(gray);
    while gray!=0u {
        if (gray & 1u)!=0u { y ^= direction; }
        gray >>= 1u;
        direction ^= direction >> 1u;
    }
    let shift = vec2<u32>(hash(pixel ^ 0xa511e9b3u),hash(pixel ^ 0x63d83595u));
    return vec2<f32>((vec2<u32>(x,y) ^ shift) >> vec2<u32>(8u))/16777216.;
}
fn sun_sample(ray: Ray, purpose: u32, seed: ptr<function,u32>) -> vec3<f32> {
    if (frame.options.x & (SHADOWS|SOFT_SUN))!=(SHADOWS|SOFT_SUN) { return frame.sun.xyz; }
    // Uniform solid angle over a 0.27-degree angular radius. Compute sin
    // without subtracting nearly equal cosines at this narrow aperture.
    var u = vec2<f32>(random(seed),random(seed));
    if (frame.options.x & SINGLE_PATH)!=0u && purpose==VIEW && frame.options.z==1u {
        u = sun_sobol(frame.sampling.x,ray.pixel_seed);
    }
    let delta = u.x*0.000011103284; // 1 - cos(0.27 deg)
    return around(frame.sun.xyz,1.-delta,sqrt(delta*(2.-delta)),u.y*6.2831853);
}
fn local_color(s: Surface, light: vec3<f32>, ambient: vec3<f32>) -> vec3<f32> {
    let lit = s.color.rgb*light + ambient + s.emission;
    if s.glaze.a>0. { return linear(mix(encoded(lit),encoded(s.glaze.rgb),s.glaze.a)); }
    return lit;
}
fn physical_material(s: Surface) -> bool {
    return (s.flags & (BAKED|EMISSIVE|BLEND|WATER|GLASS|FOLIAGE))==0u
        && ((frame.options.x & PHYSICAL_BSDF)!=0u || s.pbr.y>=0.);
}
fn material_bsdf(s: Surface) -> Bsdf {
    let authored = s.pbr.y>=0.;
    let metallic = select(0.,s.pbr.x,authored);
    let albedo = clamp(s.color.rgb,vec3<f32>(0.),vec3<f32>(1.));
    let f0 = select(s.reflectance*s.specular,mix(vec3<f32>(0.04),albedo,metallic),authored);
    let alpha = select(sqrt(2./(2.+max(s.power,0.))),s.pbr.y*s.pbr.y,authored);
    return Bsdf(albedo*(1.-metallic),select(vec3<f32>(0.),clamp(f0,vec3<f32>(0.),vec3<f32>(1.)),(frame.options.x & REFLECTIONS)!=0u),clamp(alpha,0.002,1.));
}
fn direct(p: vec3<f32>, s: Surface, ray: Ray, t: f32, terminal: bool, medium: u32, purpose: u32, emission_weight: f32, cache_sun: bool, reuse_local: bool, seed: ptr<function,u32>) -> vec3<f32> {
    if (s.flags & EMISSIVE)!=0u { return s.color.rgb*emission_weight; }
    let lit = (frame.options.x & LIGHTING)!=0u;
    let baked = (s.flags & BAKED)!=0u;
    if lit && baked { return local_color(s,vec3<f32>(1.),vec3<f32>(0.)); }
    var sun_pending = (!lit || !baked) && frame.sun.w>0.02 && frame.sun.y>0.;
    var sun = frame.sun.xyz;
    var cosine = 0.;
    var sun_leg = frame.eye.w;
    var sun_tail = vec3<f32>(0.);
    var sun_loss = vec3<f32>(1.);
    if sun_pending {
        sun = sun_sample(ray,purpose,seed);
        if medium!=0u && medium!=OPEN_WATER && sun.y>0. {
            let link = sun_caustic(p,sun,medium);
            sun_tail = sun;
            sun = link.inside;
            sun_leg = link.distance;
            sun_loss = link.weight;
        }
        cosine = select(0.,max(dot(s.n,sun),0.),sun.y>0. && dot(s.face,sun)>0. && sun_leg>0.);
        // View-oriented particles approximate small, direction-independent
        // scatterers; the world shadow ray still decides their sun exposure.
        if (s.flags & PRECIPITATION)!=0u { cosine = select(0.,1.,sun.y>0.); }
        sun_pending = cosine>0.;
    }
    let physical = lit && physical_material(s);
    var b: Bsdf;
    if physical { b = material_bsdf(s); }
    let width = footprint(ray,t);
    var color = vec3<f32>(1.);
    var ambient = vec3<f32>(0.);
    var energy = frame.sun_color.rgb*cosine;
    if !lit {
        if !sun_pending || (frame.options.x & SHADOWS)==0u { return local_color(s,vec3<f32>(1.),vec3<f32>(0.)); }
        energy = vec3<f32>(0.45);
        color = vec3<f32>(0.55);
    } else if !baked {
        // Use the authored ambient only when no indirect ray replaces it.
        color = vec3<f32>(0.);
        ambient = select(vec3<f32>(0.),frame.ambient.rgb*s.ambient,terminal && !reference());
    }
    var cursor = LightCursor(0u,world[12u],0u,0u);
    var proposal = select(0u,2u,reference() && physical);
    var local_spent = false;
    let sample_local = (frame.options.x & SINGLE_PATH)!=0u && (world[10u]+moving[10u])>4u
        && (frame.options.x & SHADOWS)!=0u;
    loop {
        var light: LightRay;
        let cache = sun_pending && cache_sun && frame.sampling.y!=0u;
        if sun_pending {
            sun_pending = false;
            light = LightRay(sun,sun_leg,energy*sun_loss,sun_tail,VIEW,vec2<u32>(NIL));
        } else if proposal!=0u {
            let sky = proposal==2u;
            proposal--;
            var sample: EmissionSample;
            if sky {
                let d = sample_sky(seed);
                sample = EmissionSample(d,frame.eye.w,sky_pdf(d),vec2<u32>(NIL),vec2<f32>(0.));
            } else { sample = sample_emitter(p,seed); }
            if sample.pdf<=0. || dot(s.face,sample.dir)<=0. { continue; }
            let eval = bsdf_eval(b,s.n,-ray.dir,sample.dir);
            let cosine = dot(s.n,sample.dir);
            if cosine<=0. || eval.pdf<=0. { continue; }
            var shadow = secondary(p,sample.dir,s.face,ray,t);
            shadow.high = sample.t;
            let next_purpose = select(DIFFUSE,purpose,purpose==WATER_REFLECT || purpose==WATER_TRANSMIT);
            var radiance = vec3<f32>(0.);
            if sky { radiance = reference_sky(sample.dir); }
            else {
                let f = word((sample.key.x & DYNAMIC)!=0u,1u+material_addr((sample.key.x & DYNAMIC)!=0u,sample.key.y));
                if (next_purpose==WATER_REFLECT && (f & NO_REFLECT)!=0u)
                    || (next_purpose==WATER_TRANSMIT && (f & NO_REFRACT)!=0u) { continue; }
                radiance = sample_emission(Hit(sample.t,sample.key.x,sample.key.y,sample.bary));
            }
            let tr = media_to(shadow,sample.t,medium,sky).tr;
            let mis = balance(sample.pdf,select(eval.pdf,0.,terminal));
            light = LightRay(sample.dir,sample.t,eval.f*radiance*tr*(cosine*mis/sample.pdf),vec3<f32>(0.),next_purpose,sample.key);
        } else {
            if !lit { break; }
            if sample_local {
                if local_spent { break; }
                local_spent = true;
                light = resample_lights(p,s,&cursor,reuse_local,seed);
            } else {
                var unused: vec2<u32>;
                light = next_light(p,s,&cursor,seed,&unused);
            }
            if light.distance<=0. { break; }
            if medium!=0u && medium!=OPEN_WATER {
                let sizing = Ray(p,light.dir,0.,light.distance,0.,0.,0.,0u);
                light.energy *= water_tr(wet_distance(sizing,light.distance,medium));
            }
        }
        // One shadow traversal site for sun, static points and GFX points.
        // Drivers inline ray queries, so duplicating the call at each branch
        // can make shader compilation dominate the first frame.
        var response = vec3<f32>(1.);
        if physical && light.purpose==VIEW { response = BSDF_PI*bsdf_eval(b,s.n,-ray.dir,light.dir).f; }
        var tr = vec3<f32>(0.);
        if cache && (sun_cache_read & SUN_VALID)!=0u {
            tr = vec3<f32>(select(1.,0.,(sun_cache_read & SUN_BLOCKED)!=0u));
        } else { tr = visibility(p,s.face,light,width,ray); }
        if cache {
            // Colored transmission has no lossless binary encoding. Cache
            // only an all-clear or an opaque result in a stationary snapshot.
            if all(tr==vec3<f32>(0.)) { sun_cache_write = SUN_VALID|SUN_BLOCKED; }
            else if all(tr==vec3<f32>(1.)) { sun_cache_write = SUN_VALID; }
        }
        color += response*light.energy*tr;
    }
    if physical { return color+ambient+s.emission*emission_weight; }
    // When no lights contributed (color==0): on terminal bounce, return
    // ambient+emission for white surfaces (ambient is the intended
    // illumination there) or base color for coloured surfaces (surface
    // colour shows through without light); on non-terminal bounce with
    // lighting active, return black so the secondary diffuse ray picks up
    // indirect light (e.g. sky dome radiance) instead of the surface's own
    // base colour. Without lighting, direct() returns earlier via the
    // !lit branch. Emissive surfaces are handled at the EMISSIVE check.
    if all(color == vec3<f32>(0.)) {
        var result = s.color.rgb;
        if terminal && all(s.color.rgb == vec3<f32>(1.)) {
            result = ambient + s.emission * emission_weight;
        } else if !terminal && lit {
            result = vec3<f32>(0.);
        }
        return result;
    }
    return local_color(s,color,ambient);
}
fn hemisphere(n: vec3<f32>, seed: ptr<function,u32>) -> vec3<f32> {
    let r = random(seed);
    return around(n,sqrt(1.-r),sqrt(r),random(seed)*6.2831853);
}
fn wave(s: Surface, ray: Ray, hit: Hit) -> vec3<f32> {
    let direction = ray.dir;
    let a = bitcast<f32>(frame.waves.w);
    let uv = s.uv*2. + 0.05*vec2<f32>(cos(a),sin(a));
    var n = vec3<f32>(0.,1.,0.);
    if frame.waves.x != 0u && (s.flags & SIMPLE_WATER)==0u {
        dependencies |= ANIM_WAVES;
        let m = (hit.instance & DYNAMIC)!=0u;
        let inst = inst_addr(hit.instance);
        let v = vertex_addresses(m,inst,hit.primitive);
        let a = frame.waves.x;
        let b = frame.waves.y;
        let projected = mesh_footprint(ray,hit,m,inst,v,vec2<f32>(2.));
        let raw = mix(filtered_tile(a,uv,false,uv_taps(a,projected)).rgb,
                      filtered_tile(b,uv,false,uv_taps(b,projected)).rgb,
                      bitcast<f32>(frame.waves.z))*2.-1.;
        n = norm(vec3<f32>(raw.x*0.42,max(abs(raw.y),0.45),raw.z*0.42));
    }
    n *= select(1.,-1.,direction.y>0.);
    if dot(n,direction)>-0.01 { n = vec3<f32>(0.,select(1.,-1.,direction.y>0.),0.); }
    return n;
}
fn world_depth(p: vec3<f32>) -> f32 {
    let clip = frame.vp*vec4<f32>(p,1.);
    return clamp(clip.z/clip.w,0.,1.);
}
fn primary_guide(clip: vec4<f32>, s: Surface) -> vec4<u32> {
    let kind = select(0u,GUIDE_WATER,(s.flags & (WATER|GLASS))!=0u)
             | select(0u,GUIDE_EMISSIVE,(s.flags & EMISSIVE)!=0u)
             | select(0u,GUIDE_BAKED,(s.flags & BAKED)!=0u);
    return vec4<u32>(bitcast<u32>(clip.w),pack4x8snorm(vec4<f32>(s.n,0.)),
        pack4x8unorm(vec4<f32>(sqrt(clamp(s.color.rgb,vec3<f32>(0.),vec3<f32>(1.))),0.)),kind);
}
// Legacy blend pairs that are affine in the destination radiance. Apply
// the factors in Lighter's linear HDR space, not on gamma-encoded UNORM.
struct BlendOp { source: vec3<f32>, carried: vec3<f32> }
fn blend_factor(code: u32, c: vec4<f32>) -> vec3<f32> {
    switch code {
        case 1u: { return vec3<f32>(0.); }
        case 2u: { return vec3<f32>(1.); }
        case 3u: { return clamp(c.rgb,vec3<f32>(0.),vec3<f32>(1.)); }
        case 4u: { return 1.-clamp(c.rgb,vec3<f32>(0.),vec3<f32>(1.)); }
        case 5u: { return vec3<f32>(c.a); }
        default: { return vec3<f32>(1.-c.a); }
    }
}
fn blend_op(code: u32, flags: u32, c: vec4<f32>) -> BlendOp {
    if code==0u {
        return BlendOp(c.rgb*c.a,vec3<f32>(select(1.-c.a,1.,(flags & ADDITIVE)!=0u)));
    }
    let src = code & 15u;
    let dst = blend_factor(code>>4u,c);
    if src==9u { return BlendOp(vec3<f32>(0.),dst+c.rgb); }
    if src==10u { return BlendOp(c.rgb,dst-c.rgb); }
    return BlendOp(c.rgb*blend_factor(src,c),dst);
}
fn project_decals(s: Surface, p: vec3<f32>, ray: Ray, t: f32, original: vec3<f32>) -> vec3<f32> {
    if (s.flags & TERRAIN)==0u || s.face.y<0.15 { return original; }
    var color = original;
    for (var i=0u; i<moving[15u]; i++) {
        let at = moving[14u]+22u*i;
        let height = words2(true,at+6u);
        if p.y<height.x || p.y>height.y { continue; }
        let rel = p.xz-words2(true,at);
        let u = words2(true,at+2u);
        let v = words2(true,at+4u);
        let x = dot(u,rel);
        let y = dot(v,rel);
        // Half-open cells cannot double-apply one authored drape along seams.
        if x<0. || y<0. || x>=1. || y>=1. { continue; }
        let upper = x>=y;
        let a = select(2u,1u,upper);
        let b = 3u;
        let w = vec3<f32>(1.-max(x,y),abs(x-y),min(x,y));
        let u0 = words2(true,at+8u);
        let u1 = words2(true,at+8u+2u*a);
        let u2 = words2(true,at+8u+2u*b);
        let uv = u0*w.x+u1*w.y+u2*w.z;
        let tint = rgba(moving[at+16u])*w.x+rgba(moving[at+16u+a])*w.y+rgba(moving[at+16u+b])*w.z;
        let tile = moving[at+20u];
        let du = select(u2-u1,u1-u0,upper);
        let dv = select(u1-u0,u2-u1,upper);
        let gu = vec3<f32>(u.x*du.x+v.x*dv.x,0.,u.y*du.x+v.y*dv.x);
        let gv = vec3<f32>(u.x*du.y+v.x*dv.y,0.,u.y*du.y+v.y*dv.y);
        let code = moving[at+21u];
        let tex = filtered_tile(tile,uv,(code & CLAMP_UV)!=0u,footprint_taps(tile,ray.dir,s.face,gu,gv,footprint(ray,t)));
        let blend = blend_op(code & 255u,0u,tex*tint);
        color = blend.source+blend.carried*color;
    }
    return color;
}
struct Transport { radiance: vec4<f32>, guide: vec4<u32>, owner: u32 }
fn transport(first: Ray, primary: Hit, raster: bool, seed: ptr<function,u32>) -> Transport {
    var result = vec3<f32>(0.);
    var depth = 0.;
    var guide = vec4<u32>(0u);
    var owner = 0u;
    var layered = false;
    let categories = world[11u]|moving[11u];
    let blend_present = (categories & BLEND_MASK)!=0u;
    var start_medium = frame.water_effects.y;
    if start_medium==0u && (frame.options.x & UNDERWATER)!=0u { start_medium = OPEN_WATER; }
    var path = Path(first,vec3<f32>(1.),0u,VIEW,start_medium,vec4<f32>(0.));
    var pending = path;
    var split = false;
    let single = (frame.options.x & SINGLE_PATH)!=0u;
    // In overlay mode, trace only the primary hit plus direct lighting
    // (sun, local lights). Skip secondary diffuse/reflection bounces —
    // the raster world is the base, rays only add lighting overlays.
    let overlay = (frame.options.x & RAY_LIGHTING_OVERLAY)!=0u;
    let max_bounce = select(frame.options.y, 0u, overlay);
    for (var branch=0u; branch<2u; branch++) {
        for (var segment=0u; segment<=max_bounce; segment++) {
            let minimum = select(0.002,1.e-12,reference());
            if strongest(abs(path.weight))<minimum { break; }
            if single && path.depth>=2u {
                // Kill before tracing the next segment. The reciprocal
                // weight preserves the conditional expectation of survivors.
                let survive = clamp(strongest(abs(path.weight)),0.1,0.95);
                if random(seed)>=survive { break; }
                path.weight /= survive;
            }
            var ray = path.ray;
            // Budget only dry diffuse transport. Primary, shadow, mirrors
            // and both water/glass interface directions keep their ranges.
            if path.purpose==DIFFUSE && path.medium==0u && frame.material_time.w>0. && !reference() {
                ray.high = min(ray.high,frame.material_time.w);
            }
            let terminal = path.depth==frame.options.y;
            var solid = none(ray);
            var scan = ray;
            let solid_bit = solid_mask(path.purpose);
            let gfx_bits = select(GFX_MASK,GFX_MASKS,path.purpose==VIEW);
            let gfx_present = (categories & gfx_bits)!=0u;
            let overlays = (categories & gfx_bits & GFX_OVERLAY_MASK)!=0u;
            var mask = solid_bit;
            var after = vec2<u32>(NIL);
            var group_end = -1.;
            var gfx_depth = ray.high;
            var gfx_color = vec3<f32>(0.);
            var gfx_weight = vec3<f32>(1.);
            var s: Surface;
            var medium: SegmentMedium;
            var color = vec3<f32>(0.);
            loop {
                // Resolve solid visibility once, then process sheets in front
                // of it. Keep one traversal site for both kinds of hit.
                let gfx = mask==gfx_bits;
                let has_weight = strongest(abs(path.weight))>=minimum;
                let ordered = gfx || (mask==BLEND_MASK && has_weight && group_end>=0.);
                var r = scan;
                if gfx {
                    r = ray;
                    // Preserve author-ordered overlays at any distance. When
                    // absent, the current Z limit can prune whole BVH branches.
                    if !overlays { r.high = gfx_depth; }
                }
                else if ordered { r.high = group_end; }
                let query_mask = select(mask,0u,mask==BLEND_MASK && !has_weight);
                var hit: Hit;
                if raster && path.depth==0u && mask==solid_bit {
                    hit = primary;
                    if hit.instance!=NIL && (hit.instance & DYNAMIC)!=0u { motion_dependent = true; }
                }
                else { hit = trace(r,Query(path.purpose,query_mask,ordered,after)); }
                if mask==solid_bit {
                    solid = hit;
                    // A solid-only scene can shade its hit immediately. Skip
                    // the empty ordered/blend passes, not just their BVHs.
                    if gfx_present || blend_present {
                        mask = select(BLEND_MASK,gfx_bits,gfx_present);
                        after = vec2<u32>(select(NIL,0u,gfx_present));
                        if hit.instance!=NIL { scan.high = min(ray.high,hit.t+band(hit.t)); }
                        gfx_depth = scan.high;
                        continue;
                    }
                }
                let sheet = mask!=solid_bit && hit.instance!=NIL;
                if !sheet {
                    if gfx {
                        result += path.weight*gfx_color;
                        path.weight *= gfx_weight;
                        mask = BLEND_MASK;
                        after = vec2<u32>(NIL);
                        continue;
                    }
                    if ordered {
                        scan.low = advance(group_end);
                        group_end = -1.;
                        after = vec2<u32>(NIL);
                        continue;
                    }
                    hit = solid;
                    if hit.instance==NIL { break; }
                } else if !ordered {
                    group_end = min(hit.t+band(hit.t),scan.high);
                    scan.low = max(scan.low,hit.t-band(hit.t));
                    continue;
                }
                if gfx {
                    after = layer_key(hit);
                    let m = (hit.instance & DYNAMIC)!=0u;
                    let flags = word(m,material_addr(m,hit.primitive)+1u);
                    if (flags & OVERLAY)==0u && hit.t>gfx_depth { continue; }
                }
                s = surface(hit,ray);
                let p = ray.origin+ray.dir*hit.t;
                medium = media_to(ray,hit.t,path.medium,false);
                if (s.flags & (WATER|GLASS))!=0u { break; }
                color = vec3<f32>(0.);
                if has_weight {
                    var mis = 1.;
                    if !sheet && path.previous.w>0. && ((s.flags & EMISSIVE)!=0u || strongest(s.emission)>0.) {
                        mis = balance(path.previous.w,emitter_pdf_at(hit,path.previous.xyz,p));
                    }
                    let cache = !sheet && path.depth==0u && branch==0u && medium.body==0u;
                    var cached = vec4<f32>(0.);
                    if terminal && path.depth==1u && path.purpose==DIFFUSE && !sheet &&
                       medium.body==0u && path.medium==0u {
                        cached = screen_gi_read(p,s,hit);
                    }
                    if cached.a>0. { color = cached.rgb; }
                    else {
                        let reuse_local = cache && !layered && screen_surface(s,hit);
                        color = direct(p,s,ray,hit.t,terminal || sheet,medium.body,path.purpose,mis,cache,reuse_local,seed);
                        if cache && !layered && !terminal && path.medium==0u { screen_gi_write(p,s,hit,color); }
                    }
                }
                if !sheet {
                    color = project_decals(s,p,ray,hit.t,color);
                    break;
                }
                if path.depth==1u && branch==0u { temporal_specular_block(); }
                // Coplanar cards blend in decreasing draw-layer order. The
                // two triangles of a card share a key, so diagonals draw once.
                after = layer_key(hit);
                if path.depth==0u && branch==0u {
                    layered = true;
                    if (s.flags & WRITE_DEPTH)!=0u && s.color.a>=0.5 {
                        depth = max(depth,world_depth(p));
                    }
                }
                let overlay = (s.flags & OVERLAY)!=0u;
                let t = select(medium.tr,vec3<f32>(1.),overlay);
                let additive = (s.flags & ADDITIVE)!=0u || s.blend>>4u==2u;
                let mist = select(medium.inscatter,vec3<f32>(0.),additive || overlay);
                let blend = blend_op(s.blend,s.flags,vec4<f32>(color*t + mist,s.color.a));
                if gfx {
                    gfx_color = blend.source + blend.carried*gfx_color;
                    gfx_weight *= blend.carried;
                    if (s.flags & WRITE_DEPTH)!=0u && s.color.a>=0.5 { gfx_depth = min(gfx_depth,hit.t); }
                } else {
                    result += path.weight*blend.source;
                    path.weight *= blend.carried;
                }
            }
            if path.depth==1u && branch==0u { temporal_specular_hit(ray,solid); }
            if solid.instance==NIL {
                // No exit surface was found within the streamed world.
                if path.medium==OPEN_WATER { result += path.weight*frame.water_color.rgb; }
                else {
                    let miss = media_to(ray,ray.high,path.medium,true);
                    var mis = 1.;
                    var radiance = vec3<f32>(0.);
                    if path.previous.w>0. {
                        mis = balance(path.previous.w,sky_pdf(ray.dir));
                        radiance = reference_sky(ray.dir);
                    } else { radiance = environment(ray,path.purpose!=DIFFUSE); }
                    result += path.weight*(miss.inscatter+miss.tr*mis*radiance);
                }
                break;
            }
            let p = ray.origin + ray.dir*solid.t;
            if path.depth==0u && branch==0u {
                let clip = frame.vp*vec4<f32>(p,1.);
                depth = max(depth,clamp(clip.z/clip.w,0.,1.));
                guide = primary_guide(clip,s);
                temporal_primary(s,solid,ray,p,medium.body);
                let m = (solid.instance & DYNAMIC)!=0u;
                owner = (hash(solid.instance ^ hash(material_addr(m,solid.primitive))) & OWNER_MASK) | 1u;
            }
            result += path.weight*medium.inscatter;
            path.weight *= medium.tr;
            path.medium = medium.body;
            if strongest(abs(path.weight))<minimum { break; }
            path.previous = vec4<f32>(0.);
            if (s.flags & (WATER|GLASS))!=0u {
                let water = (s.flags & WATER)!=0u;
                let body = select(s.body,OPEN_WATER,s.body==0u);
                let simple = (s.flags & SIMPLE_WATER)!=0u && frame.water_effects.x!=0u;
                if simple {
                    result += path.weight*s.color.rgb*select(s.color.a,1.,terminal);
                    path.weight *= 1.-s.color.a;
                    if terminal || strongest(path.weight)<0.002 { break; }
                    path.ray = secondary(p,ray.dir,s.face,ray,solid.t);
                    path.medium = select(0u,body,s.face.y>=0.);
                    path.purpose = WATER_TRANSMIT;
                    path.depth++;
                    continue;
                }
                if terminal {
                    if water { result += path.weight*mix(frame.water_color.rgb,s.color.rgb,0.4); }
                    break;
                }
                let from_water = s.face.y<0.;
                let thin = !water && s.glass.w>0.;
                let ior = select(abs(s.glass.w),1.333,water);
                let from_inside = select(!s.entering && !thin,from_water,water);
                let outside = select(1.,1.333,!water && path.medium!=0u);
                let eta = select(outside/ior,ior/outside,from_inside);
                var n = s.face;
                if water { n = wave(s,ray,solid); }
                var optics = dielectric(ray.dir,n,eta);
                var reflected = norm(reflect(ray.dir,n));
                // Bump maps may not send a lobe through the wrong geometric side.
                if dot(reflected,s.face)<=0. || (optics.fresnel<1. && dot(optics.transmitted,s.face)>=0.) {
                    optics = dielectric(ray.dir,s.face,eta);
                    reflected = norm(reflect(ray.dir,s.face));
                }
                let can_reflect = (frame.options.x & REFLECTIONS)!=0u
                    && (!water || ((frame.options.x & WATER_MIRROR)!=0u && (s.flags & SIMPLE_WATER)==0u));
                let f = select(optics.fresnel,2.*optics.fresnel/(1.+optics.fresnel),thin);
                let specular = select(path.purpose,SPECULAR,path.purpose==VIEW || path.purpose==DIFFUSE);
                let reflect_purpose = select(specular,WATER_REFLECT,water);
                let reflect_medium = select(path.medium,select(0u,body,from_water),water);
                path.depth++;
                if f>=1. {
                    if !can_reflect {
                        if water { result += path.weight*frame.water_color.rgb; }
                        break;
                    }
                    path.ray = secondary(p,reflected,s.face,ray,solid.t);
                    path.purpose = reflect_purpose;
                    path.medium = reflect_medium;
                    continue;
                }
                if can_reflect && f>0. {
                    // The single-sample primary water guide deliberately
                    // refuses spatial filtering and camera reprojection.
                    // Keep its first split so animated waves do not flicker
                    // between a full-strength mirror and a transmitted ray.
                    let stable_water = water && path.purpose==VIEW && path.depth==1u && frame.options.z==1u;
                    if single && !stable_water {
                        // Sample exactly one lobe. For the chosen proposal
                        // p=F or p=1-F the BSDF/PMF ratio is exactly one. The
                        // independent eta^2/tint below still applies to T.
                        if random(seed)<f {
                            path.ray = secondary(p,reflected,s.face,ray,solid.t);
                            path.purpose = reflect_purpose;
                            path.medium = reflect_medium;
                            continue;
                        }
                    } else if !split && branch==0u {
                        pending = Path(secondary(p,reflected,s.face,ray,solid.t),path.weight*f,path.depth,reflect_purpose,reflect_medium,vec4<f32>(0.));
                        split = true;
                        path.weight *= 1.-f;
                    } else if random(seed)<f {
                        path.ray = secondary(p,reflected,s.face,ray,solid.t);
                        path.purpose = reflect_purpose;
                        path.medium = reflect_medium;
                        continue;
                    }
                } else if !water { path.weight *= 1.-f; }
                // Radiance Jacobian cancels at the two sides of a solid slab.
                path.weight *= select(s.glass.rgb,vec3<f32>(1.),water)*select(eta*eta,1.,thin);
                path.medium = select(path.medium,select(0u,body,!from_water),water);
                path.ray = secondary(p,select(optics.transmitted,ray.dir,thin),s.face,ray,solid.t);
                path.purpose = select(specular,WATER_TRANSMIT,water);
                continue;
            }
            if physical_material(s) && (frame.options.x & LIGHTING)!=0u {
                result += path.weight*color;
                if terminal { break; }
                let e = bsdf_sample(material_bsdf(s),s.n,-ray.dir,vec3<f32>(random(seed),random(seed),random(seed)));
                if e.pdf<=0. || dot(e.dir,s.face)<=0. { break; }
                path.ray = secondary(p,e.dir,s.face,ray,solid.t);
                path.weight *= e.weight;
                if reference() { path.previous = vec4<f32>(p,e.pdf); }
                if path.depth==0u { temporal_specular_leg(e.specular); }
                if path.purpose!=WATER_REFLECT && path.purpose!=WATER_TRANSMIT { path.purpose = DIFFUSE; }
                path.depth++;
                continue;
            }
            let glossy = !terminal && (s.flags & EMISSIVE)==0u && (frame.options.x & REFLECTIONS)!=0u;
            let specular = select(0.,s.reflectance,glossy);
            let specular_rgb = specular*s.specular;
            // A covered leaf is a surface, not tinted glass. Pigment
            // transmission is used for light visibility only, not to carry
            // a camera/specular ray straight through into the sky.
            let diffuse = vec3<f32>(1.)-specular_rgb;
            // Partition the material response instead of adding a full-strength
            // local diffuse response to a mirror. Cosine-weighted sampling
            // cancels Lambert's cos/pi against its PDF; throughput is albedo.
            result += path.weight*diffuse*color;
            let bounce = !terminal && (s.flags & (EMISSIVE|BAKED))==0u && (frame.options.x & LIGHTING)!=0u;
            let albedo = select(vec3<f32>(0.),s.color.rgb*diffuse,bounce);
            let matte = strongest(albedo);
            let total = specular + matte;
            if total<=0. { break; }
            path.depth++;
            if matte==0. || (specular>0. && random(seed)*(specular+matte)<specular) {
                var d = norm(reflect(ray.dir,s.n));
                if dot(d,s.face)<=0. { d = norm(reflect(ray.dir,s.face)); }
                if s.power>=0. {
                    // Sample a normalized power-cosine lobe around the mirror.
                    // Near its peak, Blinn's half-angle is half the outgoing
                    // angle, so the equivalent outgoing exponent is power/4.
                    let power = s.power*0.25;
                    let cosine = pow(max(random(seed),1.e-7),1./(power+1.));
                    d = around(d,cosine,sqrt(max(0.,1.-cosine*cosine)),random(seed)*6.2831853);
                    // Clipping the lobe conserves energy at the geometric
                    // horizon. Resampling would artificially brighten it.
                    if dot(d,s.face)<=0. { break; }
                }
                path.ray = secondary(p,d,s.face,ray,solid.t);
                path.weight *= s.specular*total;
                if path.depth==1u { temporal_specular_leg(true); }
                if path.purpose!=WATER_REFLECT && path.purpose!=WATER_TRANSMIT { path.purpose = SPECULAR; }
            } else {
                let d = hemisphere(s.n,seed);
                // Samples below the geometric face cannot carry radiance.
                // Reject without resampling, which would change the PDF.
                if dot(d,s.face)<=0. { break; }
                path.ray = secondary(p,d,s.face,ray,solid.t);
                path.weight *= (albedo/matte)*total;
                if path.purpose!=WATER_REFLECT && path.purpose!=WATER_TRANSMIT { path.purpose = DIFFUSE; }
            }
        }
        if !split { break; }
        path = pending;
    }
    guide.w |= select(0u,GUIDE_SHEET,layered);
    return Transport(vec4<f32>(max(result,vec3<f32>(0.)),depth),guide,owner);
}

fn camera_ray(id: vec2<u32>, sample: u32, seed: ptr<function,u32>) -> Ray {
    let pixel_seed = id.x + id.y*frame.extent.x + 1u;
    *seed = hash(pixel_seed ^ (sample*0x9e3779b9u));
    let spread = 2.*max(length(frame.inv_vp[0].xyz)/f32(frame.extent.x),
                        length(frame.inv_vp[1].xyz)/f32(frame.extent.y));
    let jitter = select(vec2<f32>(0.5),vec2<f32>(random(seed),random(seed)),frame.options.z>1u);
    let screen = (vec2<f32>(id)+jitter)/vec2<f32>(frame.extent.xy);
    let ndc = screen*vec2<f32>(2.,-2.)+vec2<f32>(-1.,1.);
    let near4 = frame.inv_vp*vec4<f32>(ndc,1.,1.);
    let far4 = frame.inv_vp*vec4<f32>(ndc,0.,1.);
    let near = near4.xyz/near4.w-frame.eye.xyz;
    let far = far4.xyz/far4.w-frame.eye.xyz;
    let coverage = (f32(sample)+random(seed))/f32(frame.options.z);
    return Ray(frame.eye.xyz,norm(near),length(near),length(far),0.,spread,coverage,pixel_seed);
}

/// Ray-lighting overlay flag bit in frame.options.x. When set, the ray pass
/// adds lighting on top of the raster color instead of replacing it, and
/// never outputs black: if the ray result is invalid, the raster color stands.
// Note: bit value (512u) is declared near LIGHTING/SHADOWS/REFLECTIONS above

fn render_pixel(id: vec2<u32>, primary: Hit, raster: bool, overlay: bool) {
    dependencies = 0u;
    motion_dependent = false;
    sun_cache_read = 0u;
    sun_cache_write = 0u;
    let at = id.y*frame.extent.x+id.x;
    screen_begin(at);
    temporal_begin();
    if frame.sampling.z!=0u { sun_cache_read = image[at].albedo_history & SUN_CACHE; }
    // Clear the output pixel after reading sun cache so the overlay path
    // starts with a zero raster color instead of stale data from a previous
    // render call.
    image[at] = pack_pixel(vec3<f32>(0.), 0., vec4<u32>(0u), 0u);
    var total = vec3<f32>(0.);
    var depth = 0.;
    var guide = vec4<u32>(0u);
    var unsafe_guide = 0u;
    var owner = 0u;
    for (var sample=0u; sample<frame.options.z; sample++) {
        // Camera jitter and coverage of the next sample don't depend on how
        // many random numbers the previous sample's paths consumed.
        var seed = 0u;
        let ray = camera_ray(id,sample,&seed);
        // Rotate transport independently of camera jitter/cutout coverage.
        if frame.sampling.x!=0u { seed ^= hash(frame.sampling.x); }
        let out = transport(ray,primary,raster,&seed);
        total += out.radiance.rgb;
        unsafe_guide |= out.guide.w & GUIDE_SHEET;
        if sample>0u && guide_weight(guide,out.guide)<0.25 { unsafe_guide |= GUIDE_MIXED; }
        if sample==0u || out.radiance.a>depth { guide = out.guide; owner = out.owner; }
        // Reverse Z: take the nearest of the samples for consumers of depth.
        depth = max(depth,out.radiance.a);
    }
    guide.w |= unsafe_guide|dependencies;
    var pixel = pack_pixel(total/f32(frame.options.z),depth,guide,owner|select(0u,DEP_MOVING,motion_dependent));
    pixel.albedo_history |= sun_cache_write;

    if overlay {
        // Ray-lighting overlay: add radiance on top of the raster color.
        // Never output black — if the ray result is invalid, keep raster.
        let existing = image[at];
        let existing_rgb = color_of(existing);
        let ray_rgb = color_of(pixel);
        let has_lighting = luminance(ray_rgb) > 0.;
        if has_lighting {
            // Multiply/add ray lighting onto raster color; clamp to valid range.
            let combined = existing_rgb + ray_rgb;
            var combined_pixel = pack_pixel(combined, depth, guide, owner|select(0u,DEP_MOVING,motion_dependent));
            combined_pixel.albedo_history = existing.albedo_history | sun_cache_write;
            image[at] = combined_pixel;
        } else {
            // No usable ray lighting — keep the raster color unchanged,
            // but record the depth/guide/owner from the ray hit so depth
            // consumers and temporal reprojection see the correct surface.
            var keep = pack_pixel(existing_rgb, depth, guide, owner|select(0u,DEP_MOVING,motion_dependent));
            keep.albedo_history = existing.albedo_history | sun_cache_write;
            image[at] = keep;
        }
    } else {
        // Per-pixel depth test between world and moving geometry:
        // keep the nearer surface (higher reverse-Z depth wins).
        // The ray pass traces both feeds in intersect(), but depth-testing
        // at output ensures the nearer surface always wins even when both
        // feeds contribute to the same pixel.
        let existing = image[at];
        if existing.depth == 0. || pixel.depth > existing.depth {
            image[at] = pixel;
        }
    }
    temporal_end(at);
    screen_end();
}

@compute @workgroup_size(8,8)
fn render(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=frame.extent.xy) { return; }
    let overlay = (frame.options.x & RAY_LIGHTING_OVERLAY)!=0u;
    render_pixel(id.xy,Hit(0.,NIL,0u,vec2<f32>(0.)),false,overlay);
}
