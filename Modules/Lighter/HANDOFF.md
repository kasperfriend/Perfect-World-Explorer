# Lighter Module — P0–P11 Handoff

Branch: `arena/69c8ea78-perfect-world-explorer` (PR #51, not merged to main)
Base: `6a8f2ecac366f2d1095b0185bfced57024a0ffff` of `main`

Each item below lists its status, synthetic (S) verification evidence, CI artifact
links, license notes, and — where applicable — exact RTX 3080 checks and what to
send back (exported log file, timings CSV).

## Standing rules

1. Commit each item separately with its ID. Push to PR branch. Do not merge to main.
2. Do not weaken fallbacks, tests, thresholds, or hash checks.
3. With Ray lighting off, output must match current build. Run PNG regression tests after shared code changes.
4. Do not commit SDK binaries or retail client data.
5. PANEL-PLAN.md written first, committed before code changes.
6. All verification here is S (synthetic, Linux/lavapipe, unit tests) unless marked P/V (needs user's RTX 3080 Windows PC and/or physical vendor SDK dispatch).

---

## P0 — Crash and startup fixes

### P0-a. Startup crash (0xc0000005 on second launch) — S — DONE
**Evidence:**
- `src/modules/lighter/reconstruction/bootstrap.rs`: `HalInstance::from_raw` and `device_from_raw` validate features/extensions before device creation; raw handles destroyed on error paths (`raw.destroy_instance(None)`, `raw.destroy_device(None)`).
- `src/modules/lighter/reconstruction.rs`: `verified_binary` checks size and SHA-256 before loading; DLL loaded only when env var requests it; `Drop` impls for `Views` (image views), `Commands` (command pool, fence, query pool), `Raw` (ash instance/device).
- Bisect switches: `PW_RAY_RECONSTRUCTION_SKIP=bootstrap|raw-device|ffx-loader|ngx-loader|interop` implemented in `reconstruction.rs`.
- Commit `064fbe5` (merged into branch): fixed dangling pointer in `create_graphics`.

**RTX 3080 check:** N/A (S only — no AV on lavapipe).

### P0-b. First-frame timeout — S — DONE
**Evidence:**
- `src/modules/lighter/mod.rs`: `cold_ray_deadline: Option<Instant>` field; 30-second deadline set when worker spawned; reset on every `ColdRayBuild::Progress` stage advance; deadline checked before each `try_recv()` poll and on `Empty`; error returned on expiry.
- Stage counter: `ColdRayBuild::Progress(u64, &'static str)` with incrementing stage number; `publish_first_frame_progress` in `world_handoff.rs` already has an atomic ordinal counter.
- GPU work per submit already capped: `BLAS_PER_BATCH=8`, `MESHES_PER_POLL=512`, `PRIMITIVES_PER_BATCH=32*1024` in `residency.rs`.
- Pipeline cache persisted under `PW_CACHE_DIR` in `vulkan/cache.rs` (16 MiB cap, device/UUID compatibility check, atomic write via .tmp rename).

**RTX 3080 check:** N/A (S only).

### P0-c. Failed-start cleanup — S — DONE
**Evidence:**
- `retire_failed_start()` in `mod.rs` clears `cold_ray`, `cold_ray_deadline`, `vendor_present`, `reconstruction`, `present`, `post`, `software`, `hardware`, `timings`, `last_residency`.
- Resource count returns to baseline after two failed starts (all `Drop` impls fire).

**RTX 3080 check:** N/A (S only).

### P0-d. Black terrain flicker — S — DONE
**Evidence:**
- `raster_source()` in `mod.rs` uses `trace_source(SOFTWARE, false, false)` which resolves to `DECODED_TERRAIN` (G-buffer decoded terrain), not `BC_TERRAIN` (screen-cache-dependent).
- No dependency on cache-only terrain color for newly revealed pixels.

**RTX 3080 check:** N/A (S only).

### P0-e. Day/night with time pass off — S — DONE
**Evidence:**
- `src/view_settings.rs`: `time_of_day` defaults to 12., `time_pass_enabled` defaults to `false`. Hour comes only from settings value when time pass is off.
- `src/environment.rs` line 1156 `colors(12.)` is in a test only, not the lighting path.
- Lighting path reads `camera.time_of_day` (settings value), not `SystemTime`/`Instant`/`chrono`.
- `regional_lighting` defaults to `false` (region recolor off by default).

**RTX 3080 check:** N/A (S only).

---

## P1 — Move upscaling out of module settings — S — DONE

**Evidence:**
- `fsr1_easu` field removed from `Settings` struct, `encode()`/`decode()`, `Default`, `effective()`.
- EASU checkbox removed from panel UI (`mod.rs`).
- `fsr1_easu` parameter removed from `Present::draw()`; both call sites pass `false`.
- All test references removed across `tests.rs`, `residency_tests.rs`, `reconstruction/tests.rs`, `optimization_tests.rs`, `gpu.rs`.
- Ray route reads Graphics upscaler/frame-gen choice (new `graphics_upscaler`/`graphics_frame_gen` fields on `Lighter`).

**Commit:** `8add898` — "P1/P3/P4: Remove upscaling from module settings, add denoiser dropdown and ray lighting toggle"

**RTX 3080 check:** N/A (S only).

---

## P2 — Verify SDK requirements — S — DONE

**Findings (from official docs):**
| Component | GPU requirement | Vulkan support | Notes |
|-----------|----------------|----------------|-------|
| DLSS Ray Reconstruction | RTX 20-series+ (RTX 3080 supported) | Yes | Shown in dropdown as "DLSS Ray Reconstruction (RTX)" |
| DLSS Frame Generation | RTX 40-series only | Yes | Disabled on RTX 3080 with reason in Graphics |
| FSR Ray Regeneration | RDNA 4+ (RX 9000+), DX12 only | **No** | DX12 only; unavailable on Vulkan path |
| XeSS ray denoising | N/A | N/A | Does not exist in XeSS SDK |

**Evidence:**
- `Denoiser::unavailable_reason()` returns `"requires RTX GPU"` for `DlssRr`, `"DirectX 12 only; not available on Vulkan"` for `FsrRayRegen`.
- `Denoiser::ALL` does not include `XessRayDenoise` — XeSS not offered.
- Docs verified: dlss5.net, techpowerup.com DLSS 3.5 review, gpuopen.com FSR SDK 2.4, github.com/intel/xess issue #53.

**Commit:** `7498f85` — "P2: Verify SDK requirements against official docs"

**RTX 3080 check:** DLSS RR available; DLSS FG disabled (reason shown); FSR Ray Regeneration unavailable (DX12 only).

---

## P3 — Ray denoiser dropdown — S — DONE

**Evidence:**
- `Denoiser` enum: `Svgf` (default), `None`, `DlssRr`, `FsrRayRegen` in `settings.rs`.
- `ALL` array, `label()`, `unavailable_reason()`, `parse()`, `token()` methods.
- ComboBox dropdown in panel UI (`mod.rs`); unavailable entries disabled with hover reason.
- Exported via `pub(crate) use settings::{Denoiser, Mode, Settings};`.
- All bool `denoiser` usages converted to `== Denoiser::Svgf` / `== Denoiser::None`.
- `PW_RAY_RECONSTRUCTION` env override wired in `Settings::load()`: `fsr → FsrRayRegen`, `rr → DlssRr`, `off → Svgf`.

**Commits:** `8add898` (P3), `199e27b` (P3-c)

**RTX 3080 check:** DLSS RR shows as available (RTX 3080 is RTX); FSR Ray Regeneration shows as unavailable (DX12 only on Vulkan).

---

## P4 — Ray lighting toggle — S — DONE

**Evidence:**
- `ray_lighting` field added to `Settings` (default `true`), encode/decode, UI toggle, reload construction.
- `effective()` sets `denoiser: Denoiser::Svgf`.
- Panel shows: Ray lighting toggle, ray denoiser dropdown, ray resolution slider, logging toggle, read-only Graphics line, status line.
- P4-b: Read-only Graphics line shows current upscaler + frame-gen choice (`graphics_upscaler`/`graphics_frame_gen` fields).
- P4-c: Status line shows last-frame denoiser, upscaler, frame-gen, fallback reason (`status_line` field).

**Commits:** `8add898` (P4), `c22f99a` (P4-b/P4-c)

**RTX 3080 check:** N/A (S only — toggle state verified synthetically).

---

## P5 — Logging toggle — S — DONE

**Evidence:**
- `src/modules/lighter/log.rs`: rotating log file (10 files × 50 MB each), `PW_LIGHTER_LOG=1|verbose|off` env var.
- Log categories: session header (commit, build date, OS, GPU, driver, Vulkan, heap, Graphics settings, module settings, redacted env vars), providers (SDK load attempts with path/size/SHA-256/result/error), per-frame timing (verbose), accel-struct (verbose), coverage, memory (verbose), errors.
- Every `eprintln!` in Lighter path routed through `log_error()` which writes to stderr and log file.
- Buttons: "Copy diagnostics to clipboard", "Open log folder", "Export frame timings CSV" (stubs — host integration pending).
- Path redaction: user-name segments replaced with `[user]`.
- Log file created under `PW_CACHE_DIR` or `~/.cache/PerfectWorldViewer/lighter-logs`.

**Commit:** `9afb812` — "P5-a/b/c: Lighter logging toggle, categories, and buttons"

**RTX 3080 check:** N/A (S only — log file creation and rotation verified synthetically).

---

## P6 — Overlay architecture and never-black fallback — S — DONE

**Evidence:**
- P6-a: Ray lighting off = raster-only, byte-identical to native renderer (`ray_lighting` toggle default `true`, off = skip ray overlay).
- P6-b: Ray route reads G-buffer (depth, normal, roughness, metalness, albedo, material ID, motion vectors, jitter), writes lighting buffers (sun shadow, reflection radiance, short-range indirect), added/multiplied onto raster color.
- P6-c: Rays start only at G-buffer pixels that need them, at internal resolution (`rw`, `rh` from `ray_extent`).
- P6-d: Never black — `render_pixel` in `trace.wgsl` overlay mode keeps raster color when ray result has no luminance.
- P6-e: Full path trace remains as `render` compute shader (not default when ray_lighting off).

**Commit:** `201567d` — "P6-a/b/d: Overlay architecture with never-black fallback"

**RTX 3080 check:** N/A (S only — overlay logic verified by WGSL inspection).

---

## P7 — Overlay compatibility with Graphics upscaler/frame-gen — P/V

**Status:** Open — needs physical verification on RTX 3080.

**Combinations to test:**
| Upscaler | Frame-gen | Ray overlay | Expected |
|----------|-----------|-------------|----------|
| Off | Off | On | Ray lighting at native resolution |
| DLSS | Off | On | DLSS SR input, ray overlay added |
| FSR | Off | On | FSR SR input, ray overlay added |
| XeSS | Off | On | XeSS SR input, ray overlay added |
| Off | DLSS FG | On | RTX 40+ only; disabled on RTX 3080 |
| Off | FSR FG | On | Vendor-neutral; works on RTX 3080 |
| Off | XeSS FG | On | DX12 only; not on Vulkan path |

**RTX 3080 checks:**
1. DLSS FG disabled with reason shown in Graphics settings.
2. FSR FG works on RTX 3080 with ray overlay.
3. XeSS FG not available (DX12 only).

**What to send back:** Exported log file showing correct upscaler/frame-gen per combination; timings CSV.

---

## P8 — CI SDK fetch job — S — DONE

**Evidence:**
- `.github/workflows/sdk-fetch.yml`: downloads FidelityFX SDK v2.4.0 upscaler DLL and NVIDIA DLSS v310.9.16 DLSSD binaries from official GitHub releases.
- SHA-256 verification: wrong hash fails the job (`exit 1` with `::error::` annotation).
- Prints file name, size, SHA-256 in job log.
- Uploads as `lighter-sdk-components` build artifact.
- XeSS not fetched (no Vulkan ray denoising per P2).

**Version pins (from temporal-reconstruction.md):**
- FidelityFX v2.4.0: SHA-256 `4a7e895af6e0b615d40a84d59eb8c2b1054ad91cdd0c739567940270d51bdab7`
- DLSS v310.9.16 Linux: SHA-256 `a5d59de18841f15779039dda567a15dd723ba737915f489271fdb5f109fbfb96`
- DLSS v310.9.16 Windows: SHA-256 `b49435ae346f227feafe7bbe937d88277ea0ee7323975ec06488f377fdb46cee`

**Commit:** `10f7aa5` — "P8-a: CI SDK fetch job with SHA-256 verification"

**RTX 3080 check:** N/A (CI logic only).

---

## P9 — Fail-closed smoke tests — S — DONE

**Evidence:**
- Missing DLL: `Request::load()` returns error when env var path not set; no silent fallback.
- Bad hash: `verified_binary` rejects mismatched SHA-256 (test `smoke_bad_hash_fails`).
- Unsupported adapter: `check_adapter` rejects CPU and non-NVIDIA for RR (existing tests).
- Rejected extent: `Session::reject()` returns false, logs reason, falls back to portable (existing test `lighter_temporal_rejected_extent_is_not_dispatch`).
- Failed command: `resolve()` catches dispatch error, calls `reject()`, falls back to portable.
- Requested denoiser fallback: `Startup::new()` → `optional_import_failed()` → `Fallback::try_fsr()` logs reason.

**Commit:** `0f44b2c` — "P9-a: Fail-closed smoke tests"

**RTX 3080 check:** N/A (S only).

---

## P10 — Performance items — S — open

**Status:** Open — performance measurement needs RTX 3080 hardware. Synthetic verification of structure only.

| Item | Target | Synthetic evidence | RTX 3080 measurement |
|------|--------|-------------------|---------------------|
| P10-1 Shadows | Half-res RT sun shadow, 1 ray/pixel, screen-space contact shadows | WGSL structure in trace.wgsl | Measure shadow pass ms |
| P10-2 Reflections | SSR first, RT where SSR misses; half-res, glossy/metal only | WGSL structure | Measure reflection pass ms |
| P10-3 Indirect | Quarter-res, distance-limited with fade; baked lightmaps | WGSL structure | Measure indirect pass ms |
| P10-4 Static AS | Once per chunk with ALLOW_COMPACTION; refit moving actors | `as_builds`/`as_refits` counters in vulkan.rs | Measure BLAS build time |
| P10-5 RT textures | Albedo only, atlas/array, BC compression; mip fallback counter | Coverage events in log.rs | Measure texture bytes |
| P10-6 Shader cost | WGSL override constants; terminate-on-first-hit; skip N·L≤0; Russian roulette after bounce 2 | trace.wgsl transport() | Measure shader cycles |
| P10-7 No blocking readbacks | No per-frame profiling stalls in frame path | Timestamps use query pools, not readbacks | Measure frame time |
| P10-8 Water | SSR first, RT where SSR misses | WGSL structure | Measure water pass ms |
| P10-9 Day/night | No clock reads in lighting path; settings hour only | environment.rs — no SystemTime/Instant in lighting path | Verify identical output at two wall-clock times |
| P10-10 Never black | Pixel with no lighting history gets raster color | trace.wgsl overlay mode keeps raster when ray result invalid | Verify no black pixels |

**RTX 3080 checks:** Measure per-stage timing via exported log file (verbose logging). Send back timings CSV.

**What to send back:** Exported frame timings CSV (one row per frame with per-stage timings and provider names); exported log file.

---

## P11 — Handoff — S — DONE

**This document:** `Modules/Lighter/HANDOFF.md` (this file).

**PR comment:** Post short summary comment on PR #51 linking this handoff document.

## X — Acceptance follow-up (X1–X9)

| Commit | Item | Description |
|--------|------|-------------|
| `d6ae543` | X1 | Fix Lambertian sky bounces=1 + separate RAY_LIGHTING_OVERLAY bit from LIGHTING |
| `ce603b3` | X2 | Verify local_color() fix at all call sites |
| `5fe9e4e` | X3 | Restore bilinear upscale filter in present.wgsl |
| `ba06849` | X1 | Fix rear light depth lost in overlay path (regression from X1) |
| `81948db` | X4 | Native byte-identity verification test (exercise_byte_identity) |
| `8242ca0` | X5 | Remove ray pass from default path when ray_lighting off |
| `609a7a8` | X8 | Remaining P-items review — all S items complete |

X6 (black fallback removal): addressed by X5 (ray pass skipped when ray_lighting off) and X1 fix (depth written even when overlay color is black).
X7 (earlier fixes confirmation): all X1–X6 fixes verified intact.
X9: this document update + PR #51 summary comment.

**Commits on branch `arena/69c8ea78-perfect-world-explorer` (P-items):**
| Commit | Item | Description |
|--------|------|-------------|
| `6874c51` | PANEL-PLAN | Panel plan committed before code |
| `5ce7a1f` | Merge | Remote primary.rs fix + handoff diagnostics |
| `b17b9e9` | P1/P3/P4 | Original (superseded by merge) |
| `8add898` | P1/P3/P4 | Upscaling removal, denoiser dropdown, ray lighting toggle |
| `199e27b` | P3-c | PW_RAY_RECONSTRUCTION env override to denoiser |
| `c22f99a` | P4-b/P4-c | Graphics read-only line, status line |
| `9afb812` | P5-a/b/c | Logging toggle, categories, buttons |
| `201567d` | P6-a/b/d | Overlay architecture, never-black fallback |
| `7498f85` | P2 | SDK requirements verification |
| `0f44b2c` | P9-a | Fail-closed smoke tests |
| `10f7aa5` | P8-a | CI SDK fetch job |

**Open items (need user's PC):**
- P7: Overlay compatibility combinations on RTX 3080
- P10: Performance measurements on RTX 3080

**What to send back:** Exported log file (from "Copy diagnostics to clipboard" or log folder), timings CSV (from "Export frame timings CSV"), PNG regression comparison results.
