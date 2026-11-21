@group(0) @binding(2) var reconstructed: texture_2d<f32>;
@group(0) @binding(3) var reject_history: texture_2d<f32>;

@fragment fn fragment(@builtin(position) coord: vec4<f32>) -> Output {
    // SDK color has no authority over reverse-Z. On explicitly unsupported
    // surfaces keep the current portable color and depth (not vendor history).
    let source = vec2<i32>(coord.xy*vec2<f32>(present.extent.xy)/vec2<f32>(present.extent.zw));
    let p = pixels[index(source)];
    let color = textureLoad(reconstructed,vec2<i32>(coord.xy),0).rgb;
    if textureLoad(reject_history,source,0).x>=1. || !all(abs(color)<=vec3<f32>(65504.)) {
        return resolve(coord);
    }
    return Output(vec4<f32>(max(color,vec3<f32>(0.)),1.),p.depth);
}
