// All intersections are evaluated by shader ALUs in this variant. No ray
// query capability, raster G-buffer, depth pyramid or screen-space lookup.
fn aabb(ray: Ray, lo: vec3<f32>, hi: vec3<f32>, limit: f32) -> bool {
    let parallel = abs(ray.dir)<vec3<f32>(1.e-12);
    if any(parallel & ((ray.origin<lo) | (ray.origin>hi))) { return false; }
    let inv = 1./select(ray.dir,vec3<f32>(1.),parallel);
    let a = (lo-ray.origin)*inv;
    let b = (hi-ray.origin)*inv;
    let t0 = select(min(a,b),vec3<f32>(-1.e30),parallel);
    let t1 = select(max(a,b),vec3<f32>(1.e30),parallel);
    return max(max(t0.x,t0.y),max(t0.z,ray.low)) <= min(min(t1.x,t1.y),min(t1.z,limit));
}
fn triangle(ray: Ray, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>) -> vec3<f32> {
    let e1 = b-a;
    let e2 = c-a;
    let p = cross(ray.dir,e2);
    let det = dot(e1,p);
    if abs(det)<1.e-12 { return vec3<f32>(-1.); }
    let s = ray.origin-a;
    let u = dot(s,p)/det;
    let q = cross(s,e1);
    let v = dot(ray.dir,q)/det;
    if u<0. || v<0. || u+v>1. { return vec3<f32>(-1.); }
    return vec3<f32>(dot(e2,q)/det,u,v);
}
fn mesh_hit(m: bool, instance: u32, ray: Ray, old: Hit) -> Hit {
    let inst = inst_addr(instance);
    let t = floats(m,inst);
    let s = bitcast<f32>(word(m,inst+3u));
    let c = bitcast<f32>(word(m,inst+4u));
    let si = bitcast<f32>(word(m,inst+5u));
    let local = Ray(rotate(ray.origin-t,c,-si)/s,rotate(ray.dir,c,-si)/s,ray.low,ray.high);
    let nodes = word(m,5u);
    let refs = word(m,6u);
    let end = word(m,inst+9u);
    var best = old;
    var i = word(m,inst+8u);
    while i<end {
        let n = nodes+i*9u;
        if !aabb(local,floats(m,n),floats(m,n+3u),best.t) {
            i = word(m,n+6u);
            continue;
        }
        let first = word(m,n+7u);
        let count = word(m,n+8u);
        for (var j=0u; j<count; j++) {
            let prim = word(m,refs+first+j);
            let v = vertex_addresses(m,inst,prim);
            let hit = triangle(local,floats(m,v.x),floats(m,v.y),floats(m,v.z));
            if hit.x>=ray.low && hit.x<best.t { best = Hit(hit.x,instance,prim,hit.yz); }
        }
        i++;
    }
    return best;
}
fn scene_hit(m: bool, ray: Ray, old: Hit) -> Hit {
    let nodes = word(m,5u);
    let refs = word(m,6u);
    var best = old;
    var i = word(m,7u);
    let end = word(m,8u);
    while i<end {
        let n = nodes+i*9u;
        if !aabb(ray,floats(m,n),floats(m,n+3u),best.t) {
            i = word(m,n+6u);
            continue;
        }
        let first = word(m,n+7u);
        let count = word(m,n+8u);
        for (var j=0u; j<count; j++) {
            best = mesh_hit(m,word(m,refs+first+j)+select(0u,DYNAMIC,m),ray,best);
        }
        i++;
    }
    return best;
}
fn closest(ray: Ray) -> Hit {
    let none = Hit(ray.high,NIL,0u,vec2<f32>(0.));
    return scene_hit(true,ray,scene_hit(false,ray,none));
}
