fn game_opaque(s: Surface, medium: u32, purpose: u32) -> bool {
    return frame.ray_effects.w>0. && purpose==VIEW && medium==0u
        && (s.flags & (BLEND|WATER|GLASS|FOLIAGE|CLOUD|EMISSIVE))==0u;
}

