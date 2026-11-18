struct Params { extent: vec4<u32>, history: vec4<u32> }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage,read> input_pixels: array<Pixel>;
@group(0) @binding(2) var<storage,read_write> output_pixels: array<Pixel>;

@compute @workgroup_size(8,8)
fn accumulate(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=params.extent.xy) { return; }
    let i = id.y*params.extent.x+id.x;
    var current = input_pixels[i];
    var count = 1u;
    if params.history.x!=0u {
        // No inter-pixel reads: unique invocation per element, safe in place.
        let old = output_pixels[i];
        let animations = (old.guide.w|current.guide.w) & ANIM_MASK;
        if (animations & params.history.y)==0u
            && ((old.guide.w^current.guide.w) & GUIDE_KIND)==0u
            && all(old.guide.xyz==current.guide.xyz) {
            count = min(MAX_HISTORY,(old.guide.w >> HISTORY_SHIFT)+1u);
            current.radiance = vec4<f32>(mix(old.radiance.rgb,current.radiance.rgb,1./f32(count)),current.radiance.a);
            current.guide.w |= animations;
        }
    }
    current.guide.w |= count << HISTORY_SHIFT;
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
