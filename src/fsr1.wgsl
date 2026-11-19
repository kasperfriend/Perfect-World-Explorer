// AMD FidelityFX Super Resolution 1 (FSR 1) EASU + RCAS, scalar WGSL port.
// Derived from GPUOpen-Effects/FidelityFX-FSR/ffx-fsr/ffx_fsr1.h (MIT).
// Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.
// The copyright, permission and warranty notice is in ../THIRD_PARTY_NOTICES.md.
// The 12-tap EASU neighborhood is loaded explicitly instead of using gather4;
// the direction, kernel, deringing and RCAS equations follow the reference.

struct Params {
    // The physical sizes of the source world image and native UI/composition.
    dimensions: vec4<f32>,
    // RCAS sharpness is 2^(-stops), where 0 stops is strongest.
    sharpness: vec4<f32>,
};
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

@vertex fn vs(@builtin(vertex_index) id: u32) -> @builtin(position) vec4<f32> {
    let x = f32((id << 1u) & 2u);
    let y = f32(id & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

// The source is an sRGB texture view, so textureLoad decodes it once. Clamp
// edge texels like the official reference's clamp-to-edge sampler.
fn load_texel(pos: vec2<i32>) -> vec3<f32> {
    let max_pos = vec2<i32>(textureDimensions(src)) - vec2<i32>(1);
    return textureLoad(src, clamp(pos, vec2<i32>(0), max_pos), 0).rgb;
}

@fragment fn easu(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pp = (floor(position.xy) + vec2<f32>(0.5)) * params.dimensions.xy / params.dimensions.zw - vec2<f32>(0.5);
    return vec4<f32>(easu_color(pp), 1.0);
}

@fragment fn rcas(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(position.xy);
    let b = load_texel(p + vec2<i32>( 0, -1));
    let d = load_texel(p + vec2<i32>(-1,  0));
    let e = load_texel(p);
    let f = load_texel(p + vec2<i32>( 1,  0));
    let h = load_texel(p + vec2<i32>( 0,  1));
    let mn = min(min(b, d), min(f, h));
    let mx = max(max(b, d), max(f, h));
    // The reference's min/max limiters. Bound reciprocal denominators on
    // constant black/white to avoid NaN from 0/0 at the image border.
    let hit_min = min(mn, e) / max(4.0 * mx, vec3<f32>(1e-8));
    let hit_max = (vec3<f32>(1.0) - max(mx, e)) / min(4.0 * mn - vec3<f32>(4.0), vec3<f32>(-1e-8));
    let lobes = max(-hit_min, hit_max);
    let lobe = clamp(max(max(lobes.r, lobes.g), lobes.b), -0.1875, 0.0) * params.sharpness.x;
    return vec4<f32>((e + lobe * (b + d + f + h)) / (1.0 + 4.0 * lobe), 1.0);
}
