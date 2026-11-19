// Compact visibility G-buffer. Store the same instance/primitive address
// that a ray query would return. The shading pass reconstructs position,
// material and normal (including mapped normals) using this address and UVs.
struct VisibilityVertex {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) bary: vec2<f32>,
    @location(2) @interpolate(flat) key: vec2<u32>,
}

@vertex fn visibility_vertex(@builtin(vertex_index) index: u32,
                              @builtin(instance_index) instance: u32) -> VisibilityVertex {
    let dynamic = (instance & DYNAMIC)!=0u;
    let inst = inst_addr(instance);
    let primitive = index/3u;
    let corner = index%3u;
    let v = vertex_addresses(dynamic,inst,primitive)[corner];
    let p = floats(dynamic,inst)+instance_vector(dynamic,inst,floats(dynamic,v));
    let bary = vec2<f32>(select(0.,1.,corner==1u),select(0.,1.,corner==2u));
    return VisibilityVertex(frame.vp*vec4<f32>(p,1.),p,bary,vec2<u32>(instance,primitive));
}

@fragment fn visibility_fragment(in: VisibilityVertex) -> @location(0) vec4<u32> {
    var seed = 0u;
    let ray = camera_ray(vec2<u32>(in.clip.xy),0u,&seed);
    let t = dot(in.world_pos-ray.origin,ray.dir);
    let hit = Hit(t,in.key.x,in.key.y,in.bary);
    // Same acceptance rules as the full-ray path: terrain/water edge, wind
    // and tree LOD, instance culling, cutout coverage, hidden owner, etc.
    if !accepts(hit,ray,Query(VIEW,SOLID_MASK,false,vec2<u32>(NIL))) { discard; }
    return vec4<u32>(in.key,bitcast<u32>(t),pack2x16unorm(in.bary));
}
