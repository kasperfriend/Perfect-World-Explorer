# Session handoff — 2026-11-04

Both CI jobs are green on this branch. This note records what was wrong,
what fixed it, and the one piece of work that is deliberately not done.

## CI state

| job | result |
|---|---|
| Rust checks and tests (Linux) | **pass** — all steps |
| Windows executable (x64) | **pass** |

Linux steps 6–14 all pass: x87 regional arithmetic, requirement status
audits, `cargo check --bins`, gfx-light audit, viewer unit tests, Vulkan
install, all 22 Phase-1 shader checks, terrain per-view GPU, and
`cargo fmt`. PR #18 reports `MERGEABLE` / `CLEAN`.

Status checker: **39 Implemented / 36 Partial / 1 Not implemented,
consistent, 0 errors, phase 1 complete.**

## Why the branch had never actually been tested

`Check formatting` was the **second** step of the Linux job, so every run
died there and `cargo check`, the unit tests and the whole shader suite
were **skipped on every commit**. Moving formatting to the end exposed a
real backlog:

| defect | file |
|---|---|
| double mutable borrow in the skin fallback loop (E0499) | `model_asset.rs` |
| `EcModel` fixture missing `additional_skins` (E0063) | `tools/fixtures/model_draw.rs` |
| `y` not in scope in the grass emitter (E0425) | `gpu.rs` |
| `is_empty()` on `Option<Material>` (E0599) | `bmd.rs` |
| v5 material fixture on the wrong side of the 48-byte block | `bmd.rs` |
| retail-shader test cut at the wrong `#[cfg(test)]` | `grass_instances.rs` |
| UTF-8 fixture fed to a GBK parser | `ecmodel.rs` |
| `GrassWind::update` missing after the module merge | `gpu.rs` |
| texkill keyed on an alpha the bump map does not have | `post_gpu.rs` |

## Getting the diagnosis out of CI

Raw step logs live on a blob host this sandbox cannot reach, and check-run
annotations are capped per run (the GPU checks fill the cap with
notices). Two things made failures readable:

- `cargo check` keeps its full message format and echoes `error` plus its
  own `-->` line; the test steps echo `test result` and the failing names.
- `cargo fmt --check` **already prints the "Diff in ..." hunks** —
  `rustfmt --diff` is not an option (`Unrecognized option: 'diff'`), which
  is why an earlier attempt posted an empty diff. The check output is now
  captured and posted as a PR comment.

## The shader suite had never actually gated anything

`run_check` was written as `if ! cargo run ... | tee log; then`. The tested
status belongs to `tee`, which always succeeds, so the failure branch was
dead code and **all 22 Phase-1 shader checks reported green regardless of
their result**. Capture the command's own status instead:

```bash
cargo run --locked --bin "$1" 2>&1 | tee "/tmp/gpu-$1.log"
status=${PIPESTATUS[0]}
```

Two consequences worth keeping:

- `Gpu::new` creates its shader module with **no validation error scope**,
  so invalid WGSL is not reported until pipeline creation on a real
  adapter. That is how a `cond ? a : b` (WGSL has no ternary; it is
  `select(f, t, cond)`) reached `main`'s grass injection and only failed
  when the viewer started.
- Once the checks gated, the first thing they caught was **their own
  reference oracle**, not the shader.

## Base tone: D3D's `exp` is 2^x, not e^x

This branch at one point "corrected" the refraction transmittance from
`exp2` to the natural exponential, rewrote the assertion maths and the
comments to match, and left `gpu.rs` and `terrain_gpu.rs` on base two.
That is backwards, and the GPU proved it:

| | grass fixture | BMD fixture |
|---|---|---|
| rendered pixel | `[4, 208, 7]` | `[12, 242, 22]` |
| base two | `[4, 208, 7]` | `[12, 242, 22]` |
| natural `exp` | `[4, 206, 7]` | `[12, 239, 23]` |

D3D9 defines `exp` as full-precision `2^x` (`expp` is the same base at
partial precision) and vertex assembly has no `exp2` opcode at all, so
`exp2` in WGSL is the faithful translation. `Findings/Research-log-02`
already recorded `exp2(...)` and warned in as many words that this is
"not an `exp`-base-e fog". Three shaders share the rule: grass
(`0x651319`), baked models (`0x7a91dd`) and terrain (`0x7c26e0`); grass
measures depth from the displaced vertex, the other two clamp above-plane
depth to zero.

## Grass wind: the default is 1.0 because there is no retail number

The reported symptom ("sways too much, models go completely wild") was not a
tuning preference. The recovered spring has no drag term — `0x6517f0` writes
`+0x50 = 0.0` and nothing writes it again — which is stable in the client
because the driver re-derives its force from its own arguments every frame.
Reused as a driven oscillator it is not: the grass noise samples at 20 Hz into
eight octaves (periods 12.8 s down to 0.1 s) that bracket the spring's
`2*pi*sqrt(+0x48)` = 1.99 s natural period, so it resonated.

| elapsed at 60 Hz | undamped displacement |
|---|---|
| 10 s | 0.90 |
| 60 s | 5.10 |
| 300 s | 1.07e5 |

The viewer now adds **critical damping at the recovered natural frequency**
(`2*omega`). No recovered constant changes, both gains derive from the same
`omega` so it cannot be frame-rate dependent, and the travel is bounded by the
target: 0.48 peak over ten minutes of logic time, against a 0.56 target bound.
The literal undamped step is kept in the tests as `client_step`, so the
transcription can still be checked against `tools/x87`.

On the amplitude question: the client has **no user wind scale at all**. Its
only wind inputs are the `CNoise` sample and the authored axis weights
`+0x6c..0x74 = (1.0, -0.5, 0.0)`, fixed by the constructor `0x650b40`. So
"the default game value" is 1.0 by construction — the recovered amplitude with
no viewer multiplier — and that is what the slider's mark shows. The slider
(`grass_wind_amplitude`, 0..2) scales the published history, never the driving
target, so the spring is always solved from the recovered bound.

## Outstanding, deliberately not done

`SkyGpu::new` again reads `sky_image01..03` from `configs/element_client.cfg`
and falls back to `textures/sky/clear/<name>` per slot, so the sky is no
longer pinned to the `clear` set. Retail's own `[DefEnv]` points all three
slots at `Sky\clear`, so released data renders identically; a modded cfg
with a second authored set is now honoured.

P3.01 (mesh v2/v3) stays `Implemented` on spec + synthetic evidence; it
has **zero corpus occurrences**, and that caveat is written into the row.
