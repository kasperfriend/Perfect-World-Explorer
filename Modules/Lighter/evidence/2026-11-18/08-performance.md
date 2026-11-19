# 8 — RTX 3080 timing and memory (blocked)

[Host blocker](01-sdk.md). **Physical sample count: 0.** Neither existing
synthetic CPU GPU-stage samples nor prior retail CPU PNG timings measure
this GPU. No physical medians, tail percentiles, peaks or reuse deltas were
observed.

| Route | Class | Post-warm-up RTX frames | Frame p50 / p95 / 1% low | GPU stages | Driver heap peak |
| --- | --- | ---: | --- | --- | --- |
| Native | P | 0/500 | Not measured | Not measured | Not measured |
| Software | P | 0/500 | Not measured | Not measured | Not measured |
| Hardware | P | 0/500 | Not measured | Not measured | Not measured |
| Software + FSR | V | 0/500 | Not measured | Not measured | Not measured |
| Hardware + FSR | V | 0/500 | Not measured | Not measured | Not measured |
| Software + RR | V | 0/500 | Not measured | Not measured | Not measured |
| Hardware + RR | V | 0/500 | Not measured | Not measured | Not measured |

`PW_LIGHTER_TRAVEL_SYNC=1` is an opt-in audit: 500+ measured presents,
actual wgpu queue / raw fence retirement before each present, optional GPU
AS/primary/trace/reproject/filter/ray-composition stages and a Vulkan
SDK-command-span timestamp pair. Missing timers remain empty. The CPU
present-call and queue-drain wall times are separate, **not** GPU scanout or
compositor time. GPU ray composition is not whole-viewer GPU presentation.
The synchronization and polling themselves alter cadence, so retain a
separate normal-present run. Motion PNG readback cannot be combined with
this audit. `lighter_travel_report.py --require-sdk-matrix` requires all
seven provider/route receipts on the exact Windows x64 RTX 3080 10 GB Vulkan device
and per-frame heap/trace/SDK samples.

Sample Vulkan process **device-local heap** usage each measured synced
frame (and warm-up), not renderer allocation accounting. Log a warning
at or above 7,000,000,000 B. This is a driver estimate at sample points;
exact unsampled peak or OS dedicated residency is not implied. Native is
run on Vulkan for this matched driver-heap comparison.

**Reuse A/B missing:** no physical pair at matched settings with default
and `PW_RAY_SCREEN_REUSE=both`. The report accepts a deliberate `--reuse-ab`
pair without relaxing other ray settings or camera matching. Keep separate
PNG/noise and normal-present records, rather than claiming noise gains from
frame time. See [Step 13](13-renderer.md) for prior fixture scope.

**S completion checks:** the Vulkan timestamp stand-in passed both extents
and fence-retired resize on `99f7564`. Later coverage tests bind three
synchronized Software draw samples to the submitted frame at 240×180 rays,
not to an earlier same-extent query. These are synthetic headless draws and
not the 500-present benchmark. See `08-performance/` and `10-water/`.

[Hosted Linux/Xvfb](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44523578638/job/135558183606) separately passed a **500-present
Native synthetic fixture** (synchronized, 160×96, no ray or SDK stages)
and the 30-frame normal-present control. It tested CPU queue-drain receipt
shape, not process heap sampling on RTX or any of the seven physical routes.
