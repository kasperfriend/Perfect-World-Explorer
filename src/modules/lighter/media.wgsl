// The client defines an open top surface and a wet-cell footprint, not a
// watertight volume or a calibrated extinction coefficient. A tagged path
// walks connected wet cells until the top interface or an actual dry gap.
struct WaterBody { corner: vec2<f32>, grid: f32, height: f32, size: vec2<i32>, mask: u32, links: u32 }
struct WetSpan { distance: f32, body: u32 }
struct SegmentMedium { tr: vec3<f32>, inscatter: vec3<f32>, body: u32 }
const OPEN_WATER: u32 = 0xffffffffu; // untagged analytic/fixture interface
fn water_body(id: u32) -> WaterBody {
    let at = frame.water_effects.z+8u*(id-1u);
    return WaterBody(bitcast<vec2<f32>>(vec2<u32>(texels[at],texels[at+1u])),
        bitcast<f32>(texels[at+2u]),bitcast<f32>(texels[at+3u]),
        vec2<i32>(i32(texels[at+4u]),i32(texels[at+5u])),texels[at+6u],texels[at+7u]);
}
fn wet_cell(b: WaterBody, cell: vec2<i32>) -> bool {
    if any(cell<vec2<i32>(0)) || any(cell>=b.size) { return false; }
    if b.mask==0u { return true; } // full rectangle, no bitmap read
    let i = u32(cell.x+b.size.x*cell.y);
    return (texels[b.mask+i/32u] & (1u<<(i%32u)))!=0u;
}
fn water_bounds(xz: vec2<f32>) -> bool {
    return all(xz>=frame.resident_bounds.xz) && all(xz<frame.resident_bounds.yw);
}
const WATER_AXES = vec2<f32>(1.,-1.);
// Work in world X/-Z so both grids compute the same time for a shared edge.
// Subtracting a normalized grid coordinate from the column count can lose
// enough precision near a join to make the next grid reject a wet ray.
struct WetRay { origin: vec2<f32>, speed: vec2<f32>, reciprocal: vec2<f32> }
fn wet_ray(ray: Ray) -> WetRay {
    let speed = ray.dir.xz*WATER_AXES;
    let moving = abs(speed)>vec2<f32>(1.e-12);
    let reciprocal = select(vec2<f32>(0.),1./select(vec2<f32>(1.),speed,moving),moving);
    return WetRay(ray.origin.xz*WATER_AXES,speed,reciprocal);
}
fn crossings(ray: WetRay, edge: vec2<f32>) -> vec2<f32> {
    return select(vec2<f32>(1.e30),(edge-ray.origin)*ray.reciprocal,ray.reciprocal!=vec2<f32>(0.));
}
fn water_edge(b: WaterBody, cell: vec2<f32>) -> vec2<f32> {
    return b.corner*WATER_AXES+cell*b.grid;
}
fn entered_cell(ray: WetRay, b: WaterBody, at: f32) -> vec2<i32> {
    // The approximate point locates the nearest edge. Decide which side of
    // it we are on using the same signed crossing time as the grid walk.
    // This assigns exact hits to the forward cell without bridging dry gaps.
    let point = ray.origin+ray.speed*at;
    let nearest = round((point-water_edge(b,vec2<f32>(0.)))/b.grid);
    let edge = water_edge(b,nearest);
    let forward = vec2<f32>(at)>=crossings(ray,edge);
    let above = select(ray.origin>=edge,forward==(ray.speed>vec2<f32>(0.)),ray.reciprocal!=vec2<f32>(0.));
    return vec2<i32>(nearest)-select(vec2<i32>(1),vec2<i32>(0),above);
}
fn wet_leg(ray: WetRay, b: WaterBody, start: f32, limit: f32) -> f32 {
    var cell = entered_cell(ray,b,start);
    if !wet_cell(b,cell) { return start; }
    if b.mask==0u {
        let exit = select(vec2<f32>(0.),vec2<f32>(b.size),ray.speed>=vec2<f32>(0.));
        let t = crossings(ray,water_edge(b,exit));
        return clamp(min(t.x,t.y),start,limit);
    }
    let step = select(vec2<i32>(-1),vec2<i32>(1),ray.speed>=vec2<f32>(0.));
    let positive = vec2<f32>(max(step,vec2<i32>(0)));
    var at = start;
    // A line can't cross more than width + height cells in this grid.
    for (var n=0; n<b.size.x+b.size.y; n++) {
        let t = crossings(ray,water_edge(b,vec2<f32>(cell)+positive));
        at = max(at,min(t.x,t.y));
        if at>=limit { return limit; }
        if t.x<=t.y { cell.x += step.x; }
        if t.y<=t.x { cell.y += step.y; }
        if !wet_cell(b,cell) { break; }
    }
    return at;
}
fn wet_span(ray: Ray, distance: f32, id: u32) -> WetSpan {
    if id==0u { return WetSpan(0.,0u); }
    if id==OPEN_WATER { return WetSpan(distance,id); }
    if id>frame.water_effects.w || !water_bounds(ray.origin.xz) { return WetSpan(0.,0u); }
    var b = water_body(id);
    var limit = distance;
    if ray.dir.y>0. { limit = min(limit,max(0.,(b.height-ray.origin.y)/ray.dir.y)); }
    let r = wet_ray(ray);
    let extent = frame.resident_bounds;
    let bounds = select(vec2<f32>(extent.x,-extent.w),vec2<f32>(extent.y,-extent.z),r.speed>=vec2<f32>(0.));
    let boundary = crossings(r,bounds);
    limit = min(limit,max(0.,min(boundary.x,boundary.y)));
    var at = 0.;
    var current = id;
    for (var n=0u; n<64u; n++) {
        at = wet_leg(r,b,at,limit);
        if at>=limit { return WetSpan(limit,select(0u,current,limit+1.e-5>=distance)); }
        if b.links==0u { break; }
        let link = b.links;
        let count = texels[link];
        var next = 0u;
        for (var i=0u; i<count; i++) {
            let candidate = texels[link+1u+i];
            let other = water_body(candidate);
            let cell = entered_cell(r,other,at);
            if wet_cell(other,cell) {
                next = candidate;
                b = other;
                break;
            }
        }
        if next==0u { break; }
        current = next;
    }
    return WetSpan(at,0u);
}
fn wet_distance(ray: Ray, distance: f32, id: u32) -> f32 {
    return wet_span(ray,distance,id).distance;
}
fn water_tr(distance: f32) -> vec3<f32> {
    let fog = frame.water;
    if fog.z<0.5 || distance<=0. { return vec3<f32>(1.); }
    // Map authored range/hue to RGB extinction: 5% left after one span in
    // the clearest channel. The normalized RGB hue biases the other channels.
    let hue = frame.water_color.rgb;
    let peak = max(hue.x,max(hue.y,hue.z));
    let density = 1.+0.5*select(vec3<f32>(0.),1.-hue/max(peak,1.e-5),peak>1.e-5);
    return exp2(-4.321928*density*(distance/max(fog.y-fog.x,0.01)));
}
fn media_to(ray: Ray, distance: f32, body: u32, sky: bool) -> SegmentMedium {
    let wet = wet_span(ray,distance,body);
    let tw = water_tr(wet.distance);
    // The legacy atmosphere uses a linear view-distance ramp. Sky art
    // already contains haze, so don't replace its distant color a second time.
    var ta = 1.;
    if !sky && frame.air.z>=0.5 {
        ta -= clamp((distance-wet.distance-frame.air.x)/max(frame.air.y-frame.air.x,0.01),0.,1.);
    }
    let path_color = (1.-tw)*frame.water_color.rgb+tw*(1.-ta)*frame.air_color.rgb;
    return SegmentMedium(tw*ta,path_color,wet.body);
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
    let valid = distance>0. && distance<frame.eye.w && water_bounds(exit.xz);
    if !valid { return CausticLink(vec3<f32>(0.),0.,vec3<f32>(0.)); }
    let probe = Ray(p,inside,0.,distance,0.,0.,0.,0u);
    if wet_distance(probe,distance,id)+1.e-4<distance {
        return CausticLink(vec3<f32>(0.),0.,vec3<f32>(0.));
    }
    // Preserve the horizontal interface's incident flux at the transmitted
    // angle, including Fresnel and RGB loss on the water leg.
    return CausticLink(inside,distance,water_tr(distance)*((1.-optical.fresnel)*sun.y/inside.y));
}
