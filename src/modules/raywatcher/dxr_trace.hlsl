// DXR Tier 1.1 inline ray-tracing compute shader (`cs_6_5`).
//
// Runs directly on hardware RT cores (NVIDIA RTX, AMD RDNA2+, Intel Arc) via
// `RayQuery`, matching `RAY_SHADER` in `gpu.rs` stage-for-stage so hardware and
// software paths agree on lighting, shadows, reflections, AO, and GI.

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
    float4 effects; // x: roughness_cutoff, y: effect_bits, z: bias, w: unused
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
    float phi = 2.0 * PI * u2;
    float x = r * cos(phi);
    float y = r * sin(phi);
    float z = sqrt(max(0.0, 1.0 - u1));
    float3 up = abs(n.y) < 0.999 ? float3(0.0, 1.0, 0.0) : float3(1.0, 0.0, 0.0);
    float3 t = normalize(cross(up, n));
    float3 b = cross(n, t);
    return normalize(t * x + b * y + n * z);
}

float3 environment() {
    float3 sun = max(params.sun_colour.xyz, float3(0.0, 0.0, 0.0));
    float3 amb = max(params.ambient.xyz, float3(0.0, 0.0, 0.0));
    return amb * 0.65 + sun * 0.15;
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

    RayQuery<RAY_FLAG_NONE> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_NONE, 0xffu, ray);
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

    RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH> q;
    q.TraceRayInline(scene_tlas, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH, 0xffu, ray);
    while (q.Proceed()) {
    }
    return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT;
}

Material material_of(uint tri_index) {
    uint mat_count = max(uint(params.counts.z), 1u);
    uint mat_idx = min(uint(round(tris[tri_index].b.w)), mat_count - 1u);
    return materials[mat_idx];
}

struct Surface {
    float3 colour;
    float3 reflection;
    float visibility;
    float depth;
    float reflection_distance;
    float3 normal;
    float roughness;
    float3 albedo;
};

Surface shade(float3 origin, float3 dir, inout uint state) {
    Surface surface;
    surface.colour = float3(0.0, 0.0, 0.0);
    surface.reflection = float3(0.0, 0.0, 0.0);
    surface.visibility = 1.0;
    surface.reflection_distance = params.lens.y;
    surface.depth = params.lens.y;
    surface.normal = float3(0.0, 1.0, 0.0);
    surface.roughness = 1.0;
    surface.albedo = float3(0.0, 0.0, 0.0);

    Hit hit = closest_hit(origin, dir, params.effects.z, params.lens.y);
    if (hit.index == 0xffffffffu) {
        float fog_start = params.fog.x;
        float fog_end = params.fog.y;
        if (fog_end > fog_start && fog_start >= 0.0) {
            surface.colour = params.fog_colour.xyz;
        } else {
            surface.colour = environment();
        }
        surface.depth = params.lens.y;
        return surface;
    }

    float3 hit_point = origin + dir * hit.t;
    Material mat = material_of(hit.index);
    float3 albedo = mat.albedo.rgb;
    float roughness = mat.surface.x;
    surface.depth = hit.t;
    surface.normal = hit.normal;
    surface.roughness = roughness;
    surface.albedo = albedo;

    uint effects = uint(round(params.effects.y));
    bool bits_shadows = (effects & 1u) != 0u;
    bool bits_lighting = (effects & 2u) != 0u;
    bool bits_reflections = (effects & 4u) != 0u;
    bool bits_ao = (effects & 8u) != 0u;
    bool bits_gi = (effects & 16u) != 0u;
    float bias = params.effects.z;

    float open = 1.0;
    if (bits_ao) {
        uint hits = 0u;
        const uint ao_samples = 8u;
        for (uint i = 0u; i < ao_samples; i = i + 1u) {
            float3 d = cosine_hemisphere(state, hit.normal);
            if (occluded(hit_point + hit.normal * bias, d, bias, params.fog.z)) {
                hits = hits + 1u;
            }
        }
        open = 1.0 - float(hits) / float(ao_samples);
    }
    surface.visibility = open;

    float n_dot_l = max(dot(hit.normal, params.sun_dir.xyz), 0.0);
    float3 direct = float3(0.0, 0.0, 0.0);
    if (bits_lighting) {
        float shadow = 1.0;
        if (bits_shadows) {
            if (occluded(hit_point + hit.normal * bias, params.sun_dir.xyz, bias, params.lens.y)) {
                shadow = 0.0;
            }
        }
        direct = params.sun_colour.xyz * (n_dot_l * params.lens.w * shadow);
    }
    float3 ambient = params.ambient.xyz * (open * params.tuning.z);
    float3 diffuse = bits_lighting ? (direct + ambient * albedo) : (ambient * albedo);

    if (bits_reflections && roughness <= params.effects.x) {
        float3 reflect_dir = normalize(dir - hit.normal * (2.0 * dot(dir, hit.normal)));
        Hit rhit = closest_hit(hit_point + hit.normal * bias, reflect_dir, bias, params.lens.y);
        if (rhit.index != 0xffffffffu) {
            Material rmat = material_of(rhit.index);
            float3 lit = rmat.albedo.rgb * (params.ambient.xyz * params.tuning.z +
                params.sun_colour.xyz * (max(dot(rhit.normal, params.sun_dir.xyz), 0.0) * params.lens.w));
            float f0 = 0.04 + 0.06 * (1.0 - roughness);
            float cos_theta = clamp(dot(-dir, hit.normal), 0.0, 1.0);
            float fresnel = f0 + (1.0 - f0) * pow(1.0 - cos_theta, 5.0);
            surface.reflection = lit * fresnel;
            surface.reflection_distance = rhit.t;
        } else {
            surface.reflection = environment() * 0.08;
            surface.reflection_distance = params.lens.y;
        }
    }

    if (bits_gi && params.frame.z >= 1.0 && params.tuning.w > 0.0) {
        float3 bounce = float3(0.0, 0.0, 0.0);
        float3 throughput = albedo;
        float3 source = hit_point;
        float3 normal = hit.normal;
        uint bounces = uint(params.frame.z);
        for (uint b = 0u; b < bounces; b = b + 1u) {
            float3 d = cosine_hemisphere(state, normal);
            Hit bhit = closest_hit(source + normal * bias, d, bias, params.lens.y);
            if (bhit.index != 0xffffffffu) {
                float3 bpoint = source + d * bhit.t;
                Material bmat = material_of(bhit.index);
                float3 incoming = bmat.albedo.rgb * (params.ambient.xyz * params.tuning.z +
                    params.sun_colour.xyz * (max(dot(bhit.normal, params.sun_dir.xyz), 0.0) * params.lens.w));
                bounce = bounce + throughput * incoming;
                throughput = throughput * bmat.albedo.rgb;
                source = bpoint;
                normal = bhit.normal;
            } else {
                bounce = bounce + throughput * environment();
                break;
            }
        }
        diffuse = diffuse + bounce * params.tuning.w;
    }

    surface.colour = diffuse;
    return surface;
}

[numthreads(8, 8, 1)]
void trace_pass(uint3 id : SV_DispatchThreadID) {
    uint2 extent = uint2(uint(params.extents.x), uint(params.extents.y));
    if (id.x >= extent.x || id.y >= extent.y) {
        return;
    }
    float2 grid = params.extents.xy;
    uint samples = max(uint(params.frame.y), 1u);
    float3 accum = float3(0.0, 0.0, 0.0);
    float3 accum_reflection = float3(0.0, 0.0, 0.0);
    float accum_visibility = 0.0;
    float accum_depth = 0.0;
    float accum_reflection_distance = 0.0;
    float3 accum_normal = float3(0.0, 0.0, 0.0);
    float accum_roughness = 0.0;
    float3 accum_albedo = float3(0.0, 0.0, 0.0);
    uint state = id.x * 0x9e3779b9u ^ id.y * 0x85ebca6bu ^ uint(params.frame.x) * 0xc2b2ae35u;

    for (uint s = 0u; s < samples; s = s + 1u) {
        float2 offset = float2(0.0, 0.0);
        if (samples > 1u) {
            float n = float(samples);
            offset = float2((float(s) + 0.5) / n - 0.5, (float(s % 2u) + 0.5) / 2.0) * 0.4;
        }
        float2 pixel = float2(float(id.x) + 0.5, float(id.y) + 0.5) + offset + params.tuning.xy;
        float2 ndc = float2(pixel.x / grid.x * 2.0 - 1.0, 1.0 - pixel.y / grid.y * 2.0);
        float4 near_p = mul(params.inv_vp, float4(ndc, 1.0, 1.0));
        float4 far_p = mul(params.inv_vp, float4(ndc, 0.0, 1.0));
        float3 dir = normalize(far_p.xyz / far_p.w - near_p.xyz / near_p.w);
        Surface surface = shade(params.eye.xyz, dir, state);
        accum += surface.colour;
        accum_reflection += surface.reflection;
        accum_visibility += surface.visibility;
        accum_depth += surface.depth;
        accum_reflection_distance += surface.reflection_distance;
        accum_normal += surface.normal;
        accum_roughness += surface.roughness;
        accum_albedo += surface.albedo;
    }

    float inv = 1.0 / float(samples);
    float3 avg_n = accum_normal * inv;
    float n_len = length(avg_n);
    avg_n = n_len > 1e-4 ? (avg_n / n_len) : float3(0.0, 1.0, 0.0);
    float avg_rough = clamp(accum_roughness * inv, 0.02, 1.0);
    float3 avg_albedo = accum_albedo * inv;
    float spec_f0 = 0.04 + 0.06 * (1.0 - avg_rough);

    out_colour[id.xy] = float4(accum * inv, 1.0);
    out_guide[id.xy] = float4(accum_visibility * inv, (accum_depth * inv) / params.lens.y, 0.0, 0.0);
    out_reflection[id.xy] = float4(accum_reflection * inv, (accum_reflection_distance * inv) / params.lens.y);
    out_normals[id.xy] = float4(avg_n, avg_rough);
    out_albedo[id.xy] = float4(avg_albedo, spec_f0);
}
