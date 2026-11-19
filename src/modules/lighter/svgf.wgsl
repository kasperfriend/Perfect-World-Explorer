// Prototype for isolated (reflection RGB, direct sun visibility A) planes.
// Never use on composited color: sheet ownership, emission and transmission
// are not interchangeable with these noisy stochastic terms.
struct Guide {
    normal: vec3<f32>, depth: f32,
    // Raster motion in pixels: current + motion = previous center.
    motion: vec2<f32>, material: u32, valid: u32,
}
struct Record {
    signal: vec4<f32>,
    moments: vec4<f32>, // E[luma], E[luma²], E[visibility], E[visibility²]
    count: vec4<f32>,
}
struct Params { size: vec2<u32>, step: u32, reset: u32 }
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage,read> guide: array<Guide>;
@group(0) @binding(2) var<storage,read> prior_guide: array<Guide>;
@group(0) @binding(3) var<storage,read> source: array<Record>;
@group(0) @binding(4) var<storage,read> history: array<Record>;
@group(0) @binding(5) var<storage,read_write> out: array<Record>;

fn lum(c: vec3<f32>) -> f32 { return dot(c,vec3<f32>(0.2126,0.7152,0.0722)); }
fn inside(i: vec2<i32>) -> bool { return all(i>=vec2<i32>(0)) && all(i<vec2<i32>(p.size)); }
fn idx(i: vec2<u32>) -> u32 { return i.y*p.size.x+i.x; }
fn compatible(a: Guide,b: Guide) -> bool {
    return a.valid==1u && b.valid==1u && a.material==b.material &&
        abs(a.depth-b.depth)<=max(0.001,0.02*min(a.depth,b.depth)) && dot(a.normal,b.normal)>=0.9;
}

@compute @workgroup_size(8,8)
fn moments(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=p.size) { return; }
    let i = idx(id.xy);
    let cur = source[i].signal;
    let g = guide[i];
    let l = lum(cur.rgb);
    let m = vec4<f32>(l,l*l,cur.a,cur.a*cur.a);
    out[i] = Record(cur,m,vec4<f32>(1.));
    if p.reset!=0u || !all(abs(g.motion)<vec2<f32>(p.size)) { return; }
    let prev = vec2<i32>(round(vec2<f32>(id.xy)+g.motion));
    if !inside(prev) { return; }
    let j = idx(vec2<u32>(prev));
    if !compatible(g,prior_guide[j]) { return; }
    let count = clamp(history[j].count.x+1.,1.,32.);
    out[i] = Record(mix(history[j].signal,cur,1./count),mix(history[j].moments,m,1./count),vec4<f32>(count));
}

@compute @workgroup_size(8,8)
fn atrous(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=p.size) { return; }
    let i = idx(id.xy);
    let g = guide[i];
    let center = source[i];
    out[i] = center;
    if g.valid!=1u { return; }
    let kernel = array<f32,5>(1.,4.,6.,4.,1.);
    let m = center.moments;
    let sigma = sqrt(max(vec2<f32>(0.0004),vec2<f32>(m.y-m.x*m.x,m.w-m.z*m.z)));
    var weights = vec2<f32>(0.);
    var refl = vec3<f32>(0.);
    var shadow = 0.;
    for (var y=-2; y<=2; y++) {
        for (var x=-2; x<=2; x++) {
            let j = vec2<i32>(id.xy) + vec2<i32>(x,y)*i32(p.step);
            if !inside(j) { continue; }
            let at = idx(vec2<u32>(j));
            let next = guide[at];
            if !compatible(g,next) { continue; }
            let v = source[at].signal;
            let variance_weight = exp(-abs(vec2<f32>(lum(v.rgb)-lum(center.signal.rgb),v.a-center.signal.a))/(3.*sigma));
            let edge = pow(clamp(dot(g.normal,next.normal),0.,1.),32.) * exp(-abs(g.depth-next.depth)/max(0.001,0.01*g.depth));
            let w = variance_weight * (kernel[u32(x+2)]*kernel[u32(y+2)]*edge);
            weights += w;
            refl += w.x*v.rgb;
            shadow += w.y*v.a;
        }
    }
    if all(weights>vec2<f32>(0.)) { out[i].signal = vec4<f32>(refl/weights.x, shadow/weights.y); }
}
