struct Params {
    extent: vec4<u32>,
    history: vec4<u32>,
    previous_from_current: mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage,read> input_pixels: array<Pixel>;
@group(0) @binding(2) var<storage,read_write> output_pixels: array<Pixel>;
@group(0) @binding(3) var<storage,read> previous_pixels: array<Pixel>;

fn same_history(old: Pixel, current: Pixel, expected_w: f32, reproject: bool) -> bool {
    let animations = (old.guide.w|current.guide.w) & ANIM_MASK;
    if (animations & params.history.y)!=0u || ((old.guide.w^current.guide.w) & GUIDE_KIND)!=0u { return false; }
    if !reproject { return all(old.guide.xyz==current.guide.xyz); }
    let previous_w = bitcast<f32>(old.guide.x);
    return previous_w>0. && abs(previous_w-expected_w)<0.01*expected_w
        && all(old.guide.yz==current.guide.yz);
}

fn temporal(current: Pixel, coord: vec2<u32>, i: u32) -> Pixel {
    let reproject = params.history.z!=0u;
    var at = i;
    var expected_w = 0.;
    if reproject {
        // No reliable single surface position for sky, moving water,
        // layered transparency or multi-sample silhouettes.
        if current.guide.x==0u || (current.guide.w & (GUIDE_UNSAFE|GUIDE_WATER))!=0u { return current; }
        let screen = (vec2<f32>(coord)+0.5)/vec2<f32>(params.extent.xy);
        let ndc = screen*vec2<f32>(2.,-2.)+vec2<f32>(-1.,1.);
        let previous = params.previous_from_current*vec4<f32>(ndc,current.radiance.a,1.);
        if previous.w<=0. { return current; }
        let p = previous.xyz/previous.w;
        let uv = p.xy*vec2<f32>(0.5,-0.5)+0.5;
        if any(uv<vec2<f32>(0.)) || any(uv>=vec2<f32>(1.)) || p.z<0. || p.z>1. { return current; }
        let sample = vec2<u32>(uv*vec2<f32>(params.extent.xy));
        at = sample.x + sample.y*params.extent.x;
        expected_w = previous.w*bitcast<f32>(current.guide.x);
    }
    let old = previous_pixels[at];
    if !same_history(old,current,expected_w,reproject) { return current; }
    let count = min(MAX_HISTORY,(old.guide.w >> HISTORY_SHIFT)+1u);
    let blend = max(0.1,1./f32(count));
    let guide = current.guide.w | (old.guide.w & ANIM_MASK) | (count << HISTORY_SHIFT);
    return Pixel(vec4<f32>(mix(old.radiance.rgb,current.radiance.rgb,blend),current.radiance.a), vec4<u32>(current.guide.xyz,guide));
}

@compute @workgroup_size(8,8)
fn accumulate(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=params.extent.xy) { return; }
    let i = id.y*params.extent.x+id.x;
    var current = input_pixels[i];
    if params.history.x!=0u { current = temporal(current,id.xy,i); }
    if current.guide.w >> HISTORY_SHIFT==0u { current.guide.w |= 1u << HISTORY_SHIFT; }
    output_pixels[i] = current;
}

@compute @workgroup_size(8,8)
fn denoise(@builtin(global_invocation_id) id: vec3<u32>) {
    let extent = params.extent;
    if any(id.xy>=extent.xy) { return; }
    let i = id.y*extent.x+id.x;
    var center = input_pixels[i];
    // A translucent composite and a mixed silhouette have no single guide.
    // Don't smear either across its neighbors. Sky and emissive are clean;
    // the water interface normal also says little about reflected silhouettes.
    if center.guide.x!=0u && (center.guide.w & (GUIDE_UNSAFE|GUIDE_EMISSIVE|GUIDE_WATER))==0u {
        var sum = center.radiance.rgb*4.;
        var total = 4.;
        for (var y=-1; y<=1; y++) {
            for (var x=-1; x<=1; x++) {
                if x==0 && y==0 { continue; }
                let at = vec2<i32>(id.xy)+vec2<i32>(x,y);
                if any(at<vec2<i32>(0)) || any(at>=vec2<i32>(extent.xy)) { continue; }
                let neighbor = input_pixels[u32(at.y)*extent.x+u32(at.x)];
                let weight = f32((2-abs(x))*(2-abs(y)))*guide_weight(center.guide,neighbor.guide);
                sum += neighbor.radiance.rgb*weight;
                total += weight;
            }
        }
        // As transport converges, prefer its recovered detail to spatial blur.
        let strength = inverseSqrt(f32(max(1u,center.guide.w >> HISTORY_SHIFT)));
        center.radiance = vec4<f32>(mix(center.radiance.rgb,sum/total,strength),center.radiance.a);
    }
    output_pixels[i] = center;
}
