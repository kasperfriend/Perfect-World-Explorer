// Experimental reuse at 1 spp. Each of two ray-resolution pages has three
// Pixel slots per receiver: two for a fresh local-light reservoir and its
// anchor, one for uncomposited primary direct radiance. Never read the page
// this dispatch writes. Postprocessing only names the first (image) plane.
const SCREEN_DI: u32 = 1u;
const SCREEN_GI: u32 = 2u;
const SCREEN_PREVIOUS: u32 = 4u;
const SCREEN_GI_PREVIOUS: u32 = 8u;
var<private> screen_pixel: u32;
var<private> screen_read: u32;
var<private> screen_write: u32;
var<private> screen_gi_hit: bool;

fn screen_begin(at: u32) {
    screen_pixel = at;
    screen_read = 0u;
    screen_write = 0u;
    screen_gi_hit = false;
    if (frame.sampling.w & (SCREEN_DI|SCREEN_GI))==0u { return; }
    let count = frame.extent.x*frame.extent.y;
    let page = frame.sampling.x & 1u;
    screen_read = count*(1u+3u*(page^1u));
    screen_write = count*(1u+3u*page)+3u*at;
    // Written to every lane even if this ray hits sky, water or a sheet.
    image[screen_write+1u].owner = 0u;
    image[screen_write+2u].depth = 0.;
    image[screen_write+2u].normal_flags = 0u;
}

fn screen_end() {
    if screen_gi_hit { image[screen_write+2u].normal_flags |= 0x80000000u; }
}

fn screen_surface(s: Surface, hit: Hit) -> bool {
    return (hit.instance & DYNAMIC)==0u &&
        (s.flags & (WATER|GLASS|BAKED|EMISSIVE|BLEND|CUTOUT|UV_TRANSFORM|TREE_MATERIAL|GRASS_MATERIAL|FOLIAGE))==0u;
}

struct Anchor { p: vec3<f32>, n: vec3<f32>, face: vec3<f32> }
struct Reservoir { anchor: Anchor, tag: vec2<u32>, inv_pmf: f32, live: bool }

fn previous_reservoir(at: u32, current: Anchor) -> Reservoir {
    let a = image[at];
    let b = image[at+1u];
    let source = Anchor(vec3<f32>(bitcast<f32>(a.rg),bitcast<f32>(a.bv),a.depth),
                        vec3<f32>(a.view_depth,bitcast<f32>(a.normal_flags),bitcast<f32>(a.albedo_history)),
                        vec3<f32>(bitcast<f32>(b.rg),bitcast<f32>(b.bv),b.depth));
    let radius = max(0.04,0.035*length(current.p-frame.eye.xyz));
    let valid = b.owner!=0u && distance(source.p,current.p)<=radius &&
                 dot(source.n,current.n)>0.9 && dot(source.face,current.face)>0.95;
    // The empty *result* of a valid proposal still has a receiver and a PDF
    // support function. Don't condition source admission on its chosen light.
    return Reservoir(source,vec2<u32>(a.owner,b.normal_flags),b.view_depth,valid);
}

fn replay_light(anchor: Anchor, tag: vec2<u32>) -> LightRay {
    var seed = tag.y;
    return local_light(anchor.p,anchor.n,anchor.face,tag.x,&seed);
}

fn screen_lights(p: vec3<f32>, s: Surface, tag: vec2<u32>, inv_pmf: f32, fresh: LightRay, seed: ptr<function,u32>) -> LightRay {
    var only_fresh = fresh;
    only_fresh.energy *= inv_pmf;
    if (frame.sampling.w & SCREEN_DI)==0u { return only_fresh; }
    let anchor = Anchor(p,s.n,s.face);
    image[screen_write] = Pixel(bitcast<u32>(p.x),bitcast<u32>(p.y),p.z,s.n.x,
        bitcast<u32>(s.n.y),bitcast<u32>(s.n.z),tag.x);
    image[screen_write+1u] = Pixel(bitcast<u32>(s.face.x),bitcast<u32>(s.face.y),s.face.z,inv_pmf,tag.y,0u,1u);
    if (frame.sampling.w & SCREEN_PREVIOUS)==0u { return only_fresh; }
    var proposals: array<Reservoir,3>;
    proposals[0] = Reservoir(anchor,tag,inv_pmf,true);
    proposals[1] = previous_reservoir(screen_read+3u*screen_pixel,anchor);
    // Rotate across cardinal neighbors without selecting on sample radiance.
    let xy = vec2<i32>(i32(screen_pixel%frame.extent.x),i32(screen_pixel/frame.extent.x));
    let side = (hash(screen_pixel)^frame.sampling.x)&3u;
    let pos = xy + select(vec2<i32>(select(-1,1,side==0u),0),vec2<i32>(0,select(-1,1,side==2u)),side>=2u);
    if all(pos>=vec2<i32>(0)) && all(pos<vec2<i32>(frame.extent.xy)) {
        proposals[2] = previous_reservoir(screen_read+3u*(u32(pos.y)*frame.extent.x+u32(pos.x)),anchor);
    }
    if !proposals[1].live && !proposals[2].live { return only_fresh; }
    // Reservoir selection must not advance the later BSDF path samples.
    var pick_seed = hash(*seed ^ 0x51ed270bu);
    var selected = empty_light();
    var chosen_power = 0.;
    var weight_sum = 0.;
    var chosen_source = 0u;
    for (var i=0u; i<3u; i++) {
        let q = proposals[i];
        if !q.live || q.inv_pmf<=0. { continue; }
        var candidate = fresh;
        var power: array<f32,3>;
        for (var j=0u; j<3u; j++) {
            if !proposals[j].live { continue; }
            var light = fresh;
            if i!=0u || j!=0u { light = replay_light(proposals[j].anchor,q.tag); }
            power[j] = local_power(light);
            if j==0u { candidate = light; }
        }
        let here = power[0];
        if here<=0. { continue; }
        // Partition unity over the receivers whose proposals can generate
        // this sample. This avoids dropping power at a light's range or
        // hemisphere boundary. Normalize the *fresh* source PMF, not the
        // prior merged reservoir, and always trace current visibility.
        let w = here*q.inv_pmf*(power[i]/(power[0]+power[1]+power[2]));
        weight_sum += w;
        if random(&pick_seed)*weight_sum<w {
            selected = candidate;
            chosen_power = here;
            chosen_source = i;
        }
    }
    if chosen_power>0. { selected.energy *= weight_sum/chosen_power; }
    image[screen_write+1u].albedo_history = chosen_source;
    return selected;
}

fn screen_gi_allowed(s: Surface, hit: Hit) -> bool {
    return screen_surface(s,hit) && !physical_material(s) && s.glaze.a<=0.;
}

fn screen_gi_read(p: vec3<f32>, s: Surface, hit: Hit) -> vec4<f32> {
    if (frame.sampling.w & SCREEN_GI_PREVIOUS)==0u || !screen_gi_allowed(s,hit) { return vec4<f32>(0.); }
    let clip = frame.vp*vec4<f32>(p,1.);
    if clip.w<=0. { return vec4<f32>(0.); }
    let ndc = clip.xyz/clip.w;
    let uv = ndc.xy*vec2<f32>(0.5,-0.5)+0.5;
    if any(uv<vec2<f32>(0.)) || any(uv>=vec2<f32>(1.)) || ndc.z<=0. || ndc.z>1. { return vec4<f32>(0.); }
    let ij = vec2<u32>(uv*vec2<f32>(frame.extent.xy));
    if any(ij>=frame.extent.xy) { return vec4<f32>(0.); }
    let old = image[screen_read+3u*(ij.y*frame.extent.x+ij.x)+2u];
    if old.depth<=0. || old.owner!=hit.primitive || old.view_depth!=f32(hit.instance) ||
        abs(old.depth-ndc.z)>0.002*ndc.z+0.00001 ||
        (((old.albedo_history>>24u) & (frame.sampling.w>>8u)) & ANIM_MASK)!=0u { return vec4<f32>(0.); }
    if dot(unpack4x8snorm(old.normal_flags).xyz,s.n)<0.99 ||
        strongest(abs(unpack4x8unorm(old.albedo_history).rgb-s.color.rgb))>0.015 { return vec4<f32>(0.); }
    screen_gi_hit = true;
    // Cached primary direct excluded the legacy terminal ambient term.
    return vec4<f32>(color_of(old)+frame.ambient.rgb*s.ambient,1.);
}

fn screen_gi_write(p: vec3<f32>, s: Surface, hit: Hit, color: vec3<f32>) {
    if (frame.sampling.w & SCREEN_GI)==0u || !screen_gi_allowed(s,hit) || motion_dependent { return; }
    var entry = Pixel(0u,0u,world_depth(p),f32(hit.instance),pack4x8snorm(vec4<f32>(s.n,0.)),
        (pack4x8unorm(vec4<f32>(s.color.rgb,0.)) & 0xffffffu) | ((dependencies & ANIM_MASK)<<24u),hit.primitive);
    write_radiance(&entry,color,0.);
    image[screen_write+2u] = entry;
}
