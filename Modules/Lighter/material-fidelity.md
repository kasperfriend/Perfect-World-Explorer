# Authored materials — Step 11 (RT-06)

The [baked and relit controls](README.md#baked-and-relit-materials) use a
shared Software/Vulkan shading path. The native renderer keeps its own
material and archive paths. Lighter carries authored legacy channels; it
does not infer metalness or pretend a D3D material power is a measured
roughness map. GGX and thick-surface transmission are later transport work.

## Regression cases (2026-11-18)

[Linux CI](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44412488672)
passed the trace suites on `cd2b970`: Software BVH and CPU Vulkan ray queries,
plus the streamed world. Runner: Mesa 26.0.8 / LLVM 21.1.8. The bundled
sandbox Mesa 25.0.7 hit a cold shader/first-readback timeout; local unit
and isolated sampler checks do not replace the runner's full ray tests.

- **Building:** archive-backed BMD keeps its textured ×2 stage, cutout,
  specular `[0.8, 0.6, 0.4]`, power `96`, and the optional NRF binding.
  The color-only fixture switches from encoded red `0.65` to blue `0.28`
  without changing depth. Relighting removes that bake and produces linear
  `[0.40, 0.20, 0.20]` under ambient plus emission, or `[0.60, 0.20, 0.20]`
  with the test sun and its independent diffuse term. An animated SDR UV
  transform changes the sampled texel without leaving the prior frame's color.
- **Terrain and grass:** an archive fixture follows the T2LM table for all
  sixteen areas and checks missing, day-only, and distinct day/night maps.
  The day-only bank occupies 342 KiB; both maps take 684 KiB at the 64-pixel
  ceiling. Pixel probes cover day, twilight, night, disabled lightmaps and
  relighting. The grass test crosses a tile boundary, proving that the area
  center selects the light, not the hit's x/z. Missing night preserves the
  terrain day sample but leaves grass unlit by the bake.
- **Posed SKI/GFX:** face stages go through the actual posed material feed;
  body mask alpha changes skin dye, not opacity; the eye highlight and
  eyeball gloss use their own images. A red SKI sheet with 50% material and
  50% map alpha transmits 75% of the blue behind it; the sheet doesn't write
  depth. The GFX skin feed preserves its scaled lighting coefficients without
  making alpha/additive meshes opaque.
- **Normal and glossy response:** an NRF tangent direction steers lighting
  but leaves surface depth unchanged. A single-sided test mesh is rejected
  from behind. An authored red specular channel reflects the neutral test
  panel as `[0.25, 0, 0]` in linear light. The mip/anisotropy subgroup still
  checks the Step 10 grazing contrast and preserved lower-mip color.

These are deterministic fixtures, not a retail image error bound. Real NRF
encodings, historical shader-registry variants, and nonstandard blend/depth
order are not comprehensively matched. Day/night comparisons on real maps
and physical ray-query GPUs remain in Step 21.
