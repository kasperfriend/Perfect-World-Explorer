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
    float4 fog;     // x: fog_start, y: fog_end, z: ao_radius, w: unused
    float4 counts;  // x: tri_count, y: node_count, z: mat_count, w: debug_view
    float4 point_light_meta;           // x: active light count (<= 48), y: pl_shadows, z: write_albedo, w: write_guide
    float4 point_lights_pos_rad[48];   // xyz: world pos, w: radius
    float4 point_lights_col_int[48];   // rgb: colour, w: intensity
    float4 terrain_map_bounds;         // x: min_x, y: max_z, z: inv_diameter, w: valid (>0.5)
};

struct Tri {
    float4 a;
    float4 b; // .w carries material index as float
    float4 c;
    float4 n;
};

struct Material {
    float4 albedo;  // .rgb diffuse albedo
    float4 surface; // .x roughness, .y metallic
};

cbuffer ParamsBuffer : register(b0) {
    Params params;
};

RaytracingAccelerationStructure scene_tlas : register(t0);
StructuredBuffer<Tri> tris : register(t1);
StructuredBuffer<Material> materials : register(t2);
// 1:1 screen-space G-buffer derived from the raster world pass:
// src_gbuf_pos:    .xyz world position P, .w linear distance from eye (0 = sky)
// src_gbuf_norm:   .xyz world normal N,   .w surface roughness (0.06 water, 0.85 matte)
// src_gbuf_raster: .rgb untouched raster colour at this pixel
Texture2D<float4> src_gbuf_pos : register(t3);
Texture2D<float4> src_gbuf_norm : register(t4);
Texture2D<float4> src_gbuf_raster : register(t5);
// Phase 3: Real sky (SkyGpu) + cloud billboards (CloudGpu) in a 3x2 cubemap atlas.
Texture2D<float4> src_sky_env : register(t6);
// Top-down real textured terrain albedo map (512x512 around camera anchor) for
// off-screen reflections and coloured terrain GI bounces.
Texture2D<float4> src_terrain_map : register(t7);

RWTexture2D<float4> out_colour : register(u0);
RWTexture2D<float4> out_guide : register(u1);
RWTexture2D<float4> out_reflection : register(u2);
RWTexture2D<float4> out_normals : register(u3);
RWTexture2D<float4> out_albedo : register(u4);

static const float PI = 3.14159265;

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

float3 cosine_hemisphere(inout uint state, float3 n) {
    float u1 = rand01(state);
    float u2 = rand01(state);
    float r = sqrt(u1);
    float s_phi, c_phi;
    sincos(2.0 * PI * u2, s_phi, c_phi);
    float z = sqrt(max(0.0, 1.0 - u1));
    float3 up = abs(n.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 t = normalize(cross(up, n));
    float3 b = cross(n, t);
    return t * (r * c_phi) + b * (r * s_phi) + n * z;
}

float3 cone_direction(inout uint state, float3 axis, float radius) {
    float r = sqrt(rand01(state));
    float s_phi, c_phi;
    sincos(2.0 * PI * rand01(state), s_phi, c_phi);
    float spread = r * tan(radius);
    float3 up = abs(axis.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 t = normalize(cross(up, axis));
    float3 b = cross(axis, t);
    return normalize(axis + (t * c_phi + b * s_phi) * spread);
}

struct Hit {
    uint index;
    float t;
    float3 normal;
};

static const float SKY_ENV_FACE_F = 128.0;
static const float TERRAIN_MAP_SIZE_F = 512.0;

Material material_of(uint tri_index) {
    uint mat_count = max(uint(params.counts.z), 1u);
    uint mat_idx = min(uint(tris[tri_index].b.w + 0.5), mat_count - 1u);
    return materials[mat_idx];
}

// Interleaved Gradient Noise for per-pixel decorrelation of shadow and AO disks.
float ign(uint2 px, uint frame) {
    float2 p = float2(px) + float2(frame & 63u, (frame * 7u) & 63u) * 5.588238;
    return frac(52.9829189 * frac(dot(p, float2(0.06711056, 0.00583715))));
}

bool accept_opacity(uint prim, float2 bary, uint seed, uint tri_count) {
    if (prim >= tri_count) {
        return false;
    }
    float opacity = material_of(prim).surface.w;
    if (opacity >= 0.99) {
        return true;
    }
    // Seamless organic foliage opacity test (no barycentric edge_mask so
    // multi-triangle leaf fans, fronds, sails, and banners never show triangle seams).
    uint _unused = seed;
    float clump = 1.0;
    if (opacity < 0.90) {
        uint cx = uint(bary.x * 10.0);
        uint cy = uint(bary.y * 10.0);
        uint ch = prim * 2654435761u ^ cx * 2246822519u ^ cy * 3266489917u;
        ch ^= ch >> 15u;
        clump = 0.84 + 0.28 * (float(ch & 0xffu) / 255.0);
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

// Pure fixed-function RT-core closest-hit (RAY_FLAG_FORCE_OPAQUE | SKIP_PROCEDURAL_PRIMITIVES):
// zero shader ejection, single Proceed() call, used for GI bounces and distant hits.
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

// Phase 4: Reproject world-space hit point into current frame's 1:1 raster
// G-buffer to fetch real textured raster colour when on-screen. Off-screen
// hits return alpha 0 and fall back to harvested per-material baked albedo.
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

// Phase 3: Sample the real rendered SkyGpu + CloudGpu 3x2 cubemap atlas.
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
    // Boost #6: Fixed 3x2 128px sky cubemap dimensions avoid per-ray GetDimensions().
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

// Phase 3: Reflection miss ALWAYS samples the real rendered SkyGpu + CloudGpu
// environment target (analytic gradient removed from the miss path).
float3 sample_sky_reflection(float3 dir) {
    return sample_sky_env(dir);
}

// Sample the top-down real textured terrain map for off-screen terrain reflections
// and coloured terrain GI bounces (eliminates flat olive-green terrain).
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

// Boost #5: 1-tap nearest terrain map fetch for diffuse GI bounces (4x fewer loads).
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

bool occluded(float3 origin, float3 dir, float t_min, float t_max, uint seed, uint opaque_offset, uint tri_count) {
    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = t_min;
    ray.TMax = t_max;

    RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray);
    while (q.Proceed()) {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE) {
            uint cand_prim = q.CandidatePrimitiveIndex() + (q.CandidateGeometryIndex() != 0u ? opaque_offset : 0u);
            if (accept_opacity(cand_prim, q.CandidateTriangleBarycentrics(), seed, tri_count)) {
                q.CommitNonOpaqueTriangleHit();
            }
        }
    }
    return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT;
}

// Pure fixed-function RT-core any-hit occlusion (FORCE_OPAQUE + ACCEPT_FIRST_HIT):
// zero shader ejection, used for short-range AO and secondary GI/reflection shadows.
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
    // Sky / background pixels (`dist <= 0`): skip all ray tracing so sky,
    // clouds, and horizon fog pass through 100% untouched at zero RT cost.
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
    float3 raster_rgb = src_gbuf_raster.Load(int3(id.xy, 0)).rgb;
    float3 view_dir = normalize(world_pos - params.eye.xyz);

    bool layered = params.effects.w > 0.5;
    uint effects = uint(round(params.effects.y));
    bool bits_shadows = (effects & 1u) != 0u;
    bool bits_lighting = (effects & 2u) != 0u;
    bool bits_reflections = (effects & 4u) != 0u;
    bool bits_ao = (effects & 8u) != 0u;
    bool bits_gi = (effects & 16u) != 0u;

    // Distance-scaled bias prevents self-intersection between the exact 1:1
    // raster depth surface and the BVH/BLAS geometry at all view distances.
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
    // Early-out on 100% fogged horizon pixels where layered RT weight is 0.
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
    uint state = id.x * 0x9e3779b9u ^ id.y * 0x85ebca6bu ^ uint(params.frame.x) * 0xc2b2ae35u;

    float accum_shadow = 0.0;
    float accum_open = 0.0;
    float3 accum_reflection = float3(0.0, 0.0, 0.0);
    float accum_refl_dist = 0.0;
    float3 accum_gi = float3(0.0, 0.0, 0.0);

    // 4 IGN-rotated stratified cosine-weighted contact AO rays (skipped beyond
    // 260u where 4u contact occlusion is sub-pixel).
    float ao_radius = clamp(params.fog.z, 2.0, 6.0);
    uint ao_samples = (dist < 48.0) ? 3u : ((dist < 140.0) ? 2u : 1u);
    float ao_fade = 1.0 - saturate((dist - 200.0) / 40.0);
    float rot_angle = ign(id.xy, uint(params.frame.x)) * (2.0 * PI);

    float3 up = abs(normal.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 t_axis = normalize(cross(up, normal));
    float3 b_axis = cross(normal, t_axis);

    float3 sun_up = abs(params.sun_dir.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 sun_t = normalize(cross(sun_up, params.sun_dir.xyz));
    float3 sun_b = cross(params.sun_dir.xyz, sun_t);
    float spread_tan = (params.lens.z > 1e-4) ? tan(params.lens.z) : 0.0;
    float sun_luma = dot(params.sun_colour.xyz, float3(0.2126, 0.7152, 0.0722));
    float sun_strength = smoothstep(0.28, 0.48, sun_luma);
    float gi_fade = 1.0 - saturate((dist - 150.0) / 70.0);

    for (uint s = 0u; s < samples; s = s + 1u) {
        // 2-tap IGN-rotated Vogel-disk soft sun shadow penumbra.
        float shadow = 1.0;
        if (bits_shadows && n_dot_l > 0.02 && fog_t < 0.96) {
            uint sun_taps = (spread_tan > 0.0 && dist < 140.0) ? 2u : 1u;
            uint sun_hits = 0u;
            for (uint k = 0u; k < sun_taps; k = k + 1u) {
                float3 shadow_dir = params.sun_dir.xyz;
                if (sun_taps > 1u) {
                    float r = sqrt((float(k) + 0.5) / float(sun_taps)) * spread_tan;
                    float theta = float(k) * 2.39996323 + rot_angle + float(s) * 1.6180339;
                    float s_th, c_th;
                    sincos(theta, s_th, c_th);
                    shadow_dir = normalize(params.sun_dir.xyz + (sun_t * c_th + sun_b * s_th) * r);
                }
                bool hit_s = false;
                if (dist > 240.0) {
                    // Distant shadows (>240u): pure fixed-function RT-core opaque hit (zero shader ejection).
                    hit_s = occluded_fast(ray_origin, shadow_dir, shadow_t_min, params.lens.y);
                } else {
                    hit_s = occluded(ray_origin, shadow_dir, shadow_t_min, params.lens.y, state ^ k, opaque_offset, tri_count);
                }
                if (hit_s) {
                    sun_hits = sun_hits + 1u;
                }
            }
            shadow = 1.0 - float(sun_hits) / float(sun_taps);
        }
        accum_shadow += shadow;

        // 4-sample stratified cosine-weighted contact ambient occlusion.
        float open = 1.0;
        if (bits_ao && ao_fade > 0.01 && roughness > 0.12) {
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

        // GGX importance-sampled rough reflections with smooth screen/off-screen blending.
        float refl_dist = params.lens.y;
        if (bits_reflections && roughness <= params.effects.x) {
            float3 h = ggx_sample(normal, roughness, rand01(state), rand01(state));
            float3 reflect_dir = reflect(view_dir, h);
            if (dot(reflect_dir, normal) <= 0.0) {
                reflect_dir = reflect(view_dir, normal);
            }
            float f0 = 0.04 + 0.16 * (1.0 - roughness);
            float m = 1.0 - saturate(dot(-view_dir, h));
            float m2 = m * m;
            float fresnel = f0 + (1.0 - f0) * (m2 * m2 * m);
            float refl_max_t = min(params.lens.y, 900.0);
            Hit rhit = closest_hit(ray_origin, reflect_dir, bias, refl_max_t, opaque_offset, tri_count);
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
                        : rmat.albedo.rgb;
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
                accum_reflection += lit * fresnel * (1.0 - fog_t);
                refl_dist = rhit.t;
            } else {
                float3 sky_col = sample_sky_reflection(reflect_dir);
                accum_reflection += sky_col * (fresnel * 0.65 * (1.0 - fog_t));
            }
        }
        accum_refl_dist += refl_dist;

        // Real physically-grounded Ray-Traced Global Illumination (Sky Hemisphere +
        // 1..=6 bounce coloured surface indirect lighting). Replaces flat raster ambient
        // in layered mode so there is zero double-ambient grey tint and real sky/sun
        // bounce colour bleeding across terrain, buildings, and overhangs.
        // Boost #7: Water pixels use rt.rgb purely for mirror reflections; skip GI rays on water.
        if (bits_gi && roughness > 0.12 && max_bounces >= 1u && params.tuning.w > 0.0 && gi_fade > 0.01) {
            {
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
                        // Boost #1: Use fixed-function RT-core opaque hit past 45u on bounce 0.
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
                            // Boost #4: Skip secondary shadow ray on high bounces or grazing hits.
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
                                : bmat.albedo.rgb;
                            float b_avg = dot(b_base_albedo, float3(0.3333, 0.3333, 0.3333));
                            float3 b_chroma = clamp(
                                float3(b_avg, b_avg, b_avg) + (b_base_albedo - b_avg) * 1.35,
                                float3(0.04, 0.04, 0.04),
                                float3(0.94, 0.94, 0.94));
                            float3 off_in = b_chroma * (
                                params.sun_colour.xyz * (b_ndl * b_shadow * 1.65)
                                + params.ambient.xyz * (params.tuning.z * 0.30));
                            float3 incoming = off_in;
                            if (bounce == 0u && bhit.t < 40.0) {
                                float4 b_screen = sample_screen_radiance(bhit_world, bhit.t);
                                float b_w = saturate(b_screen.a);
                                float3 on_in = b_screen.rgb * (b_ndl * b_shadow * 1.15 * sun_strength + 0.12 * day_gi_scale);
                                incoming = lerp(off_in, on_in, b_w);
                            }
                            incoming = min(incoming, float3(1.65, 1.65, 1.65));
                            gi_sum += throughput * incoming * (params.tuning.w * (1.0 - fog_t));
                            if (bounce + 1u < tap_bounces) {
                                throughput *= clamp(b_chroma * 1.08, float3(0.08, 0.08, 0.08), float3(0.88, 0.88, 0.88));
                                if (bounce >= 2u) {
                                    float rr = clamp(max(throughput.r, max(throughput.g, throughput.b)), 0.32, 0.92);
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
                            // Real directional sky-dome GI from rendered SkyGpu + CloudGpu cubemap
                            // (blue overhead sky at noon, warm gold at sunset, near-black at night).
                            float3 raw_sky = min(sample_sky_env(d), float3(1.35, 1.35, 1.35));
                            float sky_l = dot(raw_sky, float3(0.2126, 0.7152, 0.0722));
                            float3 vivid_sky = max(float3(sky_l, sky_l, sky_l) + (raw_sky - sky_l) * 1.35, float3(0.0, 0.0, 0.0));
                            float sky_w = (bounce == 0u) ? 0.48 : 0.22;
                            gi_sum += throughput * vivid_sky * (params.tuning.w * sky_w * day_gi_scale * (1.0 - fog_t));
                            break;
                        }
                    }
                }
                accum_gi += gi_sum * (gi_fade / float(gi_taps));
            }
        }
    }

    // Phase 1: Point-light shadow visibility modulation ONLY (never adds light
    // in layered mode because raster `world_models_gpu` already applies `SceneLights`).
    // Opt-in via `point_light_meta.y > 0.5` (off by default); also evaluated in debug_mode == 6.
    uint debug_mode = uint(round(params.counts.w));
    bool pl_shadows_enabled = params.point_light_meta.y > 0.5;
    float3 point_lighting = float3(0.0, 0.0, 0.0);
    float3 pl_debug_lit = float3(0.0, 0.0, 0.0);
    float pl_debug_shadow = 0.0;
    float pl_w_total = 0.0;
    float pl_w_lit = 0.0;
    if (bits_lighting) {
        uint pl_count = min(uint(round(params.point_light_meta.x)), 48u);
        uint pl_shadow_rays = 0u;
        // Pure incoming point-light + world-effect irradiance; COMPOSITE_SHADER modulates by full-res raster albedo.
        for (uint li = 0u; li < pl_count; li = li + 1u) {
            float4 pl_pr = params.point_lights_pos_rad[li];
            float3 to_l = pl_pr.xyz - world_pos;
            // Boost #21: Fast 1D AABB reject before dot/sqrt across 48 lights.
            if (abs(to_l.x) >= pl_pr.w || abs(to_l.z) >= pl_pr.w || abs(to_l.y) >= pl_pr.w) {
                continue;
            }
            float pl_d2 = dot(to_l, to_l);
            if (pl_d2 > 1e-6 && pl_d2 < pl_pr.w * pl_pr.w) {
                float pl_dist = sqrt(pl_d2);
                float3 l_dir = to_l / pl_dist;
                float pl_ndl = max(dot(normal, l_dir), 0.0);
                if (pl_ndl > 0.001) {
                    float atten = point_light_falloff(pl_dist, pl_pr.w);
                    if (atten > 1e-5) {
                        float4 pl_ci = params.point_lights_col_int[li];
                        float w = pl_ci.w * atten * pl_ndl;
                        float pl_vis = 1.0;
                        if (bits_shadows && pl_shadow_rays < 4u && pl_dist > 1.5 && w > 0.018) {
                            pl_shadow_rays = pl_shadow_rays + 1u;
                            if (occluded_fast(ray_origin, l_dir, bias * 0.5, max(pl_dist - bias, bias))) {
                                pl_vis = 0.0;
                            }
                        }
                        pl_w_total += w;
                        pl_w_lit += w * pl_vis;
                        pl_debug_lit += pl_ci.rgb * (w * pl_vis);
                        pl_debug_shadow += w * (1.0 - pl_vis);
                        point_lighting += pl_ci.rgb * (w * pl_vis * 2.1);
                    }
                }
            }
        }
        point_lighting *= (1.0 - fog_t);
    }
    float point_vis = (pl_w_total > 1e-4) ? (pl_w_lit / max(pl_w_total, 1e-5)) : 1.0;
    float point_shadow_mod = (pl_shadows_enabled && pl_w_total > 1e-4)
        ? (1.0 - clamp(pl_w_total, 0.0, 0.5) * (1.0 - point_vis))
        : 1.0;

    float inv = 1.0 / float(samples);
    float avg_shadow = accum_shadow * inv;
    float avg_open = accum_open * inv;
    float3 avg_refl = accum_reflection * inv;
    float avg_refl_dist = accum_refl_dist * inv;
    float3 avg_gi = accum_gi * inv;

    float3 out_rgb;
    float out_lf;
    if (layered) {
        // Phase 2: Separate raster lighting into sun vs ambient fractions so
        // V_sun modulates ONLY the sun contribution and AO modulates ONLY the
        // ambient contribution (exact 1:1 identity at V_sun=1, AO=1).
        float sun_e = max(sun_luma, 0.0) * n_dot_l;
        float amb_e = max(dot(params.ambient.xyz, float3(0.2126, 0.7152, 0.0722)) * params.tuning.z, 0.005);
        float sun_f = saturate(sun_e / max(sun_e + amb_e, 1e-4));
        float amb_f = 1.0 - sun_f;
        float v_sun = bits_shadows ? avg_shadow : 1.0;
        float v_ao = bits_ao ? avg_open : 1.0;
        bool gi_active = bits_gi && max_bounces >= 1u && params.tuning.w > 0.0 && gi_fade > 0.01;
        // When GI is active, replace the flat raster ambient (`amb_f`) with real
        // sky + multi-bounce RT GI instead of adding GI on top (prevents grey wash).
        float amb_keep = gi_active ? lerp(1.0, 0.06, gi_fade) : 1.0;
        float separated_lf = (amb_f * v_ao * amb_keep + sun_f * v_sun) * point_shadow_mod;
        out_lf = lerp(separated_lf, 1.0, fog_t);
        out_rgb = avg_gi + point_lighting;
    } else {
        // Opt-in replace_raster mode: keep 1:1 raster world textures/albedo
        // instead of low-poly proxy boxes, relit purely by RT sun + RT ambient + point lights.
        float3 direct = bits_lighting ? (params.sun_colour.xyz * (n_dot_l * avg_shadow)) : float3(0.0, 0.0, 0.0);
        float3 ambient = params.ambient.xyz * (avg_open * params.tuning.z);
        float3 relit = raster_rgb * (direct + ambient + avg_gi + point_lighting);
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
        if (roughness <= 0.12) {
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
        float sun_e = max(dot(params.sun_colour.xyz, float3(0.2126, 0.7152, 0.0722)), 0.0) * n_dot_l;
        float amb_e = max(dot(params.ambient.xyz, float3(0.2126, 0.7152, 0.0722)) * params.tuning.z, 0.05);
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
        float spec_f0 = 0.04 + 0.16 * (1.0 - roughness);
        out_albedo[id.xy] = float4(raster_rgb, spec_f0);
    }
}
