// Shared by both ray backends, postprocessing, and the compositor.
// .a is the current frame's closest reverse-Z depth, never filtered.
// Guide: linear camera depth (0 = sky), packed normal, packed albedo, flags.
struct Pixel { radiance: vec4<f32>, guide: vec4<u32> }
const GUIDE_SHEET: u32 = 1u;
const GUIDE_MIXED: u32 = 2u;
const GUIDE_WATER: u32 = 4u;
const GUIDE_EMISSIVE: u32 = 8u;
const GUIDE_BAKED: u32 = 16u;
const GUIDE_KIND: u32 = 31u;
const GUIDE_UNSAFE: u32 = GUIDE_SHEET|GUIDE_MIXED;
// Dependency bits also cover secondary/shadow rays, not just the first hit.
const ANIM_WAVES: u32 = 32u;
const ANIM_SCROLL: u32 = 64u;
const ANIM_SKY: u32 = 128u;
const ANIM_MASK: u32 = ANIM_WAVES|ANIM_SCROLL|ANIM_SKY;
const HISTORY_SHIFT: u32 = 24u;
const MAX_HISTORY: u32 = 32u;

fn guide_weight(a: vec4<u32>, b: vec4<u32>) -> f32 {
    if ((a.w|b.w) & GUIDE_UNSAFE)!=0u || ((a.w^b.w) & GUIDE_KIND)!=0u { return 0.; }
    if a.x==0u || b.x==0u { return select(0.,1.,a.x==b.x); }
    let z = bitcast<f32>(a.x);
    let other = bitcast<f32>(b.x);
    let relative = abs(z-other)/max(min(z,other),0.01);
    let depth_weight = 1.-smoothstep(0.005,0.04,relative);
    let na = normalize(unpack4x8snorm(a.y).xyz);
    let nb = normalize(unpack4x8snorm(b.y).xyz);
    let normal_weight = smoothstep(0.88,0.99,dot(na,nb));
    let delta = abs(unpack4x8unorm(a.z).rgb-unpack4x8unorm(b.z).rgb);
    let albedo_weight = 1.-smoothstep(0.03,0.16,max(delta.x,max(delta.y,delta.z)));
    return depth_weight*normal_weight*albedo_weight;
}
