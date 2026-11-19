// AMD FidelityFX Super Resolution 1 (FSR 1) EASU + RCAS, scalar WGSL port.
// Derived from GPUOpen-Effects/FidelityFX-FSR/ffx-fsr/ffx_fsr1.h (MIT).
// Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.
// The copyright, permission and warranty notice is in ../THIRD_PARTY_NOTICES.md.
// The 12-tap EASU neighborhood is loaded explicitly instead of using gather4;
// the direction, kernel, deringing and RCAS equations follow the reference.

// The caller supplies load_texel() in the domain of its own source image.

fn luma2(c: vec3<f32>) -> f32 {
    return c.g + 0.5 * (c.r + c.b);
}

struct Edge {
    direction: vec2<f32>,
    length: f32,
};

// One of the four bilinearly weighted gradients in FsrEasuSetF. An epsilon
// guards a flat patch against 0/0 without changing nonzero gradients.
fn edge_set(edge: Edge, weight: f32, a: f32, b: f32, c: f32, d: f32, e: f32) -> Edge {
    var result = edge;
    let dx = d - b;
    let dy = e - a;
    let range_x = max(max(abs(d - c), abs(c - b)), 1e-8);
    let range_y = max(max(abs(e - c), abs(c - a)), 1e-8);
    let len_x = min(abs(dx) / range_x, 1.0);
    let len_y = min(abs(dy) / range_y, 1.0);
    result.direction += vec2<f32>(dx, dy) * weight;
    result.length += (len_x * len_x + len_y * len_y) * weight;
    return result;
}

fn easu_weight(offset: vec2<f32>, dir: vec2<f32>, anisotropy: vec2<f32>, lobe: f32, clip: f32) -> f32 {
    let rotated = vec2<f32>(
        dot(offset, dir),
        dot(offset, vec2<f32>(-dir.y, dir.x)),
    ) * anisotropy;
    let distance2 = min(dot(rotated, rotated), clip);
    let x = (2.0 / 5.0) * distance2 - 1.0;
    let y = lobe * distance2 - 1.0;
    // WGSL pow(negative, 2) is undefined; the reference squares the values.
    return ((25.0 / 16.0) * x * x - (25.0 / 16.0 - 1.0)) * y * y;
}

// pp is the output pixel center expressed in input-texel coordinates.
fn easu_color(pp: vec2<f32>) -> vec3<f32> {
    let fp = vec2<i32>(floor(pp));
    let fraction = fract(pp);
    // The named taps follow the reference's 12-tap neighborhood:
    //     b c
    //   e f g h
    //   i j k l
    //     n o
    let b = load_texel(fp + vec2<i32>( 0, -1));
    let c = load_texel(fp + vec2<i32>( 1, -1));
    let e = load_texel(fp + vec2<i32>(-1,  0));
    let f = load_texel(fp);
    let g = load_texel(fp + vec2<i32>( 1,  0));
    let h = load_texel(fp + vec2<i32>( 2,  0));
    let i = load_texel(fp + vec2<i32>(-1,  1));
    let j = load_texel(fp + vec2<i32>( 0,  1));
    let k = load_texel(fp + vec2<i32>( 1,  1));
    let l = load_texel(fp + vec2<i32>( 2,  1));
    let n = load_texel(fp + vec2<i32>( 0,  2));
    let o = load_texel(fp + vec2<i32>( 1,  2));

    let bl = luma2(b);
    let cl = luma2(c);
    let el = luma2(e);
    let fl = luma2(f);
    let gl = luma2(g);
    let hl = luma2(h);
    let il = luma2(i);
    let jl = luma2(j);
    let kl = luma2(k);
    let ll = luma2(l);
    let nl = luma2(n);
    let ol = luma2(o);
    var edge = Edge(vec2<f32>(0.0), 0.0);
    edge = edge_set(edge, (1.0-fraction.x)*(1.0-fraction.y), bl, el, fl, gl, jl);
    edge = edge_set(edge, fraction.x*(1.0-fraction.y), cl, fl, gl, hl, kl);
    edge = edge_set(edge, (1.0-fraction.x)*fraction.y, fl, il, jl, kl, nl);
    edge = edge_set(edge, fraction.x*fraction.y, gl, jl, kl, ll, ol);
    let dir2 = dot(edge.direction, edge.direction);
    var dir = vec2<f32>(1.0, 0.0);
    if dir2 >= 1.0 / 32768.0 {
        dir = edge.direction * inverseSqrt(dir2);
    }
    let length = pow(edge.length * 0.5, 2.0);
    let stretch = dot(dir, dir) / max(abs(dir.x), abs(dir.y));
    let anisotropy = vec2<f32>(1.0 + (stretch - 1.0) * length, 1.0 - 0.5 * length);
    let lobe = 0.5 + ((0.25 - 0.04) - 0.5) * length;
    let clip = 1.0 / lobe;
    let min4 = min(min(f, g), min(j, k));
    let max4 = max(max(f, g), max(j, k));

    var color = vec3<f32>(0.0);
    var sum = 0.0;
    var weight = easu_weight(vec2<f32>( 0.0,-1.0)-fraction, dir, anisotropy, lobe, clip);
    color += b * weight; sum += weight;
    weight = easu_weight(vec2<f32>( 1.0,-1.0)-fraction, dir, anisotropy, lobe, clip);
    color += c * weight; sum += weight;
    weight = easu_weight(vec2<f32>(-1.0, 1.0)-fraction, dir, anisotropy, lobe, clip);
    color += i * weight; sum += weight;
    weight = easu_weight(vec2<f32>( 0.0, 1.0)-fraction, dir, anisotropy, lobe, clip);
    color += j * weight; sum += weight;
    weight = easu_weight(vec2<f32>( 0.0, 0.0)-fraction, dir, anisotropy, lobe, clip);
    color += f * weight; sum += weight;
    weight = easu_weight(vec2<f32>(-1.0, 0.0)-fraction, dir, anisotropy, lobe, clip);
    color += e * weight; sum += weight;
    weight = easu_weight(vec2<f32>( 1.0, 1.0)-fraction, dir, anisotropy, lobe, clip);
    color += k * weight; sum += weight;
    weight = easu_weight(vec2<f32>( 2.0, 1.0)-fraction, dir, anisotropy, lobe, clip);
    color += l * weight; sum += weight;
    weight = easu_weight(vec2<f32>( 2.0, 0.0)-fraction, dir, anisotropy, lobe, clip);
    color += h * weight; sum += weight;
    weight = easu_weight(vec2<f32>( 1.0, 0.0)-fraction, dir, anisotropy, lobe, clip);
    color += g * weight; sum += weight;
    weight = easu_weight(vec2<f32>( 1.0, 2.0)-fraction, dir, anisotropy, lobe, clip);
    color += o * weight; sum += weight;
    weight = easu_weight(vec2<f32>( 0.0, 2.0)-fraction, dir, anisotropy, lobe, clip);
    color += n * weight; sum += weight;
    // Dering to the four nearest input texels. A nonzero central tap keeps
    // sum positive even on a flat or a fully black source image.
    return clamp(color / max(sum, 1e-8), min4, max4);
}
