// Reconstructs world-space surface depth and edge-aware normal from the native
// rasterizer's reverse-Z depth buffer into the shared Pixel storage buffer.
// Runs at ray resolution (rw x rh) before hardware/software RT overlay tracing.
struct ExtractParams {
    inv_vp: mat4x4<f32>,
    eye: vec4<f32>,
    extent: vec4<u32>, // rw, rh, w, h
}

@group(0) @binding(0) var<uniform> extract: ExtractParams;
@group(0) @binding(1) var raster_depth: texture_depth_2d;
@group(0) @binding(2) var<storage, read_write> out_pixels: array<Pixel>;

fn unproject_raster(px: vec2<i32>, d: f32) -> vec3<f32> {
    let dims = vec2<f32>(extract.extent.zw);
    let uv = (vec2<f32>(px) + 0.5) / dims;
    let ndc = uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    let w4 = extract.inv_vp * vec4<f32>(ndc, d, 1.0);
    return w4.xyz / max(abs(w4.w), 1.0e-7) * select(1.0, -1.0, w4.w < 0.0);
}

fn load_depth(px: vec2<i32>, max_px: vec2<i32>) -> f32 {
    let c = clamp(px, vec2<i32>(0), max_px);
    return textureLoad(raster_depth, c, 0);
}

@compute @workgroup_size(8, 8)
fn extract_depth(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= extract.extent.xy) {
        return;
    }
    let at = id.y * extract.extent.x + id.x;
    let max_px = vec2<i32>(extract.extent.zw) - vec2<i32>(1);
    let src_f = (vec2<f32>(id.xy) + 0.5) * vec2<f32>(extract.extent.zw) / vec2<f32>(extract.extent.xy);
    let px = clamp(vec2<i32>(floor(src_f)), vec2<i32>(0), max_px);
    let d = textureLoad(raster_depth, px, 0);
    if d <= 0.0 {
        // Sky pixel: depth 0, shadow=1, ao=1, owner=0 (skipped by RT pass).
        out_pixels[at] = Pixel(0u, 0u, 0.0, 0.0, 0u, 0x00ffffffu, 0u);
        return;
    }

    let p = unproject_raster(px, d);
    let to_eye = extract.eye.xyz - p;
    let view_depth = max(length(to_eye), 0.001);
    let view_dir = to_eye / view_depth;

    // Edge-aware normal reconstruction: pick the closer depth neighbor on each axis
    // to prevent haloing across silhouette edges.
    let dl = load_depth(px + vec2<i32>(-1, 0), max_px);
    let dr = load_depth(px + vec2<i32>(1, 0), max_px);
    let du = load_depth(px + vec2<i32>(0, -1), max_px);
    let dd = load_depth(px + vec2<i32>(0, 1), max_px);

    let err_l = select(1.0e9, abs(dl - d), dl > 0.0);
    let err_r = select(1.0e9, abs(dr - d), dr > 0.0);
    let use_r = err_r <= err_l;
    let px_h = px + vec2<i32>(select(-1, 1, use_r), 0);
    let dh = select(dl, dr, use_r);

    let err_u = select(1.0e9, abs(du - d), du > 0.0);
    let err_d = select(1.0e9, abs(dd - d), dd > 0.0);
    let use_d = err_d <= err_u;
    let px_v = px + vec2<i32>(0, select(-1, 1, use_d));
    let dv = select(du, dd, use_d);

    var n = view_dir;
    if dh > 0.0 && dv > 0.0 {
        let ph = unproject_raster(px_h, dh);
        let pv = unproject_raster(px_v, dv);
        let dx = (ph - p) * select(-1.0, 1.0, use_r);
        let dy = (pv - p) * select(-1.0, 1.0, use_d);
        let raw_n = cross(dy, dx);
        let len2 = dot(raw_n, raw_n);
        if len2 > 1.0e-12 {
            n = raw_n * inverseSqrt(len2);
            if dot(n, view_dir) < 0.0 {
                n = -n;
            }
        }
    }

    let packed_n = pack4x8snorm(vec4<f32>(n, 0.0)) & 0x00ffffffu;
    out_pixels[at] = Pixel(0u, 0u, d, view_depth, packed_n, 0x00ffffffu, 1u);
}
