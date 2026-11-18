// The client defines an open top surface and a wet-cell footprint, not a
// watertight volume or a calibrated extinction coefficient. A tagged path
// keeps its particular body's medium until the top interface or a dry cell.
struct WaterBody { corner: vec2<f32>, grid: f32, height: f32, size: vec2<i32>, mask: u32 }
struct SegmentMedium { tr: vec3<f32>, inscatter: vec3<f32>, body: u32 }
const OPEN_WATER: u32 = 0xffffffffu; // untagged analytic/fixture interface
fn water_body(id: u32) -> WaterBody {
    let at = frame.water_effects.z+8u*(id-1u);
    return WaterBody(bitcast<vec2<f32>>(vec2<u32>(texels[at],texels[at+1u])),
        bitcast<f32>(texels[at+2u]),bitcast<f32>(texels[at+3u]),
        vec2<i32>(i32(texels[at+4u]),i32(texels[at+5u])),texels[at+6u]);
}
fn wet_cell(b: WaterBody, cell: vec2<i32>) -> bool {
    if any(cell<vec2<i32>(0)) || any(cell>=b.size) { return false; }
    let i = u32(cell.x+b.size.x*cell.y);
    return (texels[b.mask+i/32u] & (1u<<(i%32u)))!=0u;
}
fn water_uv(b: WaterBody, xz: vec2<f32>) -> vec2<f32> {
    return (xz-b.corner)*vec2<f32>(1.,-1.)/b.grid;
}
fn water_bounds(xz: vec2<f32>) -> bool {
    return all(xz>=frame.resident_bounds.xz) && all(xz<frame.resident_bounds.yw);
}
fn edge_time(next: f32, start: f32, speed: f32) -> f32 {
    if abs(speed)<=1.e-12 { return 1.e30; }
    return max(0.,(next-start)/speed);
}
fn wet_distance(ray: Ray, distance: f32, id: u32) -> f32 {
    if id==0u { return 0.; }
    if id==OPEN_WATER { return distance; }
    if id>frame.water_effects.w || !water_bounds(ray.origin.xz) { return 0.; }
    let b = water_body(id);
    let u = water_uv(b,ray.origin.xz);
    let motion = ray.dir.xz*vec2<f32>(1.,-1.)/b.grid;
    let step = select(vec2<i32>(-1),vec2<i32>(1),motion>=vec2<f32>(0.));
    // A boundary belongs to the cell being entered, not the one behind.
    var cell = vec2<i32>(floor(u));
    cell -= select(vec2<i32>(0),vec2<i32>(1),(u==floor(u)) & (step<vec2<i32>(0)));
    var limit = distance;
    if ray.dir.y>0. { limit = min(limit,max(0.,(b.height-ray.origin.y)/ray.dir.y)); }
    let extent = frame.resident_bounds;
    limit = min(limit,edge_time(select(extent.x,extent.y,ray.dir.x>=0.),ray.origin.x,ray.dir.x));
    limit = min(limit,edge_time(select(extent.z,extent.w,ray.dir.z>=0.),ray.origin.z,ray.dir.z));
    let positive = vec2<f32>(max(step,vec2<i32>(0)));
    var at = 0.;
    // A monotonic line cannot traverse more cells than width plus height.
    for (var n=0; n<b.size.x+b.size.y && at<limit; n++) {
        if !wet_cell(b,cell) { return at; }
        let next = vec2<f32>(cell)+positive;
        let sx = edge_time(next.x,u.x,motion.x);
        let sz = edge_time(next.y,u.y,motion.y);
        at = min(sx,sz);
        if sx<=sz { cell.x += step.x; }
        if sz<=sx { cell.y += step.y; }
    }
    return limit;
}
fn water_tr(distance: f32) -> vec3<f32> {
    let fog = frame.water;
    if fog.z<0.5 { return vec3<f32>(1.); }
    // Map authored range/hue to RGB extinction: 5% left after one span in
    // the clearest channel. The normalized RGB hue biases the other channels.
    let hue = frame.water_color.rgb;
    let peak = max(hue.x,max(hue.y,hue.z));
    let density = 1.+0.5*select(vec3<f32>(0.),1.-hue/max(peak,1.e-5),peak>1.e-5);
    return exp2(-4.321928*density*(distance/max(fog.y-fog.x,0.01)));
}
fn media_to(ray: Ray, distance: f32, body: u32, sky: bool) -> SegmentMedium {
    let wet = wet_distance(ray,distance,body);
    let tw = water_tr(wet);
    // The legacy atmosphere uses a linear view-distance ramp. Sky art
    // already contains haze, so don't replace its distant color a second time.
    var ta = 1.;
    if !sky && frame.air.z>=0.5 {
        ta -= clamp((distance-wet-frame.air.x)/max(frame.air.y-frame.air.x,0.01),0.,1.);
    }
    let path_color = (1.-tw)*frame.water_color.rgb+tw*(1.-ta)*frame.air_color.rgb;
    return SegmentMedium(tw*ta,path_color,select(0u,body,wet+1.e-5>=distance));
}

// A separate specular sun connection for a flat body. This is not an added
// texture on the bed. Inverse Snell gives the underwater direction, then a
// shadow ray tests each side of the interface at its actual endpoint.
struct CausticLink { inside: vec3<f32>, distance: f32, weight: vec3<f32> }
fn sun_caustic(p: vec3<f32>, sun: vec3<f32>, id: u32) -> CausticLink {
    if id==0u || id>frame.water_effects.w { return CausticLink(vec3<f32>(0.),0.,vec3<f32>(0.)); }
    let up = vec3<f32>(0.,1.,0.);
    let b = water_body(id);
    let optical = dielectric(-sun,up,1./1.333);
    let inside = -optical.transmitted;
    let distance = (b.height-p.y)/max(inside.y,1.e-6);
    let exit = p+inside*distance;
    let valid = distance>0. && distance<frame.eye.w && water_bounds(exit.xz)
        && wet_cell(b,vec2<i32>(floor(water_uv(b,exit.xz))));
    if !valid { return CausticLink(vec3<f32>(0.),0.,vec3<f32>(0.)); }
    let probe = Ray(p,inside,0.,distance,0.,0.,0.,0u);
    if wet_distance(probe,distance,id)+1.e-4<distance {
        return CausticLink(vec3<f32>(0.),0.,vec3<f32>(0.));
    }
    // Preserve the horizontal interface's incident flux at the transmitted
    // angle, including Fresnel and RGB loss on the water leg.
    return CausticLink(inside,distance,water_tr(distance)*((1.-optical.fresnel)*sun.y/inside.y));
}
