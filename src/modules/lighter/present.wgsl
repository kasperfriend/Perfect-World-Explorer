// Presentation consumes the finished ray image and current reverse-Z depth.
// Lens flares, HUD glow and GFX Warp would be display-space operations here or
// in native post, not glass BSDFs. Warp:1 remains explicitly unsupported:
// its authored normalmap card is withheld from world, shadow and refraction
// geometry; no fake opacity or color is inserted into the ray world.

struct Present { extent: vec4<u32>, options: vec4<u32> }
@group(0) @binding(0) var<storage,read> pixels: array<Pixel>;
@group(0) @binding(1) var<uniform> present: Present;

@vertex fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i & 1u)*4.-1.;
    let y = f32(i >> 1u)*4.-1.;
    return vec4<f32>(x,y,0.,1.);
}
fn index(p: vec2<i32>) -> u32 {
    let v = vec2<u32>(clamp(p,vec2<i32>(0),vec2<i32>(present.extent.xy)-1));
    return v.y*present.extent.x+v.x;
}
struct Output { @location(0) color: vec4<f32>, @builtin(frag_depth) depth: f32 }
@fragment fn fragment(@builtin(position) coord: vec4<f32>) -> Output {
    let extent = present.extent;
    if all(extent.xy==extent.zw) {
        let c = pixels[index(vec2<i32>(coord.xy))].radiance;
        return Output(vec4<f32>(c.rgb,1.),c.a);
    }
    let source = coord.xy*vec2<f32>(extent.xy)/vec2<f32>(extent.zw)-0.5;
    let nearest = index(vec2<i32>(floor(source+0.5)));
    let center = pixels[nearest];
    let p = vec2<i32>(floor(source));
    let f = fract(source);
    var rgb = vec3<f32>(0.);
    var total = 0.;
    for (var j=0u; j<4u; j++) {
        let off = vec2<u32>(j & 1u,j >> 1u);
        let i = index(p+vec2<i32>(off));
        let neighbor = pixels[i];
        let bilinear = select(1.-f,f,off!=vec2<u32>(0u));
        var weight = bilinear.x*bilinear.y;
        if present.options.x!=0u && i!=nearest { weight *= guide_weight(center.guide,neighbor.guide); }
        rgb += neighbor.radiance.rgb*weight;
        total += weight;
    }
    return Output(vec4<f32>(rgb/total,1.),center.radiance.a);
}
