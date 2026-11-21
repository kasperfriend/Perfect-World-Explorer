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
    float4 point_light_meta;           // x: active point light count (<= 16)
    float4 point_lights_pos_rad[16];   // xyz: world pos, w: radius
    float4 point_lights_col_int[16];   // rgb: colour, w: intensity
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

Material material_of(uint tri_index) {
    uint mat_count = max(uint(params.counts.z), 1u);
    uint mat_idx = min(uint(round(tris[tri_index].b.w)), mat_count - 1u);
    return materials[mat_idx];
}

// Interleaved Gradient Noise for per-pixel decorrelation of shadow and AO disks.
float ign(uint2 px, uint frame) {
    float2 p = float2(px) + float2(frame & 63u, (frame * 7u) & 63u) * 5.588238;
    return frac(52.9829189 * frac(dot(p, float2(0.06711056, 0.00583715))));
}

bool accept_opacity(uint prim, float2 bary, uint seed) {
    uint tri_count = uint(params.counts.x);
    if (prim >= tri_count) {
        return false;
    }
    float opacity = material_of(prim).surface.w;
    if (opacity >= 0.99) {
        return true;
    }
    uint h = prim * 747796405u ^ uint(bary.x * 4096.0) * 2891336453u ^ uint(bary.y * 4096.0) * 1664525u ^ seed;
    h ^= h >> 16u;
    float r = float(h & 0xffffu) / 65536.0;
    return r < opacity;
}

Hit closest_hit(float3 origin, float3 dir, float t_min, float t_max) {
    Hit hit;
    hit.index = 0xffffffffu;
    hit.t = t_max;
    hit.normal = float3(0.0, 1.0, 0.0);

    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = t_min;
    ray.TMax = t_max;

    RayQuery<RAY_FLAG_NONE> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_NONE, 0xffu, ray);
    while (q.Proceed()) {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE) {
            if (accept_opacity(q.CandidatePrimitiveIndex(), q.CandidateTriangleBarycentrics(), 0u)) {
                q.CommitNonOpaqueTriangleHit();
            }
        }
    }

    if (q.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
        uint prim = q.CommittedPrimitiveIndex();
        uint tri_count = uint(params.counts.x);
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

// Phase 4: Sample the raster sky on reflection miss (exact raster skybox when
// on-screen, matched horizon-to-zenith gradient + sun lobe when off-screen).
float3 sample_sky_reflection(float3 dir) {
    float3 far_pos = params.eye.xyz + dir * (params.lens.y * 0.9);
    float4 clip = mul(params.vp, float4(far_pos, 1.0));
    if (clip.w > 1e-4) {
        float3 ndc = clip.xyz / clip.w;
        float2 uv = float2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        if (uv.x >= 0.01 && uv.x <= 0.99 && uv.y >= 0.01 && uv.y <= 0.99) {
            int2 gdim = int2(int(params.extents.x), int(params.extents.y));
            int2 sp = clamp(int2(uv * params.extents.xy), int2(0, 0), gdim - int2(1, 1));
            if (src_gbuf_pos.Load(int3(sp, 0)).w <= 0.0) {
                return src_gbuf_raster.Load(int3(sp, 0)).rgb;
            }
        }
    }
    float up_t = pow(saturate(dir.y), 0.55);
    float3 zenith = max(params.ambient.xyz * 1.15, params.fog_colour.xyz * float3(0.70, 0.85, 1.15));
    float3 base = lerp(params.fog_colour.xyz, zenith, up_t);
    float sun_dot = saturate(dot(dir, params.sun_dir.xyz));
    return max(base + params.sun_colour.xyz * (pow(sun_dot, 96.0) * 1.5), float3(0.0, 0.0, 0.0));
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

bool occluded(float3 origin, float3 dir, float t_min, float t_max, uint seed) {
    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = t_min;
    ray.TMax = t_max;

    RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER, 0xffu, ray);
    while (q.Proceed()) {
        if (q.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE) {
            if (accept_opacity(q.CandidatePrimitiveIndex(), q.CandidateTriangleBarycentrics(), seed)) {
                q.CommitNonOpaqueTriangleHit();
            }
        }
    }
    return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT;
}

[numthreads(8, 8, 1)]
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
        out_guide[id.xy] = float4(1.0, 1.0, 0.0, 0.0);
        out_reflection[id.xy] = float4(0.0, 0.0, 0.0, 1.0);
        out_normals[id.xy] = float4(0.0, 1.0, 0.0, 1.0);
        out_albedo[id.xy] = float4(0.0, 0.0, 0.0, 0.04);
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
    float bias = max(params.effects.z, max(0.15, dist * 0.003));
    float3 ray_origin = world_pos + normal * bias - view_dir * (bias * 0.35);
    float n_dot_l = max(dot(normal, params.sun_dir.xyz), 0.0);

    float fog_t = 0.0;
    float fog_start = params.fog.x;
    float fog_end = params.fog.y;
    if (fog_end > fog_start && fog_start >= 0.0) {
        fog_t = saturate((dist - fog_start) / (fog_end - fog_start));
    }

    uint samples = max(uint(params.frame.y), 1u);
    uint state = id.x * 0x9e3779b9u ^ id.y * 0x85ebca6bu ^ uint(params.frame.x) * 0xc2b2ae35u;

    float accum_shadow = 0.0;
    float accum_open = 0.0;
    float3 accum_reflection = float3(0.0, 0.0, 0.0);
    float accum_refl_dist = 0.0;
    float3 accum_gi = float3(0.0, 0.0, 0.0);

    // Phase 6: 8 stratified cosine-weighted hemisphere AO samples within
    // world-space contact radius (eliminates coarse 2-tap circle blotches).
    float ao_radius = clamp(params.fog.z, 2.0, 6.0);
    uint ao_samples = 8u;
    float rot_angle = ign(id.xy, uint(params.frame.x)) * (2.0 * PI);

    float3 up = abs(normal.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 t_axis = normalize(cross(up, normal));
    float3 b_axis = cross(normal, t_axis);

    float3 sun_up = abs(params.sun_dir.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 sun_t = normalize(cross(sun_up, params.sun_dir.xyz));
    float3 sun_b = cross(params.sun_dir.xyz, sun_t);

    for (uint s = 0u; s < samples; s = s + 1u) {
        // Phase 5: Decorrelated 4-sample Vogel-disk soft sun shadow penumbra.
        float shadow = 1.0;
        if (bits_shadows && n_dot_l > 0.02) {
            uint sun_taps = params.lens.z > 1e-4 ? 4u : 1u;
            uint sun_hits = 0u;
            float spread_tan = tan(params.lens.z);
            for (uint k = 0u; k < sun_taps; k = k + 1u) {
                float3 shadow_dir = params.sun_dir.xyz;
                if (sun_taps > 1u) {
                    float r = sqrt((float(k) + 0.5) / float(sun_taps)) * spread_tan;
                    float theta = float(k) * 2.39996323 + rot_angle + float(s) * 1.6180339;
                    float s_th, c_th;
                    sincos(theta, s_th, c_th);
                    shadow_dir = normalize(params.sun_dir.xyz + (sun_t * c_th + sun_b * s_th) * r);
                }
                if (occluded(ray_origin, shadow_dir, bias * 0.5, params.lens.y, state ^ k)) {
                    sun_hits = sun_hits + 1u;
                }
            }
            shadow = 1.0 - float(sun_hits) / float(sun_taps);
        }
        accum_shadow += shadow;

        // Phase 6: 8-sample stratified cosine-weighted contact ambient occlusion.
        float open = 1.0;
        if (bits_ao) {
            uint hits = 0u;
            for (uint i = 0u; i < ao_samples; i = i + 1u) {
                float u1 = (float(i) + rand01(state)) / float(ao_samples);
                float phi = float(i) * 2.39996323 + rot_angle + float(s) * 0.7548776;
                float r = sqrt(u1);
                float s_phi, c_phi;
                sincos(phi, s_phi, c_phi);
                float z = sqrt(max(0.0, 1.0 - u1));
                float3 d = t_axis * (r * c_phi) + b_axis * (r * s_phi) + normal * z;
                if (occluded(ray_origin, d, bias * 0.5, ao_radius, state ^ i)) {
                    hits = hits + 1u;
                }
            }
            open = 1.0 - float(hits) / float(ao_samples);
        }
        accum_open += open;

        // Phase 4: GGX importance-sampled rough reflections with on-screen
        // textured raster hit colour (falling back to harvested baked albedo
        // off-screen) and raster sky reflection on miss + Fresnel.
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
            Hit rhit = closest_hit(ray_origin, reflect_dir, bias, params.lens.y);
            if (rhit.index != 0xffffffffu) {
                float3 hit_pos = ray_origin + reflect_dir * rhit.t;
                float4 screen_col = sample_screen_radiance(hit_pos, rhit.t);
                float3 lit = screen_col.rgb;
                if (screen_col.a < 0.5) {
                    Material rmat = material_of(rhit.index);
                    float r_ndl = max(dot(rhit.normal, params.sun_dir.xyz), 0.0);
                    float r_shadow = 1.0;
                    if (bits_shadows && r_ndl > 0.02) {
                        float3 rhit_pos = hit_pos + rhit.normal * bias;
                        if (occluded(rhit_pos, params.sun_dir.xyz, bias * 0.5, params.lens.y, state ^ 0x517cc1b7u)) {
                            r_shadow = 0.0;
                        }
                    }
                    lit = rmat.albedo.rgb * (params.ambient.xyz * params.tuning.z +
                        params.sun_colour.xyz * (r_ndl * r_shadow));
                }
                accum_reflection += lit * fresnel * (1.0 - fog_t);
                refl_dist = rhit.t;
            } else {
                float3 sky_col = sample_sky_reflection(reflect_dir);
                accum_reflection += sky_col * (fresnel * 0.65 * (1.0 - fog_t));
            }
        }
        accum_refl_dist += refl_dist;

        // Phase 5: 3-tap stratified cosine-weighted indirect GI bounce using
        // estimated receiver albedo (not already-lit raster colour) and the
        // screen-space radiance cache for textured colour bleeding (falling
        // back to harvested baked albedo off-screen).
        if (bits_gi && params.frame.z >= 1.0 && params.tuning.w > 0.0) {
            uint gi_taps = 3u;
            float3 est_direct = params.sun_colour.xyz * n_dot_l + params.ambient.xyz * params.tuning.z;
            float3 receiver_albedo = clamp(
                raster_rgb / max(est_direct, float3(0.25, 0.25, 0.25)),
                float3(0.05, 0.05, 0.05),
                float3(0.95, 0.95, 0.95));
            float3 gi_sum = float3(0.0, 0.0, 0.0);
            for (uint g = 0u; g < gi_taps; g = g + 1u) {
                float u1 = (float(g) + rand01(state)) / float(gi_taps);
                float phi = float(g) * 2.39996323 + rot_angle + 1.0471975 + float(s) * 0.7548776;
                float r = sqrt(u1);
                float s_phi, c_phi;
                sincos(phi, s_phi, c_phi);
                float z = sqrt(max(0.0, 1.0 - u1));
                float3 d = t_axis * (r * c_phi) + b_axis * (r * s_phi) + normal * z;
                Hit bhit = closest_hit(ray_origin, d, bias, min(params.lens.y, 48.0));
                if (bhit.index != 0xffffffffu) {
                    float b_ndl = max(dot(bhit.normal, params.sun_dir.xyz), 0.0);
                    if (b_ndl > 0.01) {
                        float3 bhit_world = ray_origin + d * bhit.t;
                        float b_shadow = 1.0;
                        if (bits_shadows) {
                            float3 bhit_pos = bhit_world + bhit.normal * bias;
                            if (occluded(bhit_pos, params.sun_dir.xyz, bias * 0.5, params.lens.y, state ^ (g * 73u + 11u))) {
                                b_shadow = 0.0;
                            }
                        }
                        if (b_shadow > 0.0) {
                            float4 b_screen = sample_screen_radiance(bhit_world, bhit.t);
                            float3 incoming = b_screen.rgb;
                            if (b_screen.a < 0.5) {
                                Material bmat = material_of(bhit.index);
                                incoming = bmat.albedo.rgb * params.sun_colour.xyz * (b_ndl * b_shadow);
                            }
                            gi_sum += receiver_albedo * incoming * (params.tuning.w * 0.55 * (1.0 - fog_t));
                        }
                    }
                }
            }
            accum_gi += gi_sum * (1.0 / float(gi_taps));
        }
    }

    // Phase 1: Point-light shadow visibility modulation ONLY (never adds light
    // in layered mode because raster `world_models_gpu` already applies `SceneLights`).
    // Opt-in via `point_light_meta.y > 0.5` (off by default); also evaluated in debug_mode == 6.
    uint debug_mode = uint(round(params.counts.w));
    bool pl_shadows_enabled = params.point_light_meta.y > 0.5;
    float3 point_lighting = float3(0.0, 0.0, 0.0);
    float pl_w_total = 0.0;
    float pl_w_lit = 0.0;
    if ((pl_shadows_enabled || !layered || debug_mode == 6u) && bits_lighting) {
        uint pl_count = min(uint(round(params.point_light_meta.x)), 16u);
        for (uint li = 0u; li < pl_count; li = li + 1u) {
            float4 pl_pr = params.point_lights_pos_rad[li];
            float4 pl_ci = params.point_lights_col_int[li];
            float3 to_l = pl_pr.xyz - world_pos;
            float pl_dist = length(to_l);
            if (pl_dist > 1e-3 && pl_dist < pl_pr.w) {
                float3 l_dir = to_l / pl_dist;
                float pl_ndl = max(dot(normal, l_dir), 0.0);
                if (pl_ndl > 0.001) {
                    float atten = point_light_falloff(pl_dist, pl_pr.w);
                    if (atten > 1e-5) {
                        float pl_vis = 1.0;
                        if (bits_shadows && occluded(ray_origin, l_dir, bias * 0.5, max(pl_dist - bias, bias), state ^ (li * 131u + 17u))) {
                            pl_vis = 0.0;
                        }
                        float w = pl_ci.w * atten * pl_ndl;
                        pl_w_total += w;
                        pl_w_lit += w * pl_vis;
                        if (!layered) {
                            point_lighting += raster_rgb * pl_ci.rgb * (w * pl_vis);
                        }
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
        float sun_e = max(dot(params.sun_colour.xyz, float3(0.2126, 0.7152, 0.0722)), 0.0) * n_dot_l;
        float amb_e = max(dot(params.ambient.xyz, float3(0.2126, 0.7152, 0.0722)) * params.tuning.z, 0.05);
        float sun_f = saturate(sun_e / max(sun_e + amb_e, 1e-4));
        float amb_f = 1.0 - sun_f;
        float v_sun = bits_shadows ? avg_shadow : 1.0;
        float v_ao = bits_ao ? avg_open : 1.0;
        float separated_lf = (amb_f * v_ao + sun_f * v_sun) * point_shadow_mod;
        out_lf = lerp(separated_lf, 1.0, fog_t);
        out_rgb = avg_gi;
    } else {
        // Opt-in replace_raster mode: keep 1:1 raster world textures/albedo
        // instead of low-poly proxy boxes, relit purely by RT sun + RT ambient + point lights.
        float3 direct = bits_lighting ? (params.sun_colour.xyz * (n_dot_l * avg_shadow)) : float3(0.0, 0.0, 0.0);
        float3 ambient = params.ambient.xyz * (avg_open * params.tuning.z);
        float3 relit = raster_rgb * (direct + ambient) + avg_gi + point_lighting;
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
        out_rgb = float3(point_vis, point_vis, point_vis);
        out_lf = 1.0;
        store_refl = float3(0.0, 0.0, 0.0);
    }

    float spec_f0 = 0.04 + 0.16 * (1.0 - roughness);
    out_colour[id.xy] = float4(out_rgb, out_lf);
    out_guide[id.xy] = float4(avg_open, dist / params.lens.y, avg_shadow, roughness);
    out_reflection[id.xy] = float4(store_refl, avg_refl_dist / params.lens.y);
    out_normals[id.xy] = float4(normal, roughness);
    out_albedo[id.xy] = float4(raster_rgb, spec_f0);
}
