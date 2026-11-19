# Real-map capture matrix (RT-17)

**RT-17 is open.** The checked-in [retail manifest](../../tools/fixtures/lighter-retail-scenes.json)
contains three compatible v12 instances: the BMD/GFX-heavy `a01` (City of
Abominations), the flat `a02` instance, and the `a07` wet/dry shoreline. It
requests each camera at noon/clear, midnight/clear and noon/rain. A capture
on lavapipe is a retail-asset result, not physical GPU evidence. A map name or
a source inventory alone does not prove that a particular subject appears in a
camera's frame.

| Required subject | Current scope | Still needed |
| --- | --- | --- |
| Dense city | `a01` is an instance with 328 authored BMD owners; it is useful scenery evidence. | Visually approved dense-city camera; matched physical captures. |
| Forest | Vegetation pixel/motion fixtures and mainland source counts only. | Complete installed client with its matching baked SpeedTree cache, a visible forest camera, all three renderers. |
| Open distance | Height audits and a flat instance; not a long-distance mainland view. | Camera above the near/far seam and a grazing skyline; compare the seam and skyline on real terrain in each mode. |
| Water | `a07` shoreline at `(16,356,84)`, yaw `0`, pitch `−0.60`. | Physical captures; confirm a matched client with usable lightmaps and review the shoreline/reflection and submerged view separately. |
| Cave/instance | `a01`, `a02`; authored BMD/GFX totals are enforced. | Physical captures and localized inspection; wood/rock textures and missing content can both affect pixel error. |
| Effects-heavy | Authored `a01` GFX roots and admitted draws, including reasons for inactive/pending roots. | Physical captures, a frame with visible authored effects, plus inspection of their ordering, lights and night/rain behavior. |

## Reproduce and extend

Use the optional **Lighter retail capture matrix** GitHub Actions workflow
for the checksum-pinned CPU Vulkan subset. It collects PNGs, Step 9 counts,
metadata, absolute-difference PNGs and `matrix.json` (including any failed
cases). No game packages are uploaded. The pinned `a07` data currently
requires terrain lightmaps to be off for **both** modes. An optional asset
missing from an input tree remains an omission, not a parity pass.

On a Linux or Windows host with a Vulkan ray-query GPU, build Explorer from
this revision. Install Pillow in a Python 3.10+ environment. Open a complete
client with the appropriate tree cache, scout the camera poses, and copy the
manifest to a separate file. The viewer's **F8** camera mark logs XYZ, yaw,
pitch and hour. Retain the same pose for each condition. Use `city`, `forest`,
`open_distance`, `water`, `cave_instance` and `effects` to declare the intended
subjects. Give the cases distinct IDs and use `min_loaded` for the relevant
inventory rows (for example `trees` in a forest or `effect_draws` in an effects
case). Add `regions` with normalized `[x0,y0,x1,y1]` rectangles around the
building, water surface, canopy, skyline, or effect being reviewed. Use the
actual PNGs to choose those rectangles; the generic screen bands are not
object masks.

```sh
python3 -m pip install Pillow==11.3.0
python3 tools/lighter_capture_matrix.py \
  --profile physical --require-complete --timed-frames 30 \
  --manifest /path/to/verified-cameras.json \
  --viewer target/release/perfect-world-explorer --client /path/to/client \
  --output target/lighter-retail-physical
```

Add `--hybrid-ab` to the physical run to render a fourth path: the same
Hardware settings with raster primary visibility enabled. The tool rejects a
full-ray fallback for this variant and writes both the Hardware-vs-Hybrid
regional diff and per-stage timing. At the headless CLI level,
`PW_PREVIEW_HYBRID_PRIMARY=1` requests that variant and
`PW_PREVIEW_SINGLE_PATH=0` uses split transport for a control.

Use the `.exe` build on Windows. Choose the expected Vulkan adapter on a
multi-GPU host; the tool rejects a CPU/virtual adapter for a physical report,
and Hardware mode fails instead of substituting raster or compute. Each pair
must agree on adapter/driver, extent, camera/frustum, time, animation/weather
step, lightmaps and other shared controls. Software and Hardware must also
agree on effective ray settings. The physical completeness check tests the
**declared** scene/condition cells, timing samples and successful captures;
it does not inspect the artistic subject or assert pixel parity.

- Matched captures are 960×720 sRGB. The viewer's **Native renderer** is the
  baseline, not a frame from `elementclient.exe`. The default is 480×360 rays
  (50%), one sample, one bounce, 8× anisotropy, no progressive accumulation.
  The CPU workflow uses 240×180 rays (25%) to bound CPU ray-tracing time;
  it does not establish quality or performance at interactive defaults.
- Fixed 16 ms logic warm-ups run once per frame, followed by a zero-delta
  capture. Schema 3 freezes both the logic clock and elapsed animation phase
  for water/vegetation. The image is hashed as decoded RGBA8 and verified
  before diffing. The inventory is from the pictured frame, not a later
  timed frame while assets/effects may continue to be admitted.
- Each region records the mean absolute difference of **encoded RGB8**
  channels (0–255, alpha excluded), plus the per-channel means and pixel boxes.
  Differences include lighting, LOD, admission, reconstruction, blending and
  authored texture omissions. There is no approved numerical parity threshold.
- Optional frame samples time a warmed draw **and synchronized GPU readback**.
  Report p50 and nearest-rank p95, sample count, device and driver. These
  values are not live FPS or GPU-only stage durations. The asynchronous
  Software/Vulkan build, trace and filter samples, when supported, are
  collected separately from the logs. `gpu_completed_stage_ms` records
  sampled GPU stages for the warmed draws when available; `primary` and
  `traversal` should be considered together in the Hybrid comparison.
  Keep [synthetic GPU budget results](gpu-budget.md)
  separate from these retail captures.

## Completed CPU Vulkan matrix — run started 2026-11-18 UTC

[Run 44424106472 (`7c57586`)](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44424106472)
passed all nine Native/Software pairs. The
[`lighter-retail-matrix` artifact](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44424106472/artifacts/14483069898)
contains 18 captures, nine absolute-difference images, and the inventories,
metadata, logs and structured measurements. The runner calculated the values
below from the decoded PNGs on **llvmpipe, Mesa 25.0.7 / LLVM 19.1.7**. Signed
blob downloads were unavailable in this workspace, so the region metrics are
reported from CI, not from visual inspection here.

Both modes used 960×720 captures with a fixed animation clock. Software
traced 240×180 rays. Noon at hour 12 is used for clear day and forced rain;
night is hour 0/clear. Warm-up was 24 steps for `a01`/`a02` and 12 for `a07`.
Terrain lightmaps are on for `a01`/`a02` and off for `a07` in both modes.

| Camera / condition | Whole | Upper third | Middle third | Lower third | Center |
| --- | ---: | ---: | ---: | ---: | ---: |
| `a01-day` | 29.732 | 1.475 | 1.945 | 85.776 | 1.771 |
| `a01-night` | 56.248 | 31.965 | 51.533 | 85.246 | 51.487 |
| `a01-rain` | 29.713 | 1.489 | 1.960 | 85.691 | 1.805 |
| `a02-day` | 15.829 | 1.239 | 4.238 | 42.011 | 2.565 |
| `a02-night` | 40.314 | 32.095 | 46.834 | 42.011 | 47.721 |
| `a02-rain` | 15.834 | 1.259 | 4.252 | 41.992 | 2.592 |
| `a07-day` | 6.751 | 4.811 | 7.459 | 7.984 | 7.746 |
| `a07-night` | 5.877 | 4.125 | 6.677 | 6.830 | 7.305 |
| `a07-rain` | 6.798 | 4.893 | 7.513 | 7.989 | 7.840 |

| Map | Eye XYZ | Yaw / pitch | Ray buildings loaded / skipped / failed | GFX roots loaded / skipped / failed |
| --- | --- | --- | ---: | ---: |
| `a01` | `(0,260.39215,0)` | `0 / −0.15` | `316 / 12 / 0` | `25 / 279 / 0` |
| `a02` | `(0,332.56122,0)` | `0 / −0.10` | `22 / 10 / 0` | `25 / 17 / 0` |
| `a07` | `(16,356,84)` | `0 / −0.60` | `116 / 0 / 0` | `13 / 9 / 0` |

Counts were unchanged among the three conditions of each view. `a07` had
one authored water owner and admitted it under each condition. The large
lower-band daylight differences in `a01`/`a02` and the broader differences
at night need visual investigation. The smaller `a07` errors are **not** a
shoreline parity result. The measured regions do not isolate individual
surfaces or particular reflection/refraction and shadow paths.

| Camera / condition | Native p50 / p95 (ms) | Software p50 / p95 (ms) |
| --- | ---: | ---: |
| `a01-day` | 149.3/151.4 | 669.6/674.8 |
| `a01-night` | 113.8/115.9 | 654.2/655.8 |
| `a01-rain` | 115.9/116.8 | 1193.8/1201.8 |
| `a02-day` | 112.3/114.9 | 484.0/499.7 |
| `a02-night` | 111.8/115.9 | 476.8/478.9 |
| `a02-rain` | 114.1/115.4 | 989.8/991.5 |
| `a07-day` | 1639.3/1648.9 | 4381.2/4437.2 |
| `a07-night` | 1651.7/1674.2 | 3709.9/3717.9 |
| `a07-rain` | 1647.5/1653.8 | 5039.6/5123.6 |

Three warmed draw-plus-readback samples per mode; with three samples the
nearest-rank p95 is the maximum. These are CPU Vulkan capture diagnostics,
not live viewer frame times or estimates of physical GPU performance.
[Linux and Windows PR checks on the same code revision](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44424114678)
passed separately, including the synthetic ray pixels, inventory and live
recovery fixture. They do not add retail Hardware evidence.

## Earlier retail evidence (2026-11-18)

- [Run 44406210481](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44406210481),
  `7a71750`: **complete** earlier Software-only `a01`/`a02` capture with both
  inventories and PNGs (`lighter-retail-coverage`). See [the counts and hashes](scene-coverage.md#retail-software-captures--2026-11-18).
- [Run 44415619628](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44415619628):
  **partial** later refresh; `a01`/`a02` completed before the unsupported
  `a07` T2LM stopped it. No complete artifact or `a07` result from that run.
- [Run 44416746078](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44416746078),
  `234075e`: matched daylight Native/Software `a07` frames and one admitted
  water body, lightmaps off, in `lighter-retail-coverage`. The signed artifact
  download was unavailable in this workspace; no visual parity or regional
  error conclusion from that artifact alone.
- [Run 44422660964](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44422660964),
  `bfeee2d`: all nine Native/Software pairs at 50% ray resolution in an
  [earlier matrix artifact](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44422660964/artifacts/14483193403).
  It used schema 2, before the fixed elapsed-animation clock, and did not
  request synchronized timings. Do not treat differences between these two
  runs as a single-variable quality or performance comparison.
