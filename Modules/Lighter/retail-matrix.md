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
  collected separately from the logs. Keep [synthetic GPU budget results](gpu-budget.md)
  separate from these retail captures.

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
