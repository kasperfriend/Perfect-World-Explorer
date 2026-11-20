# Phase 1 audit — 2 November 2026

This is a source/evidence audit, not a new client comparison. The current all-phase assessment is the [76-point reassessment](Eight-phase-reassessment-2026-11-02.md). The table below is corrected to current code; original execution notes below it remain historical. Phase 1 is **incomplete**; later phases are not signed off. Historical research logs describe earlier revisions and must not be read as the current implementation checklist. No percentage is assigned.

## Requirement-by-requirement findings

| Requirement | Existing evidence / implementation | Remaining acceptance work |
|---|---|---|
| Complete scene-light selection | `scene_lights.rs` builds the native grid, bounded runtime publications and source-order selection. Production ordinary model draws consume it; GFX light elements/controller-108 execute. | Recover producer lifetimes and complete object consumers, validate actual runtime input ordering. Selection is not a range cutoff; distinguish shader and fixed-function paths. |
| Identify receiving classes and material calculations | `model_light.rs` and production world submission implement ordinary weighted/rigid CPU/WGSL material variants; `scene-model-light-survey` queries world placements diagnostically. | Ordinary world material-combined constants and opaque eligibility are implemented and tested; every receiving class, special state and fixed-function alternative remain incomplete. |
| Integrate lights without double-lighting | BMD baked colors and terrain lightmaps already render. | Ordinary retained weighted/rigid world models now render and receive lights in main/reflection/refraction. Alpha/special queues and general FX producer ownership remain open; baked BMD/terrain/approved trees remain separate. |
| Regional ambient, direction, overlap and transitions | `environment.rs`, `client_region_map.rs`, optional streamed selection in `gpu.rs`; authored endpoints and durations, optional regional tree tint. | Native synchronous initial/teleport admission and persistent FIFO are implemented; PC24 transitions and quantized sun/moon diffuse/specular/ambient reach ordinary models. Independent default-area visibility fades exist. Complete material/celestial consumers remain open; tree tint remains opt-in. |
| Sun curve only at proven material consumers | `client_sun.rs` recovers sky-object direction; optional sky discs consume it. | No proof that this sky-object curve should drive every material. Leave baked material lighting unchanged. |
| Terrain/grass lightmaps, coordinates and sampler arithmetic | `terrain_gpu.rs` and `gpu.rs` implement day/night maps; grass area-center selection is optional. | Rectangular mapping, CLAMP, area-center grass selection and startup availability are traced and implemented. Native missing-day retained binding and dynamic residency remain open. |
| Baked model interpolation, flags and alpha | `bmd.rs`, `world_objects.rs`, `gpu.rs`; dual color arrays, per-mesh two-sided flag, cutouts and day/night blend. | All material/texture stages, alpha state and client-specific overrides are not complete. |
| Bloom constants, UVs, formats, transitions, order | `post_gpu.rs` has recovered blur weights and ordinary/underwater glow transitions. Existing `post-gpu-check` exercises arithmetic. | Quarter-size UNORM targets, full-resolution tap spacing, special modes, native fades, warp-before-glow and authored sharpen exist and pass GPU checks. Final complete-pass/caller coverage audit remains open. |
| Gamma/display chain | `display_gamma.rs` renders composed fullscreen world/UI once; production LUT contains all 25,856 captured x86 MSVCRT PC24 words, independently rechecked by Windows CI. | This run fixes the windowed scene-only ramp discrepancy. Monitor/compositor behavior, other color conversions and live Windows comparison remain unverified. See changes below. |
| Applicable per-pass fog | Current terrain/world/tree shaders already contain material fog; `phase1-fog-gpu-check` exercises production functions. `post_gpu.rs` retains screen-fog support. | Do not infer the entire pipeline is still screen-space from old log entries. Complete pass/variant coverage, reflection, transparency and underwater ordering still require tracing and comparison. |
| Preserve approved trees | Existing default tree lighting and geometry are left untouched. Regional tint remains optional. | Complete branch/frond/leaf/billboard material reconstruction before replacing the fallback. |
| Automated validation | Existing unit tests and diagnostic GPU binaries cover portions of these paths. | Tests do not establish live-client parity. Add real-asset/reference coverage, execute all GPU checks, and validate Windows presentation. |

## Original audit execution notes (historical; current implementation is above)

## Concrete correction in the original run

The UI previously enabled `camera.client_gamma` in windowed mode. That reached the scene postprocess, applying a gamma curve before overlays. The recorded executable evidence at `0x77b7c0` says the windowed device path skips ramp construction. The viewer now enables the emulated display ramp **only when both the option and fullscreen are enabled**. Scene-only gamma is disabled in the interactive viewer. Fullscreen still applies it once after world, postprocess, map and UI composition. The postprocess diagnostic API retains its explicit gamma argument for arithmetic testing, not as a windowed viewer feature.

The display and diagnostic post shaders now share one WGSL LUT implementation in `gamma_curve.rs`, with a CPU oracle for the recovered integer setting, 16-bit entries, interpolation and sRGB decoding. Invalid settings uploaded to either pass fall back to 41. Defaults and saved-setting keys have not changed. The graphics label now states “fullscreen only.”

### Validation actually executed

A local Rust 1.88 compiler and standard library were acquired from npm-hosted `@rustbin` component archives into ignored `.cache/`; no repository build dependencies were altered.

Standalone `rustc --test` runs:

- `gamma_curve.rs`: **4 passed** (all 101 settings × 256 LUT entries for monotonicity/endpoints; mode gating; integer/fallback settings; interpolation and sRGB boundary).
- `scene_lights.rs`: **4 passed**.
- `client_region_map.rs`: **2 passed**.
- `client_sun.rs`: **1 passed**.
- `view_settings.rs`: **2 passed**.

The initial 13 tests did not compile the application or validate WGSL. Follow-up PR CI on commit `8866669` **passed** Rust formatting, all binary compilation, viewer unit tests, production gamma/post/model-light/fog GPU regressions under software Vulkan, and the Windows x64 release build. [CI run](https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41430078405). The new `display-gamma-gpu-check` exercised UNORM and sRGB targets, resize/view recreation, RGB endpoints, near-black values, integer settings and invalid-setting fallback (40 readback cases, ±1 encoded byte tolerance).

Follow-up work fixes a CPU/WGSL disagreement in the standalone model-light helper at tiny normal and point-vector lengths. Both sides now apply the same explicitly non-client-defined finite fallback at `1e-6`. The helper remains unused by world rendering. Two new unit tests and an expanded 15-case GPU comparison cover these boundaries and nonpositive attenuation denominators. The dependency-free core script now also tests `model_light.rs`. Four additional scene-light regressions cover source-order ties independent of night color, candidate range versus shader range, nearer runtime replacement, inclusive grid bounds, invalid-input safety and stored-color immutability: **22 core tests passed locally**. CI validation of these follow-ups is tracked in the PR; it must not be inferred from the earlier run.

## Client access and sequencing

Follow-up acquired `Games/pw142/element/elementclient.exe`, `shaders.pck` and `configs.pck` through Drive, under ignored `google_drive/`. The executable SHA-256 is `a290998dc13cc5558b54bda3f65d099f95bc0702e54a0613c6d7669f35c6cdb0`, matching `client_detect.rs`. The 166 shader archive entries were extracted into ignored research storage for inspection. No proprietary assets have been added to Git.

Direct re-check of fog state is important: `0x77ab70` selects table fog (state 35) when requested and supported, otherwise vertex fog (state 140). `0x6512c2–0x6512de` requests table fog for ordinary grass, but vertex fog for its refraction mode; `0x651319` then selects the special refraction shader and uploads constant 91. Consequently ordinary `grass_wave.txt` writing a constant no-fog vertex output does **not** establish that ordinary grass should bypass fog: table fog can override it. The existing main-view grass fog was deliberately not disabled based on that shader line alone. The refraction shader computes exponential depth fog; full refraction-pass integration remains open.

Direct bloom allocation re-check: `0x40e7da` requires render format 21 (D3DFMT_A8R8G8B8); `0x40e867` supplies it to the intermediate target descriptor. `0x40e8db–0x40e987` allocates the two reduced targets using signed integer division by four. Existing UNORM quarter-size intermediates match these recovered properties for positive, normal window dimensions. This does not verify all texture-coordinate offsets or pass ordering.

The eight-phase checklist is preserved in [Eight-phase-status.md](Eight-phase-status.md). Phase 1 remains the gate. This run does not implement or claim completion of terrain parity, static-material parity, vegetation runtime parity, sky/weather, water, animated world models/effects, full-world optimization or Windows delivery. Completing those requires substantially more reverse engineering and validation than a gamma correction or a parser survey.
