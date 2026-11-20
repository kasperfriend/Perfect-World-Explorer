# Phase 5 agent 5C — P5.07, P5.08, P5.09

Owner: Agent 5C (environment transitions, sky/weather reflection + altitude,
Phase 5 CI wiring).
Branch `arena/e498c5d9-perfect-world-exlorer`, PR #15.
Native basis: `elementclient.exe` 1.4.2, MD5 `5f02895688fa580506e869156340a664`,
PE i386 image base `0x400000`, read with `objdump -d` (the same MD5 the existing
`src/*.rs` headers cite).

Scope respected: no change to sky geometry, sun/moon geometry, region settings
authoring, weather authoring or fog internals. Everything below consumes those
systems.

---

## 1. Transition / reflection state table

### 1.1 Sky resource swap — object state (`A3DSkySphere`)

| Native | Meaning | Viewer field (`sky_transition::Machine`) |
|---|---|---|
| `+0x5a4`, `+0x5a8`, `+0x5ac` | current picture handles (a whole set) | `current: Triple` |
| `+0x8c8`, `+0x8cc`, `+0x8d0` | pending picture handles | `pending: Triple` |
| `+0x5ba`, `+0x6be`, `+0x7c2` | the three name blocks | `pending`/`current` pair only; names are the loader's business |
| `+0x5b4` | counter | `counter` |
| `+0x5b0` | bound | `bound` |
| `+0x5b8` | direction: `1` counts **down**, `0` counts **up** | `count_down` |
| `+0x5b9` | textures still loading | `waiting` |
| `+0x4cc`/`+0x504` vs `+0x4e8`/`+0x544` | live vs incoming 7- and 16-dword interpolation blocks | `blend()` (linear reconstruction) |
| `[config+0x100/+0x104/+0x108]` | authored **day** triple | `day: Triple` (`author()`) |
| `[config+0x10c/+0x110/+0x114]` | authored **night** triple | `night: Triple` (`author()`) |

### 1.2 Entry points, exactly as implemented

| Address | Behaviour implemented |
|---|---|
| `0x79ac30` | the **guard**: `false` while `+0x5b4 != 0` (`0x79ac3a`) or `+0x5b9 != 0` (`0x79ac48`); `true` when any current handle is null (`0x79ac9e`) or the live block differs from the caller matrix (`0x79acd6`..`0x79ad13`); `false` when the requested names already equal the current handles (`0x79ad7d`). Never installs anything. |
| `0x79aeb0` | the **swap**: same two guards (`0x79aebc`, `0x79aec7`); sets direction `1` (`0x79aeed`); writes all three pending names; `+0x5b9 = 1`; `setle`/`dec`/`and` at `0x79b0d0`..`0x79b0dd` masks a non-positive count to `0`; that value goes to both `+0x5b0` and `+0x5b4`; count `0` runs `0x79ad90` inline and commits (`0x79b0f0`..`0x79b10b`), a positive count schedules it (`0x79b11f`). Same-as-current branch copies into `+0x4cc` and sets direction `0` (`0x79afb4`); same-as-pending copies into `+0x4e8` and keeps direction `1` (`0x79b050`). |
| `0x79a040` | the **tick**: early out on `+0x5b4 == 0` (`0x79a069`) or `+0x5b9 != 0` (`0x79a06d`); direction `0` → `+= 33`, commit at `>= bound` (`0x79a0a9`); direction `1` → `-= 33`, commit only when negative (`0x79a096`); `0x79a0c6` zeroes the counter before `0x79b180`. |
| `0x79b150` | **hurry**: inert without a counter (`0x79b156`), else `counter -= delta`, clamped up to `1` (`0x79b168`). |
| `0x79b180` | **commit**: pending handles → current slots, incoming blocks over live ones, pending cleared. |

### 1.3 Driver ladder (`0x4203f4..0x420807`)

| Phase (`config+0x70`) | Byte at `0x4204db` | Action | Address |
|---|---|---|---|
| `0` | any | day triple, uncounted | `0x420430` |
| `1` | any | night triple, uncounted | `0x420477` |
| `0 < p < 0.8` | `0` | day triple, `count = ftol(clock/2 · p · 1.25)` | `0x42054e` |
| `p >= 0.5` | `0` | night triple, `count = ftol(clock/2 · (1-p) · 2)` | `0x420666` |
| `0.8..1` | `0` | nothing | `0x420738` |
| `< 0.5` | `0` | nothing | `0x4207aa` |
| mid | `!= 0` | uncounted night pre-swap, then the counted swap | `0x4204f5`, `0x42060d` |
| any running swap | any | nothing (`+0x5b4` guard) | `0x420404` |
| counted | — | block on `+0x5b9` in 10 ms sleeps, then `hurry(clock/2 − count)` | `0x4205c0`, `0x420600` |

`0x4b6250` returns `0x249f00` (2 400 000) or `0x1b7740` (1 800 000), switching on
a float at `+0x74`: a 40- or 30-minute game day in milliseconds. `0x892e78` is
`_ftol` (truncating), and the `cltd`/`sub`/`sar` at `0x420559`..`0x42055c` is the
integer halving.

**The cadence, recovered (was the one substituted constant in P5.07).** The
driver divides that day clock straight into a counter that the tick moves by a
flat 33 per call, so the units only line up if the tick is the per-frame update
and 33 is a millisecond step. Both hold:

- `0x79a040` has **exactly one reference**, the vtable slot `0x8f14b0` of the
  `A3DSkySphere` vtable at `0x8f1490` (found by dumping the vtable, not by
  scanning for `call`). It is the object's owned per-frame update: there is no
  other caller and no config-sourced cadence — the binary has no frame-rate or
  frame-limit string at all.
- `0x8f14bc` (in the same vtable neighbourhood) holds `0x3d088889` = `1/30`, the
  factor `0x79a0e0` multiplies both interpolation blocks by. One tick is one
  Voodoo-3-era frame: **33 ms of fade**.

So a transition lasts `count` milliseconds of wall time. The counted bands
(`0.5..0.8` with `clock/2 * phase * 1.25`, `0.5..1` with `clock/2 * (1-phase) * 2`)
have a supremum of **half a game day** — 900 000 ms on the 30-minute setting,
i.e. 15 game minutes and **900 s of real time**; the other half of the day is the
uncounted endpoint switch at `phase <= 0` / `phase >= 1`. The viewer therefore
feeds `sky_transition::DAY_CLOCK_MS` (`0x1b7740`, the 30-minute setting; the
40-minute one is `0x249f00`) and replays the fade on elapsed time through
`Machine::step_ms`, whose sub-tick residual is integer milli-thirty-seconds so
that thirty ticks are exactly one second.

### 1.4 Reflection / altitude state (P5.08)

| Requirement | Implementation | Proof |
|---|---|---|
| sky must not shift with camera altitude | `sky_gpu::sky_inverse` clears the view translation (`rotation.w_axis = Vec4::W`) before inverting; `A3DSkySphere` only compares a cached position/radius tuple (`0x79acbd`..`0x79acd4`) and never feeds a position into a ray | `sky_gpu::reflection_tests::altitude_never_reaches_the_sky` (6 altitudes × 3 clip points × vertical and horizontal moves) and the GPU diagnostic's byte-exact comparison |
| sky correct in reflections | same inverse, mirrored about the water plane (`from_scale(1,-1,1)`), and the reflected uniform receives the **same** `(hour, celestial, alpha, sets, blend)` tuple | `reflection_mirrors_entire_ray_basis`, `sky_rays_follow_horizontally_corrected_camera_and_reflections`, GPU diagnostic requires the mirrored frame to differ from the normal frame |
| one per-frame snapshot | `Renderer::begin_frame` drives and steps the machine once, then uploads to both buffers; `sky_transition::view_sets` is the documented per-view gate | `gpu.rs::frame_snapshot_tests::one_snapshot_is_computed_per_frame_and_secondary_views_reuse_it` (existing) plus `phase5-sky-transition-gpu-check` iterating all six passes and asserting the counter is unchanged |
| a secondary view never advances time | mirrors keep `logic_delta_ms = 0` / `advances_time = false`; nothing outside `begin_frame` touches `sky_transition` | as above |
| clouds/precipitation in reflected/clipped views | **Partial runtime, not parity.** The separate weather renderer now loads the supplied textures and `.cld` data, selects rain/snow from EC-BSD flags, and dispatches fogged cloud/precipitation passes for main, reflection and above-water refraction. The sky shader still has no invented panorama weather lane. The camera-centered precipitation and cloud billboards remain approximations; reflection, altitude, collision, wind, lifetime and transition parity remain unvalidated. | source: `src/cloud_gpu.rs`, `src/weather_gpu.rs`, `src/gpu.rs`; tests are not a live-client parity check |

---

## 2. Native evidence

Commands (from the repo root, with the client binary in `google_drive/`):

```bash
objdump -d google_drive/elementclient.exe > /tmp/full.asm   # 1,750,808 lines
md5sum google_drive/elementclient.exe
# 5f02895688fa580506e869156340a664  (matches the MD5 cited in src/*.rs headers)
grep -n "79ac30:\|79aeb0:\|79a040:\|79b150:\|79b180:" /tmp/full.asm
grep -n "4203f4:\|420404:\|4204db:\|42054e:\|420600:\|420666:\|42071f:" /tmp/full.asm
```

Key reads, all quoted in the module documentation:

* `0x79ac30`: `test 0x5b4(%esi)` / `jne 0x79ad7d`; `test 0x5b9` / `jne 0x79ad7d`;
  four `fcomps` compares against `+0x4cc..+0x4d8`; `0x79ad7d: xor %al,%al`.
* `0x79aeb0`: `movb $1,0x5b8`; `movb $1,0x5b9`; `setle/dec/and` count mask;
  `mov %eax,0x5b0` / `mov %eax,0x5b4`; `jne 0x79b11f` timer schedule vs
  `0x79b10b: call 0x79b180` inline commit.
* `0x79a040`: `add $0xffffffdf` (i.e. −33) with `jns` (commit when negative) for
  direction `1`; `add $0x21` + `cmp %ecx,%eax` / `jl` for direction `0`.
* `0x79b150`: `sub %edi,%eax`; clamp-to-1 branch before the next tick.
* `0x4203f4..0x420807`: the phase ladder and the two `_ftol` count expressions.
* `0x4b6250`: `mov $0x249f00,%eax` / `mov $0x1b7740,%eax`, selected by
  `fcomps 0x8d58a0`.
* Constant resolution: `0x8effc4` 0.0, `0x8d58a0` 0.0, `0x8d5894` 0.5, `0x8d5c28` 1.0,
  `0x8d62dc` 0.8, `0x8d6808` 1.25, `0x8f14bc` 0.033333335, `0x98c0e8` `Textures\Sky\%s`,
  `0x98c0f8` `A3DSkySphere::TransSky()`.

---

## 3. CI evidence

Workflow: `.github/workflows/pr-checks.yml` (`PR checks and Windows build`).

| Run | Head | Result | Jobs |
|---|---|---|---|
| [41713431807](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41713431807) | `14a9237` (final: recovered clock, seam check, decision) | **success** | `Rust checks and tests (Linux)` job `126187151390` success (all steps, including the new seam check and 18 standalone tests); `Windows executable (x64)` job `126187151770` success |
| [41712740918](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41712740918) | `1bdaeb5` | **success** | both jobs success (first run with the recovered clock) |
| [41690781849](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41690781849) | `b86bbd3` | failure | `Check formatting` only; the new tests also failed to compile standalone (fixed in `1bdaeb5`) |
| [41650307251](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41650307251) | `078fe69` | **success** | both jobs success |
| [41649717773](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41649717773) | `0d6be4f` | **success** | both jobs success |
| [41649136848](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41649136848) | `a78c698` (includes this report) | **success** | both jobs success |
| [41648372049](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41648372049) | `28472c8` | **success** | `Rust checks and tests (Linux)` job `125959650883` success; `Windows executable (x64)` success |
| 41644266211 | `58d1339`+fixes | failure | compile error in the new diagnostic (fixed) |
| 41643340119 | `58d1339` | failure | `SkyGpu::new` missing the new uniform lane (fixed) |
| 41642461892 | `58d1339` | failure | formatting + the same defect (fixed) |

Linux steps in the green run (all `success`): Check formatting → x87 regional
arithmetic → requirement-status audits → **Check all binary targets** → GFX light
audit → **Run viewer unit tests** → software Vulkan → **Phase 1 production shader
regressions** (now including `phase5-sky-transition-gpu-check`) → terrain
per-view GPU check.

Commands added to the workflow:

```yaml
# Linux, "Test requirement status audits"
rustc --edition=2021 --test src/sky_transition.rs -o /tmp/sky-transition-tests
/tmp/sky-transition-tests
# Linux, "Run Phase 1 production shader regressions"
cargo run --locked --bin phase5-sky-transition-gpu-check
# Windows, "Verify native gamma and glow CPU references on Windows"
#   ... module list extended with "sky_transition"
```

Local verification in this environment (crates.io is unreachable here, so the
full `cargo check`/`cargo run` happens in CI):

* `rustfmt 1.8.0 --check` clean on every `src/*.rs` and `src/bin/*.rs`.
* `rustc --edition=2021 --test src/sky_transition.rs` → **13 passed**.
* `naga` (via the npm naga bindings) parses **and validates** the production sky
  WGSL; the pre-fix form fails with the same error CI reported.
* `tools/audit-static-model-gpu-programs.py`: **8 wired, 17 excluded with reasons,
  25 diagnostics total**.
* `tools/check-eight-phase-status.py`: `consistent: true`, 26 Implemented /
  49 Partial / 1 Not implemented, no errors.

---

## 4. Exact changed files

| File | Change |
|---|---|
| `src/sky_transition.rs` | **new**: state machine (guard/swap/tick/hurry/commit), the `0x4203f4` driver, both count expressions, `Triple`/`Request`/`Refused`, the recovered day clock (`DAY_CLOCK_MS`, `DAY_MS_LONG`/`DAY_MS_SHORT`, `clock_for_minutes`), the time-driven tick (`step_ms`, `TICKS_PER_SECOND`), **18 tests** |
| `src/bin/phase5-sky-seam-check.rs` | **new**: the two meridian bands of the production `sky_gpu::sky` mapping, both sides of each boundary, half-open band edges, and `frame_sets` group integrity over all 18 layers; pure arithmetic, no GPU, no assets, wired into the dependency-free CI step |
| `src/environment.rs` | region-crossing test that couples the sky target to the fog key (`a_region_crossing_moves_the_sky_target_with_the_fog_key`) |
| `src/bin/phase5-sky-transition-gpu-check.rs` | **new**, 412 lines: compiles the production WGSL, requires the set crossfade to move ≥10 % of pixels, requires a bit-identical frame at 4000 units altitude, requires the reflected frame to differ, and asserts no pass advances the machine |
| `src/sky_gpu.rs` | `Uniform.sets`, `set_hour()` (WGSL has no closures), `i32()` casts, translation-free `sky_inverse`, `update(..., sets, reflected)`, two new tests, the weather-lane assertion |
| `src/gpu.rs` | `sky_transition` field + `sky_tick_residual`, one `drive` + `step_ms` per `begin_frame`, the recovered `DAY_CLOCK_MS`, `view_sets` gate for the normal and reflected uploads, `region_light` hoisted so all consumers share one resolution |
| `src/main.rs` | `mod sky_transition;` |
| `.github/workflows/pr-checks.yml` | Phase 5 diagnostic in the Linux shader job, the seam check and the standalone suite in the dependency-free step, `sky_transition` in the Windows module list, `CARGO_TERM_COLOR=never` + check/test annotations for diagnosability |
| `tools/static-model-gpu-programs.txt` | `phase5-sky-transition-gpu-check` |
| `Findings/Eight-phase-reassessment-2026-11-02.md` | P5.07/P5.08/P5.09 evidence and gap text |
| `Findings/Unsupported-client-paths.md` | sky resources, the sky-swap duration unit, and the weather runtime entries |

---

## 5. Decision and remaining boundaries

**P5.07 and P5.09 are `Implemented`; P5.08 is `Partial`, not Yes.** The last
sky-transition clock gap is closed by evidence, not redefinition: `0x79a040` is
reached through the `A3DSkySphere` vtable slot `0x8f14b0`, `0x8f14bc` holds
`1/30` s, and `0x4b6250` returns the day length, so a count is milliseconds and
the viewer ticks on elapsed time. Sky altitude/reflection behavior is covered;
cloud and precipitation dispatch now exists in reflection/refraction, but native
weather parity and those view behaviors have not been validated. P5.08 therefore
remains open, with the evidence and outstanding boundaries recorded below.


1. **Region sky *art* is not re-authored (content, not requirement).** `sky_id_day`/`sky_id_night` are
   `configs.pck` keys; the shipped copy is not the retail v2 container
   `src/pck.rs` reads, and the loader holds one group of three panoramas. A region
   change therefore moves the fade through the clock and the flag-word gate but
   cannot change which images are loaded. `Machine::author(day, night)` is the
   single place a region mapping would land.
2. ~~**The swap-duration unit.**~~ **Closed.** The cadence is recovered: one tick
   is one frame (`0x8f14b0`, the object's only vtable update) at `1/30` s
   (`0x8f14bc`), so a count is milliseconds and the longest counted fade is half
   a game day. `VIEWER_CLOCK_MS` is gone; the viewer feeds the recovered
   `DAY_CLOCK_MS` and ticks on elapsed time.
3. **Weather in reflections (P5.08's cloud/precipitation clause).** The source-authored cloud and precipitation passes now submit in reflection and above-water refraction; the precipitation volume is camera-centered and cloud geometry is billboarding, so reflected motion/altitude and native runtime parity remain unverified.
4. **Sky seams are arithmetic, not authored pixels.** `src/bin/phase5-sky-seam-check.rs` now
   covers the meridian arithmetic (both boundaries, both sides, the half-open
   band edges, and `sky_transition::frame_sets` never mixing resource groups
   across all 18 layers of a swap), but it is arithmetic on the production
   mapping, not a comparison against the client's authored panorama images.
5. **No sky-art screenshot parity.** The GPU diagnostic uses synthetic gradients;
   it proves state and shader behaviour, not that the client's panorama images
   match at the seam.
