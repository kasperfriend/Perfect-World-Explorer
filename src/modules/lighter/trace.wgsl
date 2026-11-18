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
struct Ray { origin: vec3<f32>, dir: vec3<f32>, low: f32, high: f32 }
struct Surface {
    hit: Hit,
    n: vec3<f32>, geometric: vec3<f32>,
    uv: vec2<f32>, color: vec4<f32>, flags: u32, reflectance: f32,
}
struct Path { ray: Ray, weight: vec3<f32>, depth: u32, purpose: u32, wet: bool }

fn word(dynamic: bool, i: u32) -> u32 {
    if dynamic { return moving[i]; }
    return world[i];
}
fn floats(dynamic: bool, i: u32) -> vec3<f32> {
    return bitcast<vec3<f32>>(vec3<u32>(word(dynamic,i),word(dynamic,i+1u),word(dynamic,i+2u)));
}
fn inst_addr(instance: u32) -> u32 {
    return word((instance & DYNAMIC) != 0u, 4u) + (instance & (DYNAMIC-1u)) * 10u;
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
fn texel(addr: u32, dims: vec2<i32>, p: vec2<i32>, edge: bool) -> vec4<f32> {
    var i = (p % dims + dims) % dims;
    if edge { i = clamp(p, vec2<i32>(0), dims-1); }
    return rgba(texels[addr + 2u + u32(i.y*dims.x + i.x)]);
}
fn tile(addr: u32, uv: vec2<f32>, edge: bool) -> vec4<f32> {
    let dims = vec2<i32>(i32(texels[addr]),i32(texels[addr+1u]));
    let xy = select(fract(uv),clamp(uv,vec2<f32>(0.),vec2<f32>(1.)),edge)*vec2<f32>(dims)-0.5;
    let p = vec2<i32>(floor(xy));
    let f = fract(xy);
    return mix(mix(texel(addr,dims,p,edge), texel(addr,dims,p+vec2<i32>(1,0),edge), f.x),
               mix(texel(addr,dims,p+vec2<i32>(0,1),edge), texel(addr,dims,p+vec2<i32>(1,1),edge), f.x), f.y);
}
fn terrain_color(p: vec3<f32>) -> vec3<f32> {
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
        let c = tile(texels[at],uv*scale,false).rgb;
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
fn surface(hit: Hit, ray: Ray, purpose: u32) -> Surface {
    let dyn = (hit.instance & DYNAMIC) != 0u;
    let inst = inst_addr(hit.instance);
    let v = vertex_addresses(dyn,inst,hit.primitive);
    let mat = word(dyn,3u) + 4u*word(dyn,word(dyn,2u)+hit.primitive);
    let flags = word(dyn,mat+1u);
    let skip = Surface(Hit(hit.t,NIL,0u,vec2<f32>(0.)), vec3<f32>(0.), vec3<f32>(0.), vec2<f32>(0.), vec4<f32>(0.), flags, 0.);
    let p = ray.origin + ray.dir*hit.t;
    if (purpose==SHORE && (flags & TERRAIN)==0u)
        || (purpose==SHADOW && (flags & (BLEND|WATER))!=0u && (flags & WRITE_DEPTH)==0u)
        || (purpose==MIRROR && (flags & NO_REFLECT)!=0u)
        || (purpose==TRANSMIT && (flags & NO_REFRACT)!=0u) { return skip; }
    let weights = vec3<f32>(1.-hit.bary.x-hit.bary.y,hit.bary);
    var uv = vertex_uv(dyn,v.x)*weights.x + vertex_uv(dyn,v.y)*weights.y + vertex_uv(dyn,v.z)*weights.z;
    let c = bitcast<f32>(word(dyn,inst+4u));
    let s = bitcast<f32>(word(dyn,inst+5u));
    let a = floats(dyn,v.x);
    let gn = norm(rotate(cross(floats(dyn,v.y)-a,floats(dyn,v.z)-a),c,s));
    if purpose==VIEW && (frame.options.x & CULL_BUILDINGS)!=0u && (flags & CULL_BACK)!=0u && dot(gn,ray.dir)>=0. { return skip; }
    let raw_normal = rgba(word(dyn,v.x+3u)).xyz*weights.x + rgba(word(dyn,v.y+3u)).xyz*weights.y + rgba(word(dyn,v.z+3u)).xyz*weights.z;
    var n = norm(rotate(raw_normal*2.-1.,c,s));
    if dot(n,gn)<0. && (flags & (WATER|TERRAIN))==0u { n = -n; }
    if dot(gn,ray.dir)>0. { n = -n; }
    // Grass/frond quads are intentionally two-sided.
    if (flags & TWO_SIDED)!=0u && dot(n,ray.dir)>0. { n = -n; }
    if (flags & TERRAIN)!=0u {
        var albedo = vec3<f32>(1.);
        if purpose!=SHADOW && purpose!=SHORE { albedo = terrain_color(p); }
        return Surface(hit,n,gn,uv,vec4<f32>(albedo,1.),flags,0.);
    }
    if (flags & (BLEND|BAKED))==(BLEND|BAKED) { uv.y -= 0.5*frame.air.w; }
    let tex = tile(word(dyn,mat),uv,(flags & CLAMP_UV)!=0u);
    if (flags & CUTOUT)!=0u && tex.a<bitcast<f32>(word(dyn,mat+2u)) { return skip; }
    let tint = vertex_tint(dyn,v.x)*weights.x + vertex_tint(dyn,v.y)*weights.y + vertex_tint(dyn,v.z)*weights.z;
    var color = vec4<f32>(linear(tex.rgb)*linear(tint.rgb),tex.a*tint.a);
    if (flags & EMISSIVE)!=0u { color = vec4<f32>(linear(tex.rgb)*tint.rgb,color.a); }
    if (flags & BAKED)!=0u { color = vec4<f32>(linear(min(tex.rgb*tint.rgb*2.,vec3<f32>(1.))),color.a); }
    if (flags & BLEND)!=0u && color.a<0.003 { return skip; }
    if purpose==SHADOW && (flags & WRITE_DEPTH)!=0u && color.a<0.5 { return skip; }
    return Surface(hit,n,gn,uv,color,flags,bitcast<f32>(word(dyn,mat+3u)));
}
fn epsilon(t: f32) -> f32 {
    // Enough ULPs to cross a rejected hit at the far end of a world ray.
    return max(0.002, abs(t)*0.00000048);
}
fn next_surface(ray: Ray, purpose: u32) -> Surface {
    var r = ray;
    // Both paths use committed geometric hits, with the same alpha rules.
    // Requery beyond cutout/rejected surfaces on both paths for the same rules.
    for (var attempt=0; attempt<64; attempt++) {
        if r.low >= r.high { break; }
        let hit = closest(r);
        if hit.instance==NIL { break; }
        let sample = surface(hit,r,purpose);
        if sample.hit.instance!=NIL { return sample; }
        r.low = hit.t + epsilon(hit.t);
    }
    return Surface(Hit(ray.high,NIL,0u,vec2<f32>(0.)),vec3<f32>(0.),vec3<f32>(0.),vec2<f32>(0.),vec4<f32>(0.),0u,0.);
}
fn visibility(p: vec3<f32>, d: vec3<f32>, t: f32) -> f32 {
    if (frame.options.x & SHADOWS)==0u || t<=0.02 { return 1.; }
    let r = Ray(p+d*0.012,d,0.001,t-0.012);
    return select(0.,1.,next_surface(r,SHADOW).hit.instance==NIL);
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
fn direct(p: vec3<f32>, s: Surface) -> vec3<f32> {
    let albedo = s.color.rgb;
    if (s.flags & EMISSIVE)!=0u { return albedo; }
    let cosine = max(dot(s.n,frame.sun.xyz),0.);
    var sun_vis = 1.;
    if cosine>0. && frame.sun.w>0.02 && frame.sun.y>0. {
        sun_vis = visibility(p,frame.sun.xyz,frame.eye.w);
    }
    let lit = (frame.options.x & LIGHTING)!=0u;
    var color = albedo*(0.55+0.45*sun_vis);
    if !lit { return color; }
    if (s.flags & BAKED)==0u {
        color = albedo*(frame.ambient.rgb + frame.sun_color.rgb*cosine*sun_vis);
    }
    for (var i=0u; i<moving[10u]; i++) {
        let a = moving[9u] + i*8u;
        let to = floats(true,a)-p;
        let r = bitcast<f32>(moving[a+3u]);
        let dist = length(to);
        if dist<r && dist>0.014 {
            let wi = to/dist;
            let lambert = max(dot(wi,s.n),0.);
            if lambert>0. {
                let falloff = 1.-dist/r;
                color += albedo*floats(true,a+4u)*(lambert*falloff*falloff*visibility(p,wi,dist));
            }
        }
    }
    return color;
}
fn random(seed: ptr<function,u32>) -> f32 {
    *seed = *seed*747796405u + 2891336453u;
    let s = *seed;
    let h = ((s >> ((s >> 28u)+4u)) ^ s)*277803737u;
    return f32((h ^ (h >> 22u)) & 0xffffffu)/16777216.;
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
            var ray = path.ray;
            var s = next_surface(ray,path.purpose);
            // Sheets and ribbons composite in ray-distance order. They don't
            // take a bounce or cast the shadow of an opaque rectangle.
            for (var sheet=0u; sheet<24u; sheet++) {
                if s.hit.instance==NIL || (s.flags & BLEND)==0u { break; }
                let f = fog_amount(s.hit.t,path.wet);
                let point = ray.origin+ray.dir*s.hit.t;
                if path.depth==0u && branch==0u && (s.flags & WRITE_DEPTH)!=0u && s.color.a>=0.5 {
                    depth = max(depth,world_depth(point));
                }
                let src = direct(point,s);
                let additive = (s.flags & ADDITIVE)!=0u;
                let mist = select(select(frame.air_color.rgb,frame.water_color.rgb,path.wet),vec3<f32>(0.),additive);
                result += path.weight*s.color.a*mix(src,mist,f);
                if !additive { path.weight *= 1.-s.color.a; }
                ray.low = s.hit.t+epsilon(s.hit.t);
                s = next_surface(ray,path.purpose);
            }
            if s.hit.instance==NIL {
                result += path.weight*select(environment(ray.dir),frame.water_color.rgb,path.wet);
                break;
            }
            let p = ray.origin + ray.dir*s.hit.t;
            if path.depth==0u && branch==0u {
                depth = max(depth,world_depth(p));
            }
            let fog = fog_amount(s.hit.t,path.wet);
            result += path.weight*fog*select(frame.air_color.rgb,frame.water_color.rgb,path.wet);
            path.weight *= 1.-fog;
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
                var edge = 1.;
                if !from_water {
                    // Test the depth to the actual terrain under this wet cell
                    // (not a screen-depth sample and not the nearest props).
                    let down = vec3<f32>(0.,-1.,0.);
                    let ground = next_surface(Ray(p+down*0.001,down,0.001,0.66),SHORE);
                    edge = clamp(ground.hit.t/0.66,0.,1.);
                }
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
                    path.ray = Ray(p+reflected*0.016,reflected,0.001,frame.eye.w);
                    path.purpose = MIRROR;
                    continue;
                }
                if f>0.001 && !split && branch==0u {
                    pending = Path(Ray(p+reflected*0.016,reflected,0.001,frame.eye.w),path.weight*f,path.depth,MIRROR,path.wet);
                    split = true;
                    path.weight *= 1.-f;
                } else if f>0.001 && random(seed)<f {
                    path.ray = Ray(p+reflected*0.016,reflected,0.001,frame.eye.w);
                    path.purpose = MIRROR;
                    continue;
                }
                path.weight *= mix(vec3<f32>(1.),water_tint,edge);
                path.wet = !from_water;
                path.ray = Ray(p+t*0.016,t,0.001,frame.eye.w);
                path.purpose = TRANSMIT;
                continue;
            }
            let is_mirror = (frame.options.x & REFLECTIONS)!=0u && s.reflectance>0.01;
            let lit = (frame.options.x & LIGHTING)!=0u;
            let c = direct(p,s);
            result += path.weight*c;
            if path.depth==frame.options.y || (s.flags & EMISSIVE)!=0u || (!is_mirror && !lit) { break; }
            path.depth++;
            if is_mirror {
                let r = norm(reflect(ray.dir,s.n));
                path.ray = Ray(p+r*0.016,r,0.001,frame.eye.w);
                path.weight *= s.reflectance;
                path.purpose = MIRROR;
            } else {
                let d = hemisphere(s.n,seed);
                path.ray = Ray(p+d*0.016,d,0.001,frame.eye.w);
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
        let out = transport(Ray(frame.eye.xyz,norm(near),length(near),length(far)),&seed);
        total += out.rgb;
        // Reverse Z: take the nearest of the samples for consumers of depth.
        depth = max(depth,out.w);
    }
    image[id.y*frame.extent.x+id.x] = vec4<f32>(total/f32(frame.options.z),depth);
}
