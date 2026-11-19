// Block-aligned page rectangles, explicit logical mip selection. The texture
// unit expands BC1 and converts sRGB; integer loads cannot bleed across mips
// or neighboring tiles in the atlas. Wrap and anisotropy match the control.
@group(0) @binding(7) var bc_pages: texture_2d_array<f32>;
const BC_TEXTURES_ACTIVE: u32 = 131072u;

fn bc_sample_level(addr: u32, uv: vec2<f32>, mip: u32, fmt: u32) -> vec3<f32> {
    if ((frame.options.x & BC_TEXTURES_ACTIVE)==0u || (fmt & 256u)==0u) {
        return bc_terrain_level(addr,uv,mip,(fmt & BC_TERRAIN_1)!=0u);
    }
    let here = texels[addr+4u+texels[addr+2u]+mip];
    let origin = vec2<i32>(i32(here&255u),i32((here>>8u)&255u));
    let layer = i32(here>>16u);
    let dims = rgba_mip_dims(addr,mip);
    let tap = bilinear(0u,dims,uv,false);
    var rgb = vec3<f32>(0.);
    for (var i=0u; i<4u; i++) {
        let p = i32(tap.index[i]);
        rgb += textureLoad(bc_pages,origin+vec2<i32>(p%dims.x,p/dims.x),layer,0).rgb*tap.weight[i];
    }
    return rgb;
}
