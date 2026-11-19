// Reference proposals use solid-angle PDFs on both the sampled-light leg and
// the BSDF leg. The static and moving packets each own a power CDF. Their
// distributions are mixed by total power, without renormalizing per receiver.
const SKY_COLUMNS: u32 = 64u;
const SKY_ROWS: u32 = 32u;
const SKY_CELLS: u32 = SKY_COLUMNS*SKY_ROWS;
const SKY_UNIFORM: f32 = 1./(4.*BSDF_PI);

fn reference() -> bool { return (frame.options.x & REFERENCE_MODE)!=0u; }
fn balance(pdf: f32, other: f32) -> f32 {
    return pdf/max(pdf+other,1.e-30);
}
fn sky_bin(d: vec3<f32>) -> u32 {
    let phi = atan2(d.z,d.x)/(2.*BSDF_PI);
    let x = min(u32(fract(phi)*f32(SKY_COLUMNS)),SKY_COLUMNS-1u);
    let y = min(u32(max(0.,d.y*0.5+0.5)*f32(SKY_ROWS)),SKY_ROWS-1u);
    return y*SKY_COLUMNS+x;
}
fn sky_mass(at: u32, cell: u32) -> f32 {
    if at==0u { return 1./f32(SKY_CELLS); }
    let end = bitcast<f32>(texels[at+cell]);
    var begin = 0.;
    if cell>0u { begin = bitcast<f32>(texels[at+cell-1u]); }
    return end-begin;
}
fn sky_fade() -> f32 {
    return select(0.,clamp(bitcast<f32>(frame.sky0.w),0.,1.),frame.sky1.x!=0u);
}
fn sky_pdf(d: vec3<f32>) -> f32 {
    let cell = sky_bin(d);
    return mix(sky_mass(frame.sky_cdf.x,cell),sky_mass(frame.sky_cdf.y,cell),sky_fade())
        *f32(SKY_CELLS)*SKY_UNIFORM;
}
fn sample_sky(seed: ptr<function,u32>) -> vec3<f32> {
    let at = select(frame.sky_cdf.x,frame.sky_cdf.y,random(seed)<sky_fade());
    let u = random(seed);
    var cell = u32(u*f32(SKY_CELLS));
    if at!=0u {
        var lo = 0u;
        var hi = SKY_CELLS-1u;
        while lo<hi {
            let mid = (lo+hi)/2u;
            if u<bitcast<f32>(texels[at+mid]) { hi=mid; } else { lo=mid+1u; }
        }
        cell = lo;
    }
    let phi = (2.*BSDF_PI/f32(SKY_COLUMNS))*(f32(cell%SKY_COLUMNS)+random(seed));
    let y = (2./f32(SKY_ROWS))*(f32(cell/SKY_COLUMNS)+random(seed))-1.;
    let r = sqrt(max(0.,1.-y*y));
    return vec3<f32>(cos(phi)*r,y,sin(phi)*r);
}
fn sky_cell_rgb(at: u32, cell: u32, d: vec3<f32>) -> vec3<f32> {
    if at==0u { return analytic_sky(d); }
    let index = at+SKY_CELLS+3u*cell;
    return bitcast<vec3<f32>>(vec3<u32>(texels[index],texels[index+1u],texels[index+2u]));
}
fn reference_sky(d: vec3<f32>) -> vec3<f32> {
    let cell = sky_bin(d);
    let raw = mix(sky_cell_rgb(frame.sky_cdf.x,cell,d),sky_cell_rgb(frame.sky_cdf.y,cell,d),sky_fade());
    let tint = select(vec3<f32>(1.),clamp(frame.sky_tint.rgb,vec3<f32>(0.),vec3<f32>(1.)),frame.sky_cdf.x!=0u);
    let moon = smoothstep(0.9985,0.9992,dot(d,frame.moon_disc.xyz))*frame.moon_disc.w;
    return linear(encoded(raw)*tint)+vec3<f32>(0.45,0.5,0.6)*moon;
}
fn bilinear_sky(at: u32, x: i32, y: i32, f: vec2<f32>, d: vec3<f32>) -> vec3<f32> {
    let w = i32(SKY_COLUMNS);
    let h = i32(SKY_ROWS);
    let a = u32((x+w)%w);
    let b = u32((x+1+w)%w);
    let c = u32(clamp(y,0,h-1))*SKY_COLUMNS;
    let e = u32(clamp(y+1,0,h-1))*SKY_COLUMNS;
    return mix(mix(sky_cell_rgb(at,c+a,d),sky_cell_rgb(at,c+b,d),f.x),
               mix(sky_cell_rgb(at,e+a,d),sky_cell_rgb(at,e+b,d),f.x),f.y);
}
fn diffuse_sky(d: vec3<f32>) -> vec3<f32> {
    let phi = atan2(d.z,d.x)/(2.*BSDF_PI);
    let grid = vec2<f32>(fract(phi),d.y*0.5+0.5)*vec2<f32>(f32(SKY_COLUMNS),f32(SKY_ROWS))-0.5;
    let p = vec2<i32>(floor(grid));
    let f = fract(grid);
    let fade = sky_fade();
    var rgb = bilinear_sky(frame.sky_cdf.x,p.x,p.y,f,d);
    if fade>0. { rgb = mix(rgb,bilinear_sky(frame.sky_cdf.y,p.x,p.y,f,d),fade); }
    let moon = smoothstep(0.9985,0.9992,dot(d,frame.moon_disc.xyz))*frame.moon_disc.w;
    return linear(encoded(rgb)*clamp(frame.sky_tint.rgb,vec3<f32>(0.),vec3<f32>(1.)))+vec3<f32>(0.45,0.5,0.6)*moon;
}
// Corresponds to Surface::emitter; these use a simple filtered base channel
// and do not need a second dispatch of the authored surface interpreter.
fn reference_emitter(flags: u32, face_mode: u32, gloss: u32, emission: vec3<f32>) -> bool {
    return (flags & (TERRAIN|BAKED|BLEND|CUTOUT|WRITE_DEPTH|WATER|GLASS|PRIMARY_ONLY|FOLIAGE|
        TREE_MATERIAL|GRASS_MATERIAL|SHORE|CLOUD|OVERLAY|PRECIPITATION))==0u && face_mode==0u && gloss==0u
        && ((flags & EMISSIVE)!=0u || strongest(emission)>0.);
}
fn emitter_power(m: bool) -> f32 { return bitcast<f32>(word(m,18u)); }
fn emitter_record(m: bool, i: u32) -> vec2<u32> {
    let at = word(m,16u)+3u*i;
    return vec2<u32>(word(m,at),word(m,at+1u));
}
fn emitter_end(m: bool, i: u32) -> f32 {
    return bitcast<f32>(word(m,word(m,16u)+3u*i+2u));
}
fn emitter_mass(m: bool, i: u32) -> f32 {
    var begin = 0.;
    if i>0u { begin=emitter_end(m,i-1u); }
    return (emitter_end(m,i)-begin)/max(emitter_power(false)+emitter_power(true),1.e-30);
}
fn emitter_pdf_at(hit: Hit, origin: vec3<f32>, p: vec3<f32>) -> f32 {
    let m = (hit.instance & DYNAMIC)!=0u;
    let key = vec2<u32>(hit.instance & (DYNAMIC-1u),hit.primitive);
    var lo = 0u;
    let count = word(m,17u);
    var hi = count;
    while lo<hi {
        let mid = (lo+hi)/2u;
        let rec = emitter_record(m,mid);
        if rec.x<key.x || (rec.x==key.x && rec.y<key.y) { lo=mid+1u; }
        else { hi=mid; }
    }
    if lo==count || any(emitter_record(m,lo)!=key) { return 0.; }
    let inst = inst_addr(hit.instance);
    let g = geometric(m,inst,vertex_addresses(m,inst,hit.primitive));
    let v = p-origin;
    return emitter_mass(m,lo)*dot(v,v)/max(0.5*abs(dot(g,norm(v))),1.e-20);
}
struct EmissionSample { dir: vec3<f32>, t: f32, pdf: f32, key: vec2<u32>, bary: vec2<f32> }
fn sample_emitter(p: vec3<f32>, seed: ptr<function,u32>) -> EmissionSample {
    let power = emitter_power(false);
    let total = power+emitter_power(true);
    let empty = EmissionSample(vec3<f32>(0.),0.,0.,vec2<u32>(NIL),vec2<f32>(0.));
    if total<=0. { return empty; }
    let draw = random(seed)*total;
    let m = draw>=power && emitter_power(true)>0.;
    let u = select(draw,draw-power,m);
    var lo = 0u;
    var hi = word(m,17u)-1u;
    while lo<hi {
        let mid = (lo+hi)/2u;
        if u<emitter_end(m,mid) { hi=mid; } else { lo=mid+1u; }
    }
    let key = emitter_record(m,lo)+vec2<u32>(select(0u,DYNAMIC,m),0u);
    let inst = inst_addr(key.x);
    let v = vertex_addresses(m,inst,key.y);
    let a = floats(m,v.x);
    let e = floats(m,v.y)-a;
    let f = floats(m,v.z)-a;
    let r = sqrt(random(seed));
    let s = r*random(seed);
    let bary = vec2<f32>(r-s,s);
    let to = floats(m,inst)-p+instance_vector(m,inst,a+e*bary.x+f*bary.y);
    let t = length(to);
    if t<=1.e-5 || t>=frame.eye.w { return empty; }
    let dir = to/t;
    let g = geometric(m,inst,v);
    let flags = word(m,material_addr(m,key.y)+1u);
    if (flags & SINGLE_SIDED)!=0u && dot(g,dir)>=0. { return empty; }
    let pdf = emitter_mass(m,lo)*t*t/max(0.5*abs(dot(g,dir)),1.e-20);
    return EmissionSample(dir,t,pdf,key,bary);
}
fn sample_emission(hit: Hit) -> vec3<f32> {
    // The CPU registers only surfaces using this base RGB emission stage.
    // Do not fetch normal/face/baked stages, or invoke the full hit shader.
    let m = (hit.instance & DYNAMIC)!=0u;
    let inst = inst_addr(hit.instance);
    let v = vertex_addresses(m,inst,hit.primitive);
    let mat = material_addr(m,hit.primitive);
    let flags = word(m,mat+1u);
    let w = weights_of(hit);
    let addr = word(m,mat);
    let uv = surface_uv(m,v,w,mat,flags);
    let fmt = texels[addr+3u];
    let tex = unassociate(level(addr,uv,(flags & CLAMP_UV)!=0u,0u,fmt),fmt).rgb;
    let tint = tint_of(m,v,w).rgb;
    if (flags & EMISSIVE)!=0u { return tex*tint; }
    return tex*linear(tint)*floats(m,mat+7u);
}
