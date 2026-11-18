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
}
@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage,read> world: array<u32>;
@group(0) @binding(2) var<storage,read> moving: array<u32>;
@group(0) @binding(3) var<storage,read> texels: array<u32>;
@group(0) @binding(4) var<storage,read_write> image: array<vec4<f32>>;

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
const TWO_SIDED: u32 = 4096u;
const CULL_BACK: u32 = 8192u;
const WRITE_DEPTH: u32 = 16384u;
const IGNORE_TEXTURE_ALPHA: u32 = 32768u;
const STRICT_ALPHA: u32 = 65536u;
const SOLID_MASK: u32 = 1u;
const BLEND_MASK: u32 = 2u;
const SHADOW_MASK: u32 = 4u;
const SHORE_MASK: u32 = 8u;
const LIGHTING: u32 = 1u;
const SHADOWS: u32 = 2u;
const REFLECTIONS: u32 = 4u;
const WATER_MIRROR: u32 = 8u;
const CULL_BUILDINGS: u32 = 16u;
const UNDERWATER: u32 = 32u;
const VIEW: u32 = 0u;
const SHADOW: u32 = 1u;
const MIRROR: u32 = 2u;
const TRANSMIT: u32 = 3u;
const DIFFUSE: u32 = 4u;
const SHORE: u32 = 5u;

// The instance index (including the dynamic bit) maps directly from Vulkan's
// 24-bit instanceCustomIndex. Primitive IDs refer to the packed index array.
struct Hit { t: f32, instance: u32, primitive: u32, bary: vec2<f32> }
struct Ray {
    origin: vec3<f32>, dir: vec3<f32>, low: f32, high: f32,
    width: f32, spread: f32, coverage: f32,
}
struct Surface {
    n: vec3<f32>,
    uv: vec2<f32>, color: vec4<f32>, flags: u32, reflectance: f32,
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
fn norm(v: vec3<f32>) -> vec3<f32> {
    return v * inverseSqrt(max(dot(v,v), 1.e-20));
}
fn rgba(x: u32) -> vec4<f32> {
    return unpack4x8unorm(x);
}
fn linear(c: vec3<f32>) -> vec3<f32> {
    return select(c/12.92, pow((max(c,vec3<f32>(0.))+0.055)/1.055, vec3<f32>(2.4)), c>vec3<f32>(0.04045));
}
fn texel(base: u32, dims: vec2<i32>, p: vec2<i32>, edge: bool) -> vec4<f32> {
    var i = clamp(p, vec2<i32>(0), dims-1);
    if !edge { i = (p % dims + dims) % dims; }
    return rgba(texels[base + u32(i.y*dims.x + i.x)]);
}
fn level(addr: u32, uv: vec2<f32>, edge: bool, mip: u32) -> vec4<f32> {
    let base = addr + texels[addr+4u+mip];
    let dims = vec2<i32>(max(vec2<u32>(1u),vec2<u32>(texels[addr],texels[addr+1u])>>vec2<u32>(mip)));
    if all(dims==vec2<i32>(1)) { return rgba(texels[base]); }
    let xy = select(fract(uv),clamp(uv,vec2<f32>(0.),vec2<f32>(1.)),edge)*vec2<f32>(dims)-0.5;
    let p = vec2<i32>(floor(xy));
    let f = fract(xy);
    return mix(mix(texel(base,dims,p,edge), texel(base,dims,p+vec2<i32>(1,0),edge), f.x),
               mix(texel(base,dims,p+vec2<i32>(0,1),edge), texel(base,dims,p+vec2<i32>(1,1),edge), f.x), f.y);
}
fn tile_lod(addr: u32, uv: vec2<f32>, edge: bool, lod: f32) -> vec4<f32> {
    let mip = u32(lod);
    var c = level(addr,uv,edge,mip);
    let fraction = fract(lod);
    if fraction>0. { c = mix(c,level(addr,uv,edge,mip+1u),fraction); }
    return vec4<f32>(c.rgb/max(c.a,1./255.),c.a);
}
fn footprint(ray: Ray, t: f32) -> f32 { return ray.width+ray.spread*t; }
fn tile_lod_for(addr: u32, ray: Ray, hit: Hit, m: bool, inst: u32, v: vec3<u32>) -> f32 {
    let last = texels[addr+2u]-1u;
    if last==0u { return 0.; }
    let a = floats(m,v.x);
    let e = floats(m,v.y)-a;
    let f = floats(m,v.z)-a;
    let n = cross(e,f);
    let n2 = dot(n,n);
    if n2<1.e-20 { return 0.; }
    let u = vertex_uv(m,v.x);
    let du = vertex_uv(m,v.y)-u;
    let dv = vertex_uv(m,v.z)-u;
    let grad0 = cross(f,n);
    let grad1 = cross(n,e);
    let grad_u = length(du.x*grad0+dv.x*grad1)*f32(texels[addr]);
    let grad_v = length(du.y*grad0+dv.y*grad1)*f32(texels[addr+1u]);
    let c = bitcast<f32>(word(m,inst+4u));
    let s = bitcast<f32>(word(m,inst+5u));
    let grazing = max(0.15,abs(dot(n*inverseSqrt(n2),rotate(ray.dir,c,-s))));
    let scale = bitcast<f32>(word(m,inst+3u));
    let rho = footprint(ray,hit.t)*max(grad_u,grad_v)/(grazing*scale*n2);
    return clamp(log2(max(rho,1.)),0.,f32(last));
}
fn tile(addr: u32, uv: vec2<f32>, edge: bool) -> vec4<f32> { return tile_lod(addr,uv,edge,0.); }
fn terrain_color(p: vec3<f32>, radius: f32) -> vec3<f32> {
    let coord = clamp(vec2<f32>(p.x,-p.z)/frame.terrain.z + frame.terrain.xy*0.5,
                      vec2<f32>(0.),frame.terrain.xy-vec2<f32>(1.))/frame.terrain.w;
    let xy = vec2<u32>(floor(coord));
    let area = frame.options.w + 2u*(xy.y*u32(frame.terrain.x/frame.terrain.w)+xy.x);
    let start = texels[area];
    let count = texels[area+1u];
    var rgb = vec3<f32>(0.30,0.33,0.24);
    for (var j=0u; j<count; j++) {
        let at = start + j*5u;
        var uv = vec2<f32>(p.x, -p.z);
        switch texels[at+2u] {
            case 0u: { uv = vec2<f32>(p.z,-p.y); }
            case 2u: { uv = vec2<f32>(p.x,-p.y); }
            default: {}
        }
        let scale = bitcast<vec2<f32>>(vec2<u32>(texels[at+3u],texels[at+4u]));
        let addr = texels[at];
        let rho = vec2<f32>(f32(texels[addr]),f32(texels[addr+1u]))*abs(scale)*radius;
        let lod = clamp(log2(max(1.,max(rho.x,rho.y))),0.,f32(texels[addr+2u]-1u));
        let c = tile_lod(addr,uv*scale,false,lod).rgb;
        if j==0u { rgb = c; }
        else { rgb = mix(rgb,c,tile(texels[at+1u],fract(coord),true).r); }
    }
    return linear(rgb);
}
fn vertex_uv(m: bool, v: u32) -> vec2<f32> {
    return bitcast<vec2<f32>>(vec2<u32>(word(m,v+4u),word(m,v+5u)));
}
fn vertex_tint(m: bool, v: u32) -> vec4<f32> {
    return mix(rgba(word(m,v+7u)),rgba(word(m,v+6u)),frame.sun.w);
}
fn material_addr(m: bool, primitive: u32) -> u32 {
    return word(m,3u) + 4u*word(m,word(m,2u)+2u*primitive);
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
    if (flags & (BLEND|BAKED))==(BLEND|BAKED) { uv.y -= 0.5*frame.air.w; }
    return uv;
}
fn tint_of(m: bool, v: vec3<u32>, w: vec3<f32>) -> vec4<f32> {
    return vertex_tint(m,v.x)*w.x + vertex_tint(m,v.y)*w.y + vertex_tint(m,v.z)*w.z;
}
fn geometric(m: bool, inst: u32, v: vec3<u32>) -> vec3<f32> {
    let a = floats(m,v.x);
    let c = bitcast<f32>(word(m,inst+4u));
    let s = bitcast<f32>(word(m,inst+5u));
    return rotate(cross(floats(m,v.y)-a,floats(m,v.z)-a),c,s);
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
    let p = floats(m,inst) + rotate(floats(m,v.x),bitcast<f32>(word(m,inst+4u)),bitcast<f32>(word(m,inst+5u)))*bitcast<f32>(word(m,inst+3u));
    let key = bitcast<u32>(i32(round(dot(n,p)*64.)));
    let shape = bitcast<vec3<u32>>(vec3<i32>(round(n*16.)));
    let salt = hash(key ^ shape.x*0x9e3779b9u ^ shape.y*0x85ebca6bu ^ shape.z*0xc2b2ae35u);
    let count = frame.options.z;
    let stratum = (u32(ray.coverage*f32(count)) + (salt >> 24u)) % count;
    let u = hash(bitcast<u32>(ray.coverage) ^ salt) & 0xffffffu;
    return (f32(stratum)+f32(u)/16777216.)/f32(count);
}
// Called before committing the hit. No albedo, lighting, normals, or depth
// writes for a rejected candidate; an alpha hole never advances ray.low.
fn accepts(hit: Hit, ray: Ray, q: Query) -> bool {
    let m = (hit.instance & DYNAMIC)!=0u;
    let mat = material_addr(m,hit.primitive);
    let flags = word(m,mat+1u);
    let blend = (flags & BLEND)!=0u;
    if (q.mask==SOLID_MASK && blend) || (q.mask==BLEND_MASK && !blend)
        || (q.purpose==SHORE && (flags & TERRAIN)==0u)
        || (q.purpose==SHADOW && (flags & (BLEND|WATER))!=0u && (flags & WRITE_DEPTH)==0u)
        || (q.purpose==MIRROR && (flags & NO_REFLECT)!=0u)
        || (q.purpose==TRANSMIT && (flags & NO_REFRACT)!=0u) { return false; }
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
        let lod = tile_lod_for(addr,ray,hit,m,inst,v);
        let mip = u32(lod);
        let fraction = fract(lod);
        a = level(addr,uv,edge,mip).a;
        if (flags & CUTOUT)!=0u && (texels[addr+3u] & 1u)!=0u {
            if mip==0u && (texels[addr+3u] & 2u)==0u {
                let cut = bitcast<f32>(word(m,mat+2u));
                a = select(0.,1.,alpha_test(a,cut,flags));
            }
            if fraction>0. { a = mix(a,level(addr,uv,edge,mip+1u).a,fraction); }
            // Fixed per-pixel/per-sample stratification, never seeded by time.
            if a<=0. || (a<1. && a<=coverage_sample(ray,m,inst,v)) { return false; }
            a = 1.;
        } else {
            if fraction>0. { a = mix(a,level(addr,uv,edge,mip+1u).a,fraction); }
            if (flags & CUTOUT)!=0u {
                let cut = bitcast<f32>(word(m,mat+2u));
                if !alpha_test(a,cut,flags) { return false; }
            }
        }
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
    let c = bitcast<f32>(word(m,inst+4u));
    let s = bitcast<f32>(word(m,inst+5u));
    let raw = rgba(word(m,v.x+3u)).xyz*w.x + rgba(word(m,v.y+3u)).xyz*w.y + rgba(word(m,v.z+3u)).xyz*w.z;
    var n = norm(rotate(raw*2.-1.,c,s));
    if (flags & (WATER|TERRAIN|TWO_SIDED))!=0u {
        if dot(n,ray.dir)>0. { n = -n; }
    } else if (flags & EMISSIVE)==0u {
        let gn = geometric(m,inst,v);
        if (dot(n,gn)<0.) != (dot(gn,ray.dir)>0.) { n = -n; }
    }
    if (flags & TERRAIN)!=0u {
        return Surface(n,uv,vec4<f32>(terrain_color(p,footprint(ray,hit.t)),1.),flags,0.);
    }
    let addr = word(m,mat);
    let tex = tile_lod(addr,uv,(flags & CLAMP_UV)!=0u,tile_lod_for(addr,ray,hit,m,inst,v));
    let tint = tint_of(m,v,w);
    let alpha = select(tex.a,1.,(flags & IGNORE_TEXTURE_ALPHA)!=0u)*tint.a;
    var color = vec4<f32>(linear(tex.rgb)*linear(tint.rgb),alpha);
    if (flags & EMISSIVE)!=0u { color = vec4<f32>(linear(tex.rgb)*tint.rgb,alpha); }
    if (flags & BAKED)!=0u { color = vec4<f32>(linear(min(tex.rgb*tint.rgb*2.,vec3<f32>(1.))),alpha); }
    return Surface(n,uv,color,flags,bitcast<f32>(word(m,mat+3u)));
}
fn none(ray: Ray) -> Hit { return Hit(ray.high,NIL,0u,vec2<f32>(0.)); }
fn trace(ray: Ray, q: Query) -> Hit {
    if ray.low>ray.high || ((world[11u]|moving[11u]) & q.mask)==0u { return none(ray); }
    return intersect(ray,q);
}
fn bias(p: vec3<f32>) -> f32 {
    let a = abs(p);
    return max(0.0005,max(a.x,max(a.y,a.z))*4.77e-7);
}
fn secondary(p: vec3<f32>, d: vec3<f32>, ray: Ray, t: f32) -> Ray {
    let offset = bias(p);
    return Ray(p+d*offset,d,offset,frame.eye.w,footprint(ray,t),ray.spread,ray.coverage);
}
fn visibility(p: vec3<f32>, d: vec3<f32>, t: f32, width: f32, coverage: f32) -> f32 {
    let offset = bias(p);
    if (frame.options.x & SHADOWS)==0u || t<=2.*offset { return 1.; }
    let r = Ray(p+d*offset,d,offset,t-offset,width,0.,coverage);
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
fn sky_triple(d: vec3<f32>, layer: vec3<u32>) -> vec3<f32> {
    // Native camera-centred roof cap and panorama skirt mapping, fed with
    // this ray's direction rather than a rasterized screen coordinate.
    let y = clamp(d.y,0.,1.);
    let cap = -47.685848*y + sqrt(226.05995+2273.93991*y*y);
    let roof_uv = vec2<f32>(0.5) + d.xz*(cap/30.070580);
    let roof = tile(layer.z,roof_uv*frame.sky_uv.z+frame.sky_uv.xy,false).rgb;
    let az = atan2(max(abs(d.z),1.e-7),d.x);
    let v = mix(0.275,0.5,clamp((50.*d.y/max(length(d.xz),1.e-6)) / -10.602875,0.,1.));
    let mirrored = az>=1.5707963;
    let u = clamp(select(1.,2.,mirrored) - 2.*az/3.14159265,0.,1.);
    let other = clamp(select(2.,1.,mirrored) - 2.*az/3.14159265,0.,1.);
    let primary = select(layer.y,layer.x,d.z>=0.);
    let side = select(layer.x,layer.y,d.z>=0.);
    let seam = (1.-smoothstep(0.,0.06,abs(az-1.5707963)))*0.5;
    let a = mix(tile(primary,vec2<f32>(u,v),true).rgb,tile(primary,vec2<f32>(other,v),true).rgb,seam);
    let b = tile(side,vec2<f32>(u,v),true).rgb;
    let skirt = mix(a,b,(1.-smoothstep(0.,0.12,abs(d.z)))*0.5);
    return mix(skirt,roof,smoothstep(-0.015,0.015,d.y));
}
fn environment(dir: vec3<f32>) -> vec3<f32> {
    var sky = vec3<f32>(0.);
    if frame.sky0.x!=0u {
        var encoded = sky_triple(dir,frame.sky0.xyz);
        let f = bitcast<f32>(frame.sky0.w);
        if f>0. && frame.sky1.x!=0u {
            encoded = mix(encoded,sky_triple(dir,frame.sky1.xyz),f);
        }
        sky = linear(encoded*clamp(frame.sky_tint.rgb,vec3<f32>(0.),vec3<f32>(1.)));
    } else {
        let h = clamp(dir.y*1.5,0.,1.);
        let horizon = mix(vec3<f32>(0.022,0.030,0.064),vec3<f32>(0.60,0.72,0.85),frame.sun.w);
        let dome = mix(vec3<f32>(0.004,0.009,0.026),vec3<f32>(0.11,0.31,0.64),frame.sun.w);
        sky = mix(horizon,dome,h);
    }
    let sun = smoothstep(0.9994,0.99985,dot(dir,frame.sun_disc.xyz))*frame.sun_disc.w;
    let moon = smoothstep(0.9985,0.9992,dot(dir,frame.moon_disc.xyz))*frame.moon_disc.w;
    return sky + frame.sun_color.rgb*sun*2. + vec3<f32>(0.45,0.5,0.6)*moon;
}
struct LightRay { dir: vec3<f32>, distance: f32, energy: vec3<f32> }
struct LightCursor { feed: u32, node: u32, item: u32, end_item: u32 }
fn next_light(p: vec3<f32>, n: vec3<f32>, cursor: ptr<function,LightCursor>) -> LightRay {
    loop {
        let m = (*cursor).feed!=0u;
        if (*cursor).item<(*cursor).end_item {
            let index = word(m,word(m,6u)+(*cursor).item);
            let a = word(m,9u) + index*8u;
            (*cursor).item++;
            let to = floats(m,a)-p;
            let r = bitcast<f32>(word(m,a+3u));
            let d2 = dot(to,to);
            let cosine = dot(to,n);
            if d2>=r*r || d2<=1.e-8 || cosine<=0. { continue; }
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
fn direct(p: vec3<f32>, s: Surface, ray: Ray, t: f32) -> vec3<f32> {
    let albedo = s.color.rgb;
    if (s.flags & EMISSIVE)!=0u { return albedo; }
    let lit = (frame.options.x & LIGHTING)!=0u;
    let baked = (s.flags & BAKED)!=0u;
    let cosine = max(dot(s.n,frame.sun.xyz),0.);
    let sun_above = frame.sun.w>0.02 && frame.sun.y>0.;
    var sun_pending = !lit || !baked;
    let width = footprint(ray,t);
    var color = vec3<f32>(1.);
    var sun = frame.sun_color.rgb*cosine;
    if !lit {
        if !sun_above || cosine<=0. || (frame.options.x & SHADOWS)==0u { return albedo; }
        sun = vec3<f32>(0.45);
        color = vec3<f32>(0.55);
    } else if !baked {
        color = frame.ambient.rgb;
    }
    var cursor = LightCursor(0u,world[12u],0u,0u);
    loop {
        var light: LightRay;
        if sun_pending {
            sun_pending = false;
            light = LightRay(frame.sun.xyz,select(0.,frame.eye.w,sun_above && cosine>0.),sun);
        } else {
            if !lit { break; }
            light = next_light(p,s.n,&cursor);
            if light.distance<=0. { break; }
        }
        // One shadow traversal site for sun, static points and GFX points.
        // Drivers inline ray queries, so duplicating the call at each branch
        // can make shader compilation dominate the first frame.
        color += light.energy*visibility(p,light.dir,light.distance,width,ray.coverage);
    }
    return albedo*color;
}
fn random(seed: ptr<function,u32>) -> f32 {
    let value = hash(*seed);
    *seed = *seed*747796405u + 2891336453u;
    return f32(value & 0xffffffu)/16777216.;
}
fn hemisphere(n: vec3<f32>, seed: ptr<function,u32>) -> vec3<f32> {
    let r = random(seed);
    let angle = random(seed)*6.2831853;
    let u = norm(cross(n,select(vec3<f32>(0.,1.,0.),vec3<f32>(1.,0.,0.),abs(n.y)>0.95)));
    let v = cross(n,u);
    return norm((u*cos(angle) + v*sin(angle))*sqrt(r) + n*sqrt(1.-r));
}
fn wave(s: Surface, direction: vec3<f32>) -> vec3<f32> {
    let a = bitcast<f32>(frame.waves.w);
    let uv = s.uv*2. + 0.05*vec2<f32>(cos(a),sin(a));
    var n = vec3<f32>(0.,1.,0.);
    if frame.waves.x != 0u && (s.flags & SIMPLE_WATER)==0u {
        let raw = mix(tile(frame.waves.x,uv,false).rgb,tile(frame.waves.y,uv,false).rgb,
            bitcast<f32>(frame.waves.z))*2.-1.;
        n = norm(vec3<f32>(raw.x*0.42,max(abs(raw.y),0.45),raw.z*0.42));
    }
    n *= select(1.,-1.,direction.y>0.);
    if dot(n,direction)>-0.01 { n = vec3<f32>(0.,select(1.,-1.,direction.y>0.),0.); }
    return n;
}
fn fog_amount(distance: f32, wet: bool) -> f32 {
    let fog = select(frame.air,frame.water,wet);
    if fog.z<0.5 { return 0.; }
    return clamp((distance-fog.x)/max(fog.y-fog.x,0.01),0.,1.);
}
fn world_depth(p: vec3<f32>) -> f32 {
    let clip = frame.vp*vec4<f32>(p,1.);
    return clamp(clip.z/clip.w,0.,1.);
}
fn transport(first: Ray, seed: ptr<function,u32>) -> vec4<f32> {
    var result = vec3<f32>(0.);
    var depth = 0.;
    let wet = (frame.options.x & UNDERWATER)!=0u;
    var path = Path(first,vec3<f32>(1.),0u,VIEW,wet);
    var pending = path;
    var split = false;
    for (var branch=0u; branch<2u; branch++) {
        for (var segment=0u; segment<=frame.options.y; segment++) {
            if path.depth>frame.options.y || max(path.weight.x,max(path.weight.y,path.weight.z))<0.002 { break; }
            let ray = path.ray;
            var solid = none(ray);
            var scan = ray;
            var mask = SOLID_MASK;
            var after = vec2<u32>(NIL);
            var group_end = -1.;
            var s: Surface;
            var color = vec3<f32>(0.);
            var edge = 1.;
            loop {
                // Resolve solid visibility once. Find all eligible sheets in
                // front of it, then (for water) query the real ground below.
                // Use one traversal site for all three jobs.
                let blending = mask==BLEND_MASK;
                let has_weight = max(path.weight.x,max(path.weight.y,path.weight.z))>=0.002;
                let ordered = blending && has_weight && group_end>=0.;
                var r = scan;
                if ordered { r.high = group_end; }
                let query_mask = select(mask,0u,blending && !has_weight);
                let purpose = select(path.purpose,SHORE,mask==SHORE_MASK);
                var hit = trace(r,Query(purpose,query_mask,ordered,after));
                if mask==SHORE_MASK {
                    edge = clamp(hit.t/0.66,0.,1.);
                    break;
                }
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
                if (s.flags & WATER)!=0u {
                    if ray.dir.y<=0. && path.depth<frame.options.y {
                        let down = vec3<f32>(0.,-1.,0.);
                        scan = Ray(p+down*0.001,down,0.001,0.66,0.,0.,ray.coverage);
                        mask = SHORE_MASK;
                        continue;
                    }
                    break;
                }
                color = vec3<f32>(0.);
                if has_weight { color = direct(p,s,ray,hit.t); }
                if !sheet { break; }
                // Coplanar cards blend in decreasing draw-layer order. The
                // two triangles of a card share a key, so diagonals draw once.
                after = layer_key(hit);
                if path.depth==0u && branch==0u && (s.flags & WRITE_DEPTH)!=0u && s.color.a>=0.5 {
                    depth = max(depth,world_depth(p));
                }
                let f = fog_amount(hit.t,path.wet);
                let additive = (s.flags & ADDITIVE)!=0u;
                let mist = select(select(frame.air_color.rgb,frame.water_color.rgb,path.wet),vec3<f32>(0.),additive);
                result += path.weight*s.color.a*mix(color,mist,f);
                if !additive { path.weight *= 1.-s.color.a; }
            }
            if solid.instance==NIL {
                result += path.weight*select(environment(ray.dir),frame.water_color.rgb,path.wet);
                break;
            }
            let p = ray.origin + ray.dir*solid.t;
            if path.depth==0u && branch==0u { depth = max(depth,world_depth(p)); }
            let fog = fog_amount(solid.t,path.wet);
            result += path.weight*fog*select(frame.air_color.rgb,frame.water_color.rgb,path.wet);
            path.weight *= 1.-fog;
            if max(path.weight.x,max(path.weight.y,path.weight.z))<0.002 { break; }
            if (s.flags & WATER)!=0u {
                let n = wave(s,ray.dir);
                let from_water = ray.dir.y > 0.;
                let eta = select(1./1.333,1.333,from_water);
                var t = refract(ray.dir,n,eta);
                let r = norm(reflect(ray.dir,n));
                // Intersection is with the actual flat cell, not its bump
                // map. Keep transmitted/reflected rays on the right half-space.
                if dot(t,t)>0. && t.y*ray.dir.y <= 0. { t = refract(ray.dir,vec3<f32>(0.,select(1.,-1.,from_water),0.),eta); }
                let can_reflect = (frame.options.x & (REFLECTIONS|WATER_MIRROR))==(REFLECTIONS|WATER_MIRROR)
                    && (s.flags & SIMPLE_WATER)==0u;
                let f = select(0.,edge*(0.02+0.98*pow(1.-clamp(dot(-ray.dir,n),0.,1.),5.)),can_reflect);
                let reflected = select(r,vec3<f32>(ray.dir.x,-ray.dir.y,ray.dir.z),r.y*ray.dir.y>=0.);
                let water_tint = mix(vec3<f32>(0.74,0.92,0.94),max(s.color.rgb,vec3<f32>(0.2)),0.22);
                if path.depth==frame.options.y {
                    result += path.weight*mix(frame.water_color.rgb,s.color.rgb,0.4);
                    break;
                }
                path.depth++;
                if dot(t,t)<0.0001 {
                    if !can_reflect { result += path.weight*frame.water_color.rgb; break; }
                    path.ray = secondary(p,reflected,ray,solid.t);
                    path.purpose = MIRROR;
                    continue;
                }
                if f>0.001 && !split && branch==0u {
                    pending = Path(secondary(p,reflected,ray,solid.t),path.weight*f,path.depth,MIRROR,path.wet);
                    split = true;
                    path.weight *= 1.-f;
                } else if f>0.001 && random(seed)<f {
                    path.ray = secondary(p,reflected,ray,solid.t);
                    path.purpose = MIRROR;
                    continue;
                }
                path.weight *= mix(vec3<f32>(1.),water_tint,edge);
                path.wet = !from_water;
                path.ray = secondary(p,t,ray,solid.t);
                path.purpose = TRANSMIT;
                continue;
            }
            let is_mirror = (frame.options.x & REFLECTIONS)!=0u && s.reflectance>0.01;
            let lit = (frame.options.x & LIGHTING)!=0u;
            result += path.weight*color;
            if path.depth==frame.options.y || (s.flags & EMISSIVE)!=0u || (!is_mirror && !lit) { break; }
            path.depth++;
            if is_mirror {
                let r = norm(reflect(ray.dir,s.n));
                path.ray = secondary(p,r,ray,solid.t);
                path.weight *= s.reflectance;
                path.purpose = MIRROR;
            } else {
                let d = hemisphere(s.n,seed);
                path.ray = secondary(p,d,ray,solid.t);
                path.weight *= s.color.rgb*0.28;
                path.purpose = DIFFUSE;
            }
        }
        if !split { break; }
        path = pending;
    }
    return vec4<f32>(max(result,vec3<f32>(0.)),depth);
}

@compute @workgroup_size(8,8)
fn render(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=frame.extent.xy) { return; }
    var seed = id.x + id.y*frame.extent.x + 1u;
    let spread = 2.*max(length(frame.inv_vp[0].xyz)/f32(frame.extent.x),
                        length(frame.inv_vp[1].xyz)/f32(frame.extent.y));
    var total = vec3<f32>(0.);
    var depth = 0.;
    for (var sample=0u; sample<frame.options.z; sample++) {
        let jitter = select(vec2<f32>(0.5),vec2<f32>(random(&seed),random(&seed)),frame.options.z>1u);
        let screen = (vec2<f32>(id.xy)+jitter)/vec2<f32>(frame.extent.xy);
        let ndc = screen*vec2<f32>(2.,-2.)+vec2<f32>(-1.,1.);
        let near4 = frame.inv_vp*vec4<f32>(ndc,1.,1.);
        let far4 = frame.inv_vp*vec4<f32>(ndc,0.,1.);
        let near = near4.xyz/near4.w-frame.eye.xyz;
        let far = far4.xyz/far4.w-frame.eye.xyz;
        let coverage = (f32(sample)+random(&seed))/f32(frame.options.z);
        let out = transport(Ray(frame.eye.xyz,norm(near),length(near),length(far),0.,spread,coverage),&seed);
        total += out.rgb;
        // Reverse Z: take the nearest of the samples for consumers of depth.
        depth = max(depth,out.w);
    }
    image[id.y*frame.extent.x+id.x] = vec4<f32>(total/f32(frame.options.z),depth);
}
