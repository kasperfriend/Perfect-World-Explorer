// Imaginator hardware tracer: DXR 1.1 inline RayQuery, compute shader.
//
// Compiled to DXIL (shader model 6.5) by DXC:
//
//   tools/imaginator-shaders.sh
//
// It must land beside the module as `imaginator/dxr_trace.cso`. The module
// reports its absence on its own page and keeps the compute tracer running, so a
// missing blob degrades the *tracer*, never the viewer.
//
// Bindings mirror `src/imaginator/gpu.rs`'s compute tracer:
//   b0 cbuffer Params
//   t0 TLAS (raytracing acceleration structure)
//   t1 per-triangle record (xyz normal, w material index)
//   t2 material table (rgb albedo, a specular)
//   u0 output texture (rgba32float)
//
// Output contract, identical to the compute tracer:
//   r = reflection radiance, g = indirect radiance, b = sun visibility,
//   a = view distance (negative when the primary ray escaped).

struct Params
{
    float4 eye;
    float4 forward;      // .w = tan(fov_y / 2)
    float4 right;
    float4 up;
    float4 sun;
    float4 sun_color;
    float4 sky_up;
    float4 sky_horizon;
    uint4  config;       // x bounces, y sample, z samples, w term mask
    float4 screen;       // x aspect, y height, z scene limit, w unused
};

ConstantBuffer<Params> p : register(b0);
RaytracingAccelerationStructure scene : register(t0);
Buffer<float4> tri_normals : register(t1);   // xyz normal, w material index
Buffer<float4> materials : register(t2);     // rgb albedo, a specular
RWTexture2D<float4> out_tex : register(u0);

static const uint TERM_REFLECTIONS = 1u;
static const uint TERM_LIGHTING = 2u;
static const uint TERM_SHADOWS = 4u;

// --- RNG (PCG, the same shape as the WGSL) --------------------------------
uint pcg(uint v)
{
    uint state = v * 747796405u + 2891336453u;
    uint word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

float rnd(inout uint seed)
{
    seed = pcg(seed);
    return float(seed >> 8u) / 16777216.0;
}

float sky(float up)
{
    return lerp(p.sky_horizon.rgb, p.sky_up.rgb, pow(saturate(up), 0.7));
}

// One cone-jittered shadow ray, traced by the hardware traversal units.
float sun_visibility(float3 point, float3 normal, inout uint seed)
{
    float3 sun = normalize(p.sun.xyz);
    float3 helper = abs(sun.x) > 0.7 ? float3(0, 1, 0) : float3(1, 0, 0);
    float3 t = normalize(cross(sun, helper));
    float3 b = cross(sun, t);
    float angle = 6.2831853 * rnd(seed);
    float radius = 0.0105 * sqrt(rnd(seed));
    float3 dir = normalize(sun + (t * cos(angle) + b * sin(angle)) * radius);
    RayDesc ray;
    ray.Origin = point + normal * 0.02 + sun * 0.02;
    ray.Direction = dir;
    ray.TMin = 0.0;
    ray.TMax = p.screen.z;
    RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH> q;
    q.TraceRayInline(scene, RAY_FLAG_NONE, 0xFF, ray);
    q.Proceed();
    return q.CommittedStatus() == COMMITTED_NOTHING ? 1.0 : 0.0;
}

struct Hit
{
    float t;
    float3 normal;
    uint triangle;
    bool hit;
};

// The traversal reports which primitive it committed. Normals and the material
// index come from the same buffers the compute tracer walks, so both tracers
// shade identical surfaces identically.
Hit closest(float3 origin, float3 dir)
{
    RayDesc ray;
    ray.Origin = origin;
    ray.Direction = dir;
    ray.TMin = 0.0;
    ray.TMax = p.screen.z;
    RayQuery<RAY_FLAG_NONE> q;
    q.TraceRayInline(scene, RAY_FLAG_NONE, 0xFF, ray);
    q.Proceed();

    Hit h;
    h.hit = q.CommittedStatus() == COMMITTED_TRIANGLE_HIT;
    h.t = h.hit ? q.CommittedRayT() : p.screen.z;
    h.triangle = h.hit ? q.CommittedPrimitiveIndex() : 0u;
    h.normal = h.hit ? tri_normals[h.triangle].xyz : float3(0, 1, 0);
    return h;
}

float4 material_of(uint triangle)
{
    uint index = uint(tri_normals[triangle].w);
    return materials[index];
}

// One surface's own light, with no further scattering.
//
// This was recursive — a mirror hit called back into this function to shade what
// it found — and DXC rejects any recursion outright, so the shader could never
// have been compiled at all. Reflections are instead walked as a loop below,
// which is what the CPU tracer does too: a reflection ray is followed for
// `bounces` steps, losing energy at each surface, and the loop stops early once
// the throughput is negligible. Same picture, no recursion.
float3 surface_light(float3 point, float3 normal, uint terms, inout uint seed)
{
    float3 sun = normalize(p.sun.xyz);
    float lit = max(dot(normal, sun), 0.0);
    float vis = 1.0;
    if ((terms & TERM_SHADOWS) != 0u && lit > 0.0)
    {
        vis = sun_visibility(point, normal, seed);
    }
    if ((terms & TERM_LIGHTING) == 0u) { return float3(0, 0, 0); }
    return p.sun_color.rgb * (lit * vis) + sky(normal.y);
}

[numthreads(8, 8, 1)]
void cs(uint3 id : SV_DispatchThreadID)
{
    uint width, height;
    out_tex.GetDimensions(width, height);
    if (id.x >= width || id.y >= height) { return; }

    uint terms = p.config.w;
    float tan_half = p.forward.w;
    float aspect = p.screen.x;
    uint samples = max(p.config.z, 1u);
    uint seed = pcg(id.x * 1973u + id.y * 9277u + p.config.y * 26699u);

    float reflection = 0.0;
    float indirect = 0.0;
    float visibility = 0.0;
    float distance = 0.0;
    float hits = 0.0;

    for (uint s = 0; s < samples; ++s)
    {
        float jx = rnd(seed);
        float jy = rnd(seed);
        float sx = (2.0 * ((id.x + jx) / float(width)) - 1.0) * tan_half * aspect;
        float sy = (1.0 - 2.0 * ((id.y + jy) / float(height))) * tan_half;
        float3 rd = normalize(p.forward.xyz + p.right.xyz * sx + p.up.xyz * sy);
        float3 ro = p.eye.xyz;
        Hit hit = closest(ro, rd);
        if (!hit.hit) { continue; }
        hits += 1.0;
        distance += hit.t;
        float3 point = ro + rd * hit.t;
        float3 normal = normalize(hit.normal);
        if (dot(normal, rd) > 0.0) { normal = -normal; }
        float3 sun = normalize(p.sun.xyz);
        float lit = max(dot(normal, sun), 0.0);
        float vis = 1.0;
        if ((terms & TERM_SHADOWS) != 0u && lit > 0.0)
        {
            vis = sun_visibility(point, normal, seed);
        }
        visibility += vis;
        float4 mat = material_of(hit.triangle);
        if ((terms & TERM_LIGHTING) != 0u)
        {
            // Cosine-weighted bounce walk; every step is one hardware ray.
            float3 weight = mat.rgb;
            float3 origin = point + normal * 0.02;
            float3 n = normal;
            for (uint bounce = 0; bounce < p.config.x; ++bounce)
            {
                float u1 = rnd(seed);
                float u2 = rnd(seed);
                float r = sqrt(u1);
                float phi = 6.2831853 * u2;
                float3 helper = abs(n.x) > 0.7 ? float3(0, 1, 0) : float3(1, 0, 0);
                float3 t = normalize(cross(n, helper));
                float3 b = cross(n, t);
                float3 dir = normalize(t * (r * cos(phi)) + b * (r * sin(phi)) +
                                       n * sqrt(max(1.0 - u1, 0.0)));
                Hit h = closest(origin, dir);
                if (!h.hit)
                {
                    indirect += dot(weight * sky(dir.y), float3(0.3333, 0.3333, 0.3333));
                    break;
                }
                float3 bp = origin + dir * h.t;
                float3 bn = normalize(h.normal);
                if (dot(bn, dir) > 0.0) { bn = -bn; }
                float bl = max(dot(bn, sun), 0.0);
                float bv = 1.0;
                if ((terms & TERM_SHADOWS) != 0u && bl > 0.0)
                {
                    bv = sun_visibility(bp, bn, seed);
                }
                float3 incoming = p.sun_color.rgb * (bl * bv) + sky(bn.y);
                indirect += dot(weight * incoming, float3(0.3333, 0.3333, 0.3333));
                weight *= material_of(h.triangle).rgb;
                if (max(weight.r, max(weight.g, weight.b)) < 0.02) { break; }
                origin = bp + bn * 0.02;
                n = bn;
            }
        }
        if ((terms & TERM_REFLECTIONS) != 0u)
        {
            float cos_i = dot(normal, -rd);
            if (cos_i > 0.0)
            {
                // A mirror chain, walked rather than recursed: each step loses
                // the surface's specular and gathers its own light.
                float3 mirror = normalize(rd + normal * (2.0 * cos_i));
                float3 origin = point + normal * 0.02;
                float3 through = float3(1, 1, 1);
                float3 gathered = float3(0, 0, 0);
                uint max_steps = max(p.config.x, 1u);
                for (uint step = 0; step < max_steps; ++step)
                {
                    Hit h = closest(origin, mirror);
                    if (!h.hit)
                    {
                        gathered += through * sky(mirror.y);
                        break;
                    }
                    float3 hp = origin + mirror * h.t;
                    float3 hn = normalize(h.normal);
                    if (dot(hn, mirror) > 0.0) { hn = -hn; }
                    float4 hm = material_of(h.triangle);
                    gathered += through * (hm.rgb * surface_light(hp, hn, terms, seed));
                    through *= hm.a;
                    if (max(through.r, max(through.g, through.b)) < 0.02) { break; }
                    // Reflect again about the new surface.
                    float3 in_dir = mirror;
                    float c = dot(hn, -in_dir);
                    if (c <= 0.0) { break; }
                    mirror = normalize(in_dir + hn * (2.0 * c));
                    origin = hp + hn * 0.02;
                }
                reflection += dot(gathered, float3(0.3333, 0.3333, 0.3333));
            }
        }
    }

    float inv = 1.0 / float(samples);
    out_tex[int2(id.xy)] = float4(
        reflection * inv,
        indirect * inv,
        hits > 0.0 ? visibility * inv : 0.0,
        hits > 0.0 ? distance * inv : -1.0);
}
