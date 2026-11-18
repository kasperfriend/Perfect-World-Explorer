# Ray water geometry and authored materials (Step 13)

Water surfaces use the same wet cells (mask bit 0), UVs and clipped triangles
as native. Native may combine equal-height draws; Lighter keeps a BLAS and
world-space bounds per export ID. Dry cells and gaps have no interface and no
water depth. For a submerged camera the nearest drawable water plane above
its wet cell selects its start medium. Native's broader fog predicate accepts
any nonzero cell flag; flag-only cells do not initialize a Lighter medium
with no matching interface. This step does not close lateral water boundaries
or solve volume lighting; see Step 17.

- The main water normals animate with the two native DDS frames and their
  UV phase. These are **shading-only** normals: the intersected triangle's
  face remains the plane of the interface, and rejects wrong-side directions.
- A simple-water area uses the native 30-frame art, 33 ms frame selection,
  scroll and encoded `BLENDTEXTUREALPHA`/alpha-add stages. It does not reflect.
  Its remaining alpha passes a straight ray into the wet body when the bounce
  budget permits; it never changes the cell geometry.
- Authored `wave1.dds` shoreline strips share native positions, UVs,
  day/night colors, 400-unit x/z vertex fade and three scroll phases. The
  strips blend in ray space over the opaque bed/water without writing depth
  or blocking secondary rays. The ray compositing itself is linear-light,
  not the native encoded attachment's blend. They are overlay geometry, not
  added waves on the refractive interface.
- Water reflection/refraction visibility persists through later material
  bounces. Unrecovered static-water-mesh formats are not synthesized from a
  plane or from decorative shoreline strips.

## Evidence (2026-11-18)

`5ac7dd8`: native shoreline shader/mesh tests (5), water reader tests (9),
wet-cell initialization and Naga validation passed locally. The Software,
CPU Vulkan ray-query and streamed pixel suites passed on Mesa 25.0.7
(`GALLIVM_PERF=nopt`). The ray checks compare the native simple-water stage
formula and shoreline formula with known inputs, including a dry gap
between equal-height bodies and reverse-Z depth that remains on the bed
when the shoreline is present or clipped. The earlier archive fixture
still covers reflected and refracted BMD geometry. A physical-GPU comparison
is separate.

Matching v12 retail `a07` has one water owner (export 381, referenced in 26
blocks): 32 drawable cells of 1024, height 330.8305, 32-unit cells and 18
shoreline strips. The [retail job](../../.github/workflows/lighter-retail.yml)
uses eye `(16, 356, 84)`, yaw `0`, pitch `−0.60`, hour `12`, and the same
adapter for Native and Software. The first attempt, `44415303447`, failed
before rendering while fetching the identical empty cloud header; it is
not visual evidence. The retry copies the hash-verified 32-byte header from
`a01` instead of fetching a duplicate. The [second attempt](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44415619628)
stopped at an unsupported T2LM record. A third run disabled baked lightmaps
in both backends and [passed](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44416746078)
on `234075e`. Its [artifact](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44416746078/artifacts/14480306616)
contains both PNGs, the Software count gate (one water body), adapter logs,
and hashes/coverage for the other tested maps. The capture is CPU Vulkan
and does not diagnose the unsupported lightmap. The sandbox could not
retrieve the signed artifact blob to inspect the pixels. Keep this distinct
from the checked synthetic shoreline material comparisons above.
