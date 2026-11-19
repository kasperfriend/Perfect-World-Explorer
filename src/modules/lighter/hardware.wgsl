@group(0) @binding(5) var acceleration: acceleration_structure;

fn triangle_hit(h: RayIntersection) -> Hit {
    let m = (h.instance_custom_data & DYNAMIC) != 0u;
    let inst = inst_addr(h.instance_custom_data);
    return Hit(h.t,h.instance_custom_data,h.primitive_index+word(m,inst+7u),h.barycentrics);
}

// Homogeneous, unconditional BLASes can commit in hardware. Nonopaque
// candidates (including cutouts) are confirmed only after coverage testing.
// Do not confirm ordered blend hits: committing one would hide its peers at t.
fn intersect(ray: Ray, q: Query) -> Hit {
    var query: ray_query;
    let flags = select(0u,RAY_FLAG_TERMINATE_ON_FIRST_HIT,q.mask==BLOCKER_MASK);
    rayQueryInitialize(&query, acceleration, RayDesc(flags, q.mask, ray.low, ray.high, ray.origin, ray.dir));
    var best = none(ray);
    while rayQueryProceed(&query) {
        let h = rayQueryGetCandidateIntersection(&query);
        if h.kind==RAY_QUERY_INTERSECTION_TRIANGLE {
            let hit = triangle_hit(h);
            if (!q.ordered || closer(hit,best,q)) && accepts(hit,ray,q) {
                if q.ordered { best = hit; }
                else { rayQueryConfirmIntersection(&query); }
            }
        }
    }
    if q.ordered { return best; }
    let h = rayQueryGetCommittedIntersection(&query);
    if h.kind==RAY_QUERY_INTERSECTION_TRIANGLE {
        // A blocker consumer only needs the boolean; no closest-hit stage or
        // instance/material fetch after an unconditional hardware commit.
        if q.mask==BLOCKER_MASK { return Hit(h.t,h.instance_custom_data & DYNAMIC,0u,vec2<f32>(0.)); }
        return triangle_hit(h);
    }
    return best;
}

// The full-ray entry point never uses this descriptor. No dummy image or
// graphics pipeline is needed when raster visibility is disabled.
@group(0) @binding(6) var primary_keys: texture_2d<u32>;

@compute @workgroup_size(8,8)
fn render_hybrid(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy>=frame.extent.xy) { return; }
    let key = textureLoad(primary_keys,vec2<i32>(id.xy),0);
    var bary = unpack2x16unorm(key.w);
    bary /= max(1.,bary.x+bary.y);
    render_pixel(id.xy,Hit(bitcast<f32>(key.z),key.x,key.y,bary),true);
}
