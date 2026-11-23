// DXR Tier 1.1 inline ray-tracing compute shader (`cs_6_5`).
//
// Runs directly on hardware RT cores (NVIDIA RTX, AMD RDNA2+, Intel Arc) via
// `RayQuery`, anchored 1:1 to the raster world's depth-reconstructed world
// position, depth-derived surface normal, and untouched raster colour so the
// visible world is always the exact original game geometry (buildings, trees,
// terrain, water, sky, effects) — never coarse proxy boxes.

struct Params {
    column_major float4x4 inv_vp;
    column_major float4x4 vp;
    float4 eye;
    float4 sun_dir;
    float4 sun_colour;
    float4 ambient;
    float4 fog_colour;
    float4 lens;    // x: near, y: far, z: sun_angular_radius, w: sun_intensity
    float4 extents; // x: grid_w, y: grid_h, z: render_w, w: render_h
    float4 frame;   // x: frame_index, y: samples, z: bounces, w: history_valid
    float4 tuning;  // x: jitter_x, y: jitter_y, z: ambient_strength, w: bounce_strength
    float4 effects; // x: roughness_cutoff, y: effect_bits, z: bias, w: layered_mode (1=layer over raster, 0=replace)
    float4 fog;     // x: fog_start, y: fog_end, z: ao_radius, w: opaque_tri_count
    float4 counts;  // x: tri_count, y: node_count, z: mat_count, w: debug_view
    float4 point_light_meta;            // x: active light count (<= 128), y: pl_shadows, z: write_albedo, w: write_guide
    float4 point_lights_pos_rad[128];   // xyz: world pos, w: radius
    float4 point_lights_col_int[128];   // rgb: colour, w: intensity
    float4 terrain_map_bounds;          // x: min_x, y: max_z, z: inv_diameter, w: valid (>0.5)
};

struct Tri {
    float4 a;
    float4 b; // .w carries material index as float
    float4 c;
    float4 n;
};

struct Material {
    float4 albedo;  // .rgb diffuse albedo
    float4 surface; // .x roughness, .y metallic, .w opacity
};

cbuffer ParamsBuffer : register(b0) {
    Params params;
};

RaytracingAccelerationStructure scene_tlas : register(t0);
StructuredBuffer<Tri> tris : register(t1);
StructuredBuffer<Material> materials : register(t2);
// 1:1 screen-space G-buffer derived from the raster world pass:
// src_gbuf_pos:    .xyz world position P, .w linear distance from eye (0 = sky)
// src_gbuf_norm:   .xyz world normal N,   .w surface roughness (0.06 water, 0.26..0.34 glossy trim, 0.92 matte)
// src_gbuf_raster: .rgb untouched raster colour, .a water depth (world units on water pixels, 0 on land)
Texture2D<float4> src_gbuf_pos : register(t3);
Texture2D<float4> src_gbuf_norm : register(t4);
Texture2D<float4> src_gbuf_raster : register(t5);
// Real sky (SkyGpu) + cloud billboards (CloudGpu) in a 3x2 cubemap atlas.
Texture2D<float4> src_sky_env : register(t6);
// Top-down real textured terrain albedo map (512x512 around camera anchor).
Texture2D<float4> src_terrain_map : register(t7);

RWTexture2D<float4> out_colour : register(u0);
RWTexture2D<float4> out_guide : register(u1);
RWTexture2D<float4> out_reflection : register(u2);
RWTexture2D<float4> out_normals : register(u3);
RWTexture2D<float4> out_albedo : register(u4);
// Feature #6: Persistent 65,536-cell 3D World-Space Radiance Cache (1 MB VRAM).
RWStructuredBuffer<float4> radiance_cache : register(u5);

static const float PI = 3.14159265;
static const float SKY_ENV_FACE_F = 128.0;
static const float TERRAIN_MAP_SIZE_F = 512.0;
static const uint RC_MASK = 65535u;

uint hash_u32(inout uint state) {
    uint x = state;
    x ^= x << 13u;
    x ^= x >> 17u;
    x ^= x << 5u;
    state = x;
    return x;
}

float rand01(inout uint state) {
    return float(hash_u32(state) & 0x00ffffffu) / 16777216.0;
}

float luma(float3 c) {
    return dot(c, float3(0.2126, 0.7152, 0.0722));
}

// Interleaved Gradient Noise for per-pixel decorrelation of shadows, AO, and god rays.
float ign(uint2 px, uint frame) {
    float2 p = float2(px) + float2(frame & 63u, (frame * 7u) & 63u) * 5.588238;
    return frac(52.9829189 * frac(dot(p, float2(0.06711056, 0.00583715))));
}

struct Hit {
    uint index;
    float t;
    float3 normal;
};

struct ShadowResult {
    bool occluded;
    float t_blocker;
    float3 transmittance;
};

Material material_of(uint tri_index) {
    uint mat_count = max(uint(params.counts.z), 1u);
    uint mat_idx = min(uint(tris[tri_index].b.w + 0.5), mat_count - 1u);
    return materials[mat_idx];
}

// Feature #6: Spatial hash key for 3D World-Space Radiance Cache (voxel + 6-dir normal).
uint rc_slot(float3 world_pos, float3 normal, float cam_dist) {
    float cell_size = (cam_dist < 64.0) ? 4.0 : 12.0;
    int3 cell = int3(floor(world_pos / cell_size));
    float3 an = abs(normal);
    uint ndir = 0u;
    if (an.x >= an.y && an.x >= an.z) {
        ndir = (normal.x >= 0.0) ? 0u : 1u;
    } else if (an.y >= an.x && an.y >= an.z) {
        ndir = (normal.y >= 0.0) ? 2u : 3u;
    } else {
        ndir = (normal.z >= 0.0) ? 4u : 5u;
    }
    uint scale_bit = (cam_dist < 64.0) ? 0u : 1u;
    uint h = asuint(cell.x) * 73856093u
        ^ asuint(cell.y) * 19349663u
        ^ asuint(cell.z) * 83492791u
        ^ (ndir * 2654435761u)
        ^ (scale_bit * 374761393u);
    h ^= h >> 16u;
    h *= 0x85ebca6bu;
    h ^= h >> 13u;
    return h & RC_MASK;
}

float4 rc_query(float3 world_pos, float3 normal, float cam_dist) {
    uint slot = rc_slot(world_pos, normal, cam_dist);
    return radiance_cache[slot];
}

void rc_update(float3 world_pos, float3 normal, float cam_dist, float3 rad) {
    uint slot = rc_slot(world_pos, normal, cam_dist);
    float4 prev = radiance_cache[slot];
    float w = min(prev.w + 1.0, 32.0);
    float alpha = (prev.w < 1.0) ? 1.0 : max(1.0 / w, 0.08);
    float3 clamped = clamp(rad, float3(0.0, 0.0, 0.0), float3(2.5, 2.5, 2.5));
    float3 blended = lerp(prev.rgb, clamped, alpha);
    radiance_cache[slot] = float4(blended, w);
}

bool accept_opacity(uint prim, float2 bary, uint seed, uint tri_count) {
    if (prim >= tri_count) {
        return false;
    }
    float opacity = material_of(prim).surface.w;
    if (opacity >= 0.99) {
        return true;
    }
    uint _unused = seed;
    float clump = 1.0;
    if (opacity < 0.90) {
        uint cx = uint(bary.x * 7.0);
        uint cy = uint(bary.y * 7.0);
        uint ch = prim * 2654435761u ^ cx * 2246822519u ^ cy * 3266489917u;
        ch ^= ch >> 15u;
        clump = 0.42 + 0.86 * (float(ch & 0xffu) / 255.0);
    }
    float eff_opacity = saturate(opacity * clump);
    uint h = prim * 747796405u ^ uint(bary.x * 64.0) * 2891336453u ^ uint(bary.y * 64.0) * 1664525u;
    h ^= h >> 16u;
    float r = float(h & 0xffffu) / 65536.0;
    return r < eff_opacity;
}

Hit closest_hit(float3 origin, float3 dir, float t_min, float t_max, uint opaque_offset, uint tri_count) {
    Hit hit;
    hit.index = 0xffffffffu;
    hit.t = t_max;
    hit.normal = float3(0.0, 1.0, 0.0);

    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = t_min;
    ray.TMax = t_max;

    RayQuery<RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray);
    while (q.Proceed()) {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE) {
            uint cand_prim = q.CandidatePrimitiveIndex() + (q.CandidateGeometryIndex() != 0u ? opaque_offset : 0u);
            if (accept_opacity(cand_prim, q.CandidateTriangleBarycentrics(), 0u, tri_count)) {
                q.CommitNonOpaqueTriangleHit();
            }
        }
    }

    if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
        uint prim = q.CommittedPrimitiveIndex() + (q.CommittedGeometryIndex() != 0u ? opaque_offset : 0u);
        if (prim < tri_count) {
            float3 n = normalize(tris[prim].n.xyz);
            if (dot(n, dir) > 0.0) {
                n = -n;
            }
            hit.index = prim;
            hit.t = q.CommittedRayT();
            hit.normal = n;
        }
    }
    return hit;
}

Hit closest_hit_fast(float3 origin, float3 dir, float t_min, float t_max, uint opaque_offset, uint tri_count) {
    Hit hit;
    hit.index = 0xffffffffu;
    hit.t = t_max;
    hit.normal = float3(0.0, 1.0, 0.0);

    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = t_min;
    ray.TMax = t_max;

    RayQuery<RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray);
    q.Proceed();

    if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
        uint prim = q.CommittedPrimitiveIndex() + (q.CommittedGeometryIndex() != 0u ? opaque_offset : 0u);
        if (prim < tri_count) {
            float3 n = normalize(tris[prim].n.xyz);
            if (dot(n, dir) > 0.0) {
                n = -n;
            }
            hit.index = prim;
            hit.t = q.CommittedRayT();
            hit.normal = n;
        }
    }
    return hit;
}

// Feature #4 & #7: Shadow ray returning blocker distance (for PCSS contact-hardening)
// and colored translucent transmission when passing through foliage/banners.
ShadowResult trace_sun_shadow(float3 origin, float3 dir, float t_min, float t_max, uint seed, uint opaque_offset, uint tri_count, bool fast_mode) {
    ShadowResult res;
    res.occluded = false;
    res.t_blocker = -1.0;
    res.transmittance = float3(1.0, 1.0, 1.0);

    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = t_min;
    ray.TMax = t_max;

    if (fast_mode) {
        RayQuery<RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
        q.TraceRayInline(scene_tlas, RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray);
        q.Proceed();
        if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
            res.occluded = true;
            res.t_blocker = q.CommittedRayT();
            res.transmittance = float3(0.0, 0.0, 0.0);
        }
        return res;
    }

    RayQuery<RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray);
    while (q.Proceed()) {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE) {
            uint cand_prim = q.CandidatePrimitiveIndex() + (q.CandidateGeometryIndex() != 0u ? opaque_offset : 0u);
            if (accept_opacity(cand_prim, q.CandidateTriangleBarycentrics(), seed, tri_count)) {
                q.CommitNonOpaqueTriangleHit();
            }
        }
    }

    if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
        res.occluded = true;
        res.t_blocker = q.CommittedRayT();
        uint prim = q.CommittedPrimitiveIndex() + (q.CommittedGeometryIndex() != 0u ? opaque_offset : 0u);
        if (prim < tri_count) {
            Material m = material_of(prim);
            // Feature #4: Non-opaque foliage/blossoms/banners transmit subtle tinted light.
            if (m.surface.w < 0.96) {
                float tr = saturate((1.0 - m.surface.w) * 0.42 + 0.10);
                res.transmittance = clamp(m.albedo.rgb, float3(0.10, 0.10, 0.10), float3(0.95, 0.95, 0.95)) * tr;
            } else {
                res.transmittance = float3(0.0, 0.0, 0.0);
            }
        } else {
            res.transmittance = float3(0.0, 0.0, 0.0);
        }
    }
    return res;
}

bool occluded_fast(float3 origin, float3 dir, float t_min, float t_max) {
    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = t_min;
    ray.TMax = t_max;

    RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray);
    q.Proceed();
    return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT;
}

float point_light_falloff(float dist, float radius) {
    if (radius <= 1e-4 || dist >= radius) {
        return 0.0;
    }
    float ratio = dist / radius;
    float r2 = ratio * ratio;
    float r4 = r2 * r2;
    float w = max(1.0 - r4, 0.0);
    return (w * w) / (dist * dist + 1.0);
}

// Reproject world-space hit into current frame's 1:1 raster G-buffer.
float4 sample_screen_radiance(float3 world_pos, float ray_t) {
    float4 clip = mul(params.vp, float4(world_pos, 1.0));
    if (clip.w <= 1e-4) {
        return float4(0.0, 0.0, 0.0, 0.0);
    }
    float3 ndc = clip.xyz / clip.w;
    float2 uv = float2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.01 || uv.x > 0.99 || uv.y < 0.01 || uv.y > 0.99) {
        return float4(0.0, 0.0, 0.0, 0.0);
    }
    int2 gdim = int2(int(params.extents.x), int(params.extents.y));
    int2 hp = clamp(int2(uv * params.extents.xy), int2(0, 0), gdim - int2(1, 1));
    float4 gpos = src_gbuf_pos.Load(int3(hp, 0));
    if (gpos.w > 0.0 && length(gpos.xyz - world_pos) < max(2.5, ray_t * 0.08)) {
        return float4(src_gbuf_raster.Load(int3(hp, 0)).rgb, 1.0);
    }
    return float4(0.0, 0.0, 0.0, 0.0);
}

float3 sample_sky_env(float3 dir) {
    float3 d = normalize(dir);
    float3 ad = abs(d);
    uint face = 0u;
    float uc = 0.0;
    float vc = 0.0;
    float ma = 1.0;
    if (ad.x >= ad.y && ad.x >= ad.z) {
        ma = ad.x;
        if (d.x >= 0.0) { face = 0u; uc = -d.z; vc = -d.y; }
        else            { face = 1u; uc =  d.z; vc = -d.y; }
    } else if (ad.y >= ad.x && ad.y >= ad.z) {
        ma = ad.y;
        if (d.y >= 0.0) { face = 2u; uc =  d.x; vc =  d.z; }
        else            { face = 3u; uc =  d.x; vc = -d.z; }
    } else {
        ma = ad.z;
        if (d.z >= 0.0) { face = 4u; uc =  d.x; vc = -d.y; }
        else            { face = 5u; uc = -d.x; vc = -d.y; }
    }
    float2 fuv = clamp(0.5 * (float2(uc, vc) / max(ma, 1e-5) + 1.0), 0.002, 0.998);
    float fw = SKY_ENV_FACE_F;
    float fh = SKY_ENV_FACE_F;
    uint col = face % 3u;
    uint row = face / 3u;
    float fx = float(col) * fw + fuv.x * (fw - 1.0);
    float fy = float(row) * fh + fuv.y * (fh - 1.0);
    int x0 = int(floor(fx));
    int y0 = int(floor(fy));
    int x1 = min(x0 + 1, int((col + 1u) * 128u) - 1);
    int y1 = min(y0 + 1, int((row + 1u) * 128u) - 1);
    float2 frac_xy = float2(fx - float(x0), fy - float(y0));
    float3 c00 = src_sky_env.Load(int3(x0, y0, 0)).rgb;
    float3 c10 = src_sky_env.Load(int3(x1, y0, 0)).rgb;
    float3 c01 = src_sky_env.Load(int3(x0, y1, 0)).rgb;
    float3 c11 = src_sky_env.Load(int3(x1, y1, 0)).rgb;
    return max(lerp(lerp(c00, c10, frac_xy.x), lerp(c01, c11, frac_xy.x), frac_xy.y), float3(0.0, 0.0, 0.0));
}

float3 sample_sky_reflection(float3 dir) {
    return sample_sky_env(dir);
}

float3 sample_terrain_albedo(float3 world_pos, float3 fallback) {
    if (params.terrain_map_bounds.w <= 0.5) {
        return fallback;
    }
    float u = (world_pos.x - params.terrain_map_bounds.x) * params.terrain_map_bounds.z;
    float v = (params.terrain_map_bounds.y - world_pos.z) * params.terrain_map_bounds.z;
    if (u < 0.005 || u > 0.995 || v < 0.005 || v > 0.995) {
        return fallback;
    }
    float fx = u * (TERRAIN_MAP_SIZE_F - 1.0);
    float fy = v * (TERRAIN_MAP_SIZE_F - 1.0);
    int x0 = int(floor(fx));
    int y0 = int(floor(fy));
    int x1 = min(x0 + 1, 511);
    int y1 = min(y0 + 1, 511);
    float2 fr = float2(fx - float(x0), fy - float(y0));
    float3 c00 = src_terrain_map.Load(int3(x0, y0, 0)).rgb;
    float3 c10 = src_terrain_map.Load(int3(x1, y0, 0)).rgb;
    float3 c01 = src_terrain_map.Load(int3(x0, y1, 0)).rgb;
    float3 c11 = src_terrain_map.Load(int3(x1, y1, 0)).rgb;
    float3 col = lerp(lerp(c00, c10, fr.x), lerp(c01, c11, fr.x), fr.y);
    if (dot(col, float3(1.0, 1.0, 1.0)) < 1e-4) {
        return fallback;
    }
    return clamp(col, float3(0.03, 0.03, 0.03), float3(0.95, 0.95, 0.95));
}

float3 sample_terrain_albedo_fast(float3 world_pos, float3 fallback) {
    if (params.terrain_map_bounds.w <= 0.5) {
        return fallback;
    }
    float u = (world_pos.x - params.terrain_map_bounds.x) * params.terrain_map_bounds.z;
    float v = (params.terrain_map_bounds.y - world_pos.z) * params.terrain_map_bounds.z;
    if (u < 0.005 || u > 0.995 || v < 0.005 || v > 0.995) {
        return fallback;
    }
    int tx = clamp(int(u * (TERRAIN_MAP_SIZE_F - 1.0) + 0.5), 0, 511);
    int ty = clamp(int(v * (TERRAIN_MAP_SIZE_F - 1.0) + 0.5), 0, 511);
    float3 col = src_terrain_map.Load(int3(tx, ty, 0)).rgb;
    if (dot(col, float3(1.0, 1.0, 1.0)) < 1e-4) {
        return fallback;
    }
    return clamp(col, float3(0.03, 0.03, 0.03), float3(0.95, 0.95, 0.95));
}

// Feature #10: Per-pixel world-space procedural architectural & organic surface detail
// for off-screen reflections & GI hits (eliminates flat untextured grey walls off-screen).
float3 detail_offscreen_albedo(float3 hit_pos, float3 hit_normal, float3 base_albedo, float opacity) {
    float3 an = abs(hit_normal);
    if (opacity < 0.96) {
        // Organic foliage cluster variation.
        int3 fc = int3(floor(hit_pos * 1.4));
        uint h = asuint(fc.x) * 374761393u ^ asuint(fc.y) * 668265263u ^ asuint(fc.z) * 2147483647u;
        h ^= h >> 15u;
        float v = 0.85 + 0.30 * (float(h & 0xffu) / 255.0);
        return clamp(base_albedo * v, float3(0.02, 0.02, 0.02), float3(0.95, 0.95, 0.95));
    }
    if (an.y < 0.55) {
        // Vertical masonry / running-bond stone block & mortar micro-variation.
        float horiz = (an.x > an.z) ? hit_pos.z : hit_pos.x;
        float row = floor(hit_pos.y * 1.25);
        float col = floor(horiz * 0.75 + frac(row * 0.5) * 0.5);
        float2 local_uv = float2(frac(horiz * 0.75 + frac(row * 0.5) * 0.5), frac(hit_pos.y * 1.25));
        float mortar = (local_uv.x < 0.06 || local_uv.y < 0.08) ? 0.82 : 1.0;
        uint bh = asuint(int(col)) * 73856093u ^ asuint(int(row)) * 19349663u;
        bh ^= bh >> 13u;
        float tone = 0.88 + 0.24 * (float(bh & 0xffu) / 255.0);
        return clamp(base_albedo * (mortar * tone), float3(0.02, 0.02, 0.02), float3(0.95, 0.95, 0.95));
    }
    if (an.y < 0.94) {
        // Sloped roof tiles: crisp directional ridge shading.
        float ridge = 0.90 + 0.14 * sin(hit_pos.x * 4.2 + hit_pos.z * 4.2);
        return clamp(base_albedo * ridge, float3(0.02, 0.02, 0.02), float3(0.95, 0.95, 0.95));
    }
    // Horizontal paving / stone slab variation.
    int2 sc = int2(floor(hit_pos.xz * 0.5));
    uint sh = asuint(sc.x) * 73856093u ^ asuint(sc.y) * 83492791u;
    sh ^= sh >> 14u;
    float slab = 0.91 + 0.18 * (float(sh & 0xffu) / 255.0);
    return clamp(base_albedo * slab, float3(0.02, 0.02, 0.02), float3(0.95, 0.95, 0.95));
}

float2 hash22(float2 p) {
    float3 p3 = frac(float3(p.xyx) * float3(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, p3.yzx + 33.33);
    return frac((p3.xx + p3.yz) * p3.zy);
}

// Organic Voronoi cellular ridge (non-repeating natural water caustics, zero grid wallpaper).
float voronoi_ridge(float2 uv, float t) {
    float2 n = floor(uv);
    float2 f = frac(uv);
    float d1 = 8.0;
    float d2 = 8.0;
    for (int j = -1; j <= 1; j++) {
        for (int i = -1; i <= 1; i++) {
            float2 g = float2(float(i), float(j));
            float2 o = hash22(n + g);
            o = 0.5 + 0.42 * sin(t + 6.28318 * o);
            float2 r = g + o - f;
            float d = dot(r, r);
            if (d < d1) {
                d2 = d1;
                d1 = d;
            } else if (d < d2) {
                d2 = d;
            }
        }
    }
    float edge = sqrt(d2) - sqrt(d1);
    return pow(saturate(1.0 - edge * 2.3), 3.0);
}

float caustic_pattern(float2 xz, float t) {
    float2 warp = float2(sin(xz.y * 0.27 + t * 0.7), cos(xz.x * 0.23 - t * 0.6)) * 0.45;
    float c1 = voronoi_ridge((xz + warp) * 0.36 + float2(t * 0.22, -t * 0.17), t * 1.15);
    float c2 = voronoi_ridge((xz - warp * 0.7) * 0.58 + float2(-t * 0.19, t * 0.24), t * 0.85);
    return saturate(c1 * 0.65 + c2 * 0.55);
}

// Feature #1: Henyey-Greenstein forward-scattering phase for volumetric god rays.
float hg_phase(float cos_theta, float g) {
    float g2 = g * g;
    float denom = max(1.0 + g2 - 2.0 * g * cos_theta, 0.05);
    return (1.0 - g2) / (4.0 * PI * denom * sqrt(denom));
}

float3 ggx_sample(float3 normal, float roughness, float u1, float u2) {
    if (roughness < 0.06) {
        return normal;
    }
    float a = roughness * roughness;
    float a2 = a * a;
    float phi = 2.0 * PI * u1;
    float cos_theta = sqrt(max((1.0 - u2) / (1.0 + (a2 - 1.0) * u2), 0.0));
    float sin_theta = sqrt(max(1.0 - cos_theta * cos_theta, 0.0));
    float s_phi, c_phi;
    sincos(phi, s_phi, c_phi);
    float3 up = abs(normal.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 t = normalize(cross(up, normal));
    float3 b = cross(normal, t);
    return normalize(t * (c_phi * sin_theta) + b * (s_phi * sin_theta) + normal * cos_theta);
}

// Feature #8: Analytical ray-sphere intersection of active point lights & world effects
// along reflection rays (so Archosaur ring beams, portals, wandering lights, and torches
// mirror vividly in water and polished surfaces).
float3 trace_effect_light_reflections(float3 origin, float3 dir, float max_t, uint pl_count) {
    float3 glow_sum = float3(0.0, 0.0, 0.0);
    for (uint li = 0u; li < pl_count; li = li + 1u) {
        float4 pr = params.point_lights_pos_rad[li];
        if (pr.w <= 0.1) {
            continue;
        }
        float3 to_l = pr.xyz - origin;
        float t_proj = dot(to_l, dir);
        if (t_proj <= 0.4 || t_proj >= max_t) {
            continue;
        }
        float d2 = dot(to_l, to_l) - t_proj * t_proj;
        float r_core = clamp(pr.w * 0.13, 0.60, 4.2);
        float r2 = r_core * r_core;
        if (d2 < r2 * 6.5) {
            float4 ci = params.point_lights_col_int[li];
            float orb = exp(-d2 / max(r2, 0.1)) / (1.0 + t_proj * 0.011);
            glow_sum += ci.rgb * (ci.w * orb * 0.36);
        }
    }
    return min(glow_sum, float3(2.2, 2.2, 2.2));
}

float3 kind_colour(float kind) {
    if (kind < 0.5) return float3(0.04, 0.04, 0.04);
    if (kind < 1.5) return float3(0.0, 0.85, 1.0);   // cyan: water quad
    if (kind < 2.5) return float3(1.0, 0.55, 0.0);   // orange: LOD terrain
    if (kind < 3.5) return float3(0.15, 0.90, 0.25); // green: real mesh
    return float3(0.95, 0.15, 0.85);                 // magenta: instance proxy
}

[numthreads(8, 4, 1)]
void trace_pass(uint3 id : SV_DispatchThreadID) {
    uint2 extent = uint2(uint(params.extents.x), uint(params.extents.y));
    if (id.x >= extent.x || id.y >= extent.y) {
        return;
    }

    float4 pos_d = src_gbuf_pos.Load(int3(id.xy, 0));
    float dist = pos_d.w;
    if (dist <= 0.0) {
        float3 sky_rgb = src_gbuf_raster.Load(int3(id.xy, 0)).rgb;
        bool layered_sky = params.effects.w > 0.5;
        out_colour[id.xy] = float4(layered_sky ? float3(0.0, 0.0, 0.0) : sky_rgb, 1.0);
        if (params.point_light_meta.w > 0.5) {
            out_guide[id.xy] = float4(1.0, 1.0, 0.0, 0.0);
        }
        out_reflection[id.xy] = float4(0.0, 0.0, 0.0, 1.0);
        out_normals[id.xy] = float4(0.0, 1.0, 0.0, 1.0);
        if (params.point_light_meta.z > 0.5) {
            out_albedo[id.xy] = float4(0.0, 0.0, 0.0, 0.04);
        }
        return;
    }

    float3 world_pos = pos_d.xyz;
    float4 norm_r = src_gbuf_norm.Load(int3(id.xy, 0));
    float3 normal = norm_r.xyz;
    float nlen = length(normal);
    normal = nlen > 1e-4 ? (normal / nlen) : float3(0.0, 1.0, 0.0);
    float roughness = clamp(norm_r.w, 0.04, 1.0);
    float4 raster_rgba = src_gbuf_raster.Load(int3(id.xy, 0));
    float3 raster_rgb = raster_rgba.rgb;
    float water_depth = raster_rgba.a;
    float3 view_dir = normalize(world_pos - params.eye.xyz);
    bool is_water = roughness <= 0.12;
    bool is_glossy_trim = !is_water && roughness <= 0.36;

    bool layered = params.effects.w > 0.5;
    uint effects = uint(round(params.effects.y));
    bool bits_shadows = (effects & 1u) != 0u;
    bool bits_lighting = (effects & 2u) != 0u;
    bool bits_reflections = (effects & 4u) != 0u;
    bool bits_ao = (effects & 8u) != 0u;
    bool bits_gi = (effects & 16u) != 0u;
    bool bits_refraction = (effects & 32u) != 0u;
    bool bits_caustics = (effects & 64u) != 0u;

    float steepness = 1.0 - saturate(normal.y);
    float slope_bias = steepness * (0.75 + dist * 0.0065);
    float bias = max(params.effects.z, max(0.18, dist * 0.0035)) + slope_bias;
    float3 ray_origin = world_pos + normal * bias + float3(0.0, 1.0, 0.0) * (steepness * 0.55) - view_dir * (bias * 0.35);
    float shadow_t_min = max(bias * 1.15, 0.60 + steepness * 0.85);
    float ao_t_min = max(bias * 0.80, 0.40);
    float n_dot_l = max(dot(normal, params.sun_dir.xyz), 0.0);

    float fog_t = 0.0;
    float fog_start = params.fog.x;
    float fog_end = params.fog.y;
    if (fog_end > fog_start && fog_start >= 0.0) {
        fog_t = saturate((dist - fog_start) / (fog_end - fog_start));
    }

    uint debug_mode_early = uint(round(params.counts.w));
    if (fog_t >= 0.99 && layered && debug_mode_early == 0u) {
        out_colour[id.xy] = float4(0.0, 0.0, 0.0, 1.0);
        if (params.point_light_meta.w > 0.5) {
            out_guide[id.xy] = float4(1.0, 1.0, dist, 1.0);
        }
        out_reflection[id.xy] = float4(0.0, 0.0, 0.0, 1.0);
        out_normals[id.xy] = float4(normal, roughness);
        if (params.point_light_meta.z > 0.5) {
            out_albedo[id.xy] = float4(raster_rgb, roughness);
        }
        return;
    }

    uint samples = max(uint(params.frame.y), 1u);
    uint max_bounces = min(uint(round(params.frame.z)), 6u);
    uint opaque_offset = uint(round(params.fog.w));
    uint tri_count = uint(params.counts.x);
    uint pl_count = min(uint(round(params.point_light_meta.x)), 128u);
    uint state = id.x * 0x9e3779b9u ^ id.y * 0x85ebca6bu ^ uint(params.frame.x) * 0xc2b2ae35u;

    float accum_shadow = 0.0;
    float3 accum_trans = float3(0.0, 0.0, 0.0);
    float accum_open = 0.0;
    float3 accum_reflection = float3(0.0, 0.0, 0.0);
    float accum_refl_dist = 0.0;
    float3 accum_gi = float3(0.0, 0.0, 0.0);

    float ao_radius = clamp(params.fog.z, 2.0, 6.0);
    uint ao_samples = (dist < 48.0) ? 3u : ((dist < 140.0) ? 2u : 1u);
    float ao_fade = 1.0 - saturate((dist - 200.0) / 40.0);
    float ign_v = ign(id.xy, uint(params.frame.x));
    float rot_angle = ign_v * (2.0 * PI);

    float3 up = abs(normal.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 t_axis = normalize(cross(up, normal));
    float3 b_axis = cross(normal, t_axis);

    float3 sun_up = abs(params.sun_dir.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 sun_t = normalize(cross(sun_up, params.sun_dir.xyz));
    float3 sun_b = cross(params.sun_dir.xyz, sun_t);
    float spread_tan = (params.lens.z > 1e-4) ? tan(params.lens.z) : 0.0;
    float sun_luma = luma(params.sun_colour.xyz);
    float sun_strength = smoothstep(0.28, 0.48, sun_luma);
    float gi_fade = 1.0 - saturate((dist - 150.0) / 70.0);

    for (uint s = 0u; s < samples; s = s + 1u) {
        // Feature #7 (Contact-Hardening Soft Shadows / PCSS) + Feature #4 (Foliage Transmission):
        // Tap 0 measures exact blocker distance `t_blocker`; Tap 1 scales penumbra spread
        // proportionally so shadows stay razor-sharp at bases/trunks and soften further away.
        float shadow = 1.0;
        float3 trans_tint = float3(0.0, 0.0, 0.0);
        if (bits_shadows && n_dot_l > 0.02 && fog_t < 0.96) {
            bool fast_shadow = dist > 240.0;
            uint sun_taps = (spread_tan > 0.0 && dist < 140.0) ? 2u : 1u;
            ShadowResult sr0 = trace_sun_shadow(
                ray_origin, params.sun_dir.xyz, shadow_t_min, params.lens.y,
                state, opaque_offset, tri_count, fast_shadow);
            float vis_sum = sr0.occluded ? 0.0 : 1.0;
            if (sr0.occluded) {
                trans_tint += sr0.transmittance;
            }
            if (sun_taps > 1u) {
                float pcss_scale = sr0.occluded
                    ? clamp(sr0.t_blocker / 18.0, 0.14, 1.45)
                    : 0.75;
                float r = sqrt(0.75) * spread_tan * pcss_scale;
                float theta = 2.39996323 + rot_angle + float(s) * 1.6180339;
                float s_th, c_th;
                sincos(theta, s_th, c_th);
                float3 shadow_dir1 = normalize(params.sun_dir.xyz + (sun_t * c_th + sun_b * s_th) * r);
                ShadowResult sr1 = trace_sun_shadow(
                    ray_origin, shadow_dir1, shadow_t_min, params.lens.y,
                    state ^ 1u, opaque_offset, tri_count, fast_shadow);
                if (!sr1.occluded) {
                    vis_sum += 1.0;
                } else {
                    trans_tint += sr1.transmittance;
                }
            }
            shadow = vis_sum / float(sun_taps);
            trans_tint /= float(sun_taps);
        }
        accum_shadow += shadow;
        accum_trans += trans_tint;

        // Stratified cosine-weighted contact ambient occlusion.
        float open = 1.0;
        if (bits_ao && ao_fade > 0.01 && !is_water) {
            uint hits = 0u;
            for (uint i = 0u; i < ao_samples; i = i + 1u) {
                float u1 = (float(i) + rand01(state)) / float(ao_samples);
                float phi = float(i) * 2.39996323 + rot_angle + float(s) * 0.7548776;
                float r = sqrt(u1);
                float s_phi, c_phi;
                sincos(phi, s_phi, c_phi);
                float z = sqrt(max(0.0, 1.0 - u1));
                float3 d = t_axis * (r * c_phi) + b_axis * (r * s_phi) + normal * z;
                if (occluded_fast(ray_origin, d, ao_t_min, ao_radius)) {
                    hits = hits + 1u;
                }
            }
            open = lerp(1.0, 1.0 - float(hits) / float(ao_samples), ao_fade);
        }
        accum_open += open;

        // Features #2 (Water Refraction & Underwater Caustics), #3 (Calibrated Glossy Trims),
        // #8 (Analytical Reflections of World Effects & Point Lights), #10 (Off-Screen Detail).
        float refl_dist = params.lens.y;
        if (bits_reflections && roughness <= params.effects.x) {
            float3 h = ggx_sample(normal, roughness, rand01(state), rand01(state));
            float3 reflect_dir = reflect(view_dir, h);
            if (dot(reflect_dir, normal) <= 0.0) {
                reflect_dir = reflect(view_dir, normal);
            }
            // Feature #3: Conservative Fresnel & subtle cap on glossy trims so matte
            // walls/terrain stay matte and glazed/metallic accents never look wet/plastic.
            float f0 = is_water ? 0.18 : 0.04;
            float m = 1.0 - saturate(dot(-view_dir, h));
            float m2 = m * m;
            float fresnel = f0 + (1.0 - f0) * (m2 * m2 * m);
            float trim_scale = is_water
                ? 1.0
                : (0.28 * (1.0 - smoothstep(0.24, 0.37, roughness)));
            float refl_max_t = is_water ? min(params.lens.y, 900.0) : min(params.lens.y, 180.0);
            Hit rhit = closest_hit(ray_origin, reflect_dir, bias, refl_max_t, opaque_offset, tri_count);
            float eff_max_t = (rhit.index != 0xffffffffu) ? rhit.t : refl_max_t;

            float3 refl_col = float3(0.0, 0.0, 0.0);
            if (rhit.index != 0xffffffffu) {
                float3 hit_pos = ray_origin + reflect_dir * rhit.t;
                float4 screen_col = sample_screen_radiance(hit_pos, rhit.t);
                float screen_w = saturate(screen_col.a);
                float3 lit = screen_col.rgb;
                if (screen_w < 0.99) {
                    Material rmat = material_of(rhit.index);
                    float rkind = tris[rhit.index].a.w;
                    float3 r_albedo = (rkind > 1.5 && rkind < 2.5)
                        ? sample_terrain_albedo(hit_pos, rmat.albedo.rgb)
                        : detail_offscreen_albedo(hit_pos, rhit.normal, rmat.albedo.rgb, rmat.surface.w);
                    float r_ndl = max(dot(rhit.normal, params.sun_dir.xyz), 0.0);
                    float r_shadow = 1.0;
                    if (bits_shadows && r_ndl > 0.02 && rhit.t < 260.0) {
                        float3 rhit_pos = hit_pos + rhit.normal * bias;
                        if (occluded_fast(rhit_pos, params.sun_dir.xyz, bias * 0.5, params.lens.y)) {
                            r_shadow = 0.0;
                        }
                    }
                    float3 offscreen_lit = r_albedo * (params.ambient.xyz * params.tuning.z +
                        params.sun_colour.xyz * (r_ndl * r_shadow));
                    lit = lerp(offscreen_lit, screen_col.rgb, screen_w);
                }
                refl_col = lit * (fresnel * trim_scale * (1.0 - fog_t));
                refl_dist = rhit.t;
            } else {
                float3 sky_col = sample_sky_reflection(reflect_dir);
                refl_col = sky_col * (fresnel * trim_scale * 0.65 * (1.0 - fog_t));
            }

            // Feature #8: Analytical reflections of animated world effects & point lights.
            if (bits_lighting && pl_count > 0u && fog_t < 0.92) {
                float3 fx_refl = trace_effect_light_reflections(ray_origin, reflect_dir, eff_max_t, pl_count);
                refl_col += fx_refl * (fresnel * trim_scale * (1.0 - fog_t));
            }

            // Feature #2: True RT Water Refraction (Snell + Beer-Lambert absorption)
            // and shadow-gated animated refractive sun caustics on riverbeds & shores.
            if (is_water && bits_refraction && fog_t < 0.90) {
                float wdepth = (water_depth > 0.01) ? water_depth : 6.0;
                float3 refract_dir = refract(view_dir, normal, 1.0 / 1.333);
                float3 beer = exp(-wdepth * float3(0.24, 0.085, 0.045));
                float refract_w = (1.0 - fresnel) * (1.0 - fog_t);
                // Subtle turquoise depth absorption tint in shallow/mid water.
                float shallow_clear = exp(-wdepth * 0.16);
                float3 depth_tint = (beer - float3(1.0, 1.0, 1.0)) * (raster_rgb * 0.28 * refract_w);
                refl_col = max(refl_col + depth_tint, float3(0.0, 0.0, 0.0));
                if (bits_caustics && wdepth < 12.0 && shadow > 0.05 && n_dot_l > 0.08 && sun_strength > 0.05) {
                    float refr_t = wdepth / max(-refract_dir.y, 0.25);
                    float3 seabed_pos = world_pos + refract_dir * refr_t;
                    float caust = caustic_pattern(seabed_pos.xz, params.frame.x * 0.025);
                    float c_fade = smoothstep(0.15, 0.90, wdepth) * shallow_clear * exp(-dist * 0.015);
                    refl_col += params.sun_colour.xyz * beer * (caust * shadow * n_dot_l * sun_strength * refract_w * c_fade * 0.16);
                }
            }
            accum_reflection += refl_col;
        }
        accum_refl_dist += refl_dist;

        // Calibrated multi-bounce RT GI + Feature #6 (3D World-Space Radiance Cache)
        // + Feature #10 (Detailed off-screen albedo). Zero chalky grey/white wash!
        if (bits_gi && !is_water && max_bounces >= 1u && params.tuning.w > 0.0 && gi_fade > 0.01) {
            uint gi_taps = (dist < 90.0) ? 4u : ((dist < 180.0) ? 2u : 1u);
            float3 gi_sum = float3(0.0, 0.0, 0.0);
            float bounce_max_t = min(params.lens.y, 56.0);
            float day_gi_scale = 0.03 + 0.97 * sun_strength;
            for (uint g = 0u; g < gi_taps; g = g + 1u) {
                float3 throughput = float3(1.0, 1.0, 1.0);
                float3 b_origin = ray_origin;
                float3 b_t = t_axis;
                float3 b_b = b_axis;
                float3 b_n = normal;
                float b_step_bias = bias;
                uint tap_bounces = (g == 0u) ? max_bounces : 1u;
                for (uint bounce = 0u; bounce < tap_bounces; bounce = bounce + 1u) {
                    float u1 = (bounce == 0u) ? ((float(g) + rand01(state)) / float(gi_taps)) : rand01(state);
                    float phi = (bounce == 0u)
                        ? (float(g) * 2.39996323 + rot_angle + 1.0471975 + float(s) * 0.7548776)
                        : (2.0 * PI * rand01(state));
                    float r = sqrt(u1);
                    float s_phi, c_phi;
                    sincos(phi, s_phi, c_phi);
                    float z = sqrt(max(0.0, 1.0 - u1));
                    float3 d = b_t * (r * c_phi) + b_b * (r * s_phi) + b_n * z;
                    Hit bhit;
                    if (bounce == 0u && dist < 45.0) {
                        bhit = closest_hit(b_origin, d, b_step_bias, bounce_max_t, opaque_offset, tri_count);
                    } else {
                        bhit = closest_hit_fast(b_origin, d, b_step_bias, bounce_max_t, opaque_offset, tri_count);
                    }
                    if (bhit.index != 0xffffffffu) {
                        float b_ndl = max(dot(bhit.normal, params.sun_dir.xyz), 0.0);
                        float3 bhit_world = b_origin + d * bhit.t;
                        float b_bias = max(b_step_bias, (dist + bhit.t) * 0.004);
                        float b_shadow = 1.0;
                        if (bits_shadows && bounce < 2u && b_ndl > 0.08) {
                            float3 bhit_pos = bhit_world + bhit.normal * b_bias;
                            if (occluded_fast(bhit_pos, params.sun_dir.xyz, b_bias * 0.5, params.lens.y)) {
                                b_shadow = 0.0;
                            }
                        }
                        Material bmat = material_of(bhit.index);
                        float bkind = tris[bhit.index].a.w;
                        float3 b_base_albedo = (bkind > 1.5 && bkind < 2.5)
                            ? sample_terrain_albedo_fast(bhit_world, bmat.albedo.rgb)
                            : detail_offscreen_albedo(bhit_world, bhit.normal, bmat.albedo.rgb, bmat.surface.w);
                        float b_avg = dot(b_base_albedo, float3(0.3333, 0.3333, 0.3333));
                        float3 b_chroma = clamp(
                            float3(b_avg, b_avg, b_avg) + (b_base_albedo - b_avg) * 1.25,
                            float3(0.04, 0.04, 0.04),
                            float3(0.90, 0.90, 0.90));
                        float3 off_in = b_chroma * (
                            params.sun_colour.xyz * (b_ndl * b_shadow * 1.15)
                            + params.ambient.xyz * (params.tuning.z * 0.20));
                        float3 incoming = off_in;
                        if (bounce == 0u && bhit.t < 40.0) {
                            float4 b_screen = sample_screen_radiance(bhit_world, bhit.t);
                            float b_w = saturate(b_screen.a);
                            float3 on_in = b_screen.rgb * (b_ndl * b_shadow * 0.85 * sun_strength + 0.10 * day_gi_scale);
                            incoming = lerp(off_in, on_in, b_w);
                        }
                        // Feature #6: Query & update 3D World-Space Radiance Cache for deep multi-bounce GI.
                        float4 cached = rc_query(bhit_world, bhit.normal, dist + bhit.t);
                        if (cached.w >= 3.0) {
                            float cache_blend = (bounce == 0u) ? 0.25 : 0.45;
                            incoming = lerp(incoming, cached.rgb, cache_blend);
                        }
                        if (bounce == 0u && g == 0u && ((id.x + id.y + uint(params.frame.x)) & 3u) == 0u) {
                            rc_update(bhit_world, bhit.normal, dist + bhit.t, incoming);
                        }
                        incoming = min(incoming, float3(1.10, 1.10, 1.10));
                        gi_sum += throughput * incoming * (params.tuning.w * 0.85 * (1.0 - fog_t));
                        if (bounce + 1u < tap_bounces) {
                            throughput *= clamp(b_chroma * 1.02, float3(0.08, 0.08, 0.08), float3(0.84, 0.84, 0.84));
                            if (bounce >= 2u) {
                                float rr = clamp(max(throughput.r, max(throughput.g, throughput.b)), 0.30, 0.88);
                                if (rand01(state) > rr) {
                                    break;
                                }
                                throughput /= rr;
                            }
                            b_origin = bhit_world + bhit.normal * b_bias;
                            b_n = bhit.normal;
                            float3 bup = abs(b_n.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
                            b_t = normalize(cross(bup, b_n));
                            b_b = cross(b_n, b_t);
                            b_step_bias = b_bias;
                        }
                    } else {
                        // Calibrated directional sky-dome GI (matches replaced ambient energy,
                        // preventing grey/white wash over dark stone walls under DLSS RR).
                        float3 raw_sky = min(sample_sky_env(d), float3(1.10, 1.10, 1.10));
                        float sky_l = luma(raw_sky);
                        float3 vivid_sky = max(float3(sky_l, sky_l, sky_l) + (raw_sky - sky_l) * 1.25, float3(0.0, 0.0, 0.0));
                        float sky_w = (bounce == 0u) ? 0.24 : 0.14;
                        gi_sum += throughput * vivid_sky * (params.tuning.w * sky_w * day_gi_scale * (1.0 - fog_t));
                        break;
                    }
                }
            }
            accum_gi += min(gi_sum * (gi_fade / float(gi_taps)), float3(1.15, 1.15, 1.15));
        }
    }

    // Feature #4: Backlit foliage/blossom subsurface scattering + translucent canopy light.
    float3 sss_light = float3(0.0, 0.0, 0.0);
    if (bits_shadows && !is_water && fog_t < 0.92 && sun_strength > 0.05) {
        float3 avg_trans = accum_trans / float(samples);
        if (dot(avg_trans, float3(1.0, 1.0, 1.0)) > 1e-4) {
            sss_light += avg_trans * params.sun_colour.xyz * (n_dot_l * 0.55 * (1.0 - fog_t));
        }
        float back_dot = saturate(dot(view_dir, params.sun_dir.xyz));
        bool is_foliage_col = (raster_rgb.g > raster_rgb.b * 1.10 && raster_rgb.g > 0.10)
            || (raster_rgb.r > raster_rgb.g * 1.15 && raster_rgb.b > raster_rgb.g * 1.02);
        if (is_foliage_col && back_dot > 0.35 && n_dot_l < 0.35) {
            float b2 = back_dot * back_dot;
            sss_light += params.sun_colour.xyz * (b2 * b2 * 0.22 * sun_strength * (1.0 - fog_t));
        }
    }

    // Feature #2 (Underwater Caustics): Project organic Voronoi sun caustics onto
    // submerged terrain, rocks, and ruins when beneath the active water plane.
    float3 underwater_caustics = float3(0.0, 0.0, 0.0);
    float water_y = params.eye.w;
    if (bits_caustics && !is_water && water_y > -1e5 && world_pos.y < water_y - 0.05 && fog_t < 0.95 && sun_strength > 0.05) {
        float sub_depth = water_y - world_pos.y;
        if (sub_depth < 28.0 && n_dot_l > 0.05) {
            float uw_vis = bits_shadows ? (accum_shadow / float(samples)) : 1.0;
            if (uw_vis > 0.05) {
                float caust = caustic_pattern(world_pos.xz, params.frame.x * 0.025);
                float3 beer = exp(-sub_depth * float3(0.18, 0.07, 0.04));
                float fade = smoothstep(0.05, 0.50, sub_depth) * exp(-sub_depth * 0.065) * exp(-dist * 0.008);
                underwater_caustics = params.sun_colour.xyz * beer * (caust * uw_vis * n_dot_l * sun_strength * fade * 0.85);
            }
        }
    }

    // Feature #1: Sculpted 12-step Ray-Traced Volumetric God Rays.
    // Depth-stratified steps + occlusion-contrast weighting produce real sunbeams
    // carved by canopies, arches, and cliffs without a flat foggy plane over open sky.
    float3 god_rays = float3(0.0, 0.0, 0.0);
    if (bits_shadows && params.sun_dir.y > 0.05 && dist > 14.0 && fog_t < 0.94) {
        float cos_view_sun = dot(view_dir, params.sun_dir.xyz);
        if (cos_view_sun > 0.30) {
            float phase = hg_phase(cos_view_sun, 0.86);
            float march_len = min(dist, 155.0) - 3.0;
            if (march_len > 8.0 && phase > 0.06) {
                float lit_steps = 0.0;
                float occ_steps = 0.0;
                for (uint vi = 0u; vi < 12u; vi = vi + 1u) {
                    float u = (float(vi) + ign_v) / 12.0;
                    float t_vol = 3.0 + (u * sqrt(u)) * march_len;
                    float3 p_vol = params.eye.xyz + view_dir * t_vol;
                    if (occluded_fast(p_vol, params.sun_dir.xyz, 0.5, 420.0)) {
                        occ_steps += 1.0;
                    } else {
                        lit_steps += 1.0;
                    }
                }
                float shaft_vis = lit_steps / 12.0;
                float occ_frac = occ_steps / 12.0;
                // Only boost into visible shafts when geometry actually slices the sunbeam;
                // open unoccluded terrain stays clean instead of a flat foggy plane.
                float shaft_contrast = lerp(0.12, 1.0, smoothstep(0.04, 0.38, occ_frac));
                float dist_w = smoothstep(14.0, 95.0, march_len);
                float3 shaft_col = lerp(float3(0.28, 0.38, 0.58), params.sun_colour.xyz, sun_strength);
                float shaft_intensity = lerp(0.025, 0.065, sun_strength);
                god_rays = shaft_col * (shaft_vis * shaft_contrast * min(phase, 1.4) * dist_w * shaft_intensity * (1.0 - fog_t * 0.7));
            }
        }
    }

    // Feature #5: ReSTIR DI (2-Reservoir Weighted Importance Resampling) across up to 128
    // scene point lights + luminous world effects (Archosaur ring beams, portals, lanterns).
    // Traces at most 2 RT shadow rays per pixel regardless of active light density!
    uint debug_mode = uint(round(params.counts.w));
    bool pl_shadows_enabled = params.point_light_meta.y > 0.5;
    float3 point_lighting = float3(0.0, 0.0, 0.0);
    float3 pl_debug_lit = float3(0.0, 0.0, 0.0);
    float pl_debug_shadow = 0.0;
    float pl_w_total = 0.0;
    float pl_w_lit = 0.0;

    if (bits_lighting && pl_count > 0u) {
        uint res0_idx = 0xffffffffu;
        float res0_wsum = 0.0;
        float res0_phat = 0.0;
        float3 res0_col = float3(0.0, 0.0, 0.0);
        float res0_w = 0.0;

        uint res1_idx = 0xffffffffu;
        float res1_wsum = 0.0;
        float res1_phat = 0.0;
        float3 res1_col = float3(0.0, 0.0, 0.0);
        float res1_w = 0.0;

        uint cand_count = 0u;
        uint first_idx = 0xffffffffu;
        uint second_idx = 0xffffffffu;
        float first_w = 0.0;
        float second_w = 0.0;
        float3 first_col = float3(0.0, 0.0, 0.0);
        float3 second_col = float3(0.0, 0.0, 0.0);

        for (uint li = 0u; li < pl_count; li = li + 1u) {
            float4 pl_pr = params.point_lights_pos_rad[li];
            float3 to_l = pl_pr.xyz - world_pos;
            if (abs(to_l.x) >= pl_pr.w || abs(to_l.z) >= pl_pr.w || abs(to_l.y) >= pl_pr.w) {
                continue;
            }
            float pl_d2 = dot(to_l, to_l);
            if (pl_d2 <= 1e-6 || pl_d2 >= pl_pr.w * pl_pr.w) {
                continue;
            }
            float pl_dist = sqrt(pl_d2);
            float3 l_dir = to_l / pl_dist;
            float pl_ndl = max(dot(normal, l_dir), 0.0);
            if (pl_ndl <= 0.001) {
                continue;
            }
            float atten = point_light_falloff(pl_dist, pl_pr.w);
            if (atten <= 1e-5) {
                continue;
            }
            float4 pl_ci = params.point_lights_col_int[li];
            float w = pl_ci.w * atten * pl_ndl;
            float p_hat = max(luma(pl_ci.rgb), 0.05) * w;
            if (p_hat <= 1e-6) {
                continue;
            }
            cand_count = cand_count + 1u;
            if (cand_count == 1u) {
                first_idx = li;
                first_w = w;
                first_col = pl_ci.rgb;
            } else if (cand_count == 2u) {
                second_idx = li;
                second_w = w;
                second_col = pl_ci.rgb;
            }

            // Reservoir 0 update
            res0_wsum += p_hat;
            if (rand01(state) * res0_wsum <= p_hat) {
                res0_idx = li;
                res0_phat = p_hat;
                res0_col = pl_ci.rgb;
                res0_w = w;
            }
            // Reservoir 1 update
            res1_wsum += p_hat;
            if (rand01(state) * res1_wsum <= p_hat) {
                res1_idx = li;
                res1_phat = p_hat;
                res1_col = pl_ci.rgb;
                res1_w = w;
            }
        }

        if (cand_count > 0u && cand_count <= 2u) {
            // Exact evaluation when 1..=2 lights touch this pixel (zero resampling variance).
            uint idxs[2] = { first_idx, second_idx };
            float ws[2] = { first_w, second_w };
            float3 cols[2] = { first_col, second_col };
            for (uint ci = 0u; ci < cand_count; ci = ci + 1u) {
                uint idx = idxs[ci];
                float4 pl_pr = params.point_lights_pos_rad[idx];
                float3 to_l = pl_pr.xyz - world_pos;
                float pl_dist = length(to_l);
                float3 l_dir = to_l / max(pl_dist, 1e-5);
                float pl_vis = 1.0;
                if (bits_shadows && pl_dist > 1.5 && ws[ci] > 0.015) {
                    if (occluded_fast(ray_origin, l_dir, bias * 0.5, max(pl_dist - bias, bias))) {
                        pl_vis = 0.0;
                    }
                }
                pl_w_total += ws[ci];
                pl_w_lit += ws[ci] * pl_vis;
                pl_debug_lit += cols[ci] * (ws[ci] * pl_vis);
                pl_debug_shadow += ws[ci] * (1.0 - pl_vis);
                point_lighting += cols[ci] * (ws[ci] * pl_vis * 2.1);
            }
        } else if (cand_count > 2u) {
            // ReSTIR DI unbiased 2-reservoir RIS evaluation (2 shadow rays total for 3..128 lights).
            uint r_idxs[2] = { res0_idx, res1_idx };
            float r_wsums[2] = { res0_wsum, res1_wsum };
            float r_phats[2] = { res0_phat, res1_phat };
            float r_ws[2] = { res0_w, res1_w };
            float3 r_cols[2] = { res0_col, res1_col };
            for (uint ri = 0u; ri < 2u; ri = ri + 1u) {
                uint idx = r_idxs[ri];
                if (idx == 0xffffffffu || r_phats[ri] <= 1e-6) {
                    continue;
                }
                float4 pl_pr = params.point_lights_pos_rad[idx];
                float3 to_l = pl_pr.xyz - world_pos;
                float pl_dist = length(to_l);
                float3 l_dir = to_l / max(pl_dist, 1e-5);
                float pl_vis = 1.0;
                if (bits_shadows && pl_dist > 1.5) {
                    if (occluded_fast(ray_origin, l_dir, bias * 0.5, max(pl_dist - bias, bias))) {
                        pl_vis = 0.0;
                    }
                }
                float ris_scale = min(r_wsums[ri] / max(r_phats[ri], 1e-6), 8.0) * 0.5;
                float eff_w = r_ws[ri] * ris_scale;
                pl_w_total += eff_w;
                pl_w_lit += eff_w * pl_vis;
                pl_debug_lit += r_cols[ri] * (eff_w * pl_vis);
                pl_debug_shadow += eff_w * (1.0 - pl_vis);
                point_lighting += r_cols[ri] * (eff_w * pl_vis * 2.1);
            }
        }
        point_lighting = min(point_lighting * (1.0 - fog_t), float3(3.5, 3.5, 3.5));
        // Feed bright nearby point/effect lighting into the 3D World-Space Radiance Cache
        // so lanterns and beams bounce coloured light onto surrounding walls and ceilings!
        if (bits_gi && luma(point_lighting) > 0.08 && ((id.x + id.y + uint(params.frame.x)) & 7u) == 0u) {
            rc_update(world_pos, normal, dist, point_lighting * 0.55);
        }
    }

    float point_vis = (pl_w_total > 1e-4) ? (pl_w_lit / max(pl_w_total, 1e-5)) : 1.0;
    float point_shadow_mod = (pl_shadows_enabled && pl_w_total > 1e-4)
        ? (1.0 - clamp(pl_w_total, 0.0, 0.5) * (1.0 - point_vis))
        : 1.0;

    float inv = 1.0 / float(samples);
    float avg_shadow = accum_shadow * inv;
    float avg_open = accum_open * inv;
    float3 avg_refl = accum_reflection * inv + god_rays;
    float avg_refl_dist = accum_refl_dist * inv;
    float3 avg_gi = accum_gi * inv + sss_light + underwater_caustics;

    float3 out_rgb;
    float out_lf;
    if (layered) {
        float sun_e = max(sun_luma, 0.0) * n_dot_l;
        float amb_e = max(luma(params.ambient.xyz) * params.tuning.z, 0.005);
        float sun_f = saturate(sun_e / max(sun_e + amb_e, 1e-4));
        float amb_f = 1.0 - sun_f;
        float v_sun = bits_shadows ? avg_shadow : 1.0;
        float v_ao = bits_ao ? avg_open : 1.0;
        bool gi_active = bits_gi && !is_water && max_bounces >= 1u && params.tuning.w > 0.0 && gi_fade > 0.01;
        // Keep 18% of raster ambient grounded under GI by day, and darken the
        // quantization-safe night raster carrier (0.095) back to pitch black where unlit
        // so night buildings keep 100% crisp textures under point lights!
        float amb_keep = gi_active ? lerp(1.0, 0.18, gi_fade) : 1.0;
        float night_dark_scale = lerp(0.085, 1.0, smoothstep(0.0, 0.25, sun_strength));
        float separated_lf = (amb_f * v_ao * amb_keep * night_dark_scale + sun_f * v_sun) * point_shadow_mod;
        out_lf = lerp(separated_lf, 1.0, fog_t);
        out_rgb = avg_gi + point_lighting;
    } else {
        float3 direct = bits_lighting ? (params.sun_colour.xyz * (n_dot_l * avg_shadow)) : float3(0.0, 0.0, 0.0);
        float3 ambient = params.ambient.xyz * (avg_open * params.tuning.z);
        float3 relit = raster_rgb * (direct + ambient + avg_gi + point_lighting) + god_rays;
        out_rgb = lerp(relit, params.fog_colour.xyz, fog_t);
        out_lf = 1.0;
    }

    float3 store_refl = avg_refl;
    if (debug_mode == 1u) {
        out_rgb = raster_rgb;
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    } else if (debug_mode == 2u) {
        out_rgb = float3(avg_shadow, avg_shadow, avg_shadow);
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    } else if (debug_mode == 3u) {
        out_rgb = float3(avg_open, avg_open, avg_open);
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    } else if (debug_mode == 4u) {
        out_rgb = avg_gi;
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    } else if (debug_mode == 5u) {
        out_rgb = avg_refl;
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    } else if (debug_mode == 6u) {
        out_rgb = pl_debug_lit * 2.0 + float3(0.55, 0.04, 0.14) * clamp(pl_debug_shadow * 2.5, 0.0, 1.0);
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    } else if (debug_mode == 7u) {
        float occ_kind = 0.0;
        if (is_water) {
            occ_kind = 1.0;
        } else {
            Hit ph = closest_hit(params.eye.xyz, view_dir, 0.1, params.lens.y, opaque_offset, tri_count);
            if (ph.index != 0xffffffffu) {
                occ_kind = tris[ph.index].a.w;
            }
        }
        out_rgb = kind_colour(occ_kind);
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    } else if (debug_mode == 8u) {
        float rkind = 0.0;
        if (roughness <= params.effects.x) {
            float3 rdir = reflect(view_dir, normal);
            Hit rh = closest_hit(ray_origin, rdir, bias, params.lens.y, opaque_offset, tri_count);
            if (rh.index != 0xffffffffu) {
                rkind = tris[rh.index].a.w;
            }
        }
        out_rgb = kind_colour(rkind);
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    } else if (debug_mode == 9u) {
        float sun_e = max(sun_luma, 0.0) * n_dot_l;
        float amb_e = max(luma(params.ambient.xyz) * params.tuning.z, 0.05);
        float sun_f = saturate(sun_e / max(sun_e + amb_e, 1e-4));
        out_rgb = float3(sun_f, avg_shadow, sun_f * avg_shadow);
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    }

    out_colour[id.xy] = float4(out_rgb, out_lf);
    if (params.point_light_meta.w > 0.5) {
        out_guide[id.xy] = float4(avg_open, dist / params.lens.y, avg_shadow, roughness);
    }
    out_reflection[id.xy] = float4(store_refl, avg_refl_dist / params.lens.y);
    out_normals[id.xy] = float4(normal, roughness);
    if (params.point_light_meta.z > 0.5) {
        float spec_f0 = is_water ? 0.18 : (is_glossy_trim ? 0.06 : 0.02);
        out_albedo[id.xy] = float4(raster_rgb, spec_f0);
    }
}
