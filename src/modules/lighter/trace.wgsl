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
}
@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage,read> world: array<u32>;
@group(0) @binding(2) var<storage,read> moving: array<u32>;
@group(0) @binding(3) var<storage,read> texels: array<u32>;
@group(0) @binding(4) var<storage,read_write> image: array<Pixel>;
var<private> dependencies: u32;

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
const SOLID_MASK: u32 = 1u;
const BLEND_MASK: u32 = 2u;
const SHADOW_MASK: u32 = 4u;
const LIGHTING: u32 = 1u;
const SHADOWS: u32 = 2u;
const REFLECTIONS: u32 = 4u;
const WATER_MIRROR: u32 = 8u;
const CULL_BUILDINGS: u32 = 16u;
const UNDERWATER: u32 = 32u;
const SOFT_SUN: u32 = 64u;
const ANISO_WIDE: u32 = 128u;
const ANISO_ISOTROPIC: u32 = 256u;
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
}
// Order is nonzero only while enumerating the narrow distance band of
// coplanar sheets. A cursor excludes exactly the layers already composited,
// not all geometry at that depth. NIL sorts after all real keys.
struct Query { purpose: u32, mask: u32, ordered: bool, before: vec2<u32> }
struct Path { ray: Ray, weight: vec3<f32>, depth: u32, purpose: u32, wet: bool }

fn word(dynamic: bool, i: u32) -> u32 {
    if dynamic { return moving[i]; }
    return world[i];
}
fn floats(dynamic: bool, i: u32) -> vec3<f32> {
    return bitcast<vec3<f32>>(vec3<u32>(word(dynamic,i),word(dynamic,i+1u),word(dynamic,i+2u)));
}
fn inst_addr(instance: u32) -> u32 {
    return word((instance & DYNAMIC) != 0u, 4u) + (instance & (DYNAMIC-1u)) * 11u;
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

fn sample_word(word_value: u32, data: bool) -> vec4<f32> {
    let c = rgba(word_value);
    if data { return c; }
    return vec4<f32>(linear(c.rgb)*c.a,c.a);
}
fn texel(base: u32, dims: vec2<i32>, p: vec2<i32>, edge: bool, data: bool) -> vec4<f32> {
    var i = clamp(p, vec2<i32>(0), dims-1);
    if !edge { i = (p % dims + dims) % dims; }
    return sample_word(texels[base + u32(i.y*dims.x + i.x)],data);
}
fn level(addr: u32, uv: vec2<f32>, edge: bool, mip: u32, data: bool) -> vec4<f32> {
    let base = addr + texels[addr+4u+mip];
    let dims = vec2<i32>(max(vec2<u32>(1u),vec2<u32>(texels[addr],texels[addr+1u])>>vec2<u32>(mip)));
    if all(dims==vec2<i32>(1)) { return sample_word(texels[base],data); }
    let xy = select(fract(uv),clamp(uv,vec2<f32>(0.),vec2<f32>(1.)),edge)*vec2<f32>(dims)-0.5;
    let p = vec2<i32>(floor(xy));
    let f = fract(xy);
    return mix(mix(texel(base,dims,p,edge,data), texel(base,dims,p+vec2<i32>(1,0),edge,data), f.x),
               mix(texel(base,dims,p+vec2<i32>(0,1),edge,data), texel(base,dims,p+vec2<i32>(1,1),edge,data), f.x), f.y);
}
fn unassociate(c: vec4<f32>, data: bool) -> vec4<f32> {
    if data { return c; }
    return vec4<f32>(c.rgb/max(c.a,1.e-7),c.a);
}
fn footprint(ray: Ray, t: f32) -> f32 { return ray.width+ray.spread*t; }
struct Taps { step: vec2<f32>, lod: f32, count: u32 }
fn one_tap() -> Taps { return Taps(vec2<f32>(0.),0.,1u); }

// Project the round ray cone onto the hit plane and through the UV gradients.
// The 2x2 Gram matrix in texel space gives major/minor footprint axes. Use
// minor-axis LOD and integrate along the major axis, clamping anisotropy by
// widening the minor filter rather than dropping coverage at grazing angles.
fn footprint_taps(addr: u32, dir: vec3<f32>, n: vec3<f32>, gu: vec3<f32>, gv: vec3<f32>, width: f32) -> Taps {
    let dims = vec2<f32>(f32(texels[addr]),f32(texels[addr+1u]));
    if all(dims==vec2<f32>(1.)) { return one_tap(); }
    let nd = dot(n,dir);
    let inv = select(-1.,1.,nd>=0.)/max(abs(nd),0.002);
    let u = (gu-n*(dot(gu,dir)*inv))*(width*dims.x);
    let v = (gv-n*(dot(gv,dir)*inv))*(width*dims.y);
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
fn mesh_taps(addr: u32, ray: Ray, hit: Hit, m: bool, inst: u32, v: vec3<u32>, uv_scale: f32) -> Taps {
    if texels[addr]==1u && texels[addr+1u]==1u { return one_tap(); }
    let a = floats(m,v.x);
    let e = instance_vector(m,inst,floats(m,v.y)-a);
    let f = instance_vector(m,inst,floats(m,v.z)-a);
    let n = cross(e,f);
    let n2 = dot(n,n);
    if n2<1.e-20 { return one_tap(); }
    let b = vertex_uv(m,v.x);
    let du = vertex_uv(m,v.y)-b;
    let dv = vertex_uv(m,v.z)-b;
    let g0 = cross(f,n)/n2;
    let g1 = cross(n,e)/n2;
    return footprint_taps(addr,ray.dir,n*inverseSqrt(n2),du.x*g0+dv.x*g1,du.y*g0+dv.y*g1,footprint(ray,hit.t)*uv_scale);
}
fn tap_average(addr: u32, uv: vec2<f32>, edge: bool, taps: Taps, data: bool,
               threshold_top: bool, reference: f32, flags: u32) -> vec4<f32> {
    let mip = u32(taps.lod);
    let part = fract(taps.lod);
    var sum = vec4<f32>(0.);
    for (var i=0u; i<taps.count; i++) {
        let p = uv+(f32(i)+0.5-0.5*f32(taps.count))*taps.step;
        var c = level(addr,p,edge,mip,data);
        if threshold_top && mip==0u { c.a = select(0.,1.,alpha_test(c.a,reference,flags)); }
        if part>0. { c = mix(c,level(addr,p,edge,mip+1u,data),part); }
        sum += c;
    }
    return sum/f32(taps.count);
}
fn filtered_tile(addr: u32, uv: vec2<f32>, edge: bool, taps: Taps) -> vec4<f32> {
    let data = (texels[addr+3u] & LINEAR_DATA)!=0u;
    return unassociate(tap_average(addr,uv,edge,taps,data,false,0.,0u),data);
}
fn terrain_color(p: vec3<f32>, face: vec3<f32>, ray: Ray, t: f32) -> vec3<f32> {
    let coord = clamp(vec2<f32>(p.x,-p.z)/frame.terrain.z + frame.terrain.xy*0.5,
                      vec2<f32>(0.),frame.terrain.xy-vec2<f32>(1.))/frame.terrain.w;
    let xy = vec2<u32>(floor(coord));
    let area = frame.options.w + 2u*(xy.y*u32(frame.terrain.x/frame.terrain.w)+xy.x);
    let start = texels[area];
    let count = texels[area+1u];
    let width = footprint(ray,t);
    var rgb = linear(vec3<f32>(0.30,0.33,0.24));
    for (var j=0u; j<count; j++) {
        let at = start + j*5u;
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
        let c = filtered_tile(addr,vec2<f32>(dot(gu,p),dot(gv,p)),false,
            footprint_taps(addr,ray.dir,face,gu,gv,width)).rgb;
        if j==0u { rgb = c; }
        else {
            let mask = texels[at+1u];
            let s = 1./(frame.terrain.z*frame.terrain.w);
            let weight = filtered_tile(mask,fract(coord),true,
                footprint_taps(mask,ray.dir,face,vec3<f32>(s,0.,0.),vec3<f32>(0.,0.,-s),width)).r;
            rgb = mix(rgb,c,weight);
        }
    }
    return rgb;
}
fn vertex_uv(m: bool, v: u32) -> vec2<f32> {
    return bitcast<vec2<f32>>(vec2<u32>(word(m,v+4u),word(m,v+5u)));
}
fn vertex_tint(m: bool, v: u32) -> vec4<f32> {
    return mix(rgba(word(m,v+7u)),rgba(word(m,v+6u)),frame.sun.w);
}
fn material_addr(m: bool, primitive: u32) -> u32 {
    // Five words: tile, flags, alpha reference, reflectance, legacy power.
    return word(m,3u) + 5u*word(m,word(m,2u)+2u*primitive);
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
        if !key_less(key,q.before) { return false; }
        if best.instance==NIL { return true; }
        let other = layer_key(best);
        return key_less(other,key) || (all(key==other) && hit.primitive<best.primitive);
    }
    return hit.t<best.t || (hit.t==best.t && (hit.instance<best.instance ||
           (hit.instance==best.instance && hit.primitive<best.primitive)));
}
fn weights_of(hit: Hit) -> vec3<f32> {
    return vec3<f32>(1.-hit.bary.x-hit.bary.y,hit.bary);
}
fn surface_uv(m: bool, v: vec3<u32>, w: vec3<f32>, flags: u32) -> vec2<f32> {
    var uv = vertex_uv(m,v.x)*w.x + vertex_uv(m,v.y)*w.y + vertex_uv(m,v.z)*w.z;
    if (flags & (BLEND|BAKED))==(BLEND|BAKED) {
        // Alpha tests can read this before a hit is accepted or shaded.
        dependencies |= ANIM_SCROLL;
        uv.y -= 0.5*frame.air.w;
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
// Called before committing the hit. No albedo, lighting, normals, or depth
// writes for a rejected candidate; an alpha hole never advances ray.low.
fn accepts(hit: Hit, ray: Ray, q: Query) -> bool {
    let m = (hit.instance & DYNAMIC)!=0u;
    let mat = material_addr(m,hit.primitive);
    let flags = word(m,mat+1u);
    let blend = (flags & BLEND)!=0u;
    if (q.mask==SOLID_MASK && blend) || (q.mask==BLEND_MASK && !blend)
        || (q.purpose==VIEW && (flags & HIDE_PRIMARY)!=0u)
        || (q.purpose==SHADOW && (flags & (BLEND|WATER))!=0u && (flags & WRITE_DEPTH)==0u)
        || (q.purpose==WATER_REFLECT && (flags & NO_REFLECT)!=0u)
        || (q.purpose==WATER_TRANSMIT && (flags & NO_REFRACT)!=0u) { return false; }
    let cull = q.purpose==VIEW && (frame.options.x & CULL_BUILDINGS)!=0u && (flags & CULL_BACK)!=0u;
    if !cull && (flags & (CUTOUT|BLEND|WRITE_DEPTH))==0u { return true; }
    let inst = inst_addr(hit.instance);
    let v = vertex_addresses(m,inst,hit.primitive);
    if cull && dot(geometric(m,inst,v),ray.dir)>=0. { return false; }
    if (flags & (CUTOUT|BLEND|WRITE_DEPTH))==0u { return true; }
    let w = weights_of(hit);
    var a = 1.;
    if (flags & IGNORE_TEXTURE_ALPHA)==0u {
        let addr = word(m,mat);
        let uv = surface_uv(m,v,w,flags);
        let edge = (flags & CLAMP_UV)!=0u;
        let tile_flags = texels[addr+3u];
        let cutout = (flags & CUTOUT)!=0u;
        let area = cutout && (tile_flags & AREA_MIPS)!=0u;
        let cut = bitcast<f32>(word(m,mat+2u));
        let taps = mesh_taps(addr,ray,hit,m,inst,v,1.);
        // Only alpha is inspected here. Skip the RGB transfer function on
        // rejected candidates, regardless of the material colour space.
        a = tap_average(addr,uv,edge,taps,true,area && (tile_flags & AREA_TOP)==0u,cut,flags).a;
        if area {
            // Fixed per-pixel/per-sample stratification, never seeded by time.
            if a<=0. || (a<1. && a<=coverage_sample(ray,m,inst,v)) { return false; }
            a = 1.;
        } else if cutout && !alpha_test(a,cut,flags) { return false; }
    }
    if blend || (q.purpose==SHADOW && (flags & WRITE_DEPTH)!=0u) {
        a *= tint_of(m,v,w).a;
        if blend && a<=0. { return false; }
        if q.purpose==SHADOW && a<0.5 { return false; }
    }
    return true;
}
fn surface(hit: Hit, ray: Ray) -> Surface {
    let m = (hit.instance & DYNAMIC)!=0u;
    let inst = inst_addr(hit.instance);
    let v = vertex_addresses(m,inst,hit.primitive);
    let mat = material_addr(m,hit.primitive);
    let flags = word(m,mat+1u);
    let p = ray.origin + ray.dir*hit.t;
    let w = weights_of(hit);
    let uv = surface_uv(m,v,w,flags);
    let raw = rgba(word(m,v.x+3u)).xyz*w.x + rgba(word(m,v.y+3u)).xyz*w.y + rgba(word(m,v.z+3u)).xyz*w.z;
    let gn = norm(geometric(m,inst,v));
    let face = select(gn,-gn,dot(gn,ray.dir)>0.);
    // Match the native skin stage: transform posed normals as vectors, then
    // align to the actual affine triangle plane on back/grazing faces.
    let at = word(m,inst+4u);
    var n = raw*2.-1.;
    if word(m,inst+3u)==0u { n = norm(instance_basis(m,at)*n); }
    else { n = norm(rotate(n,bitcast<f32>(at),bitcast<f32>(word(m,inst+5u)))); }
    if dot(n,face)<0. { n = -n; }
    // Bad/sufficiently grazing interpolated normals must not face away from
    // the incident ray. They never decide which side of the triangle we enter.
    if dot(n,face)<1.e-4 || dot(n,ray.dir)>=0. { n = face; }
    if (flags & TERRAIN)!=0u {
        return Surface(n,face,uv,vec4<f32>(terrain_color(p,face,ray,hit.t),1.),flags,0.,-1.);
    }
    let addr = word(m,mat);
    let tex = filtered_tile(addr,uv,(flags & CLAMP_UV)!=0u,mesh_taps(addr,ray,hit,m,inst,v,1.));
    let tint = tint_of(m,v,w);
    let alpha = select(tex.a,1.,(flags & IGNORE_TEXTURE_ALPHA)!=0u)*tint.a;
    var color = vec4<f32>(tex.rgb*linear(tint.rgb),alpha);
    if (flags & EMISSIVE)!=0u { color = vec4<f32>(tex.rgb*tint.rgb,alpha); }
    if (flags & BAKED)!=0u { color = vec4<f32>(linear(min(encoded(tex.rgb)*tint.rgb*2.,vec3<f32>(1.))),alpha); }
    return Surface(n,face,uv,color,flags,bitcast<f32>(word(m,mat+3u)),bitcast<f32>(word(m,mat+4u)));
}
fn none(ray: Ray) -> Hit { return Hit(ray.high,NIL,0u,vec2<f32>(0.)); }
fn trace(ray: Ray, q: Query) -> Hit {
    if ray.low>ray.high || ((world[11u]|moving[11u]) & q.mask)==0u { return none(ray); }
    return intersect(ray,q);
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
fn visibility(p: vec3<f32>, face: vec3<f32>, light: LightRay, width: f32, source: Ray) -> f32 {
    if (frame.options.x & SHADOWS)==0u { return 1.; }
    let origin = offset_origin(p,light.dir,face);
    // Keep the shadow-ray endpoint at the light after offsetting the origin.
    let to_light = light.dir*light.distance - (origin-p);
    let distance = length(to_light);
    if distance<=1.e-5 { return 1.; }
    let r = Ray(origin,to_light/distance,0.,distance,width,0.,source.coverage,source.pixel_seed);
    return select(0.,1.,trace(r,Query(SHADOW,SHADOW_MASK,false,vec2<u32>(NIL))).instance==NIL);
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
fn environment(ray: Ray, seen_sun: bool) -> vec3<f32> {
    let dir = ray.dir;
    var sky = vec3<f32>(0.);
    if frame.sky0.x!=0u {
        var radiance = sky_triple(dir,ray.spread,frame.sky0.xyz);
        let f = bitcast<f32>(frame.sky0.w);
        if f>0. && frame.sky1.x!=0u {
            radiance = mix(radiance,sky_triple(dir,ray.spread,frame.sky1.xyz),f);
        }
        sky = linear(encoded(radiance)*clamp(frame.sky_tint.rgb,vec3<f32>(0.),vec3<f32>(1.)));
    } else {
        let h = clamp(dir.y*1.5,0.,1.);
        let horizon = mix(vec3<f32>(0.022,0.030,0.064),vec3<f32>(0.60,0.72,0.85),frame.sun.w);
        let dome = mix(vec3<f32>(0.004,0.009,0.026),vec3<f32>(0.11,0.31,0.64),frame.sun.w);
        sky = mix(horizon,dome,h);
    }
    // Diffuse paths already sample the analytic sun in direct(). The
    // decorative disc is visible to the eye, in mirrors and through water.
    let sun = select(0.,smoothstep(0.9994,0.99985,dot(dir,frame.sun_disc.xyz))*frame.sun_disc.w,seen_sun);
    let moon = smoothstep(0.9985,0.9992,dot(dir,frame.moon_disc.xyz))*frame.moon_disc.w;
    return sky + frame.sun_color.rgb*sun*2. + vec3<f32>(0.45,0.5,0.6)*moon;
}
struct LightRay { dir: vec3<f32>, distance: f32, energy: vec3<f32> }
struct LightCursor { feed: u32, node: u32, item: u32, end_item: u32 }
fn next_light(p: vec3<f32>, s: Surface, cursor: ptr<function,LightCursor>) -> LightRay {
    loop {
        let m = (*cursor).feed!=0u;
        if (*cursor).item<(*cursor).end_item {
            let index = word(m,word(m,6u)+(*cursor).item);
            let a = word(m,9u) + index*8u;
            (*cursor).item++;
            let to = floats(m,a)-p;
            let r = bitcast<f32>(word(m,a+3u));
            let d2 = dot(to,to);
            let cosine = dot(to,s.n);
            if d2>=r*r || d2<=1.e-8 || cosine<=0. || dot(to,s.face)<=0. { continue; }
            let d = sqrt(d2);
            let falloff = 1.-d/r;
            return LightRay(to/d,d,floats(m,a+4u)*(cosine/d*falloff*falloff));
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
    return LightRay(vec3<f32>(0.),0.,vec3<f32>(0.));
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
fn sun_sample(seed: ptr<function,u32>) -> vec3<f32> {
    if (frame.options.x & (SHADOWS|SOFT_SUN))!=(SHADOWS|SOFT_SUN) { return frame.sun.xyz; }
    // Uniform solid angle over a 0.27-degree angular radius. Compute sin
    // without subtracting nearly equal cosines at this narrow aperture.
    let delta = random(seed)*0.000011103284; // 1 - cos(0.27 deg)
    return around(frame.sun.xyz,1.-delta,sqrt(delta*(2.-delta)),random(seed)*6.2831853);
}
fn direct(p: vec3<f32>, s: Surface, ray: Ray, t: f32, terminal: bool, seed: ptr<function,u32>) -> vec3<f32> {
    let albedo = s.color.rgb;
    if (s.flags & EMISSIVE)!=0u { return albedo; }
    let lit = (frame.options.x & LIGHTING)!=0u;
    let baked = (s.flags & BAKED)!=0u;
    var sun_pending = (!lit || !baked) && frame.sun.w>0.02 && frame.sun.y>0.;
    var sun = frame.sun.xyz;
    var cosine = 0.;
    if sun_pending {
        sun = sun_sample(seed);
        cosine = select(0.,max(dot(s.n,sun),0.),sun.y>0. && dot(s.face,sun)>0.);
        sun_pending = cosine>0.;
    }
    let width = footprint(ray,t);
    var color = vec3<f32>(1.);
    var energy = frame.sun_color.rgb*cosine;
    if !lit {
        if !sun_pending || (frame.options.x & SHADOWS)==0u { return albedo; }
        energy = vec3<f32>(0.45);
        color = vec3<f32>(0.55);
    } else if !baked {
        // Use the authored ambient only when no indirect ray replaces it.
        color = select(vec3<f32>(0.),frame.ambient.rgb,terminal);
    }
    var cursor = LightCursor(0u,world[12u],0u,0u);
    loop {
        var light: LightRay;
        if sun_pending {
            sun_pending = false;
            light = LightRay(sun,frame.eye.w,energy);
        } else {
            if !lit { break; }
            light = next_light(p,s,&cursor);
            if light.distance<=0. { break; }
        }
        // One shadow traversal site for sun, static points and GFX points.
        // Drivers inline ray queries, so duplicating the call at each branch
        // can make shader compilation dominate the first frame.
        color += light.energy*visibility(p,s.face,light,width,ray);
    }
    return albedo*color;
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
        let raw = mix(filtered_tile(a,uv,false,mesh_taps(a,ray,hit,m,inst,v,2.)).rgb,
                      filtered_tile(b,uv,false,mesh_taps(b,ray,hit,m,inst,v,2.)).rgb,
                      bitcast<f32>(frame.waves.z))*2.-1.;
        n = norm(vec3<f32>(raw.x*0.42,max(abs(raw.y),0.45),raw.z*0.42));
    }
    n *= select(1.,-1.,direction.y>0.);
    if dot(n,direction)>-0.01 { n = vec3<f32>(0.,select(1.,-1.,direction.y>0.),0.); }
    return n;
}
struct Optics { transmitted: vec3<f32>, fresnel: f32 }
fn dielectric(d: vec3<f32>, n: vec3<f32>, eta: f32) -> Optics {
    let cos_i = clamp(dot(-d,n),0.,1.);
    let cos_t2 = 1.-eta*eta*(1.-cos_i*cos_i);
    if cos_t2<=0. { return Optics(vec3<f32>(0.),1.); }
    let cos_t = sqrt(cos_t2);
    let rs = (eta*cos_i-cos_t)/(eta*cos_i+cos_t);
    let rp = (cos_i-eta*cos_t)/(cos_i+eta*cos_t);
    return Optics(norm(eta*d+(eta*cos_i-cos_t)*n),clamp(0.5*(rs*rs+rp*rp),0.,1.));
}
fn medium_color(wet: bool) -> vec3<f32> {
    return select(frame.air_color.rgb,frame.water_color.rgb,wet);
}
fn transmittance(distance: f32, wet: bool) -> vec3<f32> {
    let fog = select(frame.air,frame.water,wet);
    if fog.z<0.5 { return vec3<f32>(1.); }
    let span = max(fog.y-fog.x,0.01);
    if !wet {
        // Keep authored atmospheric distance fog (including the no-fog toggle).
        return vec3<f32>(1.-clamp((distance-fog.x)/span,0.,1.));
    }
    // Homogeneous Beer–Lambert extinction along the segment. Interpret the
    // authored fog span as 5% transmission in the clearest channel; the fog
    // hue gives a bounded wavelength bias, not an extra tint at each crossing.
    let hue = frame.water_color.rgb;
    let peak = strongest(hue);
    let density = 1.+0.5*select(vec3<f32>(0.),1.-hue/max(peak,1.e-5),peak>1.e-5);
    return exp2(-4.321928*density*(distance/span));
}
fn world_depth(p: vec3<f32>) -> f32 {
    let clip = frame.vp*vec4<f32>(p,1.);
    return clamp(clip.z/clip.w,0.,1.);
}
fn primary_guide(clip: vec4<f32>, s: Surface) -> vec4<u32> {
    let kind = select(0u,GUIDE_WATER,(s.flags & WATER)!=0u)
             | select(0u,GUIDE_EMISSIVE,(s.flags & EMISSIVE)!=0u)
             | select(0u,GUIDE_BAKED,(s.flags & BAKED)!=0u);
    return vec4<u32>(bitcast<u32>(clip.w),pack4x8snorm(vec4<f32>(s.n,0.)),
        pack4x8unorm(vec4<f32>(sqrt(clamp(s.color.rgb,vec3<f32>(0.),vec3<f32>(1.))),0.)),kind);
}
fn transport(first: Ray, seed: ptr<function,u32>) -> Pixel {
    var result = vec3<f32>(0.);
    var depth = 0.;
    var guide = vec4<u32>(0u);
    var layered = false;
    let wet = (frame.options.x & UNDERWATER)!=0u;
    var path = Path(first,vec3<f32>(1.),0u,VIEW,wet);
    var pending = path;
    var split = false;
    for (var branch=0u; branch<2u; branch++) {
        for (var segment=0u; segment<=frame.options.y; segment++) {
            if strongest(path.weight)<0.002 { break; }
            let ray = path.ray;
            let terminal = path.depth==frame.options.y;
            var solid = none(ray);
            var scan = ray;
            var mask = SOLID_MASK;
            var after = vec2<u32>(NIL);
            var group_end = -1.;
            var s: Surface;
            var color = vec3<f32>(0.);
            loop {
                // Resolve solid visibility once, then process sheets in front
                // of it. Keep one traversal site for both kinds of hit.
                let blending = mask==BLEND_MASK;
                let has_weight = strongest(path.weight)>=0.002;
                let ordered = blending && has_weight && group_end>=0.;
                var r = scan;
                if ordered { r.high = group_end; }
                let query_mask = select(mask,0u,blending && !has_weight);
                var hit = trace(r,Query(path.purpose,query_mask,ordered,after));
                if mask==SOLID_MASK {
                    solid = hit;
                    mask = BLEND_MASK;
                    if hit.instance!=NIL { scan.high = min(ray.high,hit.t+band(hit.t)); }
                    continue;
                }
                let sheet = hit.instance!=NIL;
                if !sheet {
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
                s = surface(hit,ray);
                let p = ray.origin+ray.dir*hit.t;
                if (s.flags & WATER)!=0u { break; }
                color = vec3<f32>(0.);
                if has_weight { color = direct(p,s,ray,hit.t,terminal || sheet,seed); }
                if !sheet { break; }
                // Coplanar cards blend in decreasing draw-layer order. The
                // two triangles of a card share a key, so diagonals draw once.
                after = layer_key(hit);
                if path.depth==0u && branch==0u {
                    layered = true;
                    if (s.flags & WRITE_DEPTH)!=0u && s.color.a>=0.5 {
                        depth = max(depth,world_depth(p));
                    }
                }
                let t = transmittance(hit.t,path.wet);
                let additive = (s.flags & ADDITIVE)!=0u;
                let mist = select(medium_color(path.wet),vec3<f32>(0.),additive);
                result += path.weight*s.color.a*(color*t + mist*(1.-t));
                if !additive { path.weight *= 1.-s.color.a; }
            }
            if solid.instance==NIL {
                // No exit surface was found within the streamed world.
                if path.wet { result += path.weight*frame.water_color.rgb; }
                else { result += path.weight*environment(ray,path.purpose!=DIFFUSE); }
                break;
            }
            let p = ray.origin + ray.dir*solid.t;
            if path.depth==0u && branch==0u {
                let clip = frame.vp*vec4<f32>(p,1.);
                depth = max(depth,clamp(clip.z/clip.w,0.,1.));
                guide = primary_guide(clip,s);
            }
            let t = transmittance(solid.t,path.wet);
            result += path.weight*(1.-t)*medium_color(path.wet);
            path.weight *= t;
            if strongest(path.weight)<0.002 { break; }
            if (s.flags & WATER)!=0u {
                if terminal {
                    result += path.weight*mix(frame.water_color.rgb,s.color.rgb,0.4);
                    break;
                }
                let from_water = s.face.y<0.;
                let eta = select(1./1.333,1.333,from_water);
                let n = wave(s,ray,solid);
                var optics = dielectric(ray.dir,n,eta);
                var reflected = norm(reflect(ray.dir,n));
                // A normal map perturbs the interface but not the water cell.
                // Use one consistent flat interface if either ray would be
                // sent to the wrong side of that cell.
                if dot(reflected,s.face)<=0. || (optics.fresnel<1. && dot(optics.transmitted,s.face)>=0.) {
                    optics = dielectric(ray.dir,s.face,eta);
                    reflected = norm(reflect(ray.dir,s.face));
                }
                let can_reflect = (frame.options.x & (REFLECTIONS|WATER_MIRROR))==(REFLECTIONS|WATER_MIRROR)
                    && (s.flags & SIMPLE_WATER)==0u;
                let f = optics.fresnel;
                path.depth++;
                if f>=1. {
                    if !can_reflect { result += path.weight*frame.water_color.rgb; break; }
                    path.ray = secondary(p,reflected,s.face,ray,solid.t);
                    path.purpose = WATER_REFLECT;
                    path.wet = from_water;
                    continue;
                }
                if can_reflect && f>0. {
                    // Trace both first-water lobes without noise. At later
                    // interfaces choose by Fresnel probability; that probability
                    // accounts for the lobe weight, so don't multiply it twice.
                    if !split && branch==0u {
                        pending = Path(secondary(p,reflected,s.face,ray,solid.t),path.weight*f,path.depth,WATER_REFLECT,from_water);
                        split = true;
                        path.weight *= 1.-f;
                    } else if random(seed)<f {
                        path.ray = secondary(p,reflected,s.face,ray,solid.t);
                        path.purpose = WATER_REFLECT;
                        path.wet = from_water;
                        continue;
                    }
                }
                // Radiance transport across a change in index of refraction.
                // This factor cancels for a path that exits back into air.
                path.weight *= eta*eta;
                path.wet = !from_water;
                path.ray = secondary(p,optics.transmitted,s.face,ray,solid.t);
                path.purpose = WATER_TRANSMIT;
                continue;
            }
            let glossy = !terminal && (s.flags & EMISSIVE)==0u && (frame.options.x & REFLECTIONS)!=0u;
            let specular = select(0.,s.reflectance,glossy);
            let diffuse = 1.-specular;
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
            if matte==0. || (specular>0. && random(seed)*total<specular) {
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
                path.weight *= total;
                path.purpose = SPECULAR;
            } else {
                let d = hemisphere(s.n,seed);
                // Samples below the geometric face cannot carry radiance.
                // Reject without resampling, which would change the PDF.
                if dot(d,s.face)<=0. { break; }
                path.ray = secondary(p,d,s.face,ray,solid.t);
                path.weight *= (albedo/matte)*total;
                path.purpose = DIFFUSE;
            }
        }
        if !split { break; }
        path = pending;
    }
    guide.w |= select(0u,GUIDE_SHEET,layered);
    return Pixel(vec4<f32>(max(result,vec3<f32>(0.)),depth),guide);
}

@compute @workgroup_size(8,8)
fn render(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=frame.extent.xy) { return; }
    dependencies = 0u;
    let pixel_seed = id.x + id.y*frame.extent.x + 1u;
    let spread = 2.*max(length(frame.inv_vp[0].xyz)/f32(frame.extent.x),
                        length(frame.inv_vp[1].xyz)/f32(frame.extent.y));
    var total = vec3<f32>(0.);
    var depth = 0.;
    var guide = vec4<u32>(0u);
    var unsafe_guide = 0u;
    for (var sample=0u; sample<frame.options.z; sample++) {
        // Camera jitter and coverage of the next sample don't depend on how
        // many random numbers the previous sample's paths consumed.
        var seed = hash(pixel_seed ^ (sample*0x9e3779b9u));
        let jitter = select(vec2<f32>(0.5),vec2<f32>(random(&seed),random(&seed)),frame.options.z>1u);
        let screen = (vec2<f32>(id.xy)+jitter)/vec2<f32>(frame.extent.xy);
        let ndc = screen*vec2<f32>(2.,-2.)+vec2<f32>(-1.,1.);
        let near4 = frame.inv_vp*vec4<f32>(ndc,1.,1.);
        let far4 = frame.inv_vp*vec4<f32>(ndc,0.,1.);
        let near = near4.xyz/near4.w-frame.eye.xyz;
        let far = far4.xyz/far4.w-frame.eye.xyz;
        let coverage = (f32(sample)+random(&seed))/f32(frame.options.z);
        // Rotate the transport sequence independently of camera jitter and
        // cutout coverage, so progressive lighting cannot pump scene depth.
        if frame.sampling.x!=0u { seed ^= hash(frame.sampling.x); }
        let out = transport(Ray(frame.eye.xyz,norm(near),length(near),length(far),0.,spread,coverage,pixel_seed),&seed);
        total += out.radiance.rgb;
        unsafe_guide |= out.guide.w & GUIDE_SHEET;
        if sample>0u && guide_weight(guide,out.guide)<0.25 { unsafe_guide |= GUIDE_MIXED; }
        if sample==0u || out.radiance.a>depth { guide = out.guide; }
        // Reverse Z: take the nearest of the samples for consumers of depth.
        depth = max(depth,out.radiance.a);
    }
    guide.w |= unsafe_guide|dependencies;
    image[id.y*frame.extent.x+id.x] = Pixel(vec4<f32>(total/f32(frame.options.z),depth),guide);
}
