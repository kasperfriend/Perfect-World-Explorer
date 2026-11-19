# A6 — Targeted dry opaque transport (S)

`PW_RAY_GAME_PROFILE=1` specializes only the opted-in Hardware Hybrid
compute sibling. It requires one sample, one bounce, at most half-size
trace extent per axis, portable resolve, and neither progressive nor
Reference. A Hybrid fallback never claims to draw the game profile.

Dry opaque receivers retain base lighting; positive N·L sun connections
can trace a short visibility leg. Local lights skip shadow traversal only
on eligible receivers. Glossy/metal surfaces sample one visible-normal GGX
reflection (gloss threshold 0.35, authored metal threshold 0.55); very smooth
surfaces can instead use the authored environment. Pixels without a valid
reflection leg can add one attenuated short diffuse bounce over base
lighting; baked receivers are not darkened/replaced. Water, glass,
transparency, foliage, clouds and Reference are not opted into this policy.
The separate reflection, sun and bounce tier flags accept `off`, `near`
(default 96/120/32 units) and `full`. `off` disables that effect's RT leg,
not its base lighting/probe contribution.

S: tier admission and both specialized WGSL entries lowered through Naga;
GPU quality/performance has not been established. `game_profile_active` is
recorded per travel frame. This experimental route uses the existing global
half-size trace extent; it has no independently resolved reflection plane,
distant raster shadow cascades, or per-effect GPU ray counters yet. Until
those are implemented and quality/timing is measured, A6 remains partial.
