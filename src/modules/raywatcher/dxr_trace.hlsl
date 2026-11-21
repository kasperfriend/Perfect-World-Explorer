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
    float4 counts;  // x: tri_count, y: node_count, z: mat_count, w: unused
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

    RayQuery<RAY_FLAG_FORCE_OPAQUE> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_FORCE_OPAQUE, 0xffu, ray);
    while (q.Proceed()) {
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

bool occluded(float3 origin, float3 dir, float t_min, float t_max) {
    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = t_min;
    ray.TMax = t_max;

    RayQuery<RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER, 0xffu, ray);
    while (q.Proceed()) {
    }
    return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT;
}

Material material_of(uint tri_index) {
    uint mat_count = max(uint(params.counts.z), 1u);
    uint mat_idx = min(uint(round(tris[tri_index].b.w)), mat_count - 1u);
    return materials[mat_idx];
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

    // Short-range contact AO radius (4..6 world units) for crisp crevice
    // darkening without tracing expensive 100+ unit occlusion rays.
    float ao_radius = clamp(params.fog.z, 2.0, 6.0);
    uint ao_samples = samples > 1u ? 1u : 2u;

    float3 up = abs(normal.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 t_axis = normalize(cross(up, normal));
    float3 b_axis = cross(normal, t_axis);

    for (uint s = 0u; s < samples; s = s + 1u) {
        // 1. Real RT sun shadows (soft penumbra within sun angular disc).
        float shadow = 1.0;
        if (bits_shadows && n_dot_l > 0.02) {
            float3 shadow_dir = params.lens.z > 1e-4
                ? cone_direction(state, params.sun_dir.xyz, params.lens.z)
                : params.sun_dir.xyz;
            if (occluded(ray_origin, shadow_dir, bias * 0.5, params.lens.y)) {
                shadow = 0.0;
            }
        }
        accum_shadow += shadow;

        // 2. Short-range contact ambient occlusion.
        float open = 1.0;
        if (bits_ao) {
            uint hits = 0u;
            for (uint i = 0u; i < ao_samples; i = i + 1u) {
                float u1 = rand01(state);
                float u2 = rand01(state);
                float r = sqrt(u1);
                float s_phi, c_phi;
                sincos(2.0 * PI * u2, s_phi, c_phi);
                float z = sqrt(max(0.0, 1.0 - u1));
                float3 d = t_axis * (r * c_phi) + b_axis * (r * s_phi) + normal * z;
                if (occluded(ray_origin, d, bias * 0.5, ao_radius)) {
                    hits = hits + 1u;
                }
            }
            open = 1.0 - float(hits) / float(ao_samples);
        }
        accum_open += open;

        // 3. Crisp RT reflections on water / glossy surfaces (geometry hits
        //    only — sky misses emit 0 so raster sky reflections stay clean).
        float refl_dist = params.lens.y;
        if (bits_reflections && roughness <= params.effects.x) {
            float3 reflect_dir = reflect(view_dir, normal);
            Hit rhit = closest_hit(ray_origin, reflect_dir, bias, params.lens.y);
            if (rhit.index != 0xffffffffu) {
                Material rmat = material_of(rhit.index);
                float r_ndl = max(dot(rhit.normal, params.sun_dir.xyz), 0.0);
                float3 lit = rmat.albedo.rgb * (params.ambient.xyz * params.tuning.z +
                    params.sun_colour.xyz * r_ndl);
                float f0 = 0.04 + 0.16 * (1.0 - roughness);
                float m = 1.0 - saturate(dot(-view_dir, normal));
                float m2 = m * m;
                float fresnel = f0 + (1.0 - f0) * (m2 * m2 * m);
                accum_reflection += lit * fresnel * (1.0 - fog_t);
                refl_dist = rhit.t;
            }
        }
        accum_refl_dist += refl_dist;

        // 4. Local colored indirect GI bounce (geometry hits ONLY — sky escape
        //    emits 0 so it never adds a white/sky veil over the raster world).
        if (bits_gi && params.frame.z >= 1.0 && params.tuning.w > 0.0) {
            float3 d = cosine_hemisphere(state, normal);
            Hit bhit = closest_hit(ray_origin, d, bias, min(params.lens.y, 48.0));
            if (bhit.index != 0xffffffffu) {
                Material bmat = material_of(bhit.index);
                float b_ndl = max(dot(bhit.normal, params.sun_dir.xyz), 0.0);
                if (b_ndl > 0.01) {
                    float3 incoming = bmat.albedo.rgb * params.sun_colour.xyz * b_ndl;
                    accum_gi += raster_rgb * incoming * (params.tuning.w * 0.30 * (1.0 - fog_t));
                }
            }
        }
    }

    float inv = 1.0 / float(samples);
    float avg_shadow = accum_shadow * inv;
    float avg_open = accum_open * inv;
    float3 avg_refl = accum_reflection * inv;
    float avg_refl_dist = accum_refl_dist * inv;
    float3 avg_gi = accum_gi * inv;

    float3 out_rgb;
    float out_lf;
    if (layered) {
        // Modulate the 1:1 raster world by RT sun shadow and contact AO
        // (attenuated smoothly into raster fog), plus colored GI bounce.
        float shadow_mod = (bits_shadows && n_dot_l > 0.02) ? lerp(0.30, 1.0, avg_shadow) : 1.0;
        float ao_mod = bits_ao ? lerp(0.58, 1.0, avg_open) : 1.0;
        out_lf = lerp(shadow_mod * ao_mod, 1.0, fog_t);
        out_rgb = avg_gi;
    } else {
        // Opt-in replace_raster mode: keep 1:1 raster world textures/albedo
        // instead of low-poly proxy boxes, relit purely by RT sun + RT ambient.
        float3 direct = bits_lighting ? (params.sun_colour.xyz * (n_dot_l * avg_shadow)) : float3(0.0, 0.0, 0.0);
        float3 ambient = params.ambient.xyz * (avg_open * params.tuning.z);
        float3 relit = raster_rgb * (direct + ambient) + avg_gi;
        out_rgb = lerp(relit, params.fog_colour.xyz, fog_t);
        out_lf = 1.0;
    }

    float spec_f0 = 0.04 + 0.16 * (1.0 - roughness);
    out_colour[id.xy] = float4(out_rgb, out_lf);
    out_guide[id.xy] = float4(avg_open, dist / params.lens.y, 0.0, 0.0);
    out_reflection[id.xy] = float4(avg_refl, avg_refl_dist / params.lens.y);
    out_normals[id.xy] = float4(normal, roughness);
    out_albedo[id.xy] = float4(raster_rgb, spec_f0);
}
