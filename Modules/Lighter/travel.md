# Live travel capture (opt-in)

Use this for **normal-present cadence and streamed crossings**, separately from
[matched image quality and GPU-stage captures](retail-matrix.md). Each row
is recorded after an ordinary wgpu `present()` call on the requested route.
No pixels are read back, there are no extra GPU completion waits, and the
CSV is written only after the final sample. The first windowed present, device
setup, pipeline creation and warm-up are excluded. The frames run at their
actual wall-clock cost. The camera path is indexed by completed present calls,
so different frame rates still produce the same poses.

## Set up a matched path

1. Use the same release build, compatible client/map, Vulkan driver, monitor
   refresh rate and window resolution. For physical acceptance, run on the
   intended RTX 3080 and record the exact GPU model, driver, OS, display
   mode, build SHA, client revision and cache state. Don't pool runs made
   with different settings or shader-cache warm-up. Close other viewers and
   avoid overlays, manual input, screenshots and other GPU-heavy tasks.
2. Select the map and scout a path with **F8** camera marks. Cross more than
   one terrain/scenery boundary; include a final hold at the last pose so
   in-flight windows can settle. Use the same plan file for every run.
   The plain text format is a `map-id hour warmup-presents` header followed
   by `present-index x y z yaw pitch` rows. Indices start at zero, increase
   strictly, and interpolate linearly. Angles are radians; unwrap yaw
   between points if necessary. Pitch is limited to `[-1.5, 1.5]`.
   `#` starts a comment. The last index is inclusive. The tool accepts 1–600
   warm-up presents and 30–30,000 measured presents. Aim for hundreds of
   measured frames per route. For example (replace with your scouted marks):

   ```text
   a07 12 120
   0   16 356 84 0 -0.60
   180 80 356 84 0 -0.60
   420 80 356 84 0 -0.60
   ```

3. Set **Graphics → Upscaling and Frame generation** to **Off** and Native
   resolution scale to 100%. Keep the same lighting, cutoff, scenery, weather,
   latency, UI/HUD and whole-world settings. Turn off Lighter adaptive scale
   and opt-in ray texture/cache experiments for the control. Use identical ray
   resolution/samples/bounces/etc. in the ray routes. Settings changes, resize
   and a detected ray fallback invalidate the recording instead of changing
   the benchmark halfway through.
4. Launch the normal viewer with `WGPU_BACKEND=vulkan` and set three additional
   environment variables: `PW_LIGHTER_TRAVEL_PLAN` = absolute path to the plan,
   `PW_LIGHTER_TRAVEL_CSV` = **new file** in an existing output directory, and
   `PW_LIGHTER_TRAVEL_ROUTE` = `off`, `software`, `hardware` or `hybrid`.
   Set them before launching, e.g. in PowerShell:

   ```powershell
   $env:WGPU_BACKEND = 'vulkan'
   $env:PW_LIGHTER_TRAVEL_PLAN = 'C:\bench\path.txt'
   $env:PW_LIGHTER_TRAVEL_CSV = 'C:\bench\hardware.csv'
   $env:PW_LIGHTER_TRAVEL_ROUTE = 'hardware'
   .\perfect-world-explorer.exe
   ```

   Native starts first. For a ray route, explicitly use **Escape → Modules →
   Lighter → Reload World** with the matched setting (enable **Rasterized
   primary visibility** only for `hybrid`). The path waits for the chosen
   route. At the fixed first pose, warm-up holds until no window upload is in
   progress. It then advances on normal present calls; the in-game hour is
   pinned and the scene uses a fixed 16 ms logic tick during the flight.
   This does not freeze asynchronous asset admission, simulate a fixed wall
   speed, or reproduce animation phase on retries. Wait for the
   `normal_present presents saved` message before closing. Use separate files for warm
   repeats and for the Native, Software, Hardware and Hybrid routes.

## Read the result

```sh
python3 tools/lighter_travel_report.py --physical --device 'RTX 3080' \
  --require-commit --require-heap native.csv software.csv hardware.csv hybrid.csv
```

- The report rejects unmatched plans, driver/adapter, extents, camera poses,
  graphics settings and, among ray routes, effective settings and actual
  optional path admission. Check the printed exact adapter name (the device
  argument is a substring). `--require-commit` rejects a run without a
  resident window publication. It does not assert the same number of
  successful uploads; asynchronously built windows may be coalesced.
  A synchronized `--require-sdk-matrix` further requires Windows x64,
  the RTX 3080 10 GB discrete Vulkan device, per-frame heap/stage samples,
  and all seven routes. CSVs report architecture and largest device-local
  heap capacity separately from sampled heap usage.
- `interval_ms` is time between returns from `present()`. The report gives
  p50/p95/p99, worst interval and `1000 / mean(slowest 1% of intervals)`.
  It splits intervals observed during streaming/publication from the rest.
  `redraw_ms` is CPU redraw start to present return; acquisition and the
  present call itself have separate CPU durations. These are not GPU frame
  time, missed-vblank detection, pixel scanout or input latency; compositor
  pacing and queued work still affect them.
- The Vulkan `VK_EXT_memory_budget` process **device-local heap estimate**
  includes Native/wgpu and raw Lighter allocations on that adapter. Use the
  maximum of the sampled values and retain the trace, not just the tracked
  ray allocator bytes. Samples are taken every 30 rows and at commits;
  short allocation spikes can be missed. Values may include shared memory
  on integrated/CPU adapters or be unavailable altogether. This is not an
  exact VRAM-residency counter or a budget guarantee. Supplement it with OS
  or vendor process memory telemetry if the target is an absolute VRAM cap.
- Hybrid needs a separate [matched image / stage capture](retail-matrix.md)
  so **raster + traversal** can be compared alongside complete frame cost.
  Neither a smaller traversal marker on its own nor a CPU Vulkan fixture
  establishes a physical performance improvement.

The Xvfb smoke test uses a 160×96 synthetic Native viewer and verifies the
CSV comes from the regular present path, without screenshot readback. It
confirms capture plumbing, not a real-map crossing or a physical benchmark.

## Separate motion and synchronized runs

Set `PW_RAY_RECONSTRUCTION=off|fsr|rr` before launch for the ray routes and
follow the [verified SDK setup](temporal-reconstruction.md). The recorder
invalidates the run if the provider used differs from the requested one.
For each route, use the same plan within each of these distinct modes:

- **Normal present:** leave `PW_LIGHTER_TRAVEL_SYNC` and
  `PW_LIGHTER_TRAVEL_CAPTURE_DIR` unset. Use the frame intervals for cadence
  and streaming tails, never as GPU scanout. For the matched portable cache
  A/B, use `--reuse-ab` with one default and one `PW_RAY_SCREEN_REUSE=both`
  run of the same route. Keep progressive/history settings matched.
- **Motion PNG evidence:** set `PW_LIGHTER_TRAVEL_CAPTURE_DIR` to an empty
  directory; use a 30–600 frame plan. It writes `000000.png` onward from the
  world-color target before UI and display composition. Readback stalls
  this path. Its CSV has `capture_policy=motion_readback` and is rejected by
  both timing reports. For each provider retain the path and frame numbers
  of ghosting, reveals, foliage and water flicker; a replay cut can be
  represented by consecutive keyframes.
- **Synchronized GPU stages:** set `PW_LIGHTER_TRAVEL_SYNC=1`, unset motion
  capture, and use **500+** measured frames (last index ≥499). It drains
  the wgpu queue and raw fences before presenting, records current-draw
  GPU stages if available, and samples process device-local heap per frame
  plus warm-up. Stage markers exclude WSI/scanout; completion polling
  changes cadence and must not replace the normal-present runs.

```sh
python3 tools/lighter_travel_report.py --require-sdk-matrix \
  native-sync.csv software-sync.csv hardware-sync.csv \
  software-fsr-sync.csv hardware-fsr-sync.csv software-rr-sync.csv hardware-rr-sync.csv
```

The strict suite requires the exact Windows RTX 3080 Vulkan adapter,
matched controls, and per-frame heap, ray trace and (where applicable) SDK
samples. [Acceptance status and host requirements](REMAINING.md).
