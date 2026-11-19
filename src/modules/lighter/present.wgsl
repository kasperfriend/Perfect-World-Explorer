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
// FSR 1 EASU loads the filtered linear ray buffer directly. No extra color
// image or RCAS is required, and the ray depth path below is unchanged.
fn load_texel(pos: vec2<i32>) -> vec3<f32> {
    return color_of(pixels[index(pos)]);
}
struct Output { @location(0) color: vec4<f32>, @builtin(frag_depth) depth: f32 }
fn resolve(coord: vec4<f32>) -> Output {
    let extent = present.extent;
    if all(extent.xy==extent.zw) {
        let c = pixels[index(vec2<i32>(coord.xy))];
        return Output(vec4<f32>(color_of(c),1.),c.depth);
    }
    let source = coord.xy*vec2<f32>(extent.xy)/vec2<f32>(extent.zw)-0.5;
    let nearest = index(vec2<i32>(floor(source+0.5)));
    let center = pixels[nearest];
    if present.options.y!=0u {
        return Output(vec4<f32>(easu_color(source),1.),center.depth);
    }
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
        if present.options.x!=0u && i!=nearest { weight *= guide_weight(guide_of(center),guide_of(neighbor)); }
        rgb += color_of(neighbor)*weight;
        total += weight;
    }
    return Output(vec4<f32>(select(color_of(center),rgb/max(1.e-8,total),total>1.e-8),1.),center.depth);
}
