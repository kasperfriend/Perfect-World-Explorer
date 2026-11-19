# Lighter game-mode performance results — waiting for target capture

**No physical results are available.** Neither reported Windows startup failure
has been reproduced or attributed. No change in this round is promoted to a
default. **S** is synthetic/unit/CPU Vulkan evidence, **P** is a verified
portable/Native capture on Windows x64 / RTX 3080 10 GB, and **V** requires
actual SDK dispatch there. A provider request is not provider evidence.

## Target measurements

| 2560×1440 a07 route | Evidence and actual provider | Normal median / p95 / slowest 1% | Synchronized GPU p50 / p95 / max | Process heap peak |
|---|---|---:|---:|---:|
| Native Vulkan | Not received | — | — | — |
| Software RT | Not received | — | — | — |
| Hardware RT | Not received | — | — | — |
| Hybrid | Not received | — | — | — |
| Hardware + RR | Not received | — | — | — |
| Software + FSR | Not received | — | — | — |
| Proposed game profile | Not received | — | — | — |
| Native DX12 (separate control) | Not received | — | — | — |

| Gate | Current evidence | Criterion |
|---|---|---|
| Windows Hardware startup | 0/2 runs; no launch, WER/Display or dedicated-memory receipts | Requested renderer/provider reaches first present twice; at least 60 s from first exit to second launch. No crash, device loss or timeout. |
| Normal-present cadence | Unmeasured | Median ≤16.6 ms; slowest 1% mean ≤25 ms. CPU-observed present intervals, not scanout or GPU cost. |
| Synchronized ray scope | Unmeasured | ≤5 ms; report AS/raster/trace/resolve separately. This excludes viewer UI and scanout. |
| Streamed crossing | Unmeasured | A measured commit on the same route; no >50 ms interval during measured streaming/commit. |
| Long travel | Unmeasured | ≥600 s of active travel; sampled process device-local heap estimate strictly below 7,000,000,000 B. Not NVIDIA device-wide dedicated memory or the raw allocator’s 6.5 GB cap. |
| SDK smoke | 0/8 cases | Both requested providers × Software/Hardware × 50/100%, strict raw finite positive SDK radiance at 512² and 640² with pan/cut/resize. Fallback is failure. |
| Visual parity and promotion | Open | Matched retail color/depth/effects and motion, repeated wins on target, ten consecutive clean starts before promoting a default. |

The earlier [retail matrix](retail-matrix.md) remains **0/54 physical cells
measured**. This one a07 flight is not the RT-17 matrix and will not substitute
for it. Warp, destination-color and map-wide grass limitations are unchanged.

## S results available now

- **Startup:** the bisection and preflight checks with no vendor DLL passed.
  The watchdog advances on a strictly increasing stage ordinal. The staged
  9×9 Hardware draw published 1 copy and 3 BLAS batches. On the final small
  CPU-Vulkan run, the largest recorded paced submit was **0.119689 ms**;
  this does not predict retail driver time. The cache wrote and reopened a
  driver-accepted **32-byte** cache across two device lifetimes. No cached
  pipeline speedup was measured. Two injected failed starts released the
  charged allocations and resource owners to baseline; driver recovery was
  not simulated. [A1](evidence/perf/S-A1.md) · [A2](evidence/perf/S-A2.md) · [A3](evidence/perf/S-A3.md)
- **Native regression:** 960×720 synthetic PNGs with reconstruction requested
  as `off`, `rr` and `fsr` all matched the existing Native control byte for
  byte, SHA-256 `1b20187d05a3cacc8701e13ccbe4db5b13630fc48b4c0ec41aee39fb1f4c1798`.
  No expected hashes were changed. SDKs were not dispatched.
- **Hybrid/proxies:** [hosted CPU Vulkan](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44568900822/job/135718210202)
  passed `lighter_raster_primary_parity` on `0b4b485`: RGB8 and reverse-Z
  limits against full rays, executed first-solid counters, water/effect
  subprobes and full-ray fallback controls. The sandbox CPU ICD did not
  complete the same suite. Exact redundant-tail proxy controls retained
  2 / 20 / 20 triangles vs 20 / 20 / 20; no class LOD or retail savings claim. [A4](evidence/perf/S-A4.md) · [A5](evidence/perf/S-A5.md)
- **Targeted effects:** both game-profile shader variants lower; the
  isolated reflection and sun signal fixture changed RMSE from
  **0.180000 → 0.035973** in linear units, with motion/rejection controls.
  It is not an integrated denoiser or an old-vs-new moving retail comparison.
  [A6](evidence/perf/S-A6.md) · [A7](evidence/perf/S-A7.md)
- **Provider negatives:** missing/wrong inputs did not pass the requested
  SDK smoke. Extent rejection and the injected FSR stand-in submit failure
  rejected the SDK route and retained the portable controls. Those are S,
  not FSR or RR runtime draws. [A8](evidence/perf/S-A8.md)
- **Tail/heap controls:** adaptive timing hysteresis, Naga lowering and raw
  allocation cap tests passed. The staged compaction test drew the tiny
  static scene, with no charged savings reported. The 10-minute heap run,
  retail timing and raster/trace balance are open. [A9](evidence/perf/S-A9.md) · [A10](evidence/perf/S-A10.md)

The final local regression suite passed **1,350 Rust tests** (39 ignored,
two pre-existing CPU-driver GFX failures filtered) and **33 Python tests**.
[Hosted checks for the capture revision](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44568900822)
also passed Linux fixtures and the Windows x64 kit build/preflight.
Local S receipts are in `cache/lighter-perf/` and in `cache/Lighter-Perf-S.zip`;
[handoff](HANDOFF.md) lists exact revisions, operator steps and remaining checks.

## On receipt of the Windows ZIP

Keep the original ZIP and SHA-256. Use the exact commit and, when available,
the trusted `build.json` from the downloaded GitHub kit, not a revision copied
from an unverified receipt (Python 3.9+):

```sh
python3 tools/windows/analyze-perf-session.py /path/to/Lighter-Perf-….zip \
  --expect-commit 0b4b48527aab9dcd030565df0129622734051c07 \
  --expect-build /path/to/trusted-kit/build.json
```

`--expect-build` is optional. The analyzer verifies inventory and provenance,
refuses unsafe ZIP paths, keeps timing policies separate and writes this file
with links to the raw receipts. Exit 0 means the **recorded** numerical and
route gates passed, 1 means at least one is open/failed, and 2 means intake was
rejected. Review events and the operator’s observation before assigning a
cause; a clean query window alone cannot rule out a delayed OS report. Keep
returned raw captures out of Git.
