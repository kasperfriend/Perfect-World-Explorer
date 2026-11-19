# Lighter game-mode performance plan — PR #51

This work extends the acceptance follow-up from `5a81c22` on PR #51. The default renderer and existing Native DX12 path are controls, not experiments. Do not promote a rendering change from **S** (synthetic/unit/CPU Vulkan) evidence. **P** means a recorded portable/Native run on the Windows x64 RTX 3080 10 GB; **V** additionally means actual SDK dispatch on that machine. An SDK request resolved by a different provider is a failure for the requested provider. No results from the target have been received yet.

Physical targets at 2560×1440: ordinary present intervals median ≤16.6 ms and slowest 1% mean ≤25 ms; synchronized ray GPU scope ≤5 ms (report the stage definition and distributions separately); sampled process device-local heap <7,000,000,000 bytes over ≥600 s of travel; successful first and second Hardware launches, with no device loss, timeout or crash. Normal cadence and synchronized measurements are different runs. Do not mix them, treat output-size metadata as proof of a real provider, or conflate tracked allocations with driver heap usage.

## Validation status

- [x] **Step 0:** record the plan first (`52b9712`).
- **S passed:** A1 preflight/bisection, A2 watch/pacing/cache reopen, A3
  injected teardown, A4 hosted synthetic parity and A8 negative controls.
  Physical/retail criteria remain open.
- **Partial:** A5 exact tail only; A6
  targeted dry-surface policy without effect counters/planes; A7 isolated
  SVGF only; A9 no real-world specialization/scale acceptance; A10 no
  physical heap trace or retail compaction savings verified.
- Windows validation is manual in Explorer. No target startup, timing, heap
  or vendor-dispatch result has been verified.

Checkboxes below cover the full acceptance, including physical/quality work
where listed. S-only progress must not be used to close them.

## A — Code and synthetic validation

- [ ] **A1 — Startup bisection and ABI/ownership audit.** Independently skip bootstrap, raw-device import, FFX, NGX and reconstruction image-view/descriptor work without using invalid handles or claiming SDK success. Audit enabled features and extension lists, C signatures/layout, opt-in loading and error cleanup. Acceptance: each skip works on the corresponding synthetic path, any fixes have tests. Evidence: `evidence/perf/S-A1.md`, reconstruction tests and Module README.
- [ ] **A2 — First-frame progress and cache.** Split pipeline/shader and BLAS preparation into stages/bounded submits while the old view stays usable; record chunk, BLAS and pipeline advancement. An unchanged label must not renew the timeout. Persist a validated Vulkan pipeline cache under `PW_CACHE_DIR`. Acceptance: S watchdog and cache tests; target build-submit times (aim <1 ms, not promised by a primitive cap) and repeated Hardware launches in Explorer. Evidence: `evidence/perf/S-A2.md`.
- [ ] **A3 — Failure cleanup.** A failed start retires buffers/images/features/device before retry. Acceptance: repeat two starts with injected failures, resource/ownership counters return to baseline (S); actual driver recovery and memory timeline still P/V. Evidence: `evidence/perf/S-A3.md` and cleanup tests.
- [ ] **A4 — Hybrid primary.** Use raster solid/cutout hits with honest, ordered ray fallback for glass/water/sheets/effects that cannot be represented. Keep Hybrid opt-in. Acceptance: S color/depth parity includes those cases; per-draw traced/skipped primary counts. Do not claim a win without matched cadence. Evidence: `evidence/perf/S-A4.md`, Hybrid parity and timing/diagnostic receipts.
- [ ] **A5 — Secondary geometry.** Opt-in RT-only proxies (~10% triangles for suitable mesh classes), size culling for small props/grass, distant merged proxies, shared static BLAS and compaction, refit animated actors only. Acceptance: synthetic RT triangle/instance counters and conservative attachment/material/geometry tests; physical reflection/shadow error thresholds separate. Evidence: `evidence/perf/S-A5.md`, proxy tests and on-target observations.
- [ ] **A6 — Targeted ray effects.** Opt-in glossy/metal reflection sampling with roughness and probe policy; near/contact sun RT with distant raster shadow fallback and N·L rejection; short bounce over baked/probe indirect; per-effect distance tiers and counters. Acceptance: per-effect rays per pixel and fixture controls; offline Reference/progressive unchanged. Evidence: `evidence/perf/S-A6.md`, targeted-ray tests.
- [ ] **A7 — SVGF and sampling.** Opt-in reflection/shadow terms, temporal reprojection and variance, five spatial à-trous iterations with depth/normal/variance edge stopping, material/motion rejection, build-generated blue noise. Acceptance: synthetic reference RMSE and moving-camera ghosting comparison against old filter, measured separately. Evidence: `evidence/perf/S-A7.md`, filter/sampling tests and captures.
- [ ] **A8 — Provider fail-closed.** Negative smoke coverage for missing SDK/bad hash/rejected extent/failed command/portable; record RR → FSR → portable in explicit fallback experiment, never allow fallback to pass a requested SDK trial. Acceptance: S negative tests; finite non-black real SDK output for each requested extent. Evidence: `evidence/perf/S-A8.md`.
- [ ] **A9 — Performance infrastructure.** Opt-in 50–100% adaptive ray/internal resolution with timestamp feedback, Naga-validated WGSL specializations (media/reference/emissive/transmission), bounds and roulette/single-branch early exits; no GPU readback waits in ordinary play. Acceptance: synthetic controls for changed behavior; measure deltas on target. Evidence: `evidence/perf/S-A9.md`, shader/scale tests.
- [ ] **A10 — Streaming and memory.** Reuse/compact per A5, worker-owned chunk and texture decode, 6.5 GB ceiling policy and 7 GB warning from driver heap when supported; tracked estimate explicitly labeled. BC stays opt-in. Acceptance: S budget/warning tests; no 10-minute memory claim without target. Evidence: `evidence/perf/S-A10.md`, memory tests and on-target heap observations.
