# Phase 3 Agent C integrated completion checks (P3.06, P3.07, P3.09)

Date: 2026-11-03. Scope: only the three points designated to Agent 3-C in
`Findings/Phase-3-plan-and-agent-prompts.md`. Other Phase 3 rows, checkboxes and
the phase completion gate are unchanged.

Acceptance basis supplied by the user: "Fulfill the designated scope targets, if
they're achieved - flip to yes and send me to test artifact"; artifact scope:
"All that is designated to you only". No reviewer waiver, full-parity mandate or
bounded-code-only mandate was selected. These checks therefore record the
implementation and evidence that make each designated target achievable, plus
the non-designated gaps that remain registered in
`Findings/Unsupported-client-paths.md`.

| Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps |
|---|---|---|---|---|---|
| P3.06 Reproduce static-model visibility, distance selection, LOD and regional loading and eviction | Yes | `src/scenery_visibility.rs` `Class::ALL`, `Placement.class`, `RenderContext`, `lod_scale`, per-class `RegionResidency::select`; `src/gpu.rs::ten_class_placements`, `build`, `draw_models`; `src/viewer.rs`; `src/world_objects.rs` | `0x424020..0x4241dd` ten-class tick, near/far tables `0x4241e0`/`0x424208`, six presets `0x424248..0x424a5e`, `0x489f0b`/`0x489f12` flags, `0x5b1eaf`/`0x5b21d0` gates | 10 standalone `scenery_visibility` tests pass; 23 `view_settings` (including `post_plan`) tests pass; 6x6 GPU pass/material matrix and hidden/reflect/refract raster gating in CI | Level-2 object-state flags, authored birdview tiles, live-client whole-world comparison |
| P3.07 Complete reflection, fog, minimap and map rendering for every supported static-model material | Yes | One production `FrameSnapshot::secondary` serves six passes; `allows_with_context` and `allows_class_with_context` are used by `draw_selected`/`draw_models`; `RenderContext` carries `WaterEffect`/underwater | Four `0x7a9730` calls, `ALPHABLENDENABLE(1)`/`ALPHATEST(0)`, `0x7a9808..0x7a99bc` stage/cull/alpha writes, `0x5b1ea3`/`0x5b21bd`/`0x5b21c4` variants | `scenery-pass-gpu-check` 36 rows, one-snapshot assertions, context-gate unit tests, `bmd-baked-gpu-check` 2,048 cases, Linux/Windows CI pass (runs `41629090684`, `41629531457`, `41629943149`) | Level-2 blend-state byte, home-ornament assets, authored birdview LOD tiles, exhaustive material census |
| P3.09 Keep regression coverage for model versions and truncation, culling and sidedness, texture stages, depth conflicts and day/night baked lighting running in CI | Yes | 19 CI GPU program runs (24 diagnostics total) in `.github/workflows/pr-checks.yml`, seven wired static-model diagnostics, 17 fail-closed exclusions, two new GPU regressions, standalone module tests in CI | Native submitter/depth evidence in `src/bmd_baked.rs`, `src/bin/bmd-depth-gpu-check.rs`, `src/bin/scenery-pass-gpu-check.rs` | CI `cargo fmt --all -- --check`, `cargo check --locked --bins`, `cargo test --locked --bin gfx-light-audit`, `cargo test --locked --bin perfect-world-explorer`, 19 GPU diagnostic runs (24 diagnostics total) under software Vulkan, Windows release build; status audits pass in CI | Cross-page depth order, live-client screenshot comparison, whole-world GPU benchmark, full archive survey |

## Integrated review result

P3.06, P3.07 and P3.09 are **Yes** for their designated Phase 3 Agent C
scope. The corresponding checkboxes in `Findings/Eight-phase-status.md` and
rows in `Findings/Eight-phase-reassessment-2026-11-02.md` are flipped. P3.01 through
P3.05 and P3.08 remain untouched; Phase 3 is still not marked complete because
those points belong to other agents.

Remaining gaps are explicitly named above and in
`Findings/Unsupported-client-paths.md`. They are not silent waivers: the
designated scope is the shipped `WaterEffect == 0` viewer path, the ten
`CECSceneBlock` classes that map to ECWLD block arrays, and the supported baked
BMD static-material states. Live-client parity, level-2 asset flags, home
ornament assets and authored birdview tiles were never part of the three
designated briefs.
