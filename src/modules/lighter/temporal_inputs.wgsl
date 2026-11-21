struct Params {
    current: mat4x4<f32>, inverse_jittered: mat4x4<f32>,
    previous: mat4x4<f32>, previous_jittered: mat4x4<f32>,
    eye: vec4<f32>, lens: vec4<f32>, shape: vec4<u32>, changed: vec4<u32>,
}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage,read> raw: array<Pixel>;
@group(0) @binding(2) var<storage,read> poses: array<u32>;
@group(0) @binding(3) var<storage,read> previous_geometry: array<vec2<f32>>;
@group(0) @binding(4) var<storage,read_write> current_geometry: array<vec2<f32>>;
@group(0) @binding(5) var color: texture_storage_2d<rgba16float,write>;
@group(0) @binding(6) var depth: texture_storage_2d<r32float,write>;
@group(0) @binding(7) var velocity: texture_storage_2d<rg32float,write>;
@group(0) @binding(8) var rejection: texture_storage_2d<r32float,write>;
@group(0) @binding(9) var diffuse: texture_storage_2d<rgba16float,write>;
@group(0) @binding(10) var specular: texture_storage_2d<rgba16float,write>;
@group(0) @binding(11) var normal_roughness: texture_storage_2d<rgba16float,write>;
@group(0) @binding(12) var specular_distance: texture_storage_2d<r32float,write>;

@group(0) @binding(13) var current_mask: texture_2d<f32>;
@group(0) @binding(14) var fsr_reactive: texture_storage_2d<r32float,write>;
@group(0) @binding(15) var fsr_composition: texture_storage_2d<r32float,write>;

const DYNAMIC: u32 = 0x800000u;
const NIL: u32 = 0xffffffffu;
fn xyz(at: u32) -> vec3<f32> {
    return bitcast<vec3<f32>>(vec3<u32>(poses[at],poses[at+1u],poses[at+2u]));
}
fn row(at: u32) -> vec4<f32> { return vec4<f32>(xyz(at),bitcast<f32>(poses[at+3u])); }
fn previous_hit(hit: Pixel) -> vec4<f32> {
    let instance = hit.rg & (DYNAMIC-1u);
    if instance>=params.shape.w { return vec4<f32>(0.); }
    let at = instance*16u;
    let base = poses[at];
    let indices = vec3<u32>(hit.bv,bitcast<u32>(hit.depth),bitcast<u32>(hit.view_depth));
    if base==0u || any(indices>=vec3<u32>(poses[at+1u])) { return vec4<f32>(0.); }
    let v = base+indices*3u;
    let b = bitcast<vec2<f32>>(vec2<u32>(hit.normal_flags,hit.albedo_history));
    let local = vec4<f32>(xyz(v.x)*(1.-b.x-b.y)+xyz(v.y)*b.x+xyz(v.z)*b.y,1.);
    return vec4<f32>(dot(row(at+4u),local),dot(row(at+8u),local),dot(row(at+12u),local),1.);
}

fn screen(clip: vec4<f32>) -> vec2<f32> {
    return (clip.xy/clip.w*vec2<f32>(0.5,-0.5)+0.5)*vec2<f32>(params.shape.xy);
}
// xy are current -> previous, unjittered, in render pixels; +Y is down.
// z: 1 = no single valid surface/unsupported, .9 = conservative disocclusion.
fn reprojection(id: vec2<u32>, p: Pixel, hit: Pixel) -> vec3<f32> {
    let flags = p.normal_flags>>24u;
    if hit.owner!=0u || (flags & (GUIDE_UNSAFE|GUIDE_WATER|params.changed.x))!=0u {
        return vec3<f32>(0.,0.,1.);
    }
    if params.shape.z==0u { return vec3<f32>(0.); }
    let sky = hit.rg==NIL;
    let ndc = (vec2<f32>(id)+0.5)/vec2<f32>(params.shape.xy)*vec2<f32>(2.,-2.)+vec2<f32>(-1.,1.);
    let h = params.inverse_jittered*vec4<f32>(ndc,p.depth,1.);
    if !(abs(h.w)>1.e-12) { return vec3<f32>(0.,0.,1.); }
    let point = h.xyz/h.w;
    // Use direction, not a depth=far point, for a sky following camera rotation.
    let current = select(vec4<f32>(point,1.), vec4<f32>(point-params.eye.xyz,0.), sky);
    var old = current;
    if !sky && (hit.rg & DYNAMIC)!=0u {
        old = previous_hit(hit);
        if old.w==0. { return vec3<f32>(0.,0.,1.); }
    }
    let now = params.current*current;
    let prev = params.previous*old;
    let past = params.previous_jittered*old;
    if !(now.w>0. && prev.w>0. && past.w>0.) { return vec3<f32>(0.,0.,0.9); }
    let mv = screen(prev)-screen(now);
    let s = screen(past);
    if !all(abs(mv)<=vec2<f32>(65503.)) { return vec3<f32>(0.,0.,0.9); }
    if !(all(s>=vec2<f32>(0.)) && all(s<vec2<f32>(params.shape.xy))) { return vec3<f32>(mv,0.9); }
    let uv = vec2<u32>(s);
    let old_pixel = previous_geometry[uv.y*params.shape.x+uv.x];
    let z = old_pixel.x;
    var reject = old_pixel.y>=1.;
    if sky { reject = reject || z>0.; }
    else {
        // Both cameras have the same finite reversed-Z lens for a valid
        // history pair. Compare relative *view depth*, not distant tiny Zs.
        let lens = params.lens.xy;
        let previous_view = lens.x*lens.y/(lens.x+z*(lens.y-lens.x));
        reject = reject || !(z>0. && abs(previous_view-past.w)<=0.02*min(previous_view,past.w));
    }
    // A static receiver can still have an animated shadow/reflection.
    reject = reject || (p.owner & DEP_MOVING)!=0u && !sky && (hit.rg & DYNAMIC)==0u;
    return vec3<f32>(mv,select(0.,0.9,reject));
}

@compute @workgroup_size(8,8)
fn pack_inputs(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=params.shape.xy) { return; }
    let n = params.shape.x*params.shape.y;
    let at = id.y*params.shape.x+id.x;
    let p = raw[at];
    let mv = reprojection(id.xy,p,raw[at+n]);
    textureStore(color,id.xy,vec4<f32>(color_of(p),1.));
    textureStore(depth,id.xy,vec4<f32>(p.depth,0.,0.,0.));
    textureStore(velocity,id.xy,vec4<f32>(mv.xy,0.,0.));
    textureStore(rejection,id.xy,vec4<f32>(mv.z,0.,0.,0.));
    current_geometry[at] = vec2<f32>(p.depth,mv.z);
}

@compute @workgroup_size(8,8)
fn pack_guides(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=params.shape.xy) { return; }
    let n = params.shape.x*params.shape.y;
    let g = raw[id.y*params.shape.x+id.x+2u*n];
    let ds = unpack2x16float(g.bv);
    let nr = unpack2x16float(g.albedo_history);
    textureStore(diffuse,id.xy,vec4<f32>(unpack2x16float(g.rg),ds.x,1.));
    textureStore(specular,id.xy,vec4<f32>(ds.y,unpack2x16float(bitcast<u32>(g.depth)),1.));
    textureStore(normal_roughness,id.xy,vec4<f32>(unpack2x16float(g.normal_flags),nr));
    textureStore(specular_distance,id.xy,vec4<f32>(g.view_depth,0.,0.,0.));
}

@compute @workgroup_size(8,8)
fn pack_fsr_masks(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=params.shape.xy) { return; }
    let p = raw[id.y*params.shape.x+id.x];
    let flags = p.normal_flags>>24u;
    // FSR reactivity weights fresh color; the composition mask disables
    // history locking on layered/transmissive/material-animation samples.
    // A geometry disocclusion does not itself make that pixel a layer.
    let reject = textureLoad(current_mask,id.xy,0).x;
    let special = (flags & (GUIDE_UNSAFE|GUIDE_WATER|params.changed.x))!=0u;
    textureStore(fsr_reactive,id.xy,vec4<f32>(min(reject,0.9),0.,0.,0.));
    textureStore(fsr_composition,id.xy,vec4<f32>(f32(special),0.,0.,0.));
}
