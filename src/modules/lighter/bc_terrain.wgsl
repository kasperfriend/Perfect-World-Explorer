// Optional buffer-backed BC terrain. Integer palette expansion is identical
// to dxt.rs, then the ordinary linear-light filtering applies. Compressed
// blocks are not sampled through a native BC texture unit.
const BC_TERRAIN_1: u32 = 32u;
const BC_TERRAIN_23: u32 = 64u;

fn rgb565(c: u32) -> vec3<u32> {
    let rgb = (vec3<u32>(c) >> vec3<u32>(11u,5u,0u)) & vec3<u32>(31u,63u,31u);
    return (rgb << vec3<u32>(3u,2u,3u)) | (rgb >> vec3<u32>(2u,4u,2u));
}

fn bc_terrain_rgb(base: u32, w: u32, texel: u32, bc1: bool) -> vec3<f32> {
    let x = texel % w;
    let y = texel / w;
    let block = (y/4u)*((w+3u)/4u) + x/4u;
    // BC2/3 store color after two alpha words, which terrain ignores.
    let at = base + select(4u*block+2u, 2u*block, bc1);
    let endpoints = texels[at];
    let c0 = endpoints & 65535u;
    let c1 = endpoints >> 16u;
    let i = (texels[at+1u] >> (2u*((y%4u)*4u + x%4u))) & 3u;
    let a = rgb565(c0);
    let b = rgb565(c1);
    let four = !bc1 || c0>c1;
    var color: vec3<u32>;
    switch i {
        case 0u: { color = a; }
        case 1u: { color = b; }
        case 2u: { color = select((a+b)/2u,(2u*a+b)/3u,four); }
        default: { color = select(vec3<u32>(0u),(a+2u*b)/3u,four); }
    }
    return vec3<f32>(color)/255.;
}

fn bc_terrain_level(addr: u32, uv: vec2<f32>, mip: u32, bc1: bool) -> vec3<f32> {
    let base = addr + texels[addr+4u+mip];
    let dims = rgba_mip_dims(addr,mip);
    let tap = bilinear(0u,dims,uv,false);
    let w = u32(dims.x);
    var rgb = vec3<f32>(0.);
    for (var i=0u; i<4u; i++) {
        rgb += linear(bc_terrain_rgb(base,w,tap.index[i],bc1))*tap.weight[i];
    }
    return rgb;
}

fn terrain_tile(addr: u32, uv: vec2<f32>, taps: Taps) -> vec3<f32> {
    let fmt = texels[addr+3u];
    if (fmt & (BC_TERRAIN_1|BC_TERRAIN_23))==0u { return filtered_tile(addr,uv,false,taps).rgb; }
    let mip = u32(taps.lod);
    let part = fract(taps.lod);
    let levels = select(1u,2u,part>0.);
    var sum = vec3<f32>(0.);
    for (var i=0u; i<taps.count; i++) {
        let p = uv+(f32(i)+0.5-0.5*f32(taps.count))*taps.step;
        for (var l=0u; l<levels; l++) {
            sum += bc_terrain_level(addr,p,mip+l,fmt==BC_TERRAIN_1)*select(1.-part,part,l!=0u);
        }
    }
    return sum/f32(taps.count);
}
