// RGBA16F encoded in two words, plus untouched FP32 reverse-Z and linear
// guide depth. No shader-f16 feature required. Alpha stores log2(1+variance).
// Seven scalar words (28-byte stride); no hidden vec4 alignment padding.
struct Pixel {
    rg: u32, bv: u32, depth: f32, view_depth: f32,
    normal_flags: u32, albedo_history: u32, owner: u32,
}
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
const HISTORY_COUNT: u32 = 63u;
const SUN_VALID: u32 = 1u << 30u;
const SUN_BLOCKED: u32 = 1u << 31u;
const SUN_CACHE: u32 = SUN_VALID|SUN_BLOCKED;
const DEP_MOVING: u32 = 0x80000000u;
const OWNER_MASK: u32 = 0x7fffffffu;

fn color_of(p: Pixel) -> vec3<f32> {
    return vec3<f32>(unpack2x16float(p.rg),unpack2x16float(p.bv).x);
}
fn variance_of(p: Pixel) -> f32 { return exp2(unpack2x16float(p.bv).y)-1.; }
fn luminance(rgb: vec3<f32>) -> f32 { return dot(rgb,vec3<f32>(0.2126,0.7152,0.0722)); }
fn write_radiance(pixel: ptr<function,Pixel>, rgb: vec3<f32>, variance: f32) {
    let c = clamp(rgb,vec3<f32>(0.),vec3<f32>(65000.));
    (*pixel).rg = pack2x16float(c.xy);
    (*pixel).bv = pack2x16float(vec2<f32>(c.z,log2(1.+clamp(variance,0.,4.3e9))));
}
fn guide_of(p: Pixel) -> vec4<u32> {
    return vec4<u32>(bitcast<u32>(p.view_depth),p.normal_flags & 0xffffffu,p.albedo_history & 0xffffffu,p.normal_flags >> 24u);
}
fn pack_pixel(rgb: vec3<f32>, depth: f32, g: vec4<u32>, owner: u32) -> Pixel {
    var p = Pixel(0u,0u,depth,bitcast<f32>(g.x),(g.y & 0xffffffu)|(g.w << 24u),g.z & 0xffffffu,owner);
    write_radiance(&p,rgb,0.);
    return p;
}
fn guide_weight(a: vec4<u32>, b: vec4<u32>) -> f32 {
    if ((a.w|b.w) & GUIDE_UNSAFE)!=0u || ((a.w^b.w) & GUIDE_KIND)!=0u { return 0.; }
    if a.x==0u || b.x==0u { return select(0.,1.,a.x==b.x); }
    let z = bitcast<f32>(a.x);
    let other = bitcast<f32>(b.x);
    let relative = abs(z-other)/max(min(z,other),0.01);
    let depth_weight = 1.-smoothstep(0.005,0.04,relative);
    let na = unpack4x8snorm(a.y).xyz;
    let nb = unpack4x8snorm(b.y).xyz;
    let nd = dot(na,nb)*inverseSqrt(max(1.e-8,dot(na,na)*dot(nb,nb)));
    let normal_weight = smoothstep(0.88,0.99,nd);
    let delta = abs(unpack4x8unorm(a.z).rgb-unpack4x8unorm(b.z).rgb);
    let albedo_weight = 1.-smoothstep(0.03,0.16,max(delta.x,max(delta.y,delta.z)));
    return depth_weight*normal_weight*albedo_weight;
}
