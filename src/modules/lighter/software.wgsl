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
// Project to the dominant ray plane before testing the three shared edges.
// Adjacent triangles use the same projected endpoints, so a ray on an edge
// cannot fall out of both triangles due to independent barycentric rounding.
struct Shear { axes: vec3<u32>, factor: vec3<f32> }
fn setup(dir: vec3<f32>) -> Shear {
    let a = abs(dir);
    var z = select(1u,0u,a.x>=a.y);
    if a.z>a[z] { z = 2u; }
    let x = (z+1u)%3u;
    let y = (z+2u)%3u;
    return Shear(vec3<u32>(x,y,z),vec3<f32>(-dir[x],-dir[y],1.)/dir[z]);
}
fn project(p: vec3<f32>, shear: Shear) -> vec3<f32> {
    let z = p[shear.axes.z];
    return vec3<f32>(vec2<f32>(p[shear.axes.x],p[shear.axes.y])+z*shear.factor.xy,z*shear.factor.z);
}
fn edge_weight(a: vec2<f32>, b: vec2<f32>) -> f32 {
    // Products are rounded independently even if the backend contracts m+a;
    // swapping the two endpoints then gives an exactly opposite edge value.
    let products = a*b.yx;
    return products.x-products.y;
}
fn triangle(origin: vec3<f32>, shear: Shear, p0: vec3<f32>, p1: vec3<f32>, p2: vec3<f32>) -> vec3<f32> {
    let a = project(p0-origin,shear);
    let b = project(p1-origin,shear);
    let c = project(p2-origin,shear);
    let w = vec3<f32>(edge_weight(b.xy,c.xy),edge_weight(c.xy,a.xy),edge_weight(a.xy,b.xy));
    if any(w<vec3<f32>(0.)) && any(w>vec3<f32>(0.)) { return vec3<f32>(-1.); }
    let det = w.x+w.y+w.z;
    if det==0. { return vec3<f32>(-1.); }
    return vec3<f32>(dot(w,vec3<f32>(a.z,b.z,c.z)),w.yz)/det;
}
fn mesh_hit(m: bool, instance: u32, ray: Ray, old: Hit, q: Query) -> Hit {
    let inst = inst_addr(instance);
    if (word(m,inst+10u) & q.mask)==0u { return old; }
    let t = floats(m,inst);
    let s = bitcast<f32>(word(m,inst+3u));
    let c = bitcast<f32>(word(m,inst+4u));
    let si = bitcast<f32>(word(m,inst+5u));
    var local = ray;
    local.origin = rotate(ray.origin-t,c,-si)/s;
    local.dir = rotate(ray.dir,c,-si)/s;
    let shear = setup(local.dir);
    let nodes = word(m,5u);
    let refs = word(m,6u);
    let end = word(m,inst+9u);
    var best = old;
    var i = word(m,inst+8u);
    while i<end {
        let n = nodes+i*9u;
        if !aabb(local,floats(m,n),floats(m,n+3u),select(best.t,ray.high,q.ordered)) {
            i = word(m,n+6u);
            continue;
        }
        let first = word(m,n+7u);
        let count = word(m,n+8u);
        for (var j=0u; j<count; j++) {
            let prim = word(m,refs+first+j);
            let v = vertex_addresses(m,inst,prim);
            let t = triangle(local.origin,shear,floats(m,v.x),floats(m,v.y),floats(m,v.z));
            if t.x>=ray.low && t.x<=ray.high {
                let hit = Hit(t.x,instance,prim,t.yz);
                if closer(hit,best,q) && accepts(hit,ray,q) {
                    if q.purpose==SHADOW { return hit; }
                    best = hit;
                }
            }
        }
        i++;
    }
    return best;
}
fn scene_hit(m: bool, ray: Ray, old: Hit, q: Query) -> Hit {
    if (word(m,11u) & q.mask)==0u { return old; }
    let nodes = word(m,5u);
    let refs = word(m,6u);
    var best = old;
    var i = word(m,7u);
    let end = word(m,8u);
    while i<end {
        let n = nodes+i*9u;
        if !aabb(ray,floats(m,n),floats(m,n+3u),select(best.t,ray.high,q.ordered)) {
            i = word(m,n+6u);
            continue;
        }
        let first = word(m,n+7u);
        let count = word(m,n+8u);
        for (var j=0u; j<count; j++) {
            best = mesh_hit(m,word(m,refs+first+j)+select(0u,DYNAMIC,m),ray,best,q);
            if q.purpose==SHADOW && best.instance!=NIL { return best; }
        }
        i++;
    }
    return best;
}
fn intersect(ray: Ray, q: Query) -> Hit {
    let static_hit = scene_hit(false,ray,none(ray),q);
    if q.purpose==SHADOW && static_hit.instance!=NIL { return static_hit; }
    return scene_hit(true,ray,static_hit,q);
}
