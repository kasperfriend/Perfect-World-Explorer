# Lighter Module Settings Panel — Plan

Every item below is a checkbox with its acceptance test and verification type:
- **S** = verifiable synthetically in this workspace (host Linux, lavapipe, unit tests)
- **P/V** = needs the user's RTX 3080 Windows PC and/or physical vendor SDK dispatch

## P0 — Crash and startup fixes (done first)

### P0-a. Startup crash (0xc0000005 on second launch)
- Keep bisect switches: `PW_RAY_RECONSTRUCTION_SKIP=bootstrap|raw-device|ffx-loader|ngx-loader|interop`
- Audit `unsafe` blocks on bootstrap/raw-device/FFX/NGX/image-view/descriptor-pool paths
- Device features/extensions enabled at creation must match what was enabled
- `extern "C"` signatures match SDK headers; struct layout/alignment correct
- DLL loaded only when env var/setting requests it; every raw handle destroyed on failure
- **S test:** failed start followed by second start on lavapipe shows no leaked handles, no AV
- **Status:** open — S

### P0-b. First-frame timeout ("Preparing Lighter ray pipeline and buffers")
- Split BLAS builds, pipeline creation, shader compilation across frames
- Stage counter (chunks built, BLAS count, pipelines compiled)
- Timeout resets whenever stage counter advances; progress is not killed
- Cap GPU work per submit during build (aim <1 ms per submit; TDR guard)
- Persist Vulkan pipeline cache under `PW_CACHE_DIR`, load on next launch
- **S test:** stage-counter timeout logic; pipeline cache round trip
- **Status:** open — S

### P0-c. Failed-start cleanup
- Failed start destroys buffers, images, device before retry
- **S test:** resource count returns to baseline after two failed starts
- **Status:** open — S

### P0-d. Black terrain flicker (raster path)
- `raster_source()` takes opaque terrain color from screen caches written by compute sibling
- On camera movement, caches empty for newly revealed pixels → black output
- Fix: raster world is base color always; ray lighting added on top
- Remove dependency on cache-only terrain color
- **S test:** after camera move reveals new terrain, no black pixel where raster color is non-black
- **Status:** open — S

### P0-e. Day/night change with time pass off
- Find every clock read in lighting path (`SystemTime`, `Instant`, `chrono`, `Local::now`, client clock)
- Find every hardcoded hour including `colors(12.)` in `src/environment.rs`
- When time pass off, hour comes only from settings value
- Region recolor off by default
- **S test:** same frame at two wall-clock times gives identical lighting output
- **Status:** open — S

---

## P1 — Move upscaling out of module settings

### P1-a. Remove upscaler controls from Lighter panel
- Remove `fsr1_easu` checkbox ("FSR 1 spatial upscale (EASU)") from `src/modules/lighter/mod.rs` UI
- Remove `fsr1_easu` field from `Settings` struct and `settings.rs` encode/decode
- Ray route reads Graphics upscaler and frame-gen choice, not module settings
- **S test:** no upscaler control exists in module panel; ray route reads Graphics value
- **Status:** open — S

### P1-b. Ray route reads Graphics upscaler/frame-gen
- The overlay uses whatever upscaler and frame-gen are active in Graphics settings
- No module-level upscaler/frame-gen toggle
- **S test:** ray route reads Graphics value for upscaler and frame-gen
- **Status:** open — S

---

## P2 — Verify SDK requirements against official docs

### P2-a. DLSS RR and DLSS FG GPU requirements
- DLSS RR: RTX 20-series and later (RTX 3080 supported)
- DLSS Frame Generation: RTX 40-series only → disabled on RTX 3080 with reason
- Record exact API names, DLL versions, licenses
- **Status:** open — P/V (doc research)

### P2-b. FSR Ray Regeneration and FSR FG on Vulkan path and RTX 3080
- FSR Ray Regeneration: vendor-neutral, supported on RTX 3080
- FSR Frame Generation: vendor-neutral
- Record exact API names, licenses
- **Status:** open — P/V (doc research)

### P2-c. XeSS upscaling, frame generation, ray denoising on Vulkan
- Only if official Intel XeSS documentation supports it on this path
- If not supported, XeSS does not appear in the dropdown
- Record exact API names, licenses
- **Status:** open — P/V (doc research)

---

## P3 — Ray denoiser dropdown

### P3-a. Dropdown entries (RR shown separately)
- SVGF (portable, default)
- DLSS Ray Reconstruction (RTX only)
- FSR Ray Regeneration
- XeSS ray denoising only if verified in P2-c
- **Status:** open — S

### P3-b. Availability reasons per entry
- Unavailable entries disabled with reason tooltip ("requires RTX GPU", "SDK not found at path", etc.)
- Never silently substitute another denoiser
- **S test:** each entry maps to right denoiser; unavailable entries show reason
- **Status:** open — S

### P3-c. `PW_RAY_RECONSTRUCTION` env override
- Maps onto denoiser dropdown (fsr → FSR Ray Regeneration, rr → DLSS RR, off → SVGF)
- **Status:** open — S

---

## P4 — Ray lighting toggle

### P4-a. "Ray lighting" on/off toggle
- Off = raster world only, using Graphics upscaler and frame generation as normal
- **S test:** toggle off equals raster-only output
- **Status:** open — S

### P4-b. Read-only Graphics line
- "Upscaling and frame generation come from Graphics settings: <current choice>"
- Information only, not a control
- **Status:** open — S

### P4-c. Status line
- Last frame: ray denoiser used, upscaler inherited from Graphics, frame-gen state, fallback reason
- **Status:** open — S

---

## P5 — Logging toggle

### P5-a. Logging toggle in module settings
- Writes to log file in `PW_CACHE_DIR` or configured log folder
- Rotation: keep last 10 files, max 50 MB each
- Default verbosity: normal; Advanced: verbose
- Env var `PW_LIGHTER_LOG=1|verbose|off`
- **Status:** open — S

### P5-b. Log categories (togglable under "advanced" disclosure)
- Session header: commit, build date, Windows version, GPU, driver, Vulkan version, heap size, Graphics upscaler/frame-gen, module settings, `PW_*` env vars (redacted user-name segments)
- Providers: every SDK load attempt with path, size, SHA-256, result, exact error text; every fail-closed reason; no secret values
- Per frame (sampled/verbose): denoiser used, upscaler inherited, frame-gen state, ray resolution, per-stage timing (AS, lighting trace, denoise, upsample/compose, present), CPU frame time, fallback reason
- Acceleration structures: static BLAS built/compacted per chunk, refits, TLAS updates, copy batches/bytes, zero-check counter
- Coverage: per-map counts (loaded/skipped/failed by reason), mip fallback count, unavailable texture names
- Memory: Lighter allocation total, driver device-local usage, sampled once per second
- Errors: every `eprintln!` in Lighter path through logger with level
- **Status:** open — S

### P5-c. Buttons
- "Copy diagnostics to clipboard"
- "Open log folder"
- "Export frame timings CSV" (one row per frame with per-stage timings and provider names)
- **Status:** open — S

### P5-d. S test
- Log file created, rotated, never contains path with user-name segment
- **Status:** open — S

---

## P6 — Overlay architecture and never-black fallback

### P6-a. Raster renderer unchanged
- With Ray lighting off, output byte-identical to current build for every Graphics setting not involving ray route
- **Status:** open — S

### P6-b. Ray route is overlay
- Reads raster G-buffer (depth, normal, roughness, metalness, albedo, material ID, motion vectors, jitter)
- Writes lighting buffers: sun shadow, reflection radiance, short-range indirect
- Added/multiplied onto raster color; world not re-rendered by rays
- **Status:** open — S

### P6-c. Rays start only at G-buffer pixels that need them
- At internal resolution
- No full-screen ray pass re-tracing world
- No per-frame rebuild of world geometry
- **Status:** open — S

### P6-d. Never black
- If any stage cannot run, output raster color unchanged
- Never output black; every fallback logged
- **S test:** overlay never produces black pixel where raster is non-black
- **Status:** open — S

### P6-e. Full path trace stays as debug
- Old full-ray path = "Full path trace (slow, debug)", not default
- **Status:** open — S

---

## P7 — Overlay compatibility with Graphics upscaler/frame-gen

### P7-a. All combinations
- Graphics upscaler: Off, DLSS, FSR, XeSS
- Frame generation: as Graphics menu offers
- Frame generation works with ray overlay
- **S test per combination:** correct inputs and extents
- **Status:** open — S (combinations), P/V (physical verification)

### P7-b. DLSS FG on RTX 3080
- Requires RTX 40-series → disabled on RTX 3080 with reason shown in Graphics
- **Status:** open — P/V

### P7-c. FSR FG vendor-neutral
- Supported on RTX 3080
- **Status:** open — P/V

### P7-d. XeSS frame generation
- Only if official SDK provides it on this path
- **Status:** open — P/V

---

## P8 — CI SDK fetch job

### P8-a. CI fetches and hash-checks SDK components
- FidelityFX SDK: upscaler (Vulkan), frame generation, Ray Regeneration at version in temporal-reconstruction.md
- NVIDIA DLSS SDK: Super Resolution, Ray Reconstruction, Frame Generation DLLs, NGX import library, NGX runtime
- Intel XeSS SDK: only if Vulkan path documented
- DLSS bridge built from `tools/ray-reconstruction` in CI, not committed binary
- Print each file name, size, SHA-256 in job log
- Upload as build artifact
- **S test:** wrong hash fails the job
- **Status:** open — S (CI logic), P/V (actual SDK download)

### P8-b. License recording
- Record license for each component in docs
- DLSS, FidelityFX, XeSS licenses must be accepted by user; CI does not accept them
- **Status:** open — S (docs)

---

## P9 — Fail-closed smoke tests

### P9-a. Fail-closed per provider
- Missing DLL, bad hash, unsupported adapter, rejected extent, failed command, fallback → fail smoke test
- Requested denoiser that fell back shown as failed in status line and log
- **S tests for each failure mode**
- **Status:** open — S

---

## P10 — Performance items

### P10-1. Shadows: half-res RT sun shadow, 1 ray/pixel, screen-space contact shadows
- **S test + user measurement note**
- **Status:** open — S

### P10-2. Reflections: SSR first, RT where SSR misses or off-screen
- Half resolution, glossy/metal only
- **S test + user measurement note**
- **Status:** open — S

### P10-3. Indirect: quarter resolution, distance-limited with fade
- Baked lightmaps/probes for broad term
- **S test + user measurement note**
- **Status:** open — S

### P10-4. Static AS once per chunk with ALLOW_COMPACTION
- Moving actors refit; TLAS updated only on instance change
- Copy batches on residency change, spread over frames with byte budget
- **S test + user measurement note**
- **Status:** open — S

### P10-5. RT-path textures: albedo only, atlas/array, BC compression
- Fix mip fallback counter and one unavailable texture
- **S test + user measurement note**
- **Status:** open — S

### P10-6. Shader cost: WGSL override constants
- Compile out disabled features; terminate-on-first-hit for shadow rays
- Skip N·L ≤ 0 and back-facing rays; Russian roulette after bounce 2
- **S test + user measurement note**
- **Status:** open — S

### P10-7. No blocking readbacks during play
- No per-frame profiling stalls in frame path
- **S test + user measurement note**
- **Status:** open — S

### P10-8. Water: SSR first, RT where SSR misses
- **S test + user measurement note**
- **Status:** open — S

### P10-9. Day/night: remove clock reads from lighting path
- Time pass off → settings hour only; replace `colors(12.)`; region recolor off by default
- (Also P0-e)
- **S test + user measurement note**
- **Status:** open — S

### P10-10. Never black
- Pixel with no lighting history gets raster color
- (Also P6-d)
- **S test + user measurement note**
- **Status:** open — S

---

## P11 — Handoff

### P11-a. Write `Modules/Lighter/HANDOFF.md`
- Each item's status, S evidence, CI artifact link, license notes
- Exact RTX 3080 checks; what to send back (exported log file, timings CSV)
- Post short summary comment on PR #51 linking it
- **Status:** open — S

---

## Rules (standing)

1. Commit each item separately with its ID. Push to PR branch. Do not merge to main.
2. Do not weaken fallbacks, tests, thresholds, or hash checks.
3. With Ray lighting off, output must match current build. Run PNG regression tests after shared code changes.
4. Do not commit SDK binaries or retail client data.
5. PANEL-PLAN.md written first (this document), committed before code changes.
6. Do not stop early. Only items left open should need user's PC; each has exact check in handoff.
