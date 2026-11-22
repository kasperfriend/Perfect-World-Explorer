# Lighter Hybrid RT Overlay & Reload Status (PR #51)

Verification labels: **S** = synthetic (local Rust unit/SPIR-V/lavapipe tests and GitHub Actions CI); **HW** = real RTX 3080 Windows logs (`NVIDIA 621.14`, Vulkan).

## Reload & Overlay Readiness (R1–R8)

| Item | Commit | Status | Summary |
|---|---|---|---|
| **R1** | `53a3433` | Done (S) | Updated `fallback` check in `src/gpu.rs` (~7748) to accept `!renderer.lighter.overlays_world()`. `cargo fmt --check` passed. |
| **R2** | `269d8c5` | Done (S) | Added `Renderer::ray_lighting_active()` in `src/gpu.rs` and audited/updated all `renders_rays()` call sites. |
| **R3** | `8ebcc44`, `664a11b` | Done (S) | Switched `#[cfg(test)]` smoke-test assertions to `ray_lighting_active()` and added `r3_ray_lighting_active_rejects_errored_renderer`. |
| **R4** | `1296e26` | Done (S) | Added `Lighter::overlay_ready()` (`overlays_world() && frame_index > 0`) and `Lighter::check_reload_state()` with `reload_ray_verified` in `src/gpu.rs`. |
| **R5** | `4e7a601` | Done (S) | Added `Lighter::pending_stage()` reporting specific failure stages and logging them to `Modules/Lighter/lighter.log`. |
| **R6** | `531565f` | Done (S) | Verified reload/recovery tests plus `r6_overlay_and_reference_reload_paths_end_to_end`. |
| **R7** | `da0bb08` | Done (S) | Verified Native mode (`Mode::Off`) leaves Lighter inactive and passes lavapipe GPU/PNG regressions. |
| **R8** | `a8b3f5a` | Done (S) | Restored unlit visibility/depth setup (`lighting = false`) for green/red card probes in `exercise()`. |

## Cold-Start Compile, Caching & Telemetry (C1–C7)

| Item | Commit | Status | Summary |
|---|---|---|---|
| **C1** | `9a6ee24` | Done (S, HW) | Added `ReloadOutcome::OverlayDegraded`; hybrid overlay startup failures record in status and `lighter.log` without aborting Native rendering. |
| **C2** | `eabb600` | Done (S, HW) | Replaced 30s timeout with `ColdRayDeadline` using `FIRST_FRAME_TIMEOUT` (120s) reset on compile heartbeats. |
| **C3** | `2a68d45` | Done (S, HW) | Added `vulkan::run_with_compile_heartbeat` emitting 1s elapsed heartbeats and completion time around `create_compute_pipelines` and `Primary::new`. |
| **C4** | `0994708` | Done (S, HW) | Defaulted Windows Vulkan pipeline cache dir to `%LOCALAPPDATA%\PerfectWorldViewer\cache` and logged HIT/MISS + bytes written. Confirmed on RTX 3080: cold overlay compile `77.8s` -> warm cache HIT `0.1s` (316–392 KB cache). |
| **C5** | `909049b` | Done (S, HW) | Split `ShaderVariant::Overlay` vs `ShaderVariant::Full`: Overlay WGSL `69,727 B` / SPIR-V `104,320 B` vs Full WGSL `123,105 B` / SPIR-V `220,208 B` (-43% WGSL, -53% SPIR-V). |
| **C6** | `329957f` | Done (S) | Starts `Lighter::start_cold_ray_compile` immediately in `Renderer::new` once initial ray world installs. |
| **C7** | `d026c44` | Done (S) | Added session-relative `[+N.Ns]` elapsed timestamps to `Logger::info` lines in `Modules/Lighter/lighter.log`. |

## Flickering, BLAS Staging & Native/RT Collision Fixes (F1–F2)

| Item | Commit | Status | Summary |
|---|---|---|---|
| **F1** | `4c92ea9` | Done (S) | Eliminated 15–30 Hz RT shadow blinking/strobing by re-compositing the last completed denoised `Post` buffer (`Post::last_output()`) whenever Vulkan RT is busy tracing, gated `"Staging initial RT acceleration structures…"` on `!initial_residency_done()`, and raised startup residency batching from 1 MiB / 1 BLAS per frame (23–32s on RTX 3080) to 16 MiB / 32 BLAS / 128k primitives per batch. |
| **F2** | `f69f627` | Done (S) | Suppressed colliding Native static baked `terrain_lightmaps` and Native planar `water_reflections` (plus 2x water mirror/refraction passes) once `overlay_ready()` is active, while keeping Native 100% unchanged when Lighter is off or still compiling/staging. Refined `trace.wgsl` overlay reflections to give flat water-like surfaces crisp RT reflections while restraining matte terrain/wall grazing sheen. |
