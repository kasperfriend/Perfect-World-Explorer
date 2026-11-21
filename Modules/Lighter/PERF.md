# Lighter Hybrid RT Overlay & Reload Status (PR #51)

Verification labels: **S** = synthetic (local Rust unit/SPIR-V/lavapipe tests and GitHub Actions CI; RTX 3080 hardware verification pending user run).

| Item | Commit | Status | Summary |
|---|---|---|---|
| **R1** | `53a3433` | Done (S) | Updated `fallback` check in `src/gpu.rs` (~7748) to accept `!renderer.lighter.overlays_world()`. `cargo fmt --check` passed. |
| **R2** | `269d8c5` | Done (S) | Added `Renderer::ray_lighting_active()` in `src/gpu.rs` and audited/updated all `renders_rays()` call sites (kept `renders_rays()` for Reference world replacement, non-blocking preflight, and upscaler gating; switched progress, reload check, drawn_mode, present profiling, travel telemetry, and headless capture to `ray_lighting_active()`). |
| **R3** | `8ebcc44`, `664a11b` | Done (S) | Switched `#[cfg(test)]` smoke-test assertions (~7773, ~7782) to `ray_lighting_active()` and added `r3_ray_lighting_active_rejects_errored_renderer` verifying failure when `error()` is set. |
| **R4** | `1296e26` | Done (S) | Added `Lighter::overlay_ready()` (`overlays_world() && frame_index > 0`) and `Lighter::check_reload_state()` with `reload_ray_verified` in `src/gpu.rs` so reload verification waits until the first overlay frame actually draws (or fails immediately on error). Added `r4_reload_with_overlay_ready_reports_no_error` and `r4_reload_with_overlay_error_reports_real_failure`. |
| **R5** | `4e7a601` | Done (S) | Added `Lighter::pending_stage()` reporting specific failure stages (`pipeline compile pending`, `BLAS build pending`, `frame not yet drawn`, `renderer unarmed`, `ray lighting overlay disabled`, `resident scene not loaded`, or `lighter.error()`), logged through `Logger` into `Modules/Lighter/lighter.log`. Added `r5_each_reload_failure_path_produces_distinct_message_and_logs`. |
| **R6** | `531565f` | Done (S) | Ran `reload_world_button_applies_pending_mode_without_changing_the_live_renderer`, `gpu::recovery::tests::*`, and added `r6_overlay_and_reference_reload_paths_end_to_end` (all 4 passed). |
| **R7** | `da0bb08` | Done (S) | Verified Native mode (`Mode::Off`) leaves Lighter inactive and passes lavapipe GPU/PNG regressions (`live_readback_ring_pixels`, `cutoff_gpu_residency_repacking`, `lighter_headless_preview_png`, plus `r7_native_mode_leaves_lighter_completely_inactive`). |
| **R8** | `a8b3f5a` | Done (S) | Restored unlit visibility/depth setup (`lighting = false`) for the green/red card and sheet-ordering probes in `exercise()` before the gray-card lighting probes begin at line 637, resolving the `[0, 25, 0, 255]` vs `[0, 255, 0, 255]` failure on Linux CI without altering any expected values. |
