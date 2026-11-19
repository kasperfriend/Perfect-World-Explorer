# Local emitters and colored visibility (Step 15)

The light BVH remains an index of **influence ranges**, not a camera-nearest
list. Static records with `flags & 1` scale the encoded diffuse color by
the same `environment::night_factor` as the native model query. Other flag
bits have no established Lighter behavior. The runtime feed uses the effect
owner's current light publications, not a light that persists after its
owner stops publishing. Updating the palette or intensity does not rebuild
the geometry AS.

Shadowed static lights sample the solid angle of a small sphere and stop at
its near surface. Runtime lights sample a receiver-facing disk and stop at
the sampled disk point. The authored radius is the **influence** radius, not
an emitter measurement. Both shapes use a provisional 4% of that radius
(clamped to 0.05–1.5 world units, never over half the influence); near the
center the sampled radius is limited to below the receiver distance.
Lighting retains the legacy finite-range squared falloff and integrated
intensity, not a calibrated area-light profile. With shadows disabled the
same light is evaluated at its center.

Visibility is RGB. Alpha cutout holes do not advance the shadow ray; opaque
blockers terminate it. Leaves and clouds retain their separate transmission
models. Explicit thin glass multiplies straight visibility by its linear RGB
tint and the two-interface unpolarized Fresnel transmission `(1-F)/(1+F)`.
Multiple panes multiply, and intersections past the light sample do not
occlude it. Sixteen covered layers are supported; exhaustion conservatively
blocks the connection. Legacy alpha-blended effects are not reinterpreted
as glass. This is a direct-shadow model, not a solution for specular
connections through thick, refracting objects.

## Interactive sampling

At one sample per pixel, Single-path mode uses a digitally shifted 2D Sobol
sequence for primary soft-sun samples. For more than four published static
and runtime local lights, shadowed Single-path transport importance-resamples
one eligible local proposal per shaded hit. The influence BVHs and normal
checks still reject out-of-range and back-facing proposals before visibility.
Dividing the selected energy by its probability preserves the summed mean,
not the per-frame colors. The sun remains separate. This is single-frame
resampling, not temporal/spatial ReSTIR reuse.

With progressive sampling, a stationary one-sample hard sun can reuse a
binary clear/opaque result at an unchanged primary receiver. View, lighting,
scene, animated coverage or moving geometry changes invalidate reuse. Partial
RGB transmission is never stored as a binary result.

## Validation (2026-11-18)

`6f758c8` passed the shared Software and CPU Vulkan ray-query tests on Mesa
25.0.7. Known-value checks cover a night-only record at 0, 0.5 and 1.0 night
factor, two colored panes at 45° with an opaque-tint blocker beyond the lamp,
and partial visibility around an edge for both finite emitter shapes. The
streamed test covers a static and a currently published GFX light, owner
withdrawal, and removal of a BMD caster. After eviction it requires the same
depth and color within three 8-bit levels of the unshadowed center sample;
finite samples need not have exactly the center's incidence angle.
