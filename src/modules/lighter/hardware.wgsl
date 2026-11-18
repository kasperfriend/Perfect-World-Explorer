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
    rayQueryInitialize(&query, acceleration, RayDesc(0u, q.mask, ray.low, ray.high, ray.origin, ray.dir));
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
    if h.kind==RAY_QUERY_INTERSECTION_TRIANGLE { return triangle_hit(h); }
    return best;
}
