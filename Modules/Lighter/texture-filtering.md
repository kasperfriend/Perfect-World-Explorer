# Ray texture filtering — Step 10 (RT-05)

The [texture-bank layout and controls](README.md#ray-texture-filtering) apply
identically to Software BVH and Vulkan ray queries. Keep authored levels when
they fit, clamp short chains, and report generated/reduced tiles separately
from texture decode or lookup failures. Rays project a cone through the
world-to-UV Jacobian, then choose trilinear LOD across the narrow axis and
average up to the configured tap limit along the wide axis. The sky uses the
angular footprint of an escaping ray; water normal maps use the UV footprint
of the surface hit without an sRGB transfer.

## Checks (2026-11-18)

- The [Linux CI ray and streamed capture suites](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44408935349)
  passed on `c507d52`: Software BVH and CPU Vulkan query pixels, including the
  distinct green lower mip on a red wall, linear-light black/white bilinear
  average, cutout holes and stable minified foliage coverage. Coverage metadata
  includes the four mip counters alongside, not instead of, Step 9 accounting.
- Unit fixtures cover rectangular and short authored chains, cap-to-authored
  mip, missing/truncated lower levels, filtered linear masks, failed admission
  without counter changes, and grass texture reuse when native atlas slots
  move. Identical DDS level zero with different authored tails doesn't alias.
- In the grazing test a 128×128 brick pattern is sampled at 3.58° above tangent
  with 33×33 ray pixels. Each line of eight center-ray positions moves by
  1/16 world units per sample; probes verify depth as well as color. Contrast is
  mean red in the brick row minus mean red in the mortar row; pan range is
  max red minus min red in the brick row. Both metrics use linear 8-bit
  readback. Software/lavapipe and an isolated WGSL sampler probe agreed:

  | Anisotropy | Brick–mortar contrast | Brick red pan range |
  | ---: | ---: | ---: |
  | 1× | 0 | 0 |
  | 8× | 97 | 2 |
  | 16× | 156 | 3 |

  The isotropic sample is stable by blurring away the mortar. Anisotropy
  retains it without large swings over that pan. These are tests of this
  footprint and mip chain, not a general temporal-aliasing bound or a GPU
  timing result. There is no physical-GPU performance measurement here.

The [retail rerun](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44408924284)
was configured for the same pinned `a01`/`a02` inputs as Step 9, at 8×. It
timed out during apt setup (exit 124), before the viewer built. There are no
new retail captures or mip counters from this run. The Step 9 captures
remain the baseline; a streamed retail grazing-wall test is still pending.
