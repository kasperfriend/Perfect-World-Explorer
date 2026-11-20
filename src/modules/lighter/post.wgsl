struct Params {
    extent: vec4<u32>,
    history: vec4<u32>,
    previous_from_current: mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage,read> input_pixels: array<Pixel>;
@group(0) @binding(2) var<storage,read_write> output_pixels: array<Pixel>;
@group(0) @binding(3) var<storage,read> previous_pixels: array<Pixel>;

fn overlay_mode() -> bool { return params.history.w != 0u; }
fn unpack_sa(p: Pixel) -> vec2<f32> {
    return vec2<f32>(f32(p.albedo_history & 0xfffu) / 4095.0, f32((p.albedo_history >> 12u) & 0xfffu) / 4095.0);
}
fn pack_sa(sa: vec2<f32>, hi: u32) -> u32 {
    let s = u32(round(clamp(sa.x, 0.0, 1.0) * 4095.0)) & 0xfffu;
    let a = u32(round(clamp(sa.y, 0.0, 1.0) * 4095.0)) & 0xfffu;
    return (hi & 0xff000000u) | s | (a << 12u);
}
fn guide_weight_mode(a: vec4<u32>, b: vec4<u32>) -> f32 {
    if overlay_mode() {
        return guide_weight(vec4<u32>(a.x, a.y, 0u, a.w), vec4<u32>(b.x, b.y, 0u, b.w));
    }
    return guide_weight(a, b);
}

fn same_history(old: Pixel, current: Pixel, expected_w: f32, reproject: bool) -> bool {
    let a = guide_of(old);
    let b = guide_of(current);
    let animations = (a.w|b.w) & ANIM_MASK;
    if (old.owner & OWNER_MASK)!=(current.owner & OWNER_MASK)
        || (animations & params.history.y)!=0u || ((a.w^b.w) & GUIDE_KIND)!=0u
        || ((params.history.y & 256u)!=0u && ((old.owner|current.owner) & DEP_MOVING)!=0u) { return false; }
    if !reproject {
        return a.x==b.x && a.y==b.y && (overlay_mode() || a.z==b.z) && old.depth==current.depth;
    }
    return old.view_depth>0. && abs(old.view_depth-expected_w)<0.025*expected_w
        && guide_weight_mode(vec4<u32>(a.x,b.yzw),a)>=0.5;
}

// Clamp only a reprojected sample, and only from a usable neighborhood.
// Per-channel bounds catch old disoccluded color without eroding the mean
// of a stationary, high-variance light estimate. Depth is never averaged.
fn clipped_history(old: vec3<f32>, current: Pixel, coord: vec2<u32>) -> vec3<f32> {
    let here = color_of(current);
    var hi = here;
    var lo = here;
    var count = 0u;
    for (var i=0u; i<4u; i++) {
        let d = select(-1,1,(i & 1u)!=0u);
        let pos = vec2<i32>(coord) + select(vec2<i32>(d,0),vec2<i32>(0,d),i>=2u);
        if any(pos<vec2<i32>(0)) || any(pos>=vec2<i32>(params.extent.xy)) { continue; }
        let neighbor = input_pixels[u32(pos.y)*params.extent.x+u32(pos.x)];
        if (neighbor.owner & OWNER_MASK)!=(current.owner & OWNER_MASK)
            || guide_weight_mode(guide_of(neighbor),guide_of(current))<0.5 { continue; }
        let c = color_of(neighbor);
        hi = max(hi,c);
        lo = min(lo,c);
        count++;
    }
    return select(old,clamp(old,lo,hi),count>=2u);
}

fn temporal(current: Pixel, coord: vec2<u32>, i: u32) -> Pixel {
    let reproject = params.history.z!=0u;
    var at = i;
    var expected_w = 0.;
    if reproject {
        // No reliable single surface position for sky, water,
        // layered transparency or multi-sample silhouettes.
        if current.view_depth==0. || ((current.normal_flags>>24u) & (GUIDE_UNSAFE|GUIDE_WATER))!=0u { return current; }
        let screen = (vec2<f32>(coord)+0.5)/vec2<f32>(params.extent.xy);
        let ndc = screen*vec2<f32>(2.,-2.)+vec2<f32>(-1.,1.);
        let previous = params.previous_from_current*vec4<f32>(ndc,current.depth,1.);
        if previous.w<=0. { return current; }
        let p = previous.xyz/previous.w;
        let uv = p.xy*vec2<f32>(0.5,-0.5)+0.5;
        if any(uv<vec2<f32>(0.)) || any(uv>=vec2<f32>(1.)) || p.z<0. || p.z>1. { return current; }
        let pos = uv*vec2<f32>(params.extent.xy);
        let travel = abs(pos-vec2<f32>(coord)-0.5);
        if max(travel.x,travel.y)>32. { return current; }
        let sample = vec2<u32>(pos);
        if any(sample>=params.extent.xy) { return current; }
        at = sample.x + sample.y*params.extent.x;
        expected_w = previous.w*current.view_depth;
    }
    let old = previous_pixels[at];
    if !same_history(old,current,expected_w,reproject) { return current; }
    let count = min(MAX_HISTORY,((old.albedo_history >> HISTORY_SHIFT) & HISTORY_COUNT)+1u);
    let b = color_of(current);
    var a = color_of(old);
    if reproject { a = clipped_history(a,current,coord); }
    let old_mean = luminance(a);
    let old_variance = max(0.,variance_of(old));
    let noise = sqrt(old_variance)/max(0.04,old_mean);
    // High-variance pixels benefit from a longer window. Moving the view
    // reduces its confidence even when the old hit passes all guide tests.
    let floor_alpha = select(mix(0.10,0.035,smoothstep(0.05,0.4,noise)),0.12,reproject);
    let alpha = max(floor_alpha,1./f32(count));
    let delta = luminance(b)-old_mean;
    // Weighted central second moment; E[Y] is luminance of the stored mean RGB.
    let variance = (1.-alpha)*(old_variance+alpha*delta*delta);
    var out = current;
    if overlay_mode() {
        let sa = mix(unpack_sa(old), unpack_sa(current), alpha);
        out.albedo_history = pack_sa(sa, count << HISTORY_SHIFT);
    } else {
        out.albedo_history |= count << HISTORY_SHIFT;
    }
    out.normal_flags |= old.normal_flags & (ANIM_MASK << 24u);
    out.owner |= old.owner & DEP_MOVING;
    write_radiance(&out,mix(a,b,alpha),variance);
    return out;
}

@compute @workgroup_size(8,8)
fn accumulate(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=params.extent.xy) { return; }
    let i = id.y*params.extent.x+id.x;
    var current = input_pixels[i];
    if params.history.x!=0u { current = temporal(current,id.xy,i); }
    if ((current.albedo_history >> HISTORY_SHIFT) & HISTORY_COUNT)==0u { current.albedo_history |= 1u << HISTORY_SHIFT; }
    output_pixels[i] = current;
}

fn atrous(coord: vec2<u32>, step: i32) {
    let extent = params.extent.xy;
    if any(coord>=extent) { return; }
    let i = coord.y*extent.x+coord.x;
    var center = input_pixels[i];
    let guide = guide_of(center);
    // No spatial filtering of sky, shifted mirror/refracted silhouettes, or
    // colors with no unique owner. Leave their reverse-Z and temporal mean intact.
    if guide.x==0u || (guide.w & (GUIDE_UNSAFE|GUIDE_EMISSIVE|GUIDE_WATER))!=0u {
        output_pixels[i] = center;
        return;
    }
    let radiance = color_of(center);
    let mean = luminance(radiance);
    let variance = variance_of(center);
    let n = (center.albedo_history >> HISTORY_SHIFT) & HISTORY_COUNT;
    if !overlay_mode() && step>=4 && (n<3u || variance<0.004*max(0.0025,mean*mean)) {
        output_pixels[i] = center;
        return;
    }
    let sigma = sqrt(max(0.,variance));
    let phi = 2.5*sigma+max(0.025,0.15*mean);
    var sum = radiance*4.;
    var sa_sum = unpack_sa(center)*4.;
    var total = 4.;
    var sa_total = 4.;
    var sum_var = variance*16.;
    let is_overlay = overlay_mode();
    for (var y=-1; y<=1; y++) {
        for (var x=-1; x<=1; x++) {
            if x==0 && y==0 { continue; }
            let at = vec2<i32>(coord)+step*vec2<i32>(x,y);
            if any(at<vec2<i32>(0)) || any(at>=vec2<i32>(extent)) { continue; }
            let neighbor = input_pixels[u32(at.y)*extent.x+u32(at.x)];
            if (neighbor.owner & OWNER_MASK)!=(center.owner & OWNER_MASK) { continue; }
            let geo = guide_weight_mode(guide,guide_of(neighbor));
            if geo<=0. { continue; }
            let c = color_of(neighbor);
            let kernel_w = f32((2-abs(x))*(2-abs(y)))*geo;
            let photometric = select(1., exp(-abs(mean-luminance(c))/phi), n>=3u);
            let weight = kernel_w*photometric;
            sum += c*weight;
            sum_var += variance_of(neighbor)*weight*weight;
            total += weight;
            if is_overlay {
                sa_sum += unpack_sa(neighbor)*kernel_w;
                sa_total += kernel_w;
            }
        }
    }
    write_radiance(&center,sum/total,sum_var/(total*total));
    if is_overlay {
        center.albedo_history = pack_sa(sa_sum/sa_total, center.albedo_history);
    }
    output_pixels[i] = center;
}

@compute @workgroup_size(8,8) fn filter_1(@builtin(global_invocation_id) id: vec3<u32>) { atrous(id.xy,1); }
@compute @workgroup_size(8,8) fn filter_2(@builtin(global_invocation_id) id: vec3<u32>) { atrous(id.xy,2); }
@compute @workgroup_size(8,8) fn filter_4(@builtin(global_invocation_id) id: vec3<u32>) { atrous(id.xy,4); }
@compute @workgroup_size(8,8) fn filter_8(@builtin(global_invocation_id) id: vec3<u32>) { atrous(id.xy,8); }
@compute @workgroup_size(8,8) fn filter_16(@builtin(global_invocation_id) id: vec3<u32>) { atrous(id.xy,16); }
