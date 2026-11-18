@group(0) @binding(0) var<storage,read> pixels: array<vec4<f32>>;
@group(0) @binding(1) var<uniform> extent: vec4<u32>;

@vertex fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i & 1u)*4.-1.;
    let y = f32(i >> 1u)*4.-1.;
    return vec4<f32>(x,y,0.,1.);
}
fn pixel(p: vec2<i32>) -> vec4<f32> {
    let v = vec2<u32>(clamp(p,vec2<i32>(0),vec2<i32>(extent.xy)-1));
    return pixels[v.y*extent.x+v.x];
}
struct Output { @location(0) color: vec4<f32>, @builtin(frag_depth) depth: f32 }
@fragment fn fragment(@builtin(position) coord: vec4<f32>) -> Output {
    let source = coord.xy*vec2<f32>(extent.xy)/vec2<f32>(extent.zw)-0.5;
    let p = vec2<i32>(floor(source));
    let f = fract(source);
    let c = mix(mix(pixel(p).rgb,pixel(p+vec2<i32>(1,0)).rgb,f.x),
                mix(pixel(p+vec2<i32>(0,1)).rgb,pixel(p+vec2<i32>(1,1)).rgb,f.x),f.y);
    return Output(vec4<f32>(c,1.),pixel(vec2<i32>(floor(source+0.5))).a);
}
