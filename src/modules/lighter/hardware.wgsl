@group(0) @binding(5) var acceleration: acceleration_structure;

// Vulkan VK_KHR_ray_query traversal over a real TLAS backed by triangle BLASes.
// Force opaque to commit the nearest *geometric* hit; shared code then tests
// the authored alpha reference and visibility and reissues only rejected rays.
fn closest(ray: Ray) -> Hit {
    var query: ray_query;
    rayQueryInitialize(&query, acceleration, RayDesc(1u, 255u, ray.low, ray.high, ray.origin, ray.dir));
    while rayQueryProceed(&query) {}
    let h = rayQueryGetCommittedIntersection(&query);
    if h.kind != RAY_QUERY_INTERSECTION_NONE {
        let m = (h.instance_custom_index & DYNAMIC) != 0u;
        let inst = inst_addr(h.instance_custom_index);
        return Hit(h.t,h.instance_custom_index,h.primitive_index+word(m,inst+7u),h.barycentrics);
    }
    return Hit(ray.high,NIL,0u,vec2<f32>(0.));
}
