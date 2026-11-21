// Composites denoised RT overlay (shadows, AO, GI bounce, reflections) onto the
// existing rasterized world using depth-guided bilateral upsampling.
// Hardware blend state: out_rgb = src.rgb * 1.0 + dst.rgb * src.a.
struct Present {
    extent: vec4<u32>,
    options: vec4<u32>,
}

@group(0) @binding(0) var<storage, read> pixels: array<Pixel>;
@group(0) @binding(1) var<uniform> present: Present;
@group(0) @binding(2) var raster_depth: texture_depth_2d;

@vertex fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i & 1u) * 4.0 - 1.0;
    let y = f32(i >> 1u) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn idx(p: vec2<i32>) -> u32 {
    let v = vec2<u32>(clamp(p, vec2<i32>(0), vec2<i32>(present.extent.xy) - 1));
    return v.y * present.extent.x + v.x;
}

fn unpack_shadow_ao(packed: u32) -> vec2<f32> {
    let s = f32(packed & 0xfffu) / 4095.0;
    let a = f32((packed >> 12u) & 0xfffu) / 4095.0;
    return vec2<f32>(s, a);
}

@fragment fn fragment(@builtin(position) coord: vec4<f32>) -> @location(0) vec4<f32> {
    let full_px = clamp(
        vec2<i32>(coord.xy),
        vec2<i32>(0),
        vec2<i32>(present.extent.zw) - vec2<i32>(1),
    );
    let d_full = textureLoad(raster_depth, full_px, 0);
    if d_full <= 0.0 {
        // Sky pixel: preserve rasterized sky completely untouched.
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let extent = present.extent;
    if all(extent.xy == extent.zw) {
        let c = pixels[idx(full_px)];
        if c.depth <= 0.0 || c.view_depth >= 450.0 {
            return vec4<f32>(0.0, 0.0, 0.0, 1.0);
        }
        let sa = unpack_shadow_ao(c.albedo_history);
        let mul = clamp(sa.x * sa.y, 0.25, 1.0);
        return vec4<f32>(color_of(c), mul);
    }

    // Depth-guided 2x2 bilateral upsample from ray resolution to raster resolution.
    let source = coord.xy * vec2<f32>(extent.xy) / vec2<f32>(extent.zw) - 0.5;
    let nearest = pixels[idx(vec2<i32>(floor(source + 0.5)))];
    if nearest.view_depth >= 450.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let p = vec2<i32>(floor(source));
    let f = fract(source);
    var rgb = vec3<f32>(0.0);
    var sa_sum = vec2<f32>(0.0);
    var total = 0.0;
    var best_err = 1.0e9;
    var best_rgb = vec3<f32>(0.0);
    var best_sa = vec2<f32>(1.0, 1.0);

    for (var j = 0u; j < 4u; j++) {
        let off = vec2<u32>(j & 1u, j >> 1u);
        let neighbor = pixels[idx(p + vec2<i32>(off))];
        if neighbor.depth <= 0.0 {
            continue;
        }
        let rel_err = abs(neighbor.depth - d_full) / max(d_full, 1.0e-5);
        let sa = unpack_shadow_ao(neighbor.albedo_history);
        let c = color_of(neighbor);
        if rel_err < best_err {
            best_err = rel_err;
            best_rgb = c;
            best_sa = sa;
        }
        let bilinear = select(1.0 - f, f, off != vec2<u32>(0u));
        let depth_w = exp(-rel_err * 32.0);
        let w = bilinear.x * bilinear.y * depth_w;
        rgb += c * w;
        sa_sum += sa * w;
        total += w;
    }

    if total > 1.0e-6 {
        let sa = sa_sum / total;
        let mul = clamp(sa.x * sa.y, 0.25, 1.0);
        return vec4<f32>(rgb / total, mul);
    }
    let mul = clamp(best_sa.x * best_sa.y, 0.25, 1.0);
    return vec4<f32>(best_rgb, mul);
}
