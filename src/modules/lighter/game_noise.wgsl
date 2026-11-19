// Nonlinear bidimensional index rotations are separated from camera jitter.
fn game_noise(ray: Ray, stream: u32) -> vec2<f32> {
    let id = ray.pixel_seed - 1u;
    let x = id % frame.extent.x;
    let y = id / frame.extent.x;
    let a = (x+stream)&15u;
    let b = (y+3u*stream)&15u;
    let rank = vec2<f32>(f32(BLUE_RANK[16u*b+a]), f32(BLUE_RANK[16u*((a+5u)&15u)+((b+9u)&15u)]));
    let pass = f32(frame.sampling.x & 1023u);
    return fract((rank+0.5)/256. + pass*vec2<f32>(0.61803398875,0.754877666));
}
