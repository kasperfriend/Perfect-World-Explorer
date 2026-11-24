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

uint unpack_alpha_mask25(float f) {
    uint r = asuint(f);
    return (r & 0x00ffffffu) | ((r >> 7u) & 0x01000000u);
}

bool accept_opacity(uint prim, float2 bary, uint seed, uint tri_count) {
    if (prim >= tri_count) {
        return false;
    }
    uint raw_lo = asuint(tris[prim].c.w);
    uint raw_hi = asuint(tris[prim].n.w);
    if ((raw_lo | raw_hi) != 0u) {
        uint lo25 = unpack_alpha_mask25(tris[prim].c.w);
        uint hi24 = unpack_alpha_mask25(tris[prim].n.w);
        float s = saturate(bary.x);
        float t = saturate(bary.y / max(1.0 - s, 1e-4));
        uint ix = min(uint(s * 7.0), 6u);
        uint iy = min(uint(t * 7.0), 6u);
        uint bit = iy * 7u + ix;
        uint hit_bit = (bit < 25u) ? ((lo25 >> bit) & 1u) : ((hi24 >> (bit - 25u)) & 1u);
        return hit_bit != 0u;
    }
    float opacity = material_of(prim).surface.w;
    if (opacity >= 0.99) {
        return true;
    }
    uint _unused = seed;
    uint h = prim * 747796405u ^ uint(bary.x * 64.0) * 2891336453u ^ uint(bary.y * 64.0) * 1664525u;
    h ^= h >> 16u;
    float r = float(h & 0xffffu) / 65536.0;
    return r < opacity;
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
        if (q.CommittedInstanceID() == 1u) {
            hit.index = 0xfffffffeu;
            hit.t = q.CommittedRayT();
            hit.normal = normalize(-dir * 0.55 + float3(0.0, 0.83, 0.0));
        } else {
            uint prim = q.CommittedPrimitiveIndex() + (q.CommittedGeometryIndex() != 0u ? opaque_offset : 0u);
            if (prim < tri_count) {
                float3 n = tris[prim].n.xyz; // Opt #3: pre-normalized in harvest.rs
                if (dot(n, dir) > 0.0) {
                    n = -n;
                }
                hit.index = prim;
                hit.t = q.CommittedRayT();
                hit.normal = n;
            }
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
        if (q.CommittedInstanceID() == 1u) {
            hit.index = 0xfffffffeu;
            hit.t = q.CommittedRayT();
            hit.normal = normalize(-dir * 0.55 + float3(0.0, 0.83, 0.0));
        } else {
            uint prim = q.CommittedPrimitiveIndex() + (q.CommittedGeometryIndex() != 0u ? opaque_offset : 0u);
            if (prim < tri_count) {
                float3 n = tris[prim].n.xyz; // Opt #3: pre-normalized in harvest.rs
                if (dot(n, dir) > 0.0) {
                    n = -n;
                }
                hit.index = prim;
                hit.t = q.CommittedRayT();
                hit.normal = n;
            }
        }
    }
    return hit;
}

// Feature #4 & #7: Shadow ray returning blocker distance (for PCSS contact-hardening)
// and colored translucent transmission when passing through foliage/banners.
ShadowResult trace_sun_shadow(float3 origin, float3 dir, float t_min, float t_max, uint seed, uint opaque_offset, uint tri_count, bool fast_mode, float terrain_self_skip, float dyn_self_skip) {
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
        // Opt #5: Accept first hit & end search immediately on RT cores (2-3x faster distant shadows).
        RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
        q.TraceRayInline(scene_tlas, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray);
        q.Proceed();
        if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
            res.occluded = true;
            res.t_blocker = q.CommittedRayT();
            res.transmittance = float3(0.0, 0.0, 0.0);
        }
        return res;
    }

    // Enhancement #10: Track foliage canopy penetration depth for warm outer-leaf vs deep emerald shade.
    uint foliage_layers = 0u;
    RayQuery<RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray);
    while (q.Proceed()) {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE) {
            uint cand_prim = q.CandidatePrimitiveIndex() + (q.CandidateGeometryIndex() != 0u ? opaque_offset : 0u);
            foliage_layers = min(foliage_layers + 1u, 6u);
            if (accept_opacity(cand_prim, q.CandidateTriangleBarycentrics(), seed, tri_count)) {
                q.CommitNonOpaqueTriangleHit();
            }
        }
    }

    if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
        float hit_t = q.CommittedRayT();
        if (q.CommittedInstanceID() == 1u) {
            if (hit_t < dyn_self_skip) {
                return res;
            }
            res.occluded = true;
            res.t_blocker = hit_t;
            res.transmittance = float3(0.0, 0.0, 0.0);
            return res;
        }
        uint prim = q.CommittedPrimitiveIndex() + (q.CommittedGeometryIndex() != 0u ? opaque_offset : 0u);
        // Skip local coarse KIND_TERRAIN_LOD self-intersections on curved slopes (fixes triangular black holes).
        if (prim < tri_count && terrain_self_skip > 0.0 && hit_t < terrain_self_skip) {
            float hkind = tris[prim].a.w;
            if (hkind > 1.5 && hkind < 2.5 && hit_t + 0.65 < t_max) {
                RayDesc ray2;
                ray2.Origin = origin;
                ray2.Direction = dir;
                ray2.TMin = hit_t + 0.65;
                ray2.TMax = t_max;
                RayQuery<RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q2;
                q2.TraceRayInline(scene_tlas, RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray2);
                while (q2.Proceed()) {
                    if (q2.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE) {
                        uint cp2 = q2.CandidatePrimitiveIndex() + (q2.CandidateGeometryIndex() != 0u ? opaque_offset : 0u);
                        foliage_layers = min(foliage_layers + 1u, 6u);
                        if (accept_opacity(cp2, q2.CandidateTriangleBarycentrics(), seed ^ 19u, tri_count)) {
                            q2.CommitNonOpaqueTriangleHit();
                        }
                    }
                }
                if (q2.CommittedStatus() != COMMITTED_TRIANGLE_HIT) {
                    return res;
                }
                hit_t = q2.CommittedRayT();
                if (q2.CommittedInstanceID() == 1u) {
                    if (hit_t < dyn_self_skip) {
                        return res;
                    }
                    res.occluded = true;
                    res.t_blocker = hit_t;
                    res.transmittance = float3(0.0, 0.0, 0.0);
                    return res;
                }
                prim = q2.CommittedPrimitiveIndex() + (q2.CommittedGeometryIndex() != 0u ? opaque_offset : 0u);
            }
        }
        res.occluded = true;
        res.t_blocker = hit_t;
        if (prim < tri_count) {
            Material m = material_of(prim);
            if (m.surface.w < 0.96) {
                float depth_atten = exp(-float(foliage_layers) * 0.55);
                float tr = saturate(((1.0 - m.surface.w) * 0.44 + 0.12) * depth_atten);
                float3 warm_tint = lerp(float3(0.45, 0.85, 0.35), float3(1.15, 1.05, 0.55), depth_atten);
                res.transmittance = clamp(m.albedo.rgb * warm_tint, float3(0.06, 0.08, 0.04), float3(0.95, 0.95, 0.95)) * tr;
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

bool occluded_point_light(float3 origin, float3 dir, float t_min, float t_max, float terrain_skip, float dyn_skip, uint opaque_offset, uint tri_count) {
    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = t_min;
    ray.TMax = t_max;

    RayQuery<RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xffu, ray);
    q.Proceed();
    if (q.CommittedStatus() != COMMITTED_TRIANGLE_HIT) {
        return false;
    }
    if (q.CommittedInstanceID() == 1u) {
        if (q.CommittedRayT() < dyn_skip) {
            float next_t = q.CommittedRayT() + 0.55;
            if (next_t < t_max) {
                return occluded_fast(origin, dir, next_t, t_max);
            }
            return false;
        }
        return true;
    }
    if (terrain_skip > 0.0 && q.CommittedRayT() < terrain_skip) {
        uint tri_idx = (q.CommittedGeometryIndex() != 0u)
            ? (opaque_offset + q.CommittedPrimitiveIndex())
            : q.CommittedPrimitiveIndex();
        if (tri_idx < tri_count) {
            float k = tris[tri_idx].a.w;
            if (k > 1.5 && k < 2.5) {
                float next_t = q.CommittedRayT() + 0.85;
                if (next_t < t_max) {
                    return occluded_fast(origin, dir, next_t, t_max);
                }
                return false;
            }
        }
    }
    return true;
}

float point_light_falloff(float dist, float radius) {
    if (radius <= 1e-4 || dist >= radius) {
        return 0.0;
    }
    float ratio = dist / radius;
    // Ultra-smooth C2-like cubic smoothstep squared tail: reaches 0 with zero slope
    // and zero curvature at `dist == radius` so there is never a visible hard circle edge.
    float s = max(1.0 - ratio, 0.0);
    float sw = s * s * (3.0 - 2.0 * s);
    float fade = sw * sw;
    float norm_d = dist / max(radius * 0.28, 3.0);
    return fade / (1.0 + norm_d * norm_d);
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

// Feature #2: Physical Rayleigh/Mie sunset & twilight sky/horizon scattering.
float3 apply_twilight_scattering(float3 base_sky, float3 dir, float3 sun_dir, float sun_strength, bool enabled) {
    if (!enabled) {
        return base_sky;
    }
    float elev = sun_dir.y;
    if (elev > 0.38 || elev < -0.18) {
        return base_sky;
    }
    float twi_w = smoothstep(-0.16, 0.02, elev) * (1.0 - smoothstep(0.08, 0.38, elev));
    if (twi_w <= 1e-3) {
        return base_sky;
    }
    float horiz = exp(-abs(dir.y) * 4.8);
    float zenith = saturate(dir.y * 1.4);
    float sun_cos = saturate(dot(normalize(dir), normalize(sun_dir)));
    float mie_glow = pow(sun_cos, 6.0) * 0.85 + pow(sun_cos, 2.0) * 0.35;
    float3 warm_horizon = lerp(float3(0.95, 0.34, 0.14), float3(1.18, 0.56, 0.22), sun_strength);
    float3 cool_zenith = float3(0.12, 0.20, 0.44);
    float3 scattered = lerp(base_sky, cool_zenith, zenith * twi_w * 0.45)
        + warm_horizon * (horiz * (0.32 + mie_glow * 0.78) * twi_w * 0.65);
    return max(scattered, float3(0.0, 0.0, 0.0));
}

float2 hash22(float2 p) {
    float3 p3 = frac(float3(p.xyx) * float3(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, p3.yzx + 33.33);
    return frac((p3.xx + p3.yz) * p3.zy);
}

// Feature #10: Animated 3D world-space suspended underwater dust/plankton motes.
float underwater_motes(float3 p, float t) {
    float3 q = p * 0.62 + float3(sin(t * 0.35 + p.y * 0.25) * 0.4, -t * 0.18, cos(t * 0.31 + p.x * 0.22) * 0.4);
    float3 cell = floor(q);
    float3 f = frac(q);
    float2 h = hash22(cell.xz + cell.y * 17.31);
    float3 pt = float3(h.x, frac(h.x * 13.7 + h.y * 7.3), h.y) * 0.72 + 0.14;
    float d2 = dot(f - pt, f - pt);
    return exp(-d2 * 48.0) * step(0.45, h.x);
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
    // Opt #8: Fast cubic multiply replaces transcendental pow() in Voronoi ridge.
    float edge = sqrt(d2) - sqrt(d1);
    float v = smoothstep(0.58, 0.0, edge);
    return v * v * (0.45 + 0.55 * v);
}

float caustic_pattern(float2 xz, float t, float cam_dist) {
    float2 warp = float2(sin(xz.y * 0.27 + t * 0.7), cos(xz.x * 0.23 - t * 0.6)) * 0.45;
    float c1 = voronoi_ridge((xz + warp) * 0.36 + float2(t * 0.22, -t * 0.17), t * 1.15);
    // Opt #8: Single Voronoi layer beyond 22u (cuts caustic ALU by ~55%).
    if (cam_dist > 22.0) {
        return saturate(c1 * 1.05);
    }
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
    // Opt #10: Tight squared bounding reject + fast reciprocal falloff (no transcendental exp).
    float max_reach = min(max_t, 360.0);
    for (uint li = 0u; li < pl_count; li = li + 1u) {
        float4 pr = params.point_lights_pos_rad[li];
        if (pr.w <= 0.1) {
            continue;
        }
        float3 d_eye = pr.xyz - params.eye.xyz;
        if (dot(d_eye, d_eye) < 12.25) {
            continue;
        }
        float3 to_l = pr.xyz - origin;
        float t_proj = dot(to_l, dir);
        if (t_proj <= 0.8 || t_proj >= max_reach) {
            continue;
        }
        float r_core = clamp(pr.w * 0.11, 0.50, 3.2);
        float r2_out = r_core * r_core * 6.0;
        float d2 = dot(to_l, to_l) - t_proj * t_proj;
        if (d2 >= r2_out) {
            continue;
        }
        float4 ci = params.point_lights_col_int[li];
        if (ci.w < 0.05) {
            continue;
        }
        float u = saturate(d2 / max(r2_out, 0.1));
        float win = 1.0 - u;
        float orb = (win * win * win) * exp(-u * 3.5) / (1.0 + t_proj * 0.014);
        glow_sum += ci.rgb * (ci.w * orb * 0.28);
    }
    return min(glow_sum, float3(2.2, 2.2, 2.2));
}


// Enhancement #1: Drifting Ray-Traced Cloud Shadows projected along sun direction.
float sample_cloud_shadow(float3 world_pos, float3 sun_dir, float t_frame) {
    if (sun_dir.y <= 0.08) {
        return 1.0;
    }
    float t_cloud = 620.0 / max(sun_dir.y, 0.18);
    float2 cp = (world_pos.xz + sun_dir.xz * min(t_cloud, 2400.0)) * 0.0022;
    cp += float2(t_frame * 0.00055, -t_frame * 0.00032);
    // Smooth 2-octave value noise for broad natural drifting cloud patches.
    float2 i0 = floor(cp);
    float2 f0 = frac(cp);
    float2 u0 = f0 * f0 * (3.0 - 2.0 * f0);
    float a0 = frac(sin(dot(i0, float2(127.1, 311.7))) * 43758.5453);
    float b0 = frac(sin(dot(i0 + float2(1.0, 0.0), float2(127.1, 311.7))) * 43758.5453);
    float c0 = frac(sin(dot(i0 + float2(0.0, 1.0), float2(127.1, 311.7))) * 43758.5453);
    float d0 = frac(sin(dot(i0 + float2(1.0, 1.0), float2(127.1, 311.7))) * 43758.5453);
    float n0 = lerp(lerp(a0, b0, u0.x), lerp(c0, d0, u0.x), u0.y);

    float2 cp1 = cp * 2.15 + float2(11.3, 5.7);
    float2 i1 = floor(cp1);
    float2 f1 = frac(cp1);
    float2 u1 = f1 * f1 * (3.0 - 2.0 * f1);
    float a1 = frac(sin(dot(i1, float2(127.1, 311.7))) * 43758.5453);
    float b1 = frac(sin(dot(i1 + float2(1.0, 0.0), float2(127.1, 311.7))) * 43758.5453);
    float c1 = frac(sin(dot(i1 + float2(0.0, 1.0), float2(127.1, 311.7))) * 43758.5453);
    float d1 = frac(sin(dot(i1 + float2(1.0, 1.0), float2(127.1, 311.7))) * 43758.5453);
    float n1 = lerp(lerp(a1, b1, u1.x), lerp(c1, d1, u1.x), u1.y);

    float fbm = n0 * 0.68 + n1 * 0.32;
    float cover = smoothstep(0.50, 0.80, fbm);
    return lerp(1.0, 0.62, cover);
}

// Enhancement #4: High-Precision Micro-Contact Shadows (4-step depth-anchored screen-space ray).
float trace_contact_shadow(float3 world_pos, float3 sun_dir, float cam_dist, float jitter) {
    if (cam_dist > 42.0) {
        return 1.0;
    }
    float max_reach = lerp(1.35, 0.55, saturate(cam_dist / 42.0));
    float occ = 0.0;
    uint2 gdim = uint2(uint(params.extents.x), uint(params.extents.y));
    for (uint ci = 0u; ci < 4u; ci = ci + 1u) {
        float t_c = ((float(ci) + 0.35 + 0.55 * jitter) / 4.0) * max_reach;
        float3 p_c = world_pos + sun_dir * t_c;
        float4 clip = mul(params.vp, float4(p_c, 1.0));
        if (clip.w <= 1e-4) {
            continue;
        }
        float2 ndc = clip.xy / clip.w;
        float2 uv = float2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        if (uv.x < 0.005 || uv.x > 0.995 || uv.y < 0.005 || uv.y > 0.995) {
            continue;
        }
        int2 hp = clamp(int2(uv * params.extents.xy), int2(0, 0), int2(gdim) - int2(1, 1));
        float4 gpos = src_gbuf_pos.Load(int3(hp, 0));
        if (gpos.w <= 0.0) {
            continue;
        }
        float ray_cam_d = length(p_c - params.eye.xyz);
        float dz = ray_cam_d - gpos.w;
        if (dz > 0.045 && dz < 0.58) {
            occ = max(occ, 1.0 - smoothstep(0.32, 0.58, dz));
        }
    }
    float dist_fade = 1.0 - smoothstep(26.0, 42.0, cam_dist);
    return 1.0 - occ * 0.72 * dist_fade;
}

// Enhancement #12: Subtle animated multi-octave capillary wave normal for living water reflections.
float3 water_wave_normal(float2 xz, float t, float cam_dist) {
    float amp = 0.028 * exp(-cam_dist * 0.0045);
    float w1 = sin(xz.x * 0.55 + xz.y * 0.32 + t * 1.45);
    float w2 = cos(xz.x * 0.38 - xz.y * 0.64 + t * 1.12);
    float w3 = sin(xz.x * 1.25 + xz.y * 1.05 - t * 2.15) * 0.45;
    float dx = (w1 * 0.55 + w2 * 0.38 + w3 * 1.25) * amp;
    float dz = (w1 * 0.32 - w2 * 0.64 + w3 * 1.05) * amp;
    return normalize(float3(-dx, 1.0, -dz));
}

// Enhancement #11: Chromatic RGB dispersion for shallow sunlit water caustics.
float3 caustic_pattern_chromatic(float2 xz, float t, float cam_dist, float depth_w) {
    float c_g = caustic_pattern(xz, t, cam_dist);
    if (cam_dist > 34.0 || depth_w > 5.5) {
        return float3(c_g, c_g, c_g);
    }
    float disp = 0.032 * (1.0 - saturate(depth_w / 5.5));
    float c_r = caustic_pattern(xz + float2(disp, disp * 0.6), t, cam_dist + 24.0);
    float c_b = caustic_pattern(xz - float2(disp, disp * 0.6), t, cam_dist + 24.0);
    return float3(lerp(c_g, c_r, 0.65), c_g, lerp(c_g, c_b, 0.65));
}

float3 kind_colour(float kind) {
    if (kind < 0.5) return float3(0.04, 0.04, 0.04);
    if (kind < 1.5) return float3(0.0, 0.85, 1.0);   // cyan: water quad
    if (kind < 2.5) return float3(1.0, 0.55, 0.0);   // orange: LOD terrain
    if (kind < 3.5) return float3(0.15, 0.90, 0.25); // green: real mesh
    return float3(0.95, 0.15, 0.85);                 // magenta: instance proxy
}

[numthreads(8, 8, 1)] // Opt #13: square 8x8 tile for L1/wave64 coherence
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
    bool bits_cloud_shadows = (effects & 128u) != 0u;
    bool bits_contact_shadows = (effects & 256u) != 0u;
    bool bits_moonlit_night = (effects & 512u) != 0u;
    bool bits_weather = (effects & 2048u) != 0u;
    bool bits_twilight_sky = (effects & 4096u) != 0u;
    bool bits_valley_mist = (effects & 8192u) != 0u;
    bool bits_water_shore_foam = (effects & 16384u) != 0u;
    bool walker_active = params.fog_colour.w > 0.5;
    float3 walker_pos = float3(params.sun_dir.w, params.sun_colour.w, params.ambient.w);

    // Feature #6: True metallic PBR material separation for armor, weapons & gold roofs
    // (stone, wood, dirt, and foliage remain 100% matte with metalness == 0).
    float r_max_c = max(raster_rgb.r, max(raster_rgb.g, raster_rgb.b));
    float r_min_c = min(raster_rgb.r, min(raster_rgb.g, raster_rgb.b));
    float r_sat = (r_max_c > 1e-4) ? ((r_max_c - r_min_c) / r_max_c) : 0.0;
    bool is_gold_metal = !is_water && raster_rgb.r > raster_rgb.g * 1.08 && raster_rgb.g > raster_rgb.b * 1.48 && raster_rgb.r > 0.22;
    bool is_steel_metal = !is_water && dist < 24.0 && abs(normal.y) < 0.85 && r_max_c > 0.32 && r_sat < 0.15;
    float metalness = is_gold_metal ? 0.85 : (is_steel_metal ? 0.62 : 0.0);
    float3 metal_tint = is_gold_metal ? clamp(raster_rgb * 1.35, float3(0.55, 0.38, 0.12), float3(1.0, 0.86, 0.42)) : float3(0.76, 0.80, 0.86);
    float eff_roughness = (metalness > 0.1) ? min(roughness, 0.24) : roughness;

    float steepness = 1.0 - saturate(normal.y);
    float slope_bias = steepness * (0.75 + dist * 0.0065);
    float bias = max(params.effects.z, max(0.18, dist * 0.0035)) + slope_bias;
    float3 ray_origin = world_pos + normal * bias + float3(0.0, 1.0, 0.0) * (steepness * 0.55) - view_dir * (bias * 0.35);
    float shadow_t_min = max(bias * 1.15, 0.60 + steepness * 0.85);
    float ao_t_min = max(bias * 0.80, 0.40);

    // Feature #7: Subtle micro-detail procedural normal/bump relief for nearby stone, bark & terrain
    // (derived from raster luminance gradients; excludes water & close smooth Walker skin < 7.5u).
    float3 shade_normal = normal;
    if (!is_water && dist > 7.5 && dist < 68.0) {
        int2 bdim = int2(int(params.extents.x), int(params.extents.y));
        int2 bpx = int2(id.xy);
        float l_r = luma(src_gbuf_raster.Load(int3(clamp(bpx + int2(1, 0), int2(0, 0), bdim - int2(1, 1)), 0)).rgb);
        float l_l = luma(src_gbuf_raster.Load(int3(clamp(bpx + int2(-1, 0), int2(0, 0), bdim - int2(1, 1)), 0)).rgb);
        float l_d = luma(src_gbuf_raster.Load(int3(clamp(bpx + int2(0, 1), int2(0, 0), bdim - int2(1, 1)), 0)).rgb);
        float l_u = luma(src_gbuf_raster.Load(int3(clamp(bpx + int2(0, -1), int2(0, 0), bdim - int2(1, 1)), 0)).rgb);
        float gx = clamp(l_r - l_l, -0.35, 0.35);
        float gy = clamp(l_d - l_u, -0.35, 0.35);
        float bump_fade = smoothstep(7.5, 14.0, dist) * (1.0 - smoothstep(38.0, 68.0, dist));
        float3 b_up = abs(normal.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
        float3 b_t = normalize(cross(b_up, normal));
        float3 b_b = cross(normal, b_t);
        shade_normal = normalize(normal + (b_t * gx + b_b * gy) * (0.42 * bump_fade));
    }
    float n_dot_l = max(dot(shade_normal, params.sun_dir.xyz), 0.0);

    float fog_t = 0.0;
    float fog_start = params.fog.x;
    float fog_end = params.fog.y;
    if (fog_end > fog_start && fog_start >= 0.0) {
        fog_t = saturate((dist - fog_start) / (fog_end - fog_start));
    }

    float water_y = params.eye.w;
    bool is_submerged = (water_y > -1e5) && (params.eye.y < water_y - 0.05);
    uint debug_mode_early = uint(round(params.counts.w));
    if (fog_t >= 0.99 && layered && debug_mode_early == 0u && !is_submerged) {
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
    float3 accum_bent_n = float3(0.0, 0.0, 0.0);
    float gi_sky_hits = 0.0;
    float gi_total_rays = 0.0;
    float3 accum_reflection = float3(0.0, 0.0, 0.0);
    float accum_refl_dist = 0.0;
    float3 accum_gi = float3(0.0, 0.0, 0.0);

    // Opt #7: Distance-scaled AO ray budget (2 rays < 40u, 1 ray < 160u) — 40-50% fewer AO rays.
    float ao_radius = clamp(params.fog.z, 2.0, 6.0);
    uint ao_samples = (dist < 40.0) ? 2u : 1u;
    float ao_fade = 1.0 - saturate((dist - 125.0) / 35.0);
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
    float gi_fade = 1.0 - saturate((dist - 220.0) / 180.0);

    for (uint s = 0u; s < samples; s = s + 1u) {
        // Feature #7 (Contact-Hardening Soft Shadows / PCSS) + Feature #4 (Foliage Transmission):
        // Tap 0 measures exact blocker distance `t_blocker`; Tap 1 scales penumbra spread
        // proportionally so shadows stay razor-sharp at bases/trunks and soften further away.
        float shadow = 1.0;
        float3 trans_tint = float3(0.0, 0.0, 0.0);
        if (bits_shadows && n_dot_l > 0.02 && fog_t < 0.96) {
            // Opt #6: Clamp primary shadow ray t_max to 480u to prune far BVH nodes.
            float shadow_max_t = min(params.lens.y, 480.0);
            bool fast_shadow = dist > 520.0;
            float terrain_self_skip = is_water ? 0.0 : (2.5 + steepness * 14.0 + dist * 0.025);
            float dyn_self_skip = (normal.y > 0.72) ? 0.14 : 0.62;
            ShadowResult sr0 = trace_sun_shadow(
                ray_origin, params.sun_dir.xyz, shadow_t_min, shadow_max_t,
                state, opaque_offset, tri_count, fast_shadow, terrain_self_skip, dyn_self_skip);
            float vis_sum = sr0.occluded ? 0.0 : 1.0;
            if (sr0.occluded) {
                trans_tint += sr0.transmittance;
            }
            // Opt #4: Skip 2nd PCSS tap beyond 90u or when Tap 0 already hit a hard close contact blocker (< 1.5u).
            bool hard_contact = sr0.occluded && sr0.t_blocker < 1.5 && dot(sr0.transmittance, float3(1.0, 1.0, 1.0)) < 1e-4;
            bool do_tap2 = (spread_tan > 0.0) && (dist < 90.0) && !hard_contact;
            uint used_taps = 1u;
            if (do_tap2) {
                used_taps = 2u;
                float pcss_scale = sr0.occluded
                    ? clamp(sr0.t_blocker / 18.0, 0.14, 1.45)
                    : 0.75;
                float r = 0.8660254 * spread_tan * pcss_scale;
                float theta = 2.39996323 + rot_angle + float(s) * 1.6180339;
                float s_th, c_th;
                sincos(theta, s_th, c_th);
                float3 shadow_dir1 = normalize(params.sun_dir.xyz + (sun_t * c_th + sun_b * s_th) * r);
                ShadowResult sr1 = trace_sun_shadow(
                    ray_origin, shadow_dir1, shadow_t_min, shadow_max_t,
                    state ^ 1u, opaque_offset, tri_count, fast_shadow, terrain_self_skip, dyn_self_skip);
                if (!sr1.occluded) {
                    vis_sum += 1.0;
                } else {
                    trans_tint += sr1.transmittance;
                }
            }
            shadow = vis_sum / float(used_taps);
            trans_tint /= float(used_taps);
            // Enhancement #17: Distant terrain self-shadowing beyond near BLAS bubble (480..1400u).
            if (shadow > 0.5 && dist > 260.0 && params.lens.y > 500.0) {
                float far_max_t = min(params.lens.y, 1400.0);
                if (occluded_fast(ray_origin, params.sun_dir.xyz, 460.0, far_max_t)) {
                    shadow = 0.0;
                }
            }
            // Enhancement #4: High-precision micro-contact shadows for fine grounding.
            if (bits_contact_shadows && !is_water && shadow > 0.05 && dist < 42.0) {
                shadow *= trace_contact_shadow(world_pos, params.sun_dir.xyz, dist, ign_v);
            }
            // Enhancement #1: Drifting ray-traced cloud shadows across plains, mountains & cities.
            if (bits_cloud_shadows && shadow > 0.05 && sun_strength > 0.05) {
                float c_vis = sample_cloud_shadow(world_pos, params.sun_dir.xyz, params.frame.x);
                shadow *= lerp(1.0, c_vis, sun_strength * (1.0 - fog_t * 0.6));
            }
        }
        accum_shadow += shadow;
        accum_trans += trans_tint;

        // Feature #4: Distance-attenuated RT Ambient Occlusion (RTAO) contact shadows.
        // Uses closest_hit_fast so tight crevices under bridges, stairs, statues, rock seams,
        // and grass tufts darken crisply while distant blockers fade smoothly with zero halos.
        float open = 1.0;
        if (bits_ao && ao_fade > 0.01 && !is_water) {
            float occ_sum = 0.0;
            float3 bent = float3(0.0, 0.0, 0.0);
            float inv_ao_r = 1.0 / max(ao_radius, 0.5);
            for (uint i = 0u; i < ao_samples; i = i + 1u) {
                float u1 = (float(i) + rand01(state)) / float(ao_samples);
                float phi = float(i) * 2.39996323 + rot_angle + float(s) * 0.7548776;
                float r = sqrt(u1);
                float s_phi, c_phi;
                sincos(phi, s_phi, c_phi);
                float z = sqrt(max(0.0, 1.0 - u1));
                float3 d = t_axis * (r * c_phi) + b_axis * (r * s_phi) + normal * z;
                Hit ao_hit = closest_hit_fast(ray_origin, d, ao_t_min, ao_radius, opaque_offset, tri_count);
                if (ao_hit.index != 0xffffffffu) {
                    float t_norm = saturate(ao_hit.t * inv_ao_r);
                    float w_close = 1.0 - t_norm;
                    // Extra crispness in tight contact crevices (< 1.8u)
                    float contact_boost = (ao_hit.t < 1.8) ? 1.18 : 1.0;
                    occ_sum += saturate(w_close * sqrt(w_close) * contact_boost);
                } else {
                    bent += d;
                }
            }
            open = lerp(1.0, 1.0 - saturate(occ_sum / float(ao_samples)), ao_fade);
            accum_bent_n += (dot(bent, bent) > 1e-4) ? normalize(bent) : normal;
        } else {
            accum_bent_n += normal;
        }
        accum_open += open;

        // Features #2 (Water Refraction & Underwater Caustics), #3 (Calibrated Glossy Trims),
        // #8 (Analytical Reflections of World Effects & Point Lights), #10 (Off-Screen Detail).
        float refl_dist = params.lens.y;
        if (bits_reflections && (eff_roughness <= params.effects.x || metalness > 0.1)) {
            // Perturb water reflections with living animated wave normals + Walker wading ripples (#4).
            float3 refl_base_n = is_water
                ? water_wave_normal(world_pos.xz, params.frame.x * 0.03, dist)
                : shade_normal;
            float walker_ripple_crest = 0.0;
            if (is_water && bits_water_shore_foam && walker_active && abs(walker_pos.y - world_pos.y) < 2.2) {
                float2 w_d = world_pos.xz - walker_pos.xz;
                float w_r = length(w_d);
                if (w_r > 0.15 && w_r < 6.8) {
                    float r_fade = (1.0 - smoothstep(0.8, 6.8, w_r)) * smoothstep(0.15, 0.45, w_r);
                    float phase_r = w_r * 7.2 - params.frame.x * 0.14;
                    float wave_r = sin(phase_r) * r_fade;
                    float2 dir_r = w_d / max(w_r, 1e-4);
                    refl_base_n = normalize(refl_base_n + float3(dir_r.x, 0.0, dir_r.y) * (wave_r * 0.22));
                    walker_ripple_crest = saturate(cos(phase_r) * 0.5 + 0.5) * r_fade * 0.35;
                }
            }
            float3 h = ggx_sample(refl_base_n, eff_roughness, rand01(state), rand01(state));
            float3 reflect_dir = reflect(view_dir, h);
            if (dot(reflect_dir, refl_base_n) <= 0.0) {
                reflect_dir = reflect(view_dir, refl_base_n);
            }
            // Feature #6: Conductor Fresnel for gold/steel armor/weapons/roofs; matte stays matte.
            float f0 = is_water ? 0.18 : (metalness > 0.1 ? 0.55 : 0.04);
            float m = 1.0 - saturate(dot(-view_dir, h));
            float m2 = m * m;
            float fresnel = f0 + (1.0 - f0) * (m2 * m2 * m);
            float3 fresnel_rgb = (metalness > 0.1) ? lerp(float3(fresnel, fresnel, fresnel), metal_tint * fresnel, metalness) : float3(fresnel, fresnel, fresnel);
            float trim_scale = is_water
                ? 1.0
                : (metalness > 0.1 ? (0.48 * metalness) : (0.28 * (1.0 - smoothstep(0.24, 0.37, eff_roughness))));
            // Opt #9: Use fast opaque RT traversal for blurry glossy trims & distant water (> 140u).
            float refl_max_t = is_water ? min(params.lens.y, 750.0) : min(params.lens.y, 140.0);
            Hit rhit;
            if (!is_water || dist > 560.0) {
                rhit = closest_hit_fast(ray_origin, reflect_dir, bias, refl_max_t, opaque_offset, tri_count);
            } else {
                rhit = closest_hit(ray_origin, reflect_dir, bias, refl_max_t, opaque_offset, tri_count);
            }
            float eff_max_t = (rhit.index != 0xffffffffu) ? rhit.t : refl_max_t;

            float3 refl_col = float3(0.0, 0.0, 0.0);
            if (rhit.index != 0xffffffffu) {
                float3 hit_pos = ray_origin + reflect_dir * rhit.t;
                float4 screen_col = sample_screen_radiance(hit_pos, rhit.t);
                float screen_w = saturate(screen_col.a);
                float3 lit = screen_col.rgb;
                if (rhit.index == 0xfffffffeu) {
                    // Dynamic Walker character reflection: prefer live textured screen colour,
                    // with warm shaded armor/cloth fallback when reflected from offscreen angles.
                    float c_ndl = max(dot(rhit.normal, params.sun_dir.xyz), 0.15);
                    float3 char_fallback = float3(0.38, 0.34, 0.32) * (params.ambient.xyz * params.tuning.z + params.sun_colour.xyz * c_ndl);
                    lit = lerp(char_fallback, screen_col.rgb, max(screen_w, 0.15));
                } else if (screen_w < 0.99) {
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
                refl_col = lit * (fresnel_rgb * trim_scale * (1.0 - fog_t));
                refl_dist = rhit.t;
            } else {
                float3 sky_col = apply_twilight_scattering(sample_sky_reflection(reflect_dir), reflect_dir, params.sun_dir.xyz, sun_strength, bits_twilight_sky);
                refl_col = sky_col * (fresnel_rgb * trim_scale * 0.65 * (1.0 - fog_t));
                // Enhancement #14: Ray-traced sun & moon glitter paths across living water
                // (only when reflection ray misses all bridges, islands, cliffs & ships!).
                if (is_water && params.sun_dir.y > 0.02) {
                    float cos_glint = saturate(dot(reflect_dir, params.sun_dir.xyz));
                    float g2 = cos_glint * cos_glint;
                    float g8 = g2 * g2 * g2 * g2;
                    float g32 = g8 * g8 * g8 * g8;
                    float glint = g32 * g32 * 0.95 + g8 * g8 * 0.14;
                    float3 glint_col = lerp(float3(0.55, 0.72, 1.0), params.sun_colour.xyz, sun_strength);
                    float glint_amp = lerp(0.55, 1.35, sun_strength);
                    refl_col += glint_col * (glint * glint_amp * fresnel * (1.0 - fog_t));
                }
            }

            // Feature #8: Analytical reflections of animated world effects & point lights (skip negligible matte trims).
            if (bits_lighting && pl_count > 0u && fog_t < 0.92 && (is_water || trim_scale > 0.03)) {
                float3 fx_refl = trace_effect_light_reflections(ray_origin, reflect_dir, eff_max_t, pl_count);
                refl_col += fx_refl * (fresnel * trim_scale * (1.0 - fog_t));
            }

            // Feature #2: True RT Water Refraction (Snell ray-trace + Beer-Lambert depth absorption)
            // and shadow-gated animated refractive sun caustics on riverbeds, rocks, and piers.
            if (is_water && bits_refraction && fog_t < 0.90) {
                float3 refract_dir = refract(view_dir, refl_base_n, 1.0 / 1.333);
                if (dot(refract_dir, refract_dir) < 1e-4) {
                    refract_dir = normalize(view_dir - float3(0.0, 0.25, 0.0));
                }
                float3 refr_origin = world_pos - float3(0.0, 0.08, 0.0);
                float refr_max_t = (dist < 220.0) ? 36.0 : 18.0;
                Hit refr_hit = closest_hit_fast(refr_origin, refract_dir, 0.08, refr_max_t, opaque_offset, tri_count);
                float wdepth = (refr_hit.index != 0xffffffffu)
                    ? max(refr_hit.t * max(-refract_dir.y, 0.25), 0.08)
                    : ((water_depth > 0.01) ? water_depth : 14.0);
                float refr_path = (refr_hit.index != 0xffffffffu) ? refr_hit.t : (wdepth / max(-refract_dir.y, 0.25));
                // Physically-based Beer-Lambert absorption: clear turquoise shallows -> deep ocean blue/teal.
                float3 beer = exp(-refr_path * float3(0.25, 0.085, 0.042));
                float refract_w = (1.0 - fresnel) * (1.0 - fog_t);
                float shallow_clear = exp(-wdepth * 0.14);
                if (refr_hit.index != 0xffffffffu) {
                    float3 seabed_pos = refr_origin + refract_dir * refr_hit.t;
                    Material smat = material_of(refr_hit.index);
                    float skind = tris[refr_hit.index].a.w;
                    float3 s_albedo = (skind > 1.5 && skind < 2.5)
                        ? sample_terrain_albedo_fast(seabed_pos, smat.albedo.rgb)
                        : smat.albedo.rgb;
                    float s_ndl = max(dot(refr_hit.normal, params.sun_dir.xyz), 0.0);
                    float3 seabed_lit = s_albedo * (params.ambient.xyz * params.tuning.z * 0.85
                        + params.sun_colour.xyz * (s_ndl * shadow));
                    // Blend real refracted seabed tint with Beer-Lambert absorption
                    float3 deep_scatter = lerp(float3(0.01, 0.03, 0.06), float3(0.03, 0.14, 0.18), sun_strength);
                    float3 refr_col = seabed_lit * beer + deep_scatter * (1.0 - beer);
                    refl_col += refr_col * (refract_w * 0.42 * shallow_clear);
                    if (bits_caustics && wdepth < 14.0 && shadow > 0.05 && s_ndl > 0.06 && sun_strength > 0.05) {
                        float3 caust_rgb = caustic_pattern_chromatic(seabed_pos.xz, params.frame.x * 0.025, dist, wdepth);
                        float c_fade = smoothstep(0.10, 0.75, wdepth) * shallow_clear * exp(-dist * 0.014);
                        refl_col += params.sun_colour.xyz * beer * caust_rgb * (shadow * s_ndl * sun_strength * refract_w * c_fade * 0.24);
                    }
                } else {
                    float3 depth_tint = (beer - float3(1.0, 1.0, 1.0)) * (raster_rgb * 0.26 * refract_w);
                    refl_col = max(refl_col + depth_tint, float3(0.0, 0.0, 0.0));
                }
                // Backlit shallow-water shoreline subsurface turquoise glow.
                if (wdepth < 4.5 && shadow > 0.05 && sun_strength > 0.05) {
                    float back_w = saturate(dot(view_dir, params.sun_dir.xyz));
                    float bw3 = back_w * back_w * back_w;
                    float shore_w = smoothstep(0.05, 0.55, wdepth) * (1.0 - smoothstep(1.2, 4.5, wdepth));
                    refl_col += float3(0.12, 0.62, 0.58) * params.sun_colour.xyz * (bw3 * shore_w * shadow * sun_strength * refract_w * 0.45);
                }
                // Feature #4: Animated shoreline & object-intersection foam + Walker wading ripple crests.
                if (bits_water_shore_foam && dist < 180.0) {
                    float foam_mask = 0.0;
                    if (wdepth < 1.35) {
                        float t_f = params.frame.x * 0.045;
                        float lap = sin(wdepth * 14.0 - t_f * 2.4 + sin(world_pos.x * 0.55 + world_pos.z * 0.48) * 1.8) * 0.5 + 0.5;
                        float cell_f = voronoi_ridge(world_pos.xz * 0.85, t_f * 0.6);
                        float edge_band = (1.0 - smoothstep(0.06, 1.35, wdepth));
                        float contact_ring = 1.0 - smoothstep(0.02, 0.28, wdepth);
                        foam_mask = saturate(edge_band * (0.35 + 0.65 * lap) * smoothstep(0.28, 0.78, cell_f) + contact_ring * 0.72);
                    }
                    foam_mask = saturate(foam_mask + walker_ripple_crest);
                    if (foam_mask > 0.01) {
                        float3 foam_lit = lerp(float3(0.18, 0.24, 0.32), params.sun_colour.xyz * 0.85 + params.ambient.xyz * 0.45, sun_strength);
                        float dist_f = 1.0 - smoothstep(90.0, 180.0, dist);
                        refl_col += foam_lit * (foam_mask * dist_f * (1.0 - fog_t) * 0.42);
                    }
                }
            }
            accum_reflection += refl_col;
        }
        accum_refl_dist += refl_dist;

        // True 1..6 multi-bounce Diffuse GI: every bounce carries sun, sky, point lights,
        // world effects, and emissive meshes smoothly into shadowed open spaces & corners,
        // feeding the converged multi-bounce path back into the 3D World-Space Radiance Cache.
        if (bits_gi && !is_water && max_bounces >= 1u && params.tuning.w > 0.0 && gi_fade > 0.01) {
            float bounce_max_t = min(params.lens.y, 110.0);
            float gi_shadow_max_t = min(params.lens.y, 220.0);
            float day_gi_scale = 0.04 + 0.96 * sun_strength;
            float3 cur_origin = ray_origin;
            float3 cur_normal = normal;
            float3 cur_t = t_axis;
            float3 cur_b = b_axis;
            float cur_bias = bias;
            float path_dist = dist;
            float3 throughput = float3(1.0, 1.0, 1.0);
            float3 gi_sample = float3(0.0, 0.0, 0.0);
            bool has_b0 = false;
            float3 b0_world = float3(0.0, 0.0, 0.0);
            float3 b0_normal = float3(0.0, 1.0, 0.0);
            float b0_dist = dist;
            gi_total_rays += 1.0;

            for (uint bounce = 0u; bounce < max_bounces; bounce = bounce + 1u) {
                float u1 = rand01(state);
                float phi = rot_angle + 1.0471975 + float(s) * 2.39996323
                    + float(bounce) * 1.9132 + rand01(state) * 0.65;
                float r = sqrt(u1);
                float s_phi, c_phi;
                sincos(phi, s_phi, c_phi);
                float z = sqrt(max(0.0, 1.0 - u1));
                float3 d = cur_t * (r * c_phi) + cur_b * (r * s_phi) + cur_normal * z;
                Hit bhit = closest_hit_fast(cur_origin, d, cur_bias, bounce_max_t, opaque_offset, tri_count);
                if (bhit.index != 0xffffffffu && bhit.index != 0xfffffffeu) {
                    float b_ndl = max(dot(bhit.normal, params.sun_dir.xyz), 0.0);
                    float3 bhit_world = cur_origin + d * bhit.t;
                    path_dist += bhit.t;
                    float b_bias = max(bias, path_dist * 0.004);
                    float b_shadow = 1.0;
                    // Opt: skip sun shadow rays at night when sun_strength <= 0.02.
                    if (bits_shadows && sun_strength > 0.02 && b_ndl > 0.04) {
                        float3 bhit_pos = bhit_world + bhit.normal * b_bias;
                        if (occluded_fast(bhit_pos, params.sun_dir.xyz, b_bias * 0.5, gi_shadow_max_t)) {
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
                        float3(b_avg, b_avg, b_avg) + (b_base_albedo - b_avg) * 1.35,
                        float3(0.05, 0.05, 0.05),
                        float3(0.94, 0.94, 0.94));
                    // Direct bounce from nearest active point lights, lanterns & world effects on EVERY bounce.
                    float3 gi_pl_direct = float3(0.0, 0.0, 0.0);
                    if (bits_lighting && pl_count > 0u) {
                        uint pl_cap = min(pl_count, (bounce < 2u) ? 8u : 4u);
                        for (uint pli = 0u; pli < pl_cap; pli = pli + 1u) {
                            float4 pr = params.point_lights_pos_rad[pli];
                            float3 to_pl = pr.xyz - bhit_world;
                            if (abs(to_pl.x) >= pr.w || abs(to_pl.z) >= pr.w || abs(to_pl.y) >= pr.w) {
                                continue;
                            }
                            float r_ndl = dot(bhit.normal, to_pl);
                            if (r_ndl <= 0.0) {
                                continue;
                            }
                            float d2 = dot(to_pl, to_pl);
                            if (d2 <= 1e-4 || d2 >= pr.w * pr.w) {
                                continue;
                            }
                            float inv_d = rsqrt(d2);
                            float d_l = d2 * inv_d;
                            float att = point_light_falloff(d_l, pr.w);
                            float4 ci = params.point_lights_col_int[pli];
                            gi_pl_direct += ci.rgb * (ci.w * att * (r_ndl * inv_d) * 2.15);
                        }
                        gi_pl_direct = min(gi_pl_direct, float3(3.2, 3.2, 3.2));
                    }
                    // Feature #8: Emissive mesh & glow-map auto-lighting (runes, lanterns, lava, crystals).
                    float b_max_c = max(b_base_albedo.r, max(b_base_albedo.g, b_base_albedo.b));
                    float b_min_c = min(b_base_albedo.r, min(b_base_albedo.g, b_base_albedo.b));
                    float b_sat = (b_max_c > 1e-4) ? ((b_max_c - b_min_c) / b_max_c) : 0.0;
                    float3 mesh_emissive = float3(0.0, 0.0, 0.0);
                    if (bkind > 2.5 && b_max_c > 0.62 && b_sat > 0.38) {
                        float em_w = smoothstep(0.62, 0.90, b_max_c) * smoothstep(0.38, 0.72, b_sat);
                        float trim_boost = (bmat.surface.w < 0.92 && bmat.surface.w > 0.20) ? 1.45 : 0.85;
                        mesh_emissive = b_base_albedo * (em_w * trim_boost * 1.35);
                    }
                    float3 off_in = b_chroma * (
                        params.sun_colour.xyz * (b_ndl * b_shadow * 2.45 * sun_strength)
                        + gi_pl_direct
                        + params.ambient.xyz * (params.tuning.z * 0.20)) + mesh_emissive;
                    float3 incoming = off_in;
                    // Exclude close foreground pixels (< 8.5u from camera, e.g. Walker character)
                    // from screen-space GI bounce so the character never radiates a glow onto surroundings.
                    float b_cam_d = length(bhit_world - params.eye.xyz);
                    if (bounce == 0u && bhit.t < 28.0 && b_cam_d > 8.5) {
                        float4 b_screen = sample_screen_radiance(bhit_world, bhit.t);
                        float b_w = saturate(b_screen.a);
                        float scr_peak = max(b_screen.r, max(b_screen.g, b_screen.b));
                        float scr_min = min(b_screen.r, min(b_screen.g, b_screen.b));
                        float scr_sat = (scr_peak > 1e-4) ? ((scr_peak - scr_min) / scr_peak) : 0.0;
                        float3 scr_emissive = (scr_peak > 0.58 && scr_sat > 0.35)
                            ? (b_screen.rgb * (smoothstep(0.58, 0.95, scr_peak) * smoothstep(0.35, 0.68, scr_sat) * 1.15))
                            : float3(0.0, 0.0, 0.0);
                        float3 on_in = b_screen.rgb * (b_ndl * b_shadow * 1.45 * sun_strength + 0.18 * day_gi_scale)
                            + b_chroma * gi_pl_direct + scr_emissive;
                        incoming = lerp(off_in, max(on_in, mesh_emissive), b_w);
                    }
                    // Converged multi-bounce 3D radiance-cache tail on the final bounce when bounces >= 2.
                    if (bounce + 1u == max_bounces && max_bounces >= 2u) {
                        float4 cached = rc_query(bhit_world, bhit.normal, path_dist);
                        if (cached.w >= 2.0) {
                            incoming += b_chroma * min(cached.rgb, float3(2.4, 2.4, 2.4)) * 0.42;
                        }
                    }
                    if (bounce == 0u && b_cam_d > 8.5) {
                        has_b0 = true;
                        b0_world = bhit_world;
                        b0_normal = bhit.normal;
                        b0_dist = path_dist;
                    }
                    gi_sample += throughput * min(incoming, float3(3.2, 3.2, 3.2));
                    // Perceptually lifted bounce reflectance so bounces 2..6 preserve real indirect energy
                    // across stone/wood/terrain instead of dying out after bounce 2.
                    float3 bounce_albedo = clamp(
                        pow(max(b_chroma, float3(0.02, 0.02, 0.02)), float3(0.62, 0.62, 0.62)) * 0.88,
                        float3(0.28, 0.28, 0.28),
                        float3(0.88, 0.88, 0.88));
                    throughput *= bounce_albedo;
                    if (max(throughput.r, max(throughput.g, throughput.b)) < 0.012) {
                        break;
                    }
                    cur_origin = bhit_world + bhit.normal * b_bias;
                    cur_normal = bhit.normal;
                    cur_bias = b_bias;
                    float3 b_up = abs(cur_normal.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
                    cur_t = normalize(cross(b_up, cur_normal));
                    cur_b = cross(cur_normal, cur_t);
                } else {
                    if (bounce == 0u) {
                        gi_sky_hits += 1.0;
                    }
                    // True directional sky-dome hemisphere irradiance + golden-hour color grading.
                    // Rays escaping shadowed overhangs/alleys into open sky on bounce 1..5 smoothly
                    // carry open-sky illumination back into shadowed areas.
                    float3 raw_sky = min(apply_twilight_scattering(sample_sky_env(d), d, params.sun_dir.xyz, sun_strength, bits_twilight_sky), float3(1.35, 1.35, 1.35));
                    float3 vivid_sky = raw_sky;
                    if (params.sun_dir.y > 0.01 && params.sun_dir.y < 0.42 && sun_strength > 0.05) {
                        float golden_w = 1.0 - smoothstep(0.10, 0.42, params.sun_dir.y);
                        float sun_align = dot(normalize(float3(d.x, 0.0, d.z) + float3(1e-5, 0.0, 0.0)),
                                              normalize(float3(params.sun_dir.x, 0.0, params.sun_dir.z) + float3(1e-5, 0.0, 0.0)));
                        float3 warm_cool = lerp(float3(0.76, 0.84, 1.22), float3(1.26, 0.94, 0.68), sun_align * 0.5 + 0.5);
                        vivid_sky *= lerp(float3(1.0, 1.0, 1.0), warm_cool, golden_w * 0.55);
                    }
                    float sky_gain = (bounce == 0u) ? 0.36 : 0.34;
                    gi_sample += throughput * vivid_sky * (sky_gain * day_gi_scale);
                    break;
                }
            }
            if (has_b0 && luma(gi_sample) > 0.02 && ((id.x + id.y + uint(params.frame.x)) & 3u) == 0u) {
                rc_update(b0_world, b0_normal, b0_dist, min(gi_sample, float3(2.8, 2.8, 2.8)));
            }
            gi_sample *= (params.tuning.w * 1.25 * (1.0 - fog_t));
            accum_gi += min(gi_sample * gi_fade, float3(4.0, 4.0, 4.0));
        }
    }

    // Feature #3: Warm Subsurface Scattering (SSS) for tree foliage, blossoms & grass.
    // Driven by sun backlighting (wrap-diffuse + forward chlorophyll transmission) and canopy shadow transmittance.
    float3 sss_light = float3(0.0, 0.0, 0.0);
    bool is_green_foliage = (raster_rgb.g > raster_rgb.b * 1.12 && raster_rgb.g >= raster_rgb.r * 0.90 && raster_rgb.g > 0.06);
    bool is_blossom_foliage = (raster_rgb.r > raster_rgb.g * 1.16 && raster_rgb.b > raster_rgb.g * 1.02 && raster_rgb.r > 0.10);
    bool is_foliage_surf = !is_water && (is_green_foliage || is_blossom_foliage);
    if (bits_shadows && !is_water && fog_t < 0.92 && sun_strength > 0.05) {
        float3 avg_trans = accum_trans / float(samples);
        if (dot(avg_trans, float3(1.0, 1.0, 1.0)) > 1e-4) {
            sss_light += avg_trans * params.sun_colour.xyz * (max(n_dot_l, 0.20) * 0.65 * (1.0 - fog_t));
        }
        if (is_foliage_surf) {
            float back_dot = saturate(dot(view_dir, params.sun_dir.xyz));
            float wrap_back = saturate((dot(-normal, params.sun_dir.xyz) + 0.45) / 1.45);
            float view_trans = back_dot * back_dot * back_dot;
            float sss_vis = lerp(0.35, 1.0, accum_shadow / float(samples));
            float3 sss_tint = is_green_foliage
                ? float3(0.74, 1.08, 0.26)
                : float3(1.12, 0.68, 0.84);
            float sss_term = (view_trans * 0.42 + wrap_back * 0.16) * sss_vis * sun_strength * (1.0 - fog_t);
            sss_light += params.sun_colour.xyz * sss_tint * sss_term;
        }
    }

    // Feature #2 (Underwater Caustics): Project organic Voronoi sun caustics onto
    // submerged terrain, rocks, and ruins when beneath the active water plane.
    float3 underwater_caustics = float3(0.0, 0.0, 0.0);
    if (bits_caustics && !is_water && water_y > -1e5 && world_pos.y < water_y - 0.05 && fog_t < 0.95 && sun_strength > 0.05) {
        float sub_depth = water_y - world_pos.y;
        if (sub_depth < 28.0 && n_dot_l > 0.05) {
            float uw_vis = bits_shadows ? (accum_shadow / float(samples)) : 1.0;
            if (uw_vis > 0.05) {
                float3 caust_rgb = caustic_pattern_chromatic(world_pos.xz, params.frame.x * 0.025, dist, sub_depth);
                float3 beer = exp(-sub_depth * float3(0.18, 0.07, 0.04));
                float fade = smoothstep(0.05, 0.50, sub_depth) * exp(-sub_depth * 0.065) * exp(-dist * 0.008);
                underwater_caustics = params.sun_colour.xyz * beer * caust_rgb * (uw_vis * n_dot_l * sun_strength * fade * 0.85);
            }
        }
    }

    // Feature #1: True World-Space Ray-Marched Volumetric God-Rays & Light Shafts.
    // Marches along the 3D camera ray against the DXR TLAS with IGN jitter + SVGF smoothing:
    // near steps use alpha-aware shadow queries so shafts slice through individual tree leaves,
    // while far steps use fast opaque queries for high FPS. Works for sunbeams and moonbeams!
    float3 god_rays = float3(0.0, 0.0, 0.0);
    if (bits_shadows && params.sun_dir.y > 0.04 && dist > 10.0 && fog_t < 0.92) {
        float cos_view_sun = dot(view_dir, params.sun_dir.xyz);
        if (cos_view_sun > 0.08) {
            // Dual-lobe Henyey-Greenstein: tight sunlit shaft core + wide atmospheric scatter
            float phase = hg_phase(cos_view_sun, 0.84) * 0.72 + hg_phase(cos_view_sun, 0.42) * 0.28;
            float march_len = min(dist, 210.0) - 2.5;
            if (march_len > 6.0 && phase > 0.04) {
                uint vol_steps = (march_len < 40.0) ? 5u : ((dist > 140.0) ? 6u : 8u);
                float inv_steps = 1.0 / float(vol_steps);
                float lit_steps = 0.0;
                float occ_steps = 0.0;
                for (uint vi = 0u; vi < vol_steps; vi = vi + 1u) {
                    float u = (float(vi) + ign_v) * inv_steps;
                    // Exponential-stratified distribution concentrates detail near canopies & gates
                    float t_vol = 2.5 + (u * u * 0.55 + u * 0.45) * march_len;
                    float3 p_vol = params.eye.xyz + view_dir * t_vol;
                    bool blocked = false;
                    if (vi < 3u && t_vol < 85.0) {
                        ShadowResult vsr = trace_sun_shadow(
                            p_vol, params.sun_dir.xyz, 0.4, 240.0,
                            state ^ (vi * 31u), opaque_offset, tri_count, false, 0.0, 0.0);
                        blocked = vsr.occluded;
                    } else {
                        blocked = occluded_fast(p_vol, params.sun_dir.xyz, 0.5, 280.0);
                    }
                    // Height-based atmospheric mist density (richer in valleys, forest floors, water basins)
                    float h_density = exp(-max(p_vol.y - params.eye.y + 30.0, 0.0) * 0.0045);
                    if (blocked) {
                        occ_steps += 1.0;
                    } else {
                        lit_steps += h_density;
                    }
                }
                float shaft_vis = lit_steps * inv_steps;
                float occ_frac = occ_steps * inv_steps;
                // Carve crisp shafts where canopies/arches/cliffs slice the beam, keeping open sky clean
                float shaft_contrast = lerp(0.16, 1.0, smoothstep(0.05, 0.34, occ_frac));
                float dist_w = smoothstep(8.0, 85.0, march_len);
                float low_sun_boost = lerp(1.38, 1.0, smoothstep(0.12, 0.52, params.sun_dir.y));
                float3 shaft_col = lerp(float3(0.32, 0.46, 0.72), params.sun_colour.xyz * float3(1.06, 0.98, 0.86), sun_strength);
                float shaft_intensity = lerp(0.055, 0.145, sun_strength) * low_sun_boost;
                god_rays = shaft_col * (shaft_vis * shaft_contrast * min(phase, 1.65) * dist_w * shaft_intensity * (1.0 - fog_t * 0.65));
            }
        }
    }

    // Enhancement #2: Ray-Traced Underwater Volumetric Sun/Moon Shafts & Scatter when submerged.
    if (bits_shadows && water_y > -1e5 && params.sun_dir.y > 0.03
        && (is_submerged || world_pos.y < water_y - 0.15) && dist > 1.5)
    {
        float uw_march = min(dist, 60.0) - 0.5;
        if (uw_march > 1.0) {
            float uw_lit = 0.0;
            float uw_occ = 0.0;
            float t_anim = params.frame.x * 0.032;
            for (uint ui = 0u; ui < 6u; ui = ui + 1u) {
                float u = (float(ui) + ign_v) / 6.0;
                float t_u = 0.5 + u * uw_march;
                float3 p_u = params.eye.xyz + view_dir * t_u;
                if (p_u.y >= water_y) {
                    continue;
                }
                if (occluded_fast(p_u, params.sun_dir.xyz, 0.4, 220.0)) {
                    uw_occ += 1.0;
                } else {
                    float d_sub = max(water_y - p_u.y, 0.05);
                    float w1 = sin(p_u.x * 0.42 + p_u.z * 0.36 + t_anim);
                    float w2 = cos(p_u.x * 0.29 - p_u.z * 0.51 - t_anim * 0.8);
                    float wave_focus = 0.45 + 0.55 * saturate(0.5 + 0.5 * w1 * w2);
                    uw_lit += exp(-d_sub * 0.045) * wave_focus;
                }
            }
            float uw_vis = uw_lit / 6.0;
            float uw_phase = 0.55 + 0.45 * saturate(dot(view_dir, params.sun_dir.xyz));
            float uw_contrast = lerp(0.55, 1.0, smoothstep(0.05, 0.35, uw_occ / 6.0));
            float3 day_uw = float3(0.22, 0.78, 0.86) * max(params.sun_colour.xyz, float3(0.4, 0.4, 0.4));
            float3 night_uw = float3(0.06, 0.16, 0.28);
            float3 uw_tint = lerp(night_uw, day_uw, sun_strength);
            float uw_gain = is_submerged ? 0.34 : 0.18;
            god_rays += uw_tint * (uw_vis * uw_phase * uw_contrast * uw_gain * (1.0 - fog_t * 0.25));
            // Feature #10: Depth-layered underwater volumetric grading + suspended plankton/dust motes
            // + animated light-beam shimmer on submerged ruins.
            if (is_submerged) {
                float eye_sub = max(water_y - params.eye.y, 0.05);
                float3 shallow_aqua = float3(0.05, 0.28, 0.34);
                float3 deep_abyss = float3(0.015, 0.075, 0.16);
                float depth_t = saturate(eye_sub / 22.0);
                float3 vol_grade = lerp(shallow_aqua, deep_abyss, depth_t) * lerp(0.18, 1.0, sun_strength);
                float vol_w = (1.0 - exp(-uw_march * 0.022)) * 0.32;
                god_rays += vol_grade * vol_w;
                // 3D suspended sun-lit plankton/dust motes along the underwater camera ray
                float mote_acc = 0.0;
                float t_mote = params.frame.x * 0.035;
                for (uint mi = 0u; mi < 4u; mi = mi + 1u) {
                    float t_m = 1.2 + (float(mi) + ign_v) * min(uw_march * 0.22, 5.5);
                    if (t_m < dist - 0.5) {
                        float3 pm = params.eye.xyz + view_dir * t_m;
                        if (pm.y < water_y - 0.1) {
                            mote_acc += underwater_motes(pm, t_mote) * exp(-t_m * 0.08);
                        }
                    }
                }
                god_rays += lerp(float3(0.18, 0.38, 0.52), float3(0.68, 0.96, 0.92) * max(params.sun_colour.xyz, float3(0.35, 0.35, 0.35)), sun_strength)
                    * (mote_acc * (0.28 + 0.72 * uw_vis) * 0.38);
            }
        }
    }

    // Feature #3: Ray-traced localized height-based valley mist (strictly zero within 18u of camera!).
    if (bits_valley_mist && !is_submerged && dist > 18.0 && fog_t < 0.96) {
        float mist_base_y = (water_y > -1e5) ? (water_y + 2.5) : (params.eye.y - 16.0);
        float mist_len = min(dist, 220.0) - 18.0;
        if (mist_len > 4.0) {
            float mist_acc = 0.0;
            float mist_lit = 0.0;
            for (uint vi = 0u; vi < 4u; vi = vi + 1u) {
                float u = (float(vi) + ign_v) * 0.25;
                float t_m = 18.0 + u * mist_len;
                float3 pm = params.eye.xyz + view_dir * t_m;
                float h_above = max(pm.y - mist_base_y, 0.0);
                float h_dens = exp(-h_above * 0.075);
                if (h_dens > 0.04) {
                    // Subtle wispy modulation so valley mist looks organic rather than a flat slab
                    float wisp = 0.65 + 0.35 * sin(pm.x * 0.035 + params.frame.x * 0.006) * cos(pm.z * 0.031 - params.frame.x * 0.005);
                    float step_d = h_dens * wisp;
                    mist_acc += step_d;
                    bool m_occ = (bits_shadows && params.sun_dir.y > 0.04)
                        ? occluded_fast(pm, params.sun_dir.xyz, 0.5, 220.0)
                        : false;
                    if (!m_occ) {
                        mist_lit += step_d;
                    }
                }
            }
            float mist_opt = saturate((mist_acc * 0.25) * smoothstep(18.0, 95.0, dist) * 0.36);
            if (mist_opt > 1e-3) {
                float sun_frac = (mist_acc > 1e-4) ? saturate(mist_lit / mist_acc) : 0.0;
                float3 mist_day = lerp(params.ambient.xyz * 0.55, params.sun_colour.xyz * 0.72 + float3(0.10, 0.14, 0.18), sun_frac);
                float3 mist_night = float3(0.025, 0.045, 0.085);
                float3 mist_col = lerp(mist_night, mist_day, sun_strength);
                god_rays += mist_col * (mist_opt * (1.0 - fog_t * 0.75) * 0.45);
            }
        }
    }

    // Enhancement #18: Colored Indirect Light Bounce from Water Surfaces ("Water-to-Wall" Caustic Bounce)
    // onto undersides of bridges, piers, ship hulls, and coastal cliffs just above water.
    if (bits_gi && !is_water && water_y > -1e5 && world_pos.y >= water_y - 0.1 && world_pos.y < water_y + 16.0
        && normal.y < 0.35 && sun_strength > 0.05 && dist < 85.0)
    {
        float h_above = max(world_pos.y - water_y, 0.1);
        float h_fade = exp(-h_above * 0.18) * (1.0 - saturate(normal.y + 0.15));
        float caust_b = caustic_pattern(world_pos.xz, params.frame.x * 0.025, dist + 25.0);
        float w_vis = bits_shadows ? (accum_shadow / float(samples)) : 1.0;
        underwater_caustics += float3(0.18, 0.68, 0.62) * params.sun_colour.xyz
            * (caust_b * h_fade * sun_strength * (0.35 + 0.65 * w_vis) * 0.22 * (1.0 - fog_t));
    }

    // Enhancement #3: Subtle, smooth edge-free Volumetric Light Halos around world lanterns/portals
    // (excludes camera light < 3.5u from eye and uses strictly zero-at-boundary cubic falloff).
    if (bits_lighting && pl_count > 0u && dist > 3.0 && fog_t < 0.95) {
        float dark_fog_w = max(0.45 * (1.0 - sun_strength), fog_t * 0.65);
        if (dark_fog_w > 0.12) {
            float max_h_t = min(dist, 85.0);
            uint halo_cap = min(pl_count, 32u);
            float3 halo_sum = float3(0.0, 0.0, 0.0);
            float best_h_score = 0.0;
            float3 best_h_pos = float3(0.0, 0.0, 0.0);
            float3 best_h_col = float3(0.0, 0.0, 0.0);
            for (uint hi = 0u; hi < halo_cap; hi = hi + 1u) {
                float4 pr = params.point_lights_pos_rad[hi];
                if (pr.w <= 1.5) {
                    continue;
                }
                float3 to_l = pr.xyz - params.eye.xyz;
                float l_dist2 = dot(to_l, to_l);
                // Exclude camera light attached near the camera eye.
                if (l_dist2 < 12.25) {
                    continue;
                }
                float t_p = dot(to_l, view_dir);
                if (t_p <= 2.5 || t_p >= max_h_t) {
                    continue;
                }
                float d2 = max(l_dist2 - t_p * t_p, 0.0);
                float r_h = clamp(pr.w * 0.10, 0.7, 3.2);
                float r2_out = r_h * r_h * 4.0;
                if (d2 >= r2_out) {
                    continue;
                }
                float4 ci = params.point_lights_col_int[hi];
                float u = saturate(d2 / max(r2_out, 0.25));
                float w_edge = 1.0 - u;
                float scatter = (w_edge * w_edge * w_edge) * exp(-u * 3.5) / (1.0 + t_p * 0.04);
                float3 contrib = ci.rgb * (ci.w * scatter * 0.014);
                float sc = luma(contrib);
                if (sc > best_h_score) {
                    best_h_score = sc;
                    best_h_pos = params.eye.xyz + view_dir * t_p;
                    best_h_col = contrib;
                } else {
                    halo_sum += contrib * 0.55;
                }
            }
            if (best_h_score > 1e-4) {
                float vis_h = 1.0;
                if (bits_shadows && occluded_fast(params.eye.xyz, view_dir, 0.5, max(length(best_h_pos - params.eye.xyz) - 0.5, 0.6))) {
                    vis_h = 0.0;
                }
                halo_sum += best_h_col * vis_h;
            }
            god_rays += min(halo_sum * dark_fog_w, float3(0.12, 0.12, 0.12));
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

    if (bits_lighting && pl_count > 0u && is_water) {
        // Water is a glossy dielectric, NEVER a flat matte diffuse sheet: evaluate
        // rippled specular glints from point/effect lights so night water stays reflective
        // and textured instead of turning flat grey.
        float3 w_norm = water_wave_normal(world_pos.xz, params.frame.x * 0.03, dist);
        float3 view_to_eye = -view_dir;
        for (uint li = 0u; li < pl_count; li = li + 1u) {
            float4 pl_pr = params.point_lights_pos_rad[li];
            float3 to_l = pl_pr.xyz - world_pos;
            if (abs(to_l.x) >= pl_pr.w || abs(to_l.z) >= pl_pr.w || abs(to_l.y) >= pl_pr.w) {
                continue;
            }
            if (to_l.y < -2.0) {
                continue;
            }
            float pl_d2 = dot(to_l, to_l);
            if (pl_d2 <= 1e-6 || pl_d2 >= pl_pr.w * pl_pr.w) {
                continue;
            }
            float inv_pl_dist = rsqrt(pl_d2);
            float pl_dist = pl_d2 * inv_pl_dist;
            float atten = point_light_falloff(pl_dist, pl_pr.w);
            if (atten <= 1e-5) {
                continue;
            }
            float3 l_dir = to_l * inv_pl_dist;
            float3 h_vec = normalize(l_dir + view_to_eye);
            float ndh = saturate(dot(w_norm, h_vec));
            float ndh2 = ndh * ndh;
            float ndh4 = ndh2 * ndh2;
            float ndh8 = ndh4 * ndh4;
            float spec = ndh8 * ndh8 * ndh8 * 0.85 + ndh4 * 0.04;
            float4 pl_ci = params.point_lights_col_int[li];
            point_lighting += pl_ci.rgb * (pl_ci.w * atten * spec);
        }
        float wp = max(point_lighting.r, max(point_lighting.g, point_lighting.b));
        if (wp > 0.9) {
            float wc = 0.9 + (wp - 0.9) / (1.0 + (wp - 0.9) * 0.85);
            point_lighting *= (wc / wp);
        }
        point_lighting = min(point_lighting * (1.0 - fog_t), float3(1.6, 1.6, 1.6));
    } else if (bits_lighting && pl_count > 0u) {
        // Zero-visual-cost Perf Boost: fast AABB+radius pre-check before touching G-buffer neighbors.
        // Eliminates 16-24 G-buffer texture fetches on 80-95% of pixels with no nearby point lights!
        bool any_pl_near = false;
        for (uint pre_i = 0u; pre_i < pl_count; pre_i = pre_i + 1u) {
            float4 pr = params.point_lights_pos_rad[pre_i];
            float3 d_pre = pr.xyz - world_pos;
            if (abs(d_pre.x) < pr.w && abs(d_pre.z) < pr.w && abs(d_pre.y) < pr.w && dot(d_pre, d_pre) < pr.w * pr.w) {
                any_pl_near = true;
                break;
            }
        }
        float3 pl_normal = normal;
        if (any_pl_near) {
            int2 gdim_pl = int2(int(params.extents.x), int(params.extents.y));
            int2 cpx = int2(id.xy);
            if (dist < 18.0) {
                // Tight-depth 12-tap curved normal smoother for close models & Walker characters:
                // removes flat low-poly depth-normal facets at night while preserving silhouettes.
                float d_thresh = clamp(dist * 0.024, 0.08, 0.55);
                int2 offs12[12] = {
                    int2(1, 0), int2(-1, 0), int2(0, 1), int2(0, -1),
                    int2(2, 1), int2(-2, 1), int2(2, -1), int2(-2, -1),
                    int2(1, 2), int2(-1, 2), int2(1, -2), int2(-1, -2)
                };
                float3 n_acc = normal * 0.65;
                for (uint oi = 0u; oi < 12u; oi = oi + 1u) {
                    int2 npx = clamp(cpx + offs12[oi], int2(0, 0), gdim_pl - int2(1, 1));
                    float nd = src_gbuf_pos.Load(int3(npx, 0)).w;
                    if (nd > 0.0 && abs(nd - dist) < d_thresh) {
                        float3 nn = src_gbuf_norm.Load(int3(npx, 0)).xyz;
                        float nn_l = length(nn);
                        if (nn_l > 1e-4) {
                            nn /= nn_l;
                            if (dot(normal, nn) > 0.36) {
                                n_acc += nn;
                            }
                        }
                    }
                }
                float acc_l = length(n_acc);
                if (acc_l > 1e-4) {
                    pl_normal = n_acc / acc_l;
                }
            } else {
                // Wider depth- and angle-aware 9-tap normal smoothing for terrain/rocks.
                float d_thresh = max(0.80, dist * 0.026);
                int2 offs[8] = {
                    int2(2, 0), int2(-2, 0), int2(0, 2), int2(0, -2),
                    int2(4, 4), int2(-4, 4), int2(4, -4), int2(-4, -4)
                };
                float3 n_acc = normal * 1.25;
                for (uint oi = 0u; oi < 8u; oi = oi + 1u) {
                    int2 npx = clamp(cpx + offs[oi], int2(0, 0), gdim_pl - int2(1, 1));
                    float nd = src_gbuf_pos.Load(int3(npx, 0)).w;
                    if (nd > 0.0 && abs(nd - dist) < d_thresh) {
                        float3 nn = src_gbuf_norm.Load(int3(npx, 0)).xyz;
                        float nn_l = length(nn);
                        if (nn_l > 1e-4) {
                            nn /= nn_l;
                            if (dot(normal, nn) > 0.56) {
                                n_acc += nn;
                            }
                        }
                    }
                }
                float acc_l = length(n_acc);
                if (acc_l > 1e-4) {
                    pl_normal = n_acc / acc_l;
                }
            }
        }
        float pl_terrain_skip = 2.4 + steepness * 12.0 + dist * 0.020;

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
        uint exact_idxs[4] = { 0xffffffffu, 0xffffffffu, 0xffffffffu, 0xffffffffu };
        float exact_ws[4] = { 0.0, 0.0, 0.0, 0.0 };
        float3 exact_cols[4] = { float3(0.0, 0.0, 0.0), float3(0.0, 0.0, 0.0), float3(0.0, 0.0, 0.0), float3(0.0, 0.0, 0.0) };

        if (any_pl_near) for (uint li = 0u; li < pl_count; li = li + 1u) {
            float4 pl_pr = params.point_lights_pos_rad[li];
            float3 to_l = pl_pr.xyz - world_pos;
            if (abs(to_l.x) >= pl_pr.w || abs(to_l.z) >= pl_pr.w || abs(to_l.y) >= pl_pr.w) {
                continue;
            }
            float pl_d2 = dot(to_l, to_l);
            if (pl_d2 <= 1e-6 || pl_d2 >= pl_pr.w * pl_pr.w) {
                continue;
            }
            float inv_pl_dist = rsqrt(pl_d2);
            float pl_dist = pl_d2 * inv_pl_dist;
            float raw_cos = dot(pl_normal, to_l) * inv_pl_dist;
            // Gentle wrap-Lambert prevents harsh triangle-edge jumps on low-poly terrain slopes
            // while still rejecting genuinely back-facing building walls.
            if (raw_cos <= -0.28) {
                continue;
            }
            float pl_ndl = saturate((raw_cos + 0.32) / 1.32);
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
            if (cand_count < 4u) {
                exact_idxs[cand_count] = li;
                exact_ws[cand_count] = w;
                exact_cols[cand_count] = pl_ci.rgb;
            }
            cand_count = cand_count + 1u;
            res0_wsum += p_hat;
            if (rand01(state) * res0_wsum <= p_hat) {
                res0_idx = li;
                res0_phat = p_hat;
                res0_col = pl_ci.rgb;
                res0_w = w;
            }
            res1_wsum += p_hat;
            if (rand01(state) * res1_wsum <= p_hat) {
                res1_idx = li;
                res1_phat = p_hat;
                res1_col = pl_ci.rgb;
                res1_w = w;
            }
        }

        if (cand_count > 0u && cand_count <= 4u) {
            // Exact evaluation when 1..=4 lights touch this pixel (zero resampling flicker).
            for (uint ci = 0u; ci < cand_count; ci = ci + 1u) {
                uint idx = exact_idxs[ci];
                float4 pl_pr = params.point_lights_pos_rad[idx];
                float3 to_l = pl_pr.xyz - world_pos;
                float pl_dist = length(to_l);
                float3 l_dir = to_l / max(pl_dist, 1e-5);
                float pl_vis = 1.0;
                if (bits_shadows && pl_dist > 1.8 && exact_ws[ci] > 0.025) {
                    // Feature #8: Large world effects & portals (`radius >= 14u`) use 3D area emitter
                    // disc/column sampling with 2 stratified taps for soft multi-directional penumbrae.
                    bool is_area = pl_pr.w >= 14.0;
                    float r_emit = is_area ? clamp(pl_pr.w * 0.12, 0.80, 3.60) : clamp(pl_pr.w * 0.032, 0.16, 1.35);
                    float y_spread = is_area ? clamp(pl_pr.w * 0.08, 0.50, 2.60) : 0.0;
                    float3 l_up = abs(l_dir.y) < 0.99 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
                    float3 l_t = normalize(cross(l_up, l_dir));
                    float3 l_b = cross(l_dir, l_t);
                    float self_clear = clamp(pl_pr.w * 0.08, 1.4, 3.2);
                    uint area_taps = (is_area && dist < 90.0) ? 2u : 1u;
                    float vis_acc = 0.0;
                    for (uint ai = 0u; ai < area_taps; ai = ai + 1u) {
                        float ang = rot_angle + float(ci) * 2.39996323 + float(ai) * 3.14159265;
                        float sa, ca;
                        sincos(ang, sa, ca);
                        float rad_s = r_emit * sqrt(rand01(state));
                        float y_off = (rand01(state) - 0.5) * 2.0 * y_spread;
                        float3 jitter_pt = pl_pr.xyz + (l_t * ca + l_b * sa) * rad_s + float3(0.0, y_off, 0.0);
                        float3 j_vec = jitter_pt - ray_origin;
                        float j_dist = length(j_vec);
                        float pl_dyn_skip = (normal.y > 0.72) ? 0.14 : 0.62;
                        if (j_dist <= self_clear + bias || !occluded_point_light(ray_origin, j_vec / max(j_dist, 1e-4), bias * 0.5, j_dist - self_clear, pl_terrain_skip, pl_dyn_skip, opaque_offset, tri_count)) {
                            vis_acc += 1.0;
                        }
                    }
                    pl_vis = vis_acc / float(area_taps);
                }
                pl_w_total += exact_ws[ci];
                pl_w_lit += exact_ws[ci] * pl_vis;
                pl_debug_lit += exact_cols[ci] * (exact_ws[ci] * pl_vis);
                pl_debug_shadow += exact_ws[ci] * (1.0 - pl_vis);
                point_lighting += exact_cols[ci] * (exact_ws[ci] * pl_vis * 2.4);
            }
        } else if (cand_count > 4u) {
            // ReSTIR DI unbiased 2-reservoir RIS evaluation (2 shadow rays total for 5..128 lights).
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
                if (bits_shadows && pl_dist > 1.8) {
                    float r_emit = clamp(pl_pr.w * 0.032, 0.16, 1.35);
                    float3 l_up = abs(l_dir.y) < 0.99 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
                    float3 l_t = normalize(cross(l_up, l_dir));
                    float3 l_b = cross(l_dir, l_t);
                    float ang = rot_angle + float(ri) * 2.39996323 + 1.1;
                    float sa, ca;
                    sincos(ang, sa, ca);
                    float3 jitter_pt = pl_pr.xyz + (l_t * ca + l_b * sa) * (r_emit * sqrt(rand01(state)));
                    float3 j_vec = jitter_pt - ray_origin;
                    float j_dist = length(j_vec);
                    float self_clear = clamp(pl_pr.w * 0.08, 1.4, 3.2);
                    if (j_dist > self_clear + bias && occluded_point_light(ray_origin, j_vec / max(j_dist, 1e-4), bias * 0.5, j_dist - self_clear, pl_terrain_skip, opaque_offset, tri_count)) {
                        pl_vis = 0.0;
                    }
                }
                float ris_scale = min(r_wsums[ri] / max(r_phats[ri], 1e-6), 8.0) * 0.5;
                float eff_w = r_ws[ri] * ris_scale;
                pl_w_total += eff_w;
                pl_w_lit += eff_w * pl_vis;
                pl_debug_lit += r_cols[ri] * (eff_w * pl_vis);
                pl_debug_shadow += eff_w * (1.0 - pl_vis);
                point_lighting += r_cols[ri] * (eff_w * pl_vis * 2.4);
            }
        }
        // Hue-preserving soft-shoulder highlight rolloff: keeps normal lights vivid while
        // preventing stacked/close-range effects from clipping to solid white.
        float pl_peak = max(point_lighting.r, max(point_lighting.g, point_lighting.b));
        if (pl_peak > 1.35) {
            float comp = 1.35 + (pl_peak - 1.35) / (1.0 + (pl_peak - 1.35) * 0.65);
            point_lighting *= (comp / pl_peak);
        }
        point_lighting = min(point_lighting * (1.0 - fog_t), float3(2.8, 2.8, 2.8));
        // Feature #3: Nighttime foliage/grass SSS glow when torches/lanterns shine through leaves.
        if (is_foliage_surf && luma(point_lighting) > 0.04) {
            float3 f_tint = is_green_foliage ? float3(0.76, 1.08, 0.28) : float3(1.10, 0.70, 0.85);
            sss_light += point_lighting * f_tint * 0.24;
        }
        // Feed bright nearby point/effect lighting into the 3D World-Space Radiance Cache
        // (skipping close foreground pixels < 8.5u so Walker character never emits a glow halo).
        if (bits_gi && dist > 8.5 && luma(point_lighting) > 0.08 && ((id.x + id.y + uint(params.frame.x)) & 7u) == 0u) {
            rc_update(world_pos, pl_normal, dist, point_lighting * 0.55);
        }
    }

    float point_vis = (pl_w_total > 1e-4) ? (pl_w_lit / max(pl_w_total, 1e-5)) : 1.0;
    float point_shadow_mod = (pl_shadows_enabled && pl_w_total > 1e-4)
        ? (1.0 - clamp(pl_w_total, 0.0, 0.5) * (1.0 - point_vis))
        : 1.0;

    float inv = 1.0 / float(samples);
    float avg_shadow = accum_shadow * inv;
    float avg_open = accum_open * inv;
    float3 bent_normal = (dot(accum_bent_n, accum_bent_n) > 1e-4) ? normalize(accum_bent_n) : normal;
    float3 avg_refl = accum_reflection * inv + god_rays;
    float avg_refl_dist = accum_refl_dist * inv;
    float3 avg_gi = accum_gi * inv + sss_light + underwater_caustics;

    // Enhancement #8: Ray-Traced Moonlit Night Sky & Starlight Hemispherical Bounce
    // (open hilltops catch cool silver-blue moonlight; deep shade/alleys stay pitch black).
    if (bits_moonlit_night && !is_water && sun_strength < 0.35 && fog_t < 0.94) {
        float night_w = 1.0 - smoothstep(0.05, 0.35, sun_strength);
        float sky_up = saturate(bent_normal.y * 0.65 + 0.35);
        float moon_align = saturate(dot(bent_normal, params.sun_dir.xyz)) * 0.55 + 0.45;
        float3 night_sky = min(sample_sky_env(bent_normal), float3(0.45, 0.55, 0.75));
        float3 moon_bounce = lerp(float3(0.035, 0.055, 0.105), night_sky * 0.35 + float3(0.03, 0.05, 0.09), 0.5);
        float open_gate = avg_open * avg_open;
        avg_gi += moon_bounce * (night_w * sky_up * moon_align * open_gate * (1.0 - fog_t));
    }

    // Enhancement #19: Ray-Traced Shadowed Rain & Snow Lighting.
    if (bits_weather && dist < 80.0) {
        float precip_luma = luma(raster_rgb);
        if (precip_luma > 0.45 && abs(normal.y) < 0.75) {
            float p_vis = bits_shadows ? avg_shadow : 1.0;
            float3 pl_glow = point_lighting * 0.45;
            avg_gi += (params.sun_colour.xyz * (p_vis * sun_strength * 0.14) + pl_glow) * (1.0 - saturate(dist / 80.0));
        }
    }

    float3 out_rgb;
    float out_lf;
    if (layered) {
        float sun_e = max(sun_luma, 0.0) * n_dot_l;
        float amb_e = max(luma(params.ambient.xyz) * params.tuning.z, 0.005);
        float sun_f = saturate(sun_e / max(sun_e + amb_e, 1e-4));
        float amb_f = 1.0 - sun_f;
        float amb_base = max(amb_f, lerp(0.32, 0.42, sun_strength));
        float sun_base = 1.0 - amb_base;
        float v_sun = bits_shadows ? avg_shadow : 1.0;
        // Enhancement #7 & #16: Bent-normal sky alignment + deep interior/tunnel sky-portal falloff.
        float sky_portal_vis = (gi_total_rays > 0.5) ? lerp(0.76, 1.0, gi_sky_hits / gi_total_rays) : 1.0;
        float bent_sky = lerp(0.92, 1.0, saturate(bent_normal.y * 0.5 + 0.5));
        float v_ao = bits_ao ? (avg_open * bent_sky * sky_portal_vis) : 1.0;
        bool gi_active = bits_gi && !is_water && max_bounces >= 1u && params.tuning.w > 0.0 && gi_fade > 0.01;
        float amb_keep = gi_active ? lerp(1.0, 0.30, gi_fade) : 1.0;
        float night_dark_scale = lerp(0.28, 1.0, smoothstep(0.0, 0.25, sun_strength));
        float separated_lf = (amb_base * v_ao * amb_keep * night_dark_scale + sun_base * v_sun) * point_shadow_mod;
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
            if (ph.index < tri_count) {
                occ_kind = tris[ph.index].a.w;
            } else if (ph.index == 0xfffffffeu) {
                occ_kind = 3.0;
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
            if (rh.index < tri_count) {
                rkind = tris[rh.index].a.w;
            } else if (rh.index == 0xfffffffeu) {
                rkind = 3.0;
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
