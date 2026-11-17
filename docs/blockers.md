# Blockers

Each entry: step, error, what was tried, workaround chosen. A blocker keeps
the viewer working with the affected provider disabled; it is never marked
done.

## B-1 — No Rust toolchain in the working environment (Phases 0–6)

- **Step:** Phase 0/1 — `cargo build --release`, `cargo test --bin perfect-world-explorer`.
- **Error:** `cargo: command not found`; no `rustup`; no `rustc`; apt is not
  usable (`Permission denied` on the package lists and the Debian mirrors are
  unreachable); the sandbox's outbound hosts are limited to github.com,
  codeload.github.com, api.github.com, registry.npmjs.org, pypi.org and
  files.pythonhosted.org — none of which serve a Rust toolchain.
- **Tried:** `find / -name cargo`, `apt-get update`, checking
  `~/.cargo`, `/usr/local/cargo`. Installed `web-tree-sitter` +
  `tree-sitter-rust` from the npm registry to get a real Rust *parser*
  (`tools/check-rust-syntax.js`), which catches syntax regressions in the
  files edited here; it is not a type checker and does not replace `cargo
  check`.
- **Workaround:** code is written against the existing source with surgical
  edits; every touched `.rs` file is run through the tree-sitter syntax
  check. Compile/test verification is deferred to the PR's CI (Linux job:
  rustfmt, `cargo check --bins`, unit tests; Windows job:
  `cargo build --release`; both jobs also run GPU-check/audit steps that ran
  green). Local `cargo` results in the report are **not verified**.
- **CI result (2026-11-14):** run 43616119506 on HEAD `a75149f` — Linux pass
  (2m45s), Windows pass (4m34s). CI surfaced and this branch fixed:
  E0505 (`SdkReport` moved `dir` while the probe closure still borrowed it —
  `src/upscaling.rs`), E0599 (missing `Upscaler` trait import in
  `dx12-upscale-check.rs`), one stale test expectation for the shortened
  XeSS-FG reason string, rustfmt deltas across five files, and a mistake of
  mine while applying the rustfmt diff by hand (an add-only hunk was inserted
  at offset 0 of `src/main.rs`, breaking the crate attribute line — repaired
  in `a75149f` after CI caught it).
- **Status:** open locally (no toolchain here), closed on CI for HEAD.

## B-2 — No GPU (Phases 1, 2, 3, 4, 6)

- **Step:** Phase 1 GPU checks (`water-pass-gpu-check`, `simple-water-gpu-check`,
  `post-gpu-check`, `display-gamma-gpu-check`, …), and any runtime verification
  of an upscaler/frame-gen provider.
- **Error:** neither `nvidia-smi` nor `/dev/dri` exists in this sandbox; no
  Vulkan/D3D12 adapter can be created locally.
- **Tried:** `nvidia-smi`, `ls /dev/dri` — both absent.
- **Workaround:** GPU checks are marked **not verified** locally; CI runs the
  existing suite headless on lavapipe. The `--diagnose-upscale` /
  `--diagnose-framegen` viewer flags (formerly the `dx12-*-check` bins) are
  written to succeed with no GPU (they print the support matrix and SDK
  probe report, exit 0), so their logic paths can be exercised without
  hardware.
- **Status:** open (needs a Windows RTX/AMD/Intel machine + the SDKs).

## B-3 — Vendor SDKs cannot be downloaded here (Phases 3, 4, 5)

- **Step:** Phase 3/4 — integrate NVIDIA DLSS/Streamline, AMD FSR, Intel XeSS
  SDKs; Phase 5 — redistribution terms.
- **Error:** the only reachable hosts are github.com, codeload.github.com,
  api.github.com, registry.npmjs.org, pypi.org, files.pythonhosted.org. AMD's
  FSR SDK is on a reachable host (github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK)
  and its signed DX12 binaries were vendored into `sdk/` at the user's request
  (2026-11-14): `amd_fidelityfx_loader_dx12.dll`, `amd_fidelityfx_upscaler_dx12.dll`,
  `amd_fidelityfx_framegeneration_dx12.dll` (FSR SDK v2.4.0). The Intel XeSS
  SDK 3.0.2 and the NVIDIA DLSS/NGX runtime (from `github.com/NVIDIA/DLSS`,
  `lib/Windows_x86_64/rel/`) followed the same way, with their license texts
  vendored beside the binaries; see `THIRD_PARTY_NOTICES.md`.
- **Tried:** reachability is defined by the sandbox proxy; no fetch to vendor
  hosts was attempted beyond the allowlist (they are unreachable).
- **Workaround:** **no proprietary SDK binaries are committed**. The only
  binaries in the repository are the AMD FSR SDK 2.4.0 signed DX12 DLLs
  (MIT-style terms, notices reproduced in `sdk/AMD_FidelityFX-SDK-2.4.0-LICENSE.md`,
  `THIRD_PARTY_NOTICES.md`; SHA256 recorded there). Instead:
  - `src/main.rs` (the former `src/upscaling.rs`, merged into the viewer
    binary) implements the full option state machine, pairing table, jitter
    sequence, per-provider internal resolutions and quality modes — all
    unit-tested logic, compiled into the viewer.
  - `probe_sdk` (in the same merged module) probes a user-selected SDK directory
    (`PW_UPSCALER_SDK_DIR`, the `upscale_sdk_dir` settings key, or
    `<exe>\sdk\`) for the documented DLL names and reports what it finds;
    every provider stays disabled with a visible reason until a real SDK is
    present.
  - AMD FSR **is** now implemented (`src/fsr.rs`, Windows + DX12 only) and
    wired behind the same `Upscaling` setting as three selectable providers.
    The Graphics tab lists **FSR 1** (in-tree WGSL EASU + RCAS spatial pass,
    no FFX context and no SDK), **FSR 2** (FFX provider 2.3.4), **FSR 3.1**
    (FFX provider 3.1.6) and **FSR 4.1** (FFX provider 4.1.2, ML; the option
    is disabled up front unless an RDNA 4 / RX 9000 series AMD GPU is
    detected). Legacy `upscaling=fsr` + `fsr_version=` settings migrate on
    read (`1|2|3|4` → `fsr1|fsr2|fsr3|fsr4`; absent/`0` → `fsr4`). The AMD
    loader DLL is
    loaded with LoadLibraryW/GetProcAddress; the FFX 2.4.0 upscaler provider
    is enumerated with `ffxQueryDescGetVersions` and pinned with
    `ffxOverrideVersion`; the world projection is jittered with the FFX jitter
    sequence; motion vectors are reconstructed from reversed-Z depth by a
    fullscreen pass (camera motion only — no per-actor velocity); the FFX
    dispatch runs on its own native D3D12 command list created by the same
    device, chained to wgpu through `wgpu::Device::as_hal`
    (wgpu-hal 0.19 is now a direct dependency and exposes the raw handles);
    wgpu and FFX synchronise by queue order on the single direct queue wgpu
    creates, and every native barrier restores the state wgpu's tracker
    expects. If the loader or any call fails, the viewer logs and falls back
    to the bilinear path for the rest of the session. This FFI was written
    without a GPU to test on: the jitter sign, the motion-vector direction
    and basic operation are all runtime-unverified. Intel XeSS Super
    Resolution and XeSS Frame Generation are now **implemented but equally
    runtime-unverified** (same depth-reprojection MV pass, same FFI pattern;
    XeLL low-latency context owned by the FG integration). DLSS Super
    Resolution and DLSS Frame Generation are implemented over the same MV pass
    and are equally runtime-unverified (`src/dlss/`), driven over NGX's snippet
    ABI with a parameter table the viewer owns, because the shipped NGX DLLs
    export no parameter allocator.
  - **Intel XeSS is now vendored too** (2026-11-15): the XeSS SDK 3.0.2 release
    is published on a reachable host — `https://github.com/intel/xess`, tag
    `v3.0.2`, whose tree (codeload tarball) is the same material the
    `XeSS_SDK_3.0.2.zip` asset is built from and carries `bin/libxess.dll`,
    `bin/libxess_fg.dll` and `bin/libxell.dll`. The three files were copied
    into `sdk/` unmodified. The Intel Simplified Software License (vendored as
    `sdk/Intel_XeSS-SDK-3.0.2-LICENSE.txt`) permits redistributing the
    unmodified binaries provided the notice travels with them, so the license
    and the SDK's `third-party-programs.txt` sit beside them and
    `THIRD_PARTY_NOTICES.md` records each file's size and SHA-256.
    `.gitignore` un-ignores exactly those files (and the Intel headers), and
    `tools/package-windows.ps1` stages them into every package by default.
  - `tools/verify-vendor-sdks.py` (new) makes the wiring checkable without a
    GPU: it parses the PE export tables and proves every symbol the loaders
    resolve is exported (`xessD3D12*` 7/7, `xefgSwapChain*` 14/14, `xell*` 6/6,
    `ffx*` 5/5), compares the FFI constants with the vendored headers, and
    checks that the probe, the loaders, the packaging script and the ignore
    rules agree. B-3 is therefore **closed for XeSS/XeSS-FG/XeLL** in the
    sense that the files are present and resolvable; what remains open is
    hardware verification (B-2).
  - Licenses/attribution are recorded in `THIRD_PARTY_NOTICES.md` from the
    vendor terms as published (with the requirement to re-check the license
    shipped inside the SDK you download).
- **Status:** closed for AMD FSR, Intel XeSS/XeSS-FG/XeLL and NVIDIA
  DLSS/DLSS-G (binaries vendored, exports and constants verified). Streamline
  stays out: `sl.*.dll` is neither vendored nor staged, and the DLSS-G
  integration deliberately uses its own swapchain instead. Hardware
  verification is tracked in B-2.

## B-4 — Native pass-by-pass DX12 rewrite (Phase 2)

- **Step:** Phase 2 — device/swapchain, descriptor heaps, root signatures,
  PSOs, command lists, fences, barriers, upload path, and the terrain /
  static / SpeedTree / water / post / UI passes as native D3D12.
- **Error:** the change touches ~20 GPU modules and ~90 diagnostic binaries
  and cannot be compiled or tested here (B-1, B-2). A rewrite that "mostly
  looks right" but does not compile would break `main` for every user and is
  worse than the current `wgpu` path, which already runs on the D3D12 API on
  Windows through wgpu-hal (naga → HLSL → DXIL/DXBC).
- **Tried:** evaluated a from-scratch `d3d12`/`windows-rs` port — rejected:
  the crates are unreachable (crates.io is not an allowed host), so even the
  *binding* layer cannot be added to `Cargo.toml`.
- **Workaround:** §6 of `docs/dx12-port-notes.md` — DX12 is made the explicit
  default backend on Windows through wgpu; the native port is specified in
  §7 of that document but deliberately not attempted.
- **Status:** open (blocked on toolchain + GPU + Windows; crates.io
  unreachable).

## B-5 — Motion vectors / jitter / depth-in-SDK-format (Phase 3.14)

- **Step:** Phase 3.14 — enable jitter in the projection matrix, output motion
  vectors for all moving geometry (incl. Walker and Populator actors), write
  depth in the SDK-required format.
- **Error:** requires touching the shared projection used by every pass and
  every vertex shader, plus per-pass velocity outputs (skinning path included)
  and depth-format changes. Cannot be compiled or visually verified (B-1, B-2),
  and any mistake is a full render breakage, not a partial failure.
- **Tried:** scoped the change to: a unit-tested jitter sequence + matrix
  helper (`jitter_*`, now in `src/main.rs`) and a documented target/format
  requirement in `docs/dx12-port-notes.md` §7. The generic projection and
  shaders are **not** modified for other paths. The FSR path (`src/fsr.rs`,
  Windows + DX12) jitters only the world projection (UI/minimap/compositor
  stay unjittered), reconstructs camera-only motion vectors from depth, and
  feeds the SDK a raw R32_FLOAT copy of the reversed-Z depth buffer.
- **Workaround:** the FFX upscaler (Upscaling 4.1.2) consumes camera-motion MVs; actors that move in
  world space are reprojected as if static (ghosting on animated geometry is
  expected). There is still no per-object or skeletal velocity pass. XeSS uses
  the same camera-only MVs, and DLSS/DLSS-G consume them unchanged.
- **Status:** open (per-actor velocity pass + a verified FSR run on a real
  Windows GPU).

## B-6 — Open PR overlap (Phase 0.3)

- **Step:** Phase 0.3 — read the open PRs and avoid duplicating them. **Both
  were read on 2026-11-14** (files and diff, not just titles).
- **Finding:**
  - **#37** "Graphics: FSR1 upscaling and full vendor option lists (SDKs
    pending)" adds `src/upscaling.rs`, `src/render_scale.rs` changes, an FSR1
    EASU/RCAS WGSL pass, Graphics-tab lists for DLSS/FSR1/FSR2/FSR3/FSR4.1/XeSS
    and an independent frame-generation list, `THIRD_PARTY_NOTICES.md`, a README
    section and `GRAPHICS_TECHNOLOGIES.md`.
  - **#38** "Add named world selection to Teleport List" touches
    `Findings/Unsupported-client-paths.md`, `README.md`, `src/client_detect.rs`,
    `src/environment.rs`, `src/gpu.rs`, `src/grass.rs`, `src/main.rs`,
    `src/material.rs`, `src/modules/mod.rs`, `src/modules/populator/mod.rs`,
    `src/music.rs`, `src/navigation.rs`, `src/terrain_gpu.rs`,
    `src/terrain_store.rs`, `src/tree_source.rs`, `src/view_settings.rs`,
    `src/water.rs`, `src/windows_setup.rs`, `src/world_catalog.rs` (new).
- **Impact:** my implementation also creates `src/upscaling.rs` and edits the
  same files as #37: `README.md`, `tools/package-windows.ps1`, `src/gpu.rs`,
  `src/view_settings.rs`, `THIRD_PARTY_NOTICES.md` — expected textual
  conflicts for two PRs building the same feature from `main`. Against
  **#38**, the overlap is four files, all verified non-algorithmic:
  `README.md` (distinct sections: #38 edits the intro/teleport text, this
  branch edits the resolution-scale bullet, license section and layout),
  `src/gpu.rs` (#38 edits the terrain-material load, adapter/window-title,
  music/ambience and Teleport-List regions; this branch edits the instance
  creation, `render_extent` and Graphics-tab line — adjacent hunks, no shared
  changed lines), `src/main.rs` (#38 adds `mod world_catalog;` and terrain
  plumbing; this branch only *moved* `mod upscaling;`), and
  `src/view_settings.rs` (this branch edits the `encode()` format-string
  line and inserts keys/tests; #38 appends `selected_world` to the same
  format-string line and adds its own test — a textual conflict on that one
  line plus the tests tail; resolution = keep both, order irrelevant).
  `.github/workflows/pr-checks.yml` is **not** touched by this branch.
  This branch deliberately does **not** contain #37's FSR1 EASU/RCAS pass
  and does not pretend its vendor SDKs either — this is the SDK-integration
  follow-up to #37, not a duplicate of it. Nothing from #38 or #37 was
  copied into this branch.
- **Status:** open (PRs must be rebased/merged in order by the maintainer).

## B-7 — Frame pacing / UI input rate (Phase 4.21)

- **Step:** Phase 4.21 — with FG on, present frames in order and never drop
  the UI input rate; test with the diagnostics frame counter.
- **Error:** requires a working frame-generation provider and present loop
  (B-3) plus a Windows GPU (B-2). The FFX frame generation swapchain
  present loop is now implemented in `src/fsr/fg.rs` (proxy
  `IDXGISwapChain4` on the hwnd, UI composition texture,
  frame pacing inside `Present(1, 0)`); it has not run on hardware, so the
  pacing behaviour and the input-rate contract are unverified.
- **2026-11-15 hardware run:** FG reported "falls back to standard present".
  Root causes found in review (never hardware-verified): the FG create chain
  dropped the DX12 backend descriptor whenever a provider override was set
  (`fg_desc.p_next` was overwritten), and the upscaler chain omitted the
  mandatory `ffxCreateContextDescUpscaleVersion`, so every FFX path failed at
  `ffxCreateContext`. Fixes (version descriptor linked, backend threaded
  through, zero provider ids filtered, debug callback installed, provider
  retry-down, fence-before-allocator-reset) are on the branch; awaiting the
  next hardware run. See `docs/dx12-port-notes.md` §6a.
- **XeSS frame generation (2026-11-15):** the XeSS-FG integration implements
  the same present-loop contract — proxy swapchain created by
  `libxess_fg.dll` for the hwnd (`xefgSwapChainD3D12InitFromSwapChainDesc`),
  UI via `HUDLESS_UITEXTURE`, frame pacing inside `Present(1, 0)`, XeLL pacing
  markers — but it has not run on hardware either, so the pacing behaviour
  and the input-rate contract are unverified for both providers. See
  `docs/dx12-port-notes.md` (XeSS section) and `src/xess/fg.rs`.
- **Tried:** diagnostics `TimingFrame`/`EffectFrames` channels exist
  (`src/diagnostics.rs`) and are the right probe; the pacing contract is
  documented for the implementer in `README.md` and
  `docs/dx12-port-notes.md` §7. The `--diagnose-framegen` viewer flag prints
  the pacing expectations it would enforce.
- **Status:** open (blocked by B-2/B-3/B-4).

## B-8 — Visual parity against the shipped client (Phases 1.7, 6.32)

- **Step:** Phase 1.7 — capture reference frames at fixed camera positions for
  terrain, SpeedTree, water, sky, Walker and Populator actors; Phase 6.32 —
  compare against the port.
- **Error:** no game client is available in the environment (and none may be
  committed), and there is no GPU (B-2). Reference frames therefore cannot be
  captured or compared.
- **Workaround:** parity is **not measured**. The unchanged rendering path is
  preserved by construction (every pixel pipeline in the world/UI chain is
  byte-identical when `Upscaling = Off`; only the Windows backend *selection*
  changes), but no frame-level evidence exists.
- **Status:** open (needs client + Windows GPU).

## B-9 — XeLL latency pacing is unmeasurable here

- **Step:** Phase 4 — Settings → Graphics → **Latency** (`VSync` / `XeLL`).
- **Error:** XeLL reduces input-to-screen latency by delaying the CPU start of a
  frame; whether it does anything at all depends on the driver, the monitor and
  the application's frame time. None of those exist in this environment (no
  Windows, no GPU, no display), and the CI runners have no GPU either.
- **What is verified:** the integration follows Intel's documented checklist —
  `xellSleep` + `xellAddMarkerData` on every frame, features controlled only
  through `xellSetSleepMode`, an idle device before the sleep mode is set,
  present kept at sync interval 1 (`PresentMode::Fifo`), the XeLL context
  destroyed after the XeSS-FG context, and a second context never created when
  XeSS-FG already owns one. The exports it resolves exist in the vendored DLL
  (`tools/verify-vendor-sdks.py`), and the mode/owner/gate logic is unit-tested.
- **Not verified:** that the library actually lowers latency on the user's
  hardware, that its measured timings match this frame structure, and the
  behaviour of the delayed `xellSleep` on a frame loop that also contains
  upscaler and frame-generation dispatches.
- **Workaround until then:** `VSync` is the default and is exactly the previous
  behaviour; a XeLL failure is sticky, logged once, shown on the mark, and
  leaves the vsync'd present path untouched.
- **Status:** open (needs a Windows/DX12 machine with a display).

## B-10 — Frame generation swapchain hand-over (screen report, Phase 4.2x)

- **Step:** Settings → Graphics → **Frame generation** = FSR or XeSS (both take
  the window from wgpu and present through a proxy swapchain of their own).
- **Symptom on the reporting machine (RX 7900-class, Adrenalin):** XeSS
  *upscaling* works, but FSR-FG reports
  `creating the frame generation swapchain failed (0x3)` and XeSS-FG reports
  `xefgSwapChainD3D12InitFromSwapChainDesc failed
  (XEFG_SWAPCHAIN_RESULT_ERROR_HRESULT_FAILURE)`. XeLL is greyed out because
  Intel enables it on a non-Intel GPU only while XeSS-FG is active (B-9), so
  both symptoms are the same failure.
- **Diagnosis:** `0x3` is `FFX_API_RETURN_ERROR_RUNTIME_ERROR` ("the underlying
  runtime returned an error") and `-17` is XeSS-FG's generic
  "an HRESULT call failed" — both mean DXGI refused
  `CreateSwapChainForHwnd` for the window. DXGI allows one flip-model swapchain
  per HWND, and dropping `wgpu::Surface` is not sufficient on its own: wgpu
  keeps the backbuffer textures of a submission alive until that submission
  retires, and each backbuffer holds a reference to the swapchain.
- **Fix:** `src/fsr/swapchain.rs` — a shared hand-over used by both frame
  generation paths. It drains the device (`poll(Wait)`), then probes the window
  with a real `CreateSwapChainForHwnd` (create and release a throwaway
  swapchain, never presented), retrying for up to ~0.3 s with a growing sleep
  and falling back through a matrix of descriptions (buffer count, alpha mode,
  scaling, tearing flag). The description DXGI accepted is the one the SDK is
  given, together with a factory parented to the device's adapter and a real
  windowed fullscreen description. Both frame generation transitions now also
  wait for the window to have presented once, and a failure is retryable by
  re-selecting the setting instead of being sticky for the session.
- **What the retest has to show:** the `[FSR FG]`/`[XeSS FG]` "hand-over" line
  in stderr/`session.log` names the accepted description, the factory and the
  attempt count — and, when it cannot find any, the exact HRESULT DXGI kept
  answering (`0x887a0001` = `DXGI_ERROR_INVALID_CALL` means the window is still
  associated with a flip-model swapchain).
- **Status:** open (fix is written and compiles on Windows CI; not yet
  confirmed on hardware).

## B-11 — XeSS Frame Generation hides the whole HUD (screen report, Phase 4.2x)

- **Step:** Settings → Graphics → **Frame generation** = XeSS Frame Generation
  with XeSS-FG initialising successfully.
- **Symptom on the reporting machine:** the scene renders and animates, the
  frame rate doubles, but *every* HUD element disappears — minimap, status
  badge, menus, all of it. FSR-FG does not show the problem.
- **Diagnosis:** the XeSS-FG initialisation asks for
  `XEFG_SWAPCHAIN_UI_MODE_HUDLESS_UITEXTURE` (mode 3). Per Intel's XeSS-FG
  guide that mode has **two required inputs** — the UI-only texture *and* the
  HUD-less colour ("HUD-less Color: Required" in the mode table) — and the
  proxy's own back buffer is only the presentation target. `finish()` tagged
  the UI texture (`XEFG_SWAPCHAIN_RES_UI`) but never the HUD-less colour
  (`XEFG_SWAPCHAIN_RES_HUDLESS_COLOR`, type 0), so the model had nothing to
  interpolate and nothing to composite the UI onto: the presented frames came
  out of the passthrough copy of the composition target, which by design does
  not contain the UI, and the HUD was gone rather than interpolated.
- **Fix:** `src/xess/fg.rs` tags the composition texture as the HUD-less colour
  on every frame (`RV_UNTIL_NEXT_PRESENT`, no command list, `PIXEL_COMPUTE_READ`
  incoming state), parks it in that state after the back-buffer copy and
  restores it to `RENDER_TARGET` in the next frame's `prepare()` — the same
  pattern already used for the UI texture, depth and motion vectors. A failed
  tag logs `[XeSS FG] tagging the HUD-less colour failed (...)` instead of
  failing silently.
- **What the retest has to show:** with XeSS-FG active the HUD stays visible
  and sharp on real *and* generated frames; `session.log` names neither of the
  two tagging failures, and its `[XeSS FG] frame generation ready (...)` line
  says `UI composition mode 3 = HUDLESS_UITEXTURE`. If the HUD is still
  missing, `PW_XEFG_UI_MODE=5` (BACKBUFFER_HUDLESS_UITEXTURE: same inputs plus
  UI refinement from the back buffer) and then `PW_XEFG_UI_MODE=2`
  (BACKBUFFER_UITEXTURE: UI texture only) isolate whether the HUD-less tag or
  the blend formula is at fault — no rebuild needed, the mode is read at
  context creation and named in that log line.
- **Status:** open (fix is written and compiles on Windows CI; not yet
  confirmed on hardware).

## B-12 — XeSS Frame Generation: black frame every other present (screen report, Phase 4.2x)

- **Step:** Settings → Graphics → **Frame generation** = XeSS Frame Generation,
  with the HUD-less colour tagging from B-11 in place (the HUD was back).
- **Symptom on the reporting machine:** the scene renders correctly, but the
  whole screen flickers black "every frame, very fast" — an alternating
  good/black image, not a partial or torn one.
- **Diagnosis:** the viewer tagged its inputs with
  `XEFG_SWAPCHAIN_RV_UNTIL_NEXT_PRESENT`, which tells XeSS-FG that it may defer
  (or skip) the copy and read the *live* texture during `Present`. Those reads
  happen on the proxy swapchain's own queue, while the viewer writes the
  composition, UI, depth and motion vectors with wgpu on the app's direct
  queue; the two queues are not synchronised, so the SDK could consume a
  half-written or not-yet-written texture. The real frame is the viewer's own
  back-buffer copy and stayed correct; the *interpolated* frame — the one the
  model derives from those inputs — came out black, which at 2× present rate
  reads as a very fast black flicker. It could not show up before B-11 because
  in that mode XeSS-FG had no HUD-less input and therefore never interpolated.
- **Fix:** every input (`DEPTH`, `MOTION_VECTOR`, `HUDLESS_COLOR`, `UI`) is now
  tagged `XEFG_SWAPCHAIN_RV_ONLY_NOW` with the command list that the viewer
  owns (`tag_frame_resource_now` in `src/xess/fg.rs`): XeSS-FG records its
  copies on that list, they execute on the app's queue in order after the
  producing work, and each texture is available again as soon as the list
  retires. The park/un-park bookkeeping (`depth_mv_parked`,
  `ui_needs_rt_restore`, `compose_parked`) is gone with it — each list now
  returns its textures to `RENDER_TARGET` itself. This is the pattern the
  vendor's own samples and third-party XeSS-FG integrations use.
- **What the retest has to show:** no black frames with XeSS-FG active, and the
  first presents in `session.log` report `2 frame(s) presented` — that line was
  added for exactly this triage: `1` means the SDK is passing frames through
  (frame generation is not actually running), `2` means it is interpolating.
  `PW_XEFG_UI_MODE` from B-11 still selects the composition mode if the HUD
  needs to be narrowed down separately.
- **Status:** open (fix is written and compiles on Windows CI; not yet
  confirmed on hardware).

## B-13 — XeSS-FG black frame every other present, second attempt (screen report)

- **Report:** the `RV_ONLY_NOW` re-tagging from B-12 changed nothing: "same
  flickering every second frame", everything else works.
- **What that rules out:** the app↔SDK copy ordering. The inputs were already
  copied onto the viewer's own command list when the flicker was reproduced, so
  the black frames are not a stale-read race on those textures.
- **Consequence — two changes, both aimed at the remaining candidates:**
  1. **UI composition is now disabled by default** (`XEFG_SWAPCHAIN_UI_MODE_NONE`,
     Intel's first recommendation). The UI is drawn into the composition, every
     presented frame is exactly what the viewer rendered, and XeSS-FG only
     consumes depth + motion vectors + the presented colour. That takes the
     HUD-less colour and the UI texture out of the critical path entirely, so
     the composition inputs can no longer be what turns a frame black. The
     sharp-UI path from B-11/B-12 is still there behind
     `PW_XEFG_UI_COMPOSITION=1`.
  2. **The proxy swapchain is created without
     `DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT`.** The shared
     hand-over sets that flag for FSR (whose swapchain manages the waitable
     object itself), and a waitable swapchain is capped at one queued frame
     unless its owner also calls `SetMaximumFrameLatency`. XeSS-FG presents
     *two* frames per app Present — the real one and the generated one — so with
     the cap in place the generated frame has nowhere to go, which is exactly
     what "a black frame every second present" looks like. The reference
     integrations create the proxy without the flag. `MakeWindowAssociation`
     (followed by `DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES`) is now
     called on the proxy's factory right after init, as those integrations do.
- **Superseded by the HUD contract in `GRAPHICS_TECHNOLOGIES.md`:** the default
  UI mode is `XEFG_SWAPCHAIN_UI_MODE_BACKBUFFER_HUDLESS` (4) again. The UI is
  drawn into a copy of the composition and the proxy extracts it from the
  presented frame, so the HUD is neither interpolated nor handed over as a
  separate texture; `PW_XEFG_UI_COMPOSITION=0` selects the `NONE` behaviour
  described above.
- **What the retest has to show:** no black frames, and the ready line in
  `session.log` reading `UI composition mode 4 = BACKBUFFER_HUDLESS`, plus
  `present id …: 2 frame(s) presented`.
- **If it still flickers, these three runs decide the next step** (all are
  environment variables, no rebuild):
  | Run | Setting | Meaning of the result |
  |---|---|---|
  | 1 | (default) | — |
  | 2 | `PW_XEFG_ENABLED=0` | Proxy stays the presenter, generation off. Flicker **gone** ⇒ the *generated* frames are black: the interpolation inputs (depth, motion vectors, jitter, frame constants) are next. Flicker **still there** ⇒ presenting through the proxy itself is broken on this machine. |
  | 3 | `PW_XEFG_UI_COMPOSITION=1` | Back to the sharp-UI composition path. Flicker returns ⇒ it is specific to the composition inputs; no flicker ⇒ that path is usable again. |
- **Status:** open (fix is written and compiles on Windows CI; not yet
  confirmed on hardware).

## B-14 — Multi frame generation is hardware-gated (screen report, Phase 4.2x)

- **Step:** Settings → Graphics → **Frame generation** = DLSS Frame Generation
  with **Frame multiplier** = x3 / x4 / x6.
- **What the runtime allows:** NGX reports
  `DLSSG.MultiFrameCountMax` from the GPU architecture. On Ada (RTX 40 series)
  it is **1**, i.e. one interpolated frame per rendered frame, which is the 2x
  path this project has always used. Blackwell (RTX 50 series) reports up to
  **5** — the x6 entry. There is no supported way to ask an Ada card for more:
  third-party "MFG unlockers" patch `nvngx_dlssg.dll` in memory, which this
  project does not do, so the entries that the hardware cannot serve are greyed
  out in the combo with the architecture reason next to them
  (`crate::multiplier_support`).
- **What is implemented:** the real NGX multi frame generation path — one
  `EvaluateFeature` per generated frame with `DLSSG.MultiFrameCount` = n,
  `DLSSG.MultiFrameIndex` = 1..n and a monotonic `DLSSG.BackbufferFrameID`, each
  generated frame writing into its own backbuffer (`desc.BufferCount` is sized
  `n + 2` *before* the swapchain is created), the whole group presented with
  `Present(1, 0)` so the generated frames land on consecutive vblanks. XeSS-FG
  takes the same setting through `maxInterpolatedFrames` +
  `xefgSwapChainSetNumInterpolatedFrames`, clamped to the
  `maxSupportedInterpolations` the SDK reports (logged). FSR-FG is 2x by design
  (`numGeneratedFrames`).
- **Not verified:** that DLSS-G fills every requested index on Blackwell, that
  the pacing holds at x3/x4/x6 under a real vblank, and the XeSS-FG clamp on
  Intel hardware. No Blackwell GPU exists in this environment; the 2x path is
  what was previously written and is unchanged apart from the present interval.
- **Why the present interval changed:** presenting the generated frames with
  `Present(0, 0)` let the *next* real frame onto the same vblank, so a group
  collapsed into fewer flips — reported as "stutters when an upscaler and frame
  generation are both on". Every present in a group now uses `Present(1, 0)`.
- **Status:** open (needs a Blackwell machine for x3/x4/x6; x2 is the previously
  reported configuration and needs a re-report).

## B-15 — DLSS refused to initialise: `0xBAD00002` (screen report, RTX 3080)

- **Step:** Settings → Graphics → **Upscaling** = DLSS, any quality. Reported on
  an RTX 3080 as `NVSDK_NGX_D3D12_Init_Ext failed (0xbad00002
  FAIL_PlatformError)` followed by `DLSS failed -> bilinear`; FSR and XeSS were
  fine on the same frame, so it was never a GPU or driver-support verdict.
- **Error:** `0xBAD00002` / `FAIL_PlatformError` from
  `NVSDK_NGX_D3D12_Init_Ext`, from every DLSS build tried
  (`DLSS 2.3` … `3.7`), on hardware NGX supports.
- **Cause:** the entry point the viewer used is the *snippet* variant. Both
  vendored DLLs (`nvngx_dlss.dll`, `nvngx_dlssg.dll`) are NGX snippets, which
  are written to be loaded and driven by NGX **Core** — the `nvngx.dll` the
  display driver installs into `%SystemRoot%\System32`. The snippet's
  `NVSDK_NGX_D3D12_Init_Ext` prologue resolves the module the call came from
  (`GetModuleHandleExW(FROM_ADDRESS, <return address>, &h)`), takes that
  module's file name and requires it to contain `nvngx.dll`; the viewer's own
  `.exe` never matches, so the snippet refuses with `FAIL_PlatformError` before
  it looks at the device. NGX also resolves its feature DLLs from the running
  executable's folder, not the game's.
- **Fix:** the viewer now bootstraps NGX Core the way an `nvsdk_ngx_*.lib`
  application does (`src/dlss/loader.rs`): load `%SystemRoot%\System32\nvngx.dll`,
  initialise it once per process with `NVSDK_NGX_D3D12_Init_with_ProjectID`
  (GUID project id, `NVSDK_NGX_ENGINE_TYPE_CUSTOM`; falls back to
  `…D3D12_Init` when a driver rejects the project id), then create and evaluate
  both features through Core. `NVSDK_NGX_FeatureCommonInfo.PathListInfo` lists
  the viewer's `sdk/` folder first, so the vendored DLSS build is the one Core
  loads rather than the copy in the driver store. The parameter map is Core's
  (`…AllocateParameters` + `NVSDK_NGX_Parameter_Set*`); the in-tree table stays
  as the fallback for driver builds that do not export those.
- **Diagnostics added:** the loaded core path, the application data folder (now
  `%LOCALAPPDATA%\PerfectWorldExplorer\NGX`, because NGX answers
  `FAIL_PlatformError` when it cannot write its log next to an executable in
  `Program Files`), each init attempt's named result, every NGX log line
  (through `NVSDK_NGX_LoggingInfo`, prefixed `[NGX …]`, written to
  `session.log` alongside the viewer's own lines) and, after init,
  `SuperSampling.Available` and `SuperSampling.FeatureInitResult`.
- **Not verified:** that Core accepts the project id on every driver branch and
  that DLSS then creates and evaluates on the 3080 — no GPU in this
  environment. If it still fails, the log now says which part: the core's load
  path, the refused init call with its named result, NGX's own reason, or
  `SuperSampling.Available`.
- **If `nvngx.dll` is missing:** a driver install cleaned by NVCleanstall/DDU
  can omit the NGX component. The error names the path it tried.
- **Status:** open on hardware (needs a re-report on the 3080).

## B-16 — "DLSS is blurry at Quality" and per-upscaler sharpening (screen report)

- **Report:** DLSS at `upscale_quality=quality` looked blurry next to FSR, even
  though the same quality entry is 1.5× on both.
- **What the ratios show:** they are identical and match the vendor tables
  (`src/main.rs::internal_factor`): Quality `1/1.5`, Balanced `1/1.7`,
  Performance `0.5`, Ultra Performance `1/3`. So the softness was not a wrong
  render resolution — it was the reconstruction pipeline.
- **Cause 1 — motion-vector units.** DLSS assumes the motion-vector buffer is at
  *output* resolution unless `NVSDK_NGX_DLSS_Feature_Flags_MVLowRes` is set, and
  this viewer writes them at *render* resolution. At Quality that made NGX read
  1.5× more motion than the world actually had, so history was matched to the
  wrong pixels and detail was averaged away. XeSS is the mirror image: its
  `XESS_INIT_FLAG_HIGH_RES_MV` (1<<0) is what requests *output*-resolution
  vectors, so leaving it clear — as the viewer does — is already correct for
  render-resolution vectors, and `XESS_INIT_FLAG_USE_NDC_VELOCITY` (1<<4) stays
  clear because the vectors are in pixels. `MV.Scale.X/Y` stays 1.0 for the same
  reason (already in pixels).
- **Cause 2 — no sharpening at all.** DLSS was handed a hard-coded
  `Sharpness = 0`, i.e. none, while FSR arrives with RCAS at 0.75. The vendored
  header deprecates `NVSDK_NGX_DLSS_Feature_Flags_DoSharpening`
  (`nvsdk_ngx_defs.h`: "Sharpness is not supported") but still honours
  `NVSDK_NGX_Parameter_Sharpness` (`"Sharpness"`), which is what the new
  `dlss_sharpness` setting drives.
- **Cause 3 — frame time.** `InFrameTimeDeltaInMsec` was unset, so NGX had no
  idea how fast content moved and kept its accumulation at whatever its default
  is. It is now measured per evaluate call.
- **Sharpening for every provider:** `fsr1_sharpness` (RCAS stops),
  `fsr3_sharpness` (FFX, now also shown for FSR 2), `dlss_sharpness` (NGX) and
  `xess_sharpness` (the viewer's own pass in `FsrComposite`, because Intel's SDK
  3.0.2 has no sharpening control). Defaults: 0.2 / 0.75 / 0.35 / 0.35.
- **Verified here:** the WGSL compiles through a WGSL parser (`wgsl_reflect`:
  entry points and the group-0 binding layout match the Rust bind group), every
  constant used was read out of the vendored headers, the settings round-trip
  test covers both new keys, and rustfmt + the tree-sitter syntax check pass.
- **Not verified:** how it *looks*. There is no GPU in this environment, so the
  sharpness defaults (0.35) are a judgement call, and the claim that MVLowRes
  removes the softness needs a screen report on the RTX 3080. If 0.35 is too
  much, the slider goes to 0 for the raw vendor look.
- **Status:** open on hardware (re-report needed: Quality at 0 / 0.35 / 0.75).

## B-17 — "DLSS/XeSS still blurry, the sharpness slider does nothing" (screen report)

- **Report:** after B-16, DLSS and XeSS still looked blurry, their sharpness
  sliders changed nothing, and DLAA / Ultra Quality looked identical to
  Upscaling Off.
- **What those three symptoms have in common:** a provider that is *requested*
  but not *running*. `crate::render_extent` answers from the support table, so
  the world kept rendering at 1280×720 while the actual present path was
  `RenderScale::blit` — a plain bilinear stretch. That is blurrier than
  Upscaling Off (native 1080p), it makes every sharpness slider inert (the
  provider's composite pass never runs), and at a 1.0× ratio the stretch is the
  identity, which is exactly "DLAA looks the same as off".
- **Fixed:** the world's render extent now follows the provider that is
  actually alive (`RenderScale::input_extent`), so a failed provider degrades to
  a *native* render instead of a stretched one. The Graphics tab shows
  `DLSS/XeSS/FSR is NOT running - the world is rendered at native resolution
  instead` with the SDK's own reason, the corner mark says the same, and a
  failed init is retried three times, three seconds apart, instead of being
  fatal for the session. The DLSS/XeSS badges now carry
  `render → display - sharp <value>` so a screenshot says everything.
- **Also fixed:** the DLSS sharpness slider could never have worked through NGX.
  Every shipped wrapper writes `NVSDK_NGX_Parameter_Sharpness` in the **create**
  parameter map (`nvsdk_ngx_helpers_d3d.h`, `NVSDK_NGX_D3D12_DlssCreateParams`),
  and the feature flag that enables NGX's own sharpening is deprecated
  ("Sharpness is not supported"). The slider now runs the viewer's own
  RCAS-style pass in `DlssUpscaler::composite`, the same one XeSS uses, with the
  gain scaled to 1.5 at the top of the range.
- **Diagnostics added:** `[DLSS] first evaluate OK (1280x720 -> 1920x1080,
  quality)` / `[XeSS] first execute OK (…)` in `session.log`, a `[upscaler]`
  line every time the live state changes (`… running 1280x720 -> 1920x1080
  sharp 0.35` or `upscaling=DLSS is NOT running, world rendered 1920x1080
  (native)`), and failure lines that say "rendering at native resolution
  instead" with the SDK's reason.
- **Not verified:** which branch the report was actually on. If the mark already
  showed a green `DLSS (quality)` badge, then DLSS *was* evaluating and the
  softness has another cause — in that case the badge's new
  `1280x720 -> 1920x1080 - sharp 0.35` line plus `[DLSS] first evaluate OK` in
  the log decides it, and the next report should quote them.
- **Status:** open on hardware (needs one more screen report).

## B-18 — "with DLSS on, the water breaks" + `EvaluateFeature(DLSS)` fails `0xbad00005`

- **Report:** with DLSS selected, the water renders wrong and the Graphics tab
  says `DLSS is NOT running - the world is rendered at native resolution
  instead (no upscaling)`. `session.log` carries
  `NVSDK_NGX_D3D12_EvaluateFeature(DLSS) failed (0xbad00005
  FAIL_InvalidParameter)` — the feature *creates* and then never evaluates.
- **Cause 1 (the failure):** `NGX_D3D12_EVALUATE_DLSS_EXT` in the vendored
  `sdk/include/nvidia/nvsdk_ngx_helpers_d3d.h` sets
  `NVSDK_NGX_Parameter_DLSS_Pre_Exposure` and `…_DLSS_Exposure_Scale` on every
  evaluate, converting `0.0` to `1.0` (`nvsdk_ngx_defs.h:813-814`). NGX divides
  by the pre-exposure, so the parameter map's default of 0 is rejected with
  `FAIL_InvalidParameter`. This viewer never set either key, so *every*
  evaluate failed — DLSS could not run at all. `src/dlss/imp.rs` now sets both
  to 1.0, and for parity with the same wrapper also sets `TonemapperType`, the
  two indicator inverts, the twelve subrect bases (zero, full-frame) and the
  optional resources the wrapper binds as `nullptr` (transparency mask,
  exposure texture, bias-current mask, the `GBuffer.*` set, 3D and reflection
  motion vectors, `DepthHighRes`, `Position.ViewSpace`,
  `RayTracingHitDistance`, `IsParticleMask`, `AnimatedTextureMask`). Types
  follow the wrapper exactly: `SetUI` → `set_u32`, `SetF` → `set_f32`,
  `SetI` → `set_i32`.
- **Cause 2 (the water):** the fallback added in B-17 resized the world, depth,
  water and post targets against `RenderScale::input_extent()` *inside the
  redraw loop*, but `Renderer::render` and the three frame-generation contexts
  were still handed `applied_extent` — the support table's answer (1280×720 at
  Quality) even when nothing was running. The buffers became 1920×1080 while
  every pass still believed it drew at 1280×720: the water uniform's
  `size: [w, h, …]` (reflection/refraction UV math), the viewport and the
  projections all disagreed with the actual targets. Fixed by deciding the
  extent once per frame in the settings block (`world_extent`: provider alive →
  `applied_extent`, otherwise the plain scaled extent), reallocating only when
  it moves, and passing `world_extent` to `Renderer::render` and to the
  FSR/XeSS/DLSS frame-generation contexts and their recreate checks.
- **Rule:** every per-frame pass must be told the extent its targets are
  actually allocated at. `RenderScale::input_extent()` is the authority; no
  pass may be handed `applied_extent` directly, because that value describes
  what the *settings* asked for, not what is alive.
- **Status:** fixed in source; needs one hardware report with DLSS at Quality to
  confirm `[DLSS] first evaluate OK (1280x720 -> 1920x1080, quality)` and
  `[upscaler] DLSS (quality) running 1280x720 -> 1920x1080 sharp 0.35`.

## B-19 — "DLSS is NOT running … `Init_with_ProjectID` is not exported, `Init` -> `0xbad0000c`"

- **Report:** water is fine now, but DLSS still reports `DLSS is NOT running -
  the world is rendered at native resolution instead (no upscaling)`, with
  `NVSDK_NGX_D3D12_Init_with_ProjectID is not exported` and
  `NVSDK_NGX_D3D12_Init -> 0xbad0000c FAIL_OutOfDate`.
- **What the codes actually mean:** `nvsdk_ngx_defs.h:152-154` defines
  `FAIL_OutOfDate` as "a function was used which requires a newer version of the
  NVIDIA Display Driver or feature library than is currently installed". So the
  module that answered is *older than the driver* — it is not an overlay, an
  injector or a GPU problem, which is what the old hint pointed at.
- **Cause:** `find_ngx_core` checked `%SystemRoot%\System32` **first**, and then
  returned the first file it found. That is not where NGX Core lives: driver
  471.11 and later install it as `_nvngx.dll` in
  `System32\DriverStore\FileRepository\*`, and `nvsdk_ngx_loader.h`
  (`NGXLoadCoreLibrary`) never looks in `System32` at all. A leftover legacy
  `nvngx.dll` in `System32` therefore shadowed the real core, answered
  `FAIL_OutOfDate` to everything newer than itself, and — because only one
  candidate was ever tried — ended the search for the whole session.
- **Fixed:** `ngx_core_candidates` lists every module that can provide the SDK
  side of NGX: each `DriverStore` package with a core (newest first), then the
  executable's and the viewer's `sdk/` folders, then the legacy `System32` copy;
  `_nvngx.dll` before `nvngx.dll` in every folder, duplicates removed. `NgxCore`
  tries **all** of them until one initialises, so a stale copy cannot block a
  good one, and each rejection is logged as
  `[NGX] <path> cannot be used: <reason>`. The vendored snippet is appended as
  the last resort: a snippet resolves the core itself and forwards the
  SDK-facing entry points — the earlier `0xBAD00002` finding was about
  `Init_Ext`, the Core-to-snippet entry point, which the viewer never calls.
- **Rule:** never let a *discovery* failure be fatal while untried candidates
  remain. `system32` is a legacy location, not the install location, for
  NVIDIA's NGX component.
- **Status:** fixed in source; needs a hardware report. A working run shows
  either no `[NGX] <path> cannot be used:` lines at all, or a few followed by
  `[NGX] … is the first of N candidate NGX modules that initialised`, then
  `[DLSS] first evaluate OK (…)`.

## B-20 — "none of the 3 NVSDK_NGX modules found could be used"

- **Report:** `_nvngx.dll` and `nvngx.dll` from the driver store both answered
  `Init_with_ProjectID is not exported, Init -> 0xbad0000c FAIL_OutOfDate`; the
  vendored `sdk\nvngx_dlss.dll` answered
  `Init -> 0xbad00001 FAIL_FeatureNotSupported`. In the *same* `session.log`,
  one run earlier, the **same build** and the **same `_nvngx.dll`** initialised
  successfully through `NVSDK_NGX_D3D12_Init` after a ~10 s stall.
- **What that means:** a single `Init` attempt per module is the actual bug.
  Which entry point a given driver implements, and which SDK API version it
  accepts, cannot be known up front: this machine's core has no
  `Init_with_ProjectID` at all, so the application-id `Init` is the only way in,
  and its answer varies run to run.
- **Fixed:** `NgxCore::initialise` tries every documented entry point, each with
  a ladder of SDK API versions (`0x15` -> `0x0F` -> `0x03`), because
  `FAIL_OutOfDate` is defined as "a function was used which requires a newer
  version of the NVIDIA Display Driver or feature library than is currently
  installed":
  1. `NVSDK_NGX_D3D12_Init_with_ProjectID`,
  2. `NVSDK_NGX_D3D12_Init_ProjectID` (DLSS 2.1-era),
  3. `NVSDK_NGX_D3D12_Init` (application id, SDK signature),
  4. `NVSDK_NGX_D3D12_Init_Ext` (application id, parameter map),
  5. `NVSDK_NGX_D3D12_Init` with the Core-to-snippet signature — **only** on an
     `nvngx_dlss*.dll`; a core would read the version we pass as a
     `FeatureCommonInfo*` and dereference a small integer.
- **Diagnostics:** every candidate logs its PE file version
  (`[NGX] candidate …\nvngx.dll version 32.0.15.6094`) and every variant logs
  its outcome, so the next report says which build answered what.
- **Also fixed, from the run that got past init:** that run created DLSS and
  then failed `EvaluateFeature(DLSS)` with `0xbad00005 FAIL_InvalidParameter`
  again. The `NVSDK_NGX_Parameter_*` helpers are now resolved from the core
  *and*, when it does not export them, from the feature snippet — a legacy
  `_nvngx.dll` exports `CreateFeature`/`EvaluateFeature` but not the whole
  parameter surface. `NVSDK_NGX_D3D12_GetParameters` joins the map sources after
  `AllocateParameters` and `GetCapabilityParameters`: it is deprecated and it is
  the only allocator an older core has, and NGX validating a map it did not
  create is exactly the `FAIL_InvalidParameter` case. Falling back to the
  viewer's own table is logged loudly now.
- **Status:** fixed in source; needs one hardware report. A working run shows
  `[NGX] initialised through <variant> (SDK API 0x…)` followed by
  `[NGX] parameter map: NGX's own, from <allocator>` and then
  `[DLSS] first evaluate OK (…)`.

## B-21 — "it simply crashes when selecting DLSS upscaling"

- **Report:** selecting DLSS kills the process. `session.log` ends one line after
  `[NGX] initialising through …\_nvngx.dll (application data folder: …)`, twice,
  with `[NGX] candidate … version 32.0.16.2114` above it.
- **Cause:** the init ladder added in B-20 called
  `NVSDK_NGX_D3D12_Init_ProjectID` — a DLSS 2.1-era name that the vendored
  `nvsdk_ngx.h` does not declare. Its signature was a guess, and a driver that
  implements that name differently (or implements a *newer* argument list under
  it) dereferences whatever lands in the register the missing argument came from.
  A version number read as a `FeatureCommonInfo*` is a crash, not a rejected
  call. Nothing else in that build had never been called before.
- **Fixed:** the name is gone. Every remaining variant uses the signature the
  vendored header declares — `…_Init_with_ProjectID`, `…_Init`, `…_Init_Ext`, and
  `…_Init` in the Core-to-snippet flavour on `nvngx_dlss*.dll` only.
- **Diagnostics added, so the next attempt is decidable rather than another
  round trip:**
  * `[NGX] calling <name> (SDK API 0x…)` is written before every call, so a
    crash names the entry point and the version rung that caused it;
  * the candidate line lists the module's real `NVSDK_NGX_D3D12_Init*` exports,
    read from its PE export directory
    (`[NGX] candidate …\_nvngx.dll version 32.0.16.2114, exports:
    NVSDK_NGX_D3D12_Init, …]`) — ground truth about what this driver build has;
  * the version ladder only steps down on `FAIL_OutOfDate`, the one result that
    means "the version asked for is newer than this module". Every other code
    would answer identically at every version, and each extra call is another
    chance to be inside the driver when it fails.
- **Rule:** an entry point may only be called with the argument list a header in
  `sdk/include` declares. "It is probably the same name from an older SDK" is
  not a licence to invent a prototype; a wrong prototype inside a graphics
  driver is a process kill.

## B-22 — DLSS initialises and creates its feature, then every evaluate is `0xbad00005` (hardware reports, RTX 3080)

- **Report 1** (`session-2.log`): NGX initialises
  (`[NGX] initialised through NVSDK_NGX_D3D12_Init (SDK API 0x0015)`), DLSS
  creates its feature (`[DLSS] super resolution ready (quality, 2293x913 ->
  3440x1369)`), and every `NVSDK_NGX_D3D12_EvaluateFeature(DLSS)` then fails
  `0xbad00005 FAIL_InvalidParameter`. Above each attempt:
  `[NGX] no NVSDK_NGX_Parameter_Set* helpers were exported by the core or the
  snippet; the viewer's own parameter table is in use`, plus four
  `[FSR] … CreateCommittedResource failed … (HRESULT 0x80070057)`.
- **Report 2** (`session-3.log`, after the parameter-map fix): NGX now hands out
  its own map (`[NGX] parameter map: NGX's own, from
  NVSDK_NGX_D3D12_AllocateParameters (written through its vtable)`), and the
  four `CreateCommittedResource failed … 0x80070057` lines are still there —
  every texture, every size — followed by the same
  `EvaluateFeature(DLSS) failed (0xbad00005)`.

### Cause 1 — `D3D12_HEAP_PROPERTIES` broke its own validity rule (the real bug)

`create_in_heap` passed `CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE`
for **every** heap type. `ns-d3d12-d3d12_heap_properties` states the rule
exactly:

> When **Type** is `D3D12_HEAP_TYPE_CUSTOM`, **CPUPageProperty** and
> **MemoryPoolPreference** must not be `..._UNKNOWN`.
> When **Type** is not `D3D12_HEAP_TYPE_CUSTOM`,
> **CPUPageProperty** and **MemoryPoolPreference** must be `..._UNKNOWN`.

`NOT_AVAILABLE` is `1`, not `0`, so every `D3D12_HEAP_TYPE_DEFAULT` request was
`{DEFAULT, NOT_AVAILABLE, UNKNOWN}` — invalid — and `CreateCommittedResource`
answered `E_INVALIDARG` (`0x80070057`). The custom-heap probe then succeeded
(`{CUSTOM, NOT_AVAILABLE, L1}` *is* the documented valid custom combination), so
nothing ever failed loudly: every FSR, XeSS, DLSS-G and DLSS texture in this
viewer has been created on a **custom heap** instead of the default heap.

That is what NGX rejected. NGX validates the resources it is given — it reads
their description and their heap — and a custom-heap resource is not the
default-heap resource its kernels and views are built for, so
`EvaluateFeature(DLSS)` answered `FAIL_InvalidParameter` however correct the
formats (they were: `R8G8B8A8_UNORM`, `R32_FLOAT`, `R16G16_FLOAT`,
`R8G8B8A8_UNORM`+UAV) and the sizes were.

**Fixed:** `CPUPageProperty` is `..._UNKNOWN` for every non-custom heap and
`NOT_AVAILABLE` only for `D3D12_HEAP_TYPE_CUSTOM`. The probe matrix stays as a
safety net, and `NativeTexture2D::heap_kind()` now queries `GetHeapProperties`
on the resource it actually got, so the log says which heap won instead of
leaving it to inference:
`[DLSS] textures: colour default heap, depth default, motion vectors default,
output default`.

**Correction to the first report's diagnosis:** the `CreationNodeMask: 0` →
`1` change made no difference and was never the bug. The same page says
"passing zero is equivalent to passing one, in order to simplify the usage of
single-GPU adapters", which is why wgpu-hal's own zero-buffer allocation passes
zeros. The masks stay at `1` because they are explicit, not because they fix
anything.

### Cause 2 — the parameter map was a `AllocateParameters` map, not a capability map

`NVSDK_NGX_D3D12_GetCapabilityParameters` allocates a map **pre-populated** with
NGX's capabilities; `AllocateParameters` does not
(`nvsdk_ngx.h:346-359, 393-403`). NVIDIA's own DLSS samples create *and*
evaluate with the capability map, and the helpers warn about exactly this case
("You used NVSDK_NGX_AllocateParameters() for creating InParams. Try using
NVSDK_NGX_GetCapabilityParameters() instead", `nvsdk_ngx_helpers.h`). The ladder
now prefers `GetCapabilityParameters`, then `AllocateParameters`, then the
deprecated `GetParameters`.

### Cause 3 — required extent keys were only written at create time

`EvaluateFeature` answers `FAIL_InvalidParameter` for a *missing* required entry,
not only a wrong one. `Width`, `Height`, `OutWidth`, `OutHeight` and the DLSS 4
era `ResourceWidth`/`ResourceHeight`/`ResourceOutWidth`/`ResourceOutHeight`
(`nvsdk_ngx_defs.h:747-750`) are now written on every evaluate, not just once.

### Cause 4 — the parameter-map ladder was inverted (fixed in `cf98e3b`… `3fb19e7`)

The previous build required the exported `NVSDK_NGX_Parameter_Set*` setters
before asking Core for a map. Neither the vendored `nvngx_dlss.dll` nor the
driver's `_nvngx.dll` exports them — `session-3.log` lists the core's entire
parameter surface and it is nothing but `*Parameters` allocators — so the ladder
was skipped. Core's map is now filled through its own vtable (the eight `Set`
overloads are slots 0-7 of `NVSDK_NGX_Parameter`, `nvsdk_ngx_params.h:55-73`).

### What is verified rather than assumed

* every parameter name the viewer writes was diffed against
  `nvsdk_ngx_defs.h` (38 keys, all exact) and every overload type against
  `nvsdk_ngx_helpers_d3d.h:297-365` (`SetF` for jitter/sharpness/MV scale/
  pre-exposure, `SetUI` for extents/subrects/tonemapper, `SetI` for
  reset/create-flags/output-subrects/invert-axes);
* `EvaluateFeature` cannot see D3D12 resource *states*, so a missing barrier
  cannot produce `FAIL_InvalidParameter`; what it can inspect is the handle, the
  map entries, and each resource's description and heap — which is why the heap
  is the suspect that fits every observation;
* the formats are the ones the DLSS samples use, and `heap_kind()` now proves
  the heap.

### Diagnostics added in this round

`[DLSS] NGX reports SuperSampling.Available = …` before create,
`[DLSS] feature init result: …` (NGX's own verdict, read back out of the map)
after create, `[DLSS] textures: … heap` for all four resources, and the
`[NGX] parameter map: NGX's own, from …` line that already existed.

- **Status:** the heap bug is fixed and verified on hardware (`session-4.log`:
  no `CreateCommittedResource failed` line at all, and all four textures report
  `default heap`), but it was **not** what NGX rejected: evaluate still answered
  `0xbad00005`. The extent half of the same failure is **B-23**.

### Aftermath: the report that closed this cause and opened the next one

`session-4.log` also settles the other half of the diagnosis, in the opposite
direction from what this entry first assumed. `FeatureInitResult` came back
`0x00000001`, which is `NVSDK_NGX_Result_Success` (`nvsdk_ngx_defs.h:99`) — the
feature **was** created. So `SuperSampling.Available = 1` and feature creation
both succeeded, and every refusal is confined to `EvaluateFeature`. That is B-23.

### The version question

The vendored binaries are `nvngx_dlss.dll` 310.9.16 and `nvngx_dlssg.dll`
310.9.16 — exactly `NVIDIA/DLSS` `v310.9.16`, the newest release — and the
driver's core reports `32.0.16.2114`. `0xbad00005` was never an SDK-age problem.

### Frame Generation is hardware-gated, on purpose

DLSS-FG (including 4× Multi Frame Generation, which reorders and duplicates
frames on the GPU) needs an RTX 40/50-series GPU. On an RTX 3080 the Graphics
tab lists DLSS Frame Generation as disabled with the reason
(`support_framegen`, `GpuFacts::supports_dlss_frame_generation`), and
multipliers above ×2 stay disabled wherever the runtime does not report them.
DLSS Super Resolution with the transformer model does run on RTX 20/30-series
hardware.

## B-23 — DLSS creates its feature and then refuses every evaluate `0xbad00005`: the render extent was the viewer's own arithmetic (hardware report, RTX 3080)

- **Report** (`session-4.log`, HEAD `83446a5`): NGX initialises
  (`NVSDK_NGX_D3D12_Init`, SDK API `0x0015`), the capability map is NGX's own,
  `SuperSampling.Available = 1`, `[DLSS] textures: colour default heap, depth
  default, motion vectors default, output default`, `FeatureInitResult` is
  `0x00000001` = `NVSDK_NGX_Result_Success` — the feature is created — and then
  every `NVSDK_NGX_D3D12_EvaluateFeature(DLSS)` answers `0xbad00005`
  (`FAIL_InvalidParameter`), retried on several frames, with
  `[upscaler] … DLSS Super Resolution is NOT running` at the end.
- **What that rules out:** the heaps (B-22, verified fixed), the SDK and DLL age
  (`nvngx_dlss.dll` 310.9.16 is the newest `NVIDIA/DLSS` release; core
  32.0.16.2114), the parameter-map kind (a real capability map from
  `GetCapabilityParameters`, written through its vtable), and feature creation
  itself. `FAIL_InvalidParameter` on evaluate can only come from what evaluate
  is handed: the handle (valid, just created), the map entries, or the D3D12
  resources' descriptions.

### Cause — the render size was computed instead of asked for

The viewer derived its render extent from the quality ratio
(`display / scale`). The programming guide is explicit about the alternative
(§3.2.2):

> The values returned … must then be passed exactly as given to the next
> `NGX_API_CREATE_DLSS_EXT()` call … If the `InRenderSubrectDimensions` are
> outside of the min/max range, the call will fail with
> `NVSDK_NGX_Result_FAIL_InvalidParameter`.

`NGX_DLSS_GET_OPTIMAL_SETTINGS` (`nvsdk_ngx_helpers.h:65-114`) is the documented
way to obtain that range, and for a mode **without** dynamic resolution min and
max both collapse to the single optimal pair — so *any* deviation, including one
pixel, is "outside the min/max range". The arithmetic differs in exactly that
way: for 3440x1369 at Quality (ratio 1.5) the viewer produced `2293x913`, while
`1369 / 1.5 = 912.67` truncates to `912`; for the same display at Balanced the
viewer produced `2024x805` where `1369 / 1.7 = 805.3` truncates to `805`. One
mode is off by a pixel, the other is not — which is precisely the kind of
one-pixel disagreement the guide says is fatal and which the viewer could never
notice, because a wrong subrect is not something a log line reports.

**Fixed** in `src/dlss/loader.rs` + `src/dlss/imp.rs`:

* `NgxApi::optimal_settings()` reproduces the helper byte for byte — read
  `DLSSOptimalSettingsCallback` out of the capability map with
  `NVSDK_NGX_Parameter_GetVoidPointer` (vtable slot 15, the eight `Get`
  overloads' position after `GetUI` = 11, both verified by
  `SuperSampling.Available` already reading correctly), write the *user-selected*
  display size into `Width`/`Height`, `PerfQualityValue`, `RTXValue = 0`, call
  the callback, then read `OutWidth`/`OutHeight` and the
  `DLSS.Get.Dynamic.{Min,Max}.Render.{Width,Height}` range.
* The answer, not the viewer's ratio, is now what the feature is created with
  (`Width`/`Height`/`ResourceWidth`/`ResourceHeight`), what every evaluate
  re-states, and — through `DlssUpscaler::render_extent()` — what `src/gpu.rs`
  sizes the world colour/depth targets to. The extent the *settings table* asked
  for is kept separately (`requested_extent()`) and is what decides whether the
  feature must be rebuilt, so NGX moving the size by a pixel cannot cause a
  rebuild every frame.
* When the capability map carries no callback, the old behaviour is kept and the
  line says so, rather than silently pretending DLSS agreed.

### The instrumentation that makes the next report decisive

All 37 `[NGX]` lines in `session-4.log` are the viewer's own; NGX's callback
produced none. `NVSDK_NGX_FeatureCommonInfo.LoggingInfo` only reaches an
application NGX accepts as verified, and an unverified one gets nothing at all —
no error either. So:

* `MinimumLoggingLevel` is now `NVSDK_NGX_LOGGING_LEVEL_VERBOSE` (2), not `ON`,
  because that is the level the guide says reports parameter-level detail;
* `__NGX_LOG_LEVEL`, `NVNGX_LOG_LEVEL` and `__NGX_LOG_PATH_OVERRIDE` are set to
  `3` and to the viewer's application-data folder before `Init`, which is what
  NGX's own binary reads when the registry says nothing (§3.19);
* on the *first* failed evaluate the viewer now copies the tail of every `*.log`
  in `%LOCALAPPDATA%\PerfectWorldExplorer\NGX` into `session.log`
  (`[NGX file …] last N line(s)`). NGX writes those files regardless of whether
  the callback is honoured, so that is the channel that survives.

### Everything else that could produce `FAIL_InvalidParameter`, and where it stands

| Class | State |
| --- | --- |
| Subrect outside NGX's min/max range | **the cause above — fixed** |
| `Width`/`Height` vs the created textures disagreeing | already equal; now sourced from one value |
| Extent keys missing on evaluate | written on every evaluate since `3fb19e7` |
| Output texture with mips | one mip (`dlss5-d3d12-fix` report) |
| `TYPELESS` output format | typed `R8G8B8A8_UNORM`, asserted in `DlssNatives` |
| `DLSS.Render.Subrect.Dimensions` / `Output.Subrects` incoherent | both written from the same extent |
| `MVLowRes` vs `InMVScaleX/Y` | flags and scale set from one source (`MVLowRes`, scale 1.0) |
| Resource states | inputs `NON_PIXEL_SHADER_RESOURCE`, output UAV with `ALLOW_UNORDERED_ACCESS`; NGX restores states itself (§3.4) |
| Heap properties | default heap, `GetHeapProperties` verified (B-22) |
| Parameter map kind | capability map (B-22 cause 2) |
| SDK / DLL age | current (B-22, "The version question") |
| Handle validity / ordering | `CreateFeature` returns `0x1`; evaluate runs on a recording, non-copy list with the feature's node mask (B-21) |

### The audit sweep that followed

Every parameter **name** the DLSS path writes was diffed character-for-character
against `nvsdk_ngx_defs.h` and `nvsdk_ngx_defs_dlssg.h` at `v310.9.16` — 75
constants, all present — and every parameter **type** against the reference
wrappers (`NGX_D3D12_CREATE_DLSS_EXT`, `NGX_D3D12_EVALUATE_DLSS_EXT`,
`NGX_D3D12_CREATE_DLSSG`, `NGX_D3D12_EVALUATE_DLSSG`,
`NGX_DLSS_GET_OPTIMAL_SETTINGS`). One type mismatch came out of it, plus five
bugs in the failure handling around it:

1. **`PerfQualityValue` was written with the `SetUI` overload.** The reference
   writes it with `SetI`, and the two are different vtable slots (3 and 4), so
   the entry NGX's own `GetI` reads was of the wrong kind.
   `nvsdk_ngx_defs.h` defines `FAIL_InvalidParameter` as "parameters had an
   incorrect value **or type**, or a required parameter was not provided", which
   made this a live candidate for the very code being chased. It is `SetI` now.
2. **`result_name` labelled success as failure.** It named results by their low
   half-word, so `0x00000001` — `NVSDK_NGX_Result_Success` — printed as
   `FAIL_FeatureNotSupported`, and `0xbad00000` (`Result_Fail`) printed as
   `SUCCESS`. That single line is why §16 read a *created* feature as a failed
   initialisation and sent a round of diagnosis the wrong way. Success is `0x1`
   and failure is `Fail | code`; both are named from that now.
3. **No 32x32 floor.** Guide §3.3.1: "The minimum supported output resolution
   for DLSS is 32x32. Attempting to initialize DLSS with a smaller output
   resolution will fail with an Invalid Parameter error." A minimised window hit
   exactly that, spent the retry budget and left the red "DLSS is NOT running"
   line in the Graphics tab. `DlssUpscaler::new` refuses with that reason and
   `src/gpu.rs` does not ask at all below 32x32.
4. **The retry budget was never replenished.** `dlss_retries < 3` gated the
   three-second retry permanently, so three transient failures anywhere in a
   session disabled DLSS until the settings changed. A frame that evaluates now
   resets the counter.
5. **A failed NGX init was cached for the life of the process**
   (`OnceLock<Result<…>>`), turning one bad moment — an SDK folder that was
   momentarily unreadable, a driver that answered while the feature DLL was
   still loading — into "DLSS can never work in this process". Only success is
   cached now.
6. **VERBOSE NGX logging could flood `session.log`.** Every forwarded line opens
   the log file, and a refused evaluate repeats every frame. Forwarding stops
   after 4000 lines and says so.

Checked and found **correct**, so a later report can skip them: the feature
flags (`MVLowRes` set because the vectors are render-resolution and *not*
dilated, `MVJittered` clear because the reprojection pass uses the unjittered
matrices — guide §3.6.2; `DepthInverted` set because
`Camera::projection_matrix` is genuinely reverse-Z, near→1); `MV.Scale.X/Y` =
1.0 and never 0.0 (§3.6.3); motion vectors in render pixels, `previous -
current`, `[0,0]` at the top left (§3.6.1); inputs transitioned to
`NON_PIXEL_SHADER_RESOURCE` and the output created with
`ALLOW_UNORDERED_ACCESS` (§3.4, and NGX restores states itself); LDR sRGB input
with `IsHDR` clear (§3.1.2); the command-list requirements — open and recording,
node mask 0 ≡ 1, non-copy queue (`nvsdk_ngx.h:508-513`); the three-slot native
ring with its fence and bounded wait; the argument order of
`NGX_DLSS_GET_OPTIMAL_SETTINGS` itself (`nvsdk_ngx_helpers.h:65-114`); and every
DLSS-G parameter type.

- **Status:** fixed in source; needs one hardware report. A working run shows
  `[DLSS] optimal settings: quality at 3440x1369 -> render 2293x912 (range …;
  the viewer asked 2293x913)`, then `[DLSS] first evaluate OK (… -> …)` and
  `[upscaler] DLSS (quality) … running …`. If it still fails, `session.log` now
  carries NGX's own `[NGX file …]` lines naming the refused parameter, which is
  the one thing this round could not obtain.

## B-24 — No mip-map bias while an upscaler renders below display resolution (audit finding, fixed)

- **Step:** Graphics tab, DLSS/XeSS/FSR at any quality below Native AA.
- **Error:** none — a quality gap, not a failure. Guide §3.5: "When DLSS is
  active, the rendering engine must set the mip-map bias … to
  `log2(Render XResolution / Display XResolution) - 1.0 + epsilon`" so textures
  are sampled at *display* resolution rather than the lower render resolution.
  Before this fix the viewer applied no bias at all — `textureSample`
  everywhere, no `textureSampleBias`, no LOD clamp — so anything mipmapped was
  sampled as if the screen were the render resolution.
- **Fix.** The bias is computed once per frame, where the frame loop already
  knows both extents, in `crate::render_scale::set_mip_bias`: the guide's
  `log2(renderX / displayX) - 1` while a temporal upscaler is alive (DLSS,
  FSR 2, XeSS, or FSR 1 through `fsr1_eligible`), the bare ratio for the
  resolution slider on its own, clamped to `-3..=0`. Supersampled render sizes
  take no bias, and neither does the slider alone at 100 %; Native AA keeps the
  guide's `-1`, because DLAA is the same feature at a 1.0x ratio. At the reported
  2293x960 DLSS render extent on a 3440x1440 window the bias is -1.585, i.e.
  `exp2(bias) = 0.334`.
- **Plumbing.** `Uniform` — the camera buffer every world pipeline binds — gains
  a fourth `lod` lane, `[bias, exp2(bias), 0, 0]`; the three WGSL `Camera`
  mirrors (`src/gpu.rs`, `src/terrain_gpu.rs`, `src/far_trees_gpu.rs`) carry the
  same member, and `model_receiver_gpu::Params` carries `lod.x` for the object
  atlas. The minimap/full-map camera and the water-reflection camera write
  `[0, 1, 0, 0]`: they are drawn into their own targets from the same atlases
  and must keep the sharpest mips.
- **Sites that now bias.** Only the textures that actually have a chain change,
  and every one of them is a world-input atlas:
  - `src/gpu.rs` — the world material atlas (8 mips). The pass supplies its own
    gradient, so the bias is folded in as `uv_dx * camera.lod.y`.
  - `src/terrain_gpu.rs` — the nine `textureSampleGrad` calls over the
    BC/full/small layer arrays, whose chains are the full `side.ilog2() + 1`
    levels. Folded in as `dx *= l.uv * camera.lod.y`, before the sample, because
    the same gradient sets the minor-axis footprint.
  - `src/far_trees_gpu.rs` — the tree atlas (8 mips). `dx`/`dy` are scaled
    straight out of `dpdx`/`dpdy`, so the LOD the shader derives from them, the
    half-texel clamp and the sample all agree.
  - `src/model_receiver_gpu.rs` — the object atlas pages (8 mips). The three
    `textureSample` calls become `textureSampleBias(…, p.lod.x)`.
- **Deliberately not biased.** Every world texture created with
  `mip_level_count: 1` — water, shore, clouds, sky backgrounds, effects, decals,
  weather — has no chain to bias, so those pipelines are left alone rather than
  sprinkled with no-op calls. `post_gpu` runs *after* the upscale at display
  resolution and must not be biased. Explicit-LOD sites (`textureSampleLevel` in
  `sky_gpu`, `grass_instances`, the terrain masks) already pick their level on
  purpose and are untouched.
- **Verification.** Unit tests in `src/render_scale.rs` pin the formula: the
  guide value at the 2293x960/3440x1440 pair, the slider-only value, identity at
  native and supersampled sizes, and the `-3` clamp. Provenance of the formula
  is NVIDIA's own text, not a guess. What still needs hardware: that distant
  terrain and foliage at a sub-native extent read sharper without shimmering,
  and that Native AA/100 % is pixel-identical to before the change.

## B-25 — Alpha-blended world content has no depth, so its motion vectors are the background's

- **Step:** Frame generation on (FSR FG, DLSS-G or XeSS FG), or any temporal
  upscaler, with fast camera movement: clouds, torch/portal/waterfall effect
  sheets, or rain and snow in view.
- **Symptom:** that content slides, smears or doubles in the generated frames, and
  only when the camera *translates* — pure rotation looks fine.
- **Cause (code-verified).** `cloud_gpu` (billboard and native triangle path),
  `gfx_effect_gpu` (every quad slot) and `weather_gpu` all draw with
  `depth_write_enabled: false`, and `sky_gpu` does the same. The depth clear is
  `0.0`, which is the far plane in this renderer's reversed-Z convention. The
  shared motion-vector pass (`src/fsr/mv.rs`) rebuilds each pixel's world position
  from that depth buffer and reprojects it with the previous view-projection, so
  every one of those pixels is reprojected as whatever surface is behind it — the
  far plane over the sky, i.e. pure rotation. Frame generation interpolates with
  exactly those vectors, so on a translating camera the generated frames put the
  content at the wrong distance while the rendered frames put it at its own; the
  two alternate every other frame. That is the visible bug.
- **Fix.** A **depth-only coverage pass** per content class: the same geometry
  again, with a `cover` fragment entry point, `depth_write_enabled: true`,
  `depth_compare: GreaterEqual` (reverse-Z: only a nearer surface wins), colour
  writes masked off. A pixel owns the content's depth only where the content's
  *effective* alpha — the value the colour pass actually blends — is at least
  `0.5`, so semi-transparent rims stay background and keep the background's
  motion:
  - **Clouds** (`cloud_gpu`): `tex.a * p.scene.y` for billboards,
    `tex.a * 0.6`/`* 1.0` for the triangle path; own pass right after the colour
    pass, `CloudGpu::draw(.., coverage)`.
  - **Effect sheets** (`gfx_effect_gpu`): `clamp(tex.a * v.color.a, 0, 1)`; the
    twin pipeline is a `PipelineKey::coverage` variant, created with the colour
    pipeline and drawn by `Draw::draw_coverage` inside the same pass, after the
    last bucket. Element-160 meshes are skipped: `WriteZ` already recorded their
    depth in the colour pass.
  - **Precipitation** (`weather_gpu`): `tex.a * 0.52` rain, `* 0.82` snow; own
    pass after the colour pass, `WeatherGpu::draw(.., coverage)`.
  Only the **main view** asks for coverage. The mirror and refraction depth feeds
  the water and nothing reads it for motion, and writing there could only perturb
  the reflection pass. Because the coverage writes into `renderer.depth`, the
  upscalers (DLSS/FSR/XeSS) get the correction too, not just frame generation.
- **Decals need nothing.** `gfx_decal_gpu` also writes no depth, but a decal is
  coplanar with the surface it is painted on — the depth buffer already holds that
  surface's depth at those pixels, so its motion vector is already the right one.
  Only content that stands *in front of* the surface behind it loses its motion.
- **Precipitation writes no depth, on purpose.** An earlier revision of this fix
  gave the flakes a place in the depth buffer as well. That is wrong for the
  nearest content in the scene: a depth write depth-rejects everything drawn
  afterwards, so a raindrop in front of the water surface, the player or an
  add-on punched a hole the size of the drop in each of them. Alpha-blended
  precipitation does not occlude anything; it needs a motion vector, not a surface,
  and the attachment below is where it gets one. The pass still depth-*tests*, so a
  flake behind a wall claims nothing.
- **Object motion: the attachment.** Depth cannot express a *moving* surface —
  it carries no identity — so the motion-vector pass takes a second producer.
  `weather_gpu`'s coverage pass now renders into `object_mv`, an `Rgba16Float`
  target at render extent (`xy` = the pixel-space vector, `z` = a coverage flag),
  and `src/fsr/mv.rs` substitutes those vectors where the flag is set. All six
  consumers of the pass (FSR, DLSS and XeSS, on both the upscaler and the
  frame-generation paths) take the same attachment, cleared once per frame by the
  renderer. Precipitation is exact there rather than approximated: a particle's
  position is a pure function of the clock (`center = camera + right*x +
  forward*z + (0, fall, 0)`, `fall = 55 - fract(phase + seconds*speed) * 110`), so
  re-evaluating it with `seconds - dt` and the previous camera basis gives its
  true previous position. Two details that matter: the vector is built from the
  **unjittered** view-projection, because a vector must describe where the surface
  *was*, not where this frame sampled it; and a particle whose phase wraps between
  the two frames has its own-motion term dropped for that one frame instead of
  carrying a 110-unit jump across the screen.
- **Audit: what every alpha renderer needs** — the criterion is whether the pixel
  stands in front of a *distant* surface (parallax error) or is essentially
  coplanar with the surface behind it (its depth already gives the right vector):

  | renderer | depth write | verdict |
  |---|---|---|
  | clouds (`cloud_gpu`) | no | **covered** — stands in front of terrain/sky |
  | effect sheets (`gfx_effect_gpu`) | no | **covered + object motion** — no depth, because a sheet must not occlude what is drawn after it (B-26) |
  | precipitation (`weather_gpu`) | no | **covered + object motion** |
  | decals (`gfx_decal_gpu`), shore band (`shore_gpu`), baked SDR blend (`gpu.rs`), alpha model meshes (`model_receiver_gpu`) | no | nothing to do — coplanar with, or a hair behind, the surface the depth buffer already holds at that pixel |
  | water surface, complex path (`water_gpu`) | **yes** | already has depth, so already has vectors |
  | water surface, alternate path (`simple_water_gpu`) | no | **open** — see below |
  | sky (`sky_gpu`) | no | nothing to do — it *is* the far plane |
  | reflection/refraction depth | — | not an input to any motion vector |

- **What remains.** Two things. (1) *Attached* content — effect sheets, alpha model
  meshes — still carries camera-only motion (the sheet's published vector
  reprojects its own world position, which is right for a sheet that stands still
  in the world and short by the root's own speed for one that rides a walker), so a sheet hanging off a moving
  monster ghost at the monster's own speed. That is deliberately not "fixed" here:
  the model it is attached to is reprojected the same way, so the two agree, and
  giving the sheet its own vector while the model keeps a camera-only one would
  make the sheet slide against the body it is attached to. The real fix is
  per-object motion for *models*, which is a renderer-wide feature (rigid
  transforms now, skinned vertices later), not an effect fix. (2) The alternate
  water surface writes no depth; giving it coverage would also make it occlude the
  submerged half of a player model drawn afterwards, so it needs a hardware
  comparison rather than a blind change.
- **Ordering note.** Coverage runs immediately after each class's colour pass, so
  a coverage write can depth-reject content blended *later* in the same frame
  (a big glow sheet in front of the clouds can now stop the cloud behind it). In
  the client's forward ordering that content was already composited in the wrong
  order — clouds blended over effects drawn before them — so the readable result
  is no worse, but it is a real order dependence to know about.
- **Verification.** `coverage_records_depth_only_where_the_sheet_dominates` proves
  the effect twin writes the sheet's own depth at full opacity and leaves the
  cleared far plane at 20 % opacity (real device, real pipeline, depth read back
  through `TextureAspect::DepthOnly`); `both_cloud_modules_carry_the_coverage_entry_point`
  and `the_shader_carries_the_coverage_entry_point` (weather, effects) keep the
  entry points and thresholds in the WGSL; CI compiles all three modules on
  lavapipe. A hardware run still has to show: the smear gone at speed with frame
  generation on, no new shimmer along the coverage edge, and whether the residual
  own-motion ghosting of precipitation is objectionable.

## B-26 — An effect sheet's motion, without giving it a surface — fixed

- **Step:** frame generation or any upscaler on, with an animated effect root whose
  sheets sit between the camera and something drawn later — the water surface, the
  player model, an add-on.
- **Symptom (before the fix):** a hole the shape of the sheet in the surface behind
  it. A torch flame in front of a lake erased the water under the flame; a glow in
  front of the host player erased the part of the body it covered.
- **Cause.** B-25's coverage pass wrote each sheet's depth into the scene depth
  buffer at the end of the receiving-model pass, so the motion-vector pass could
  reconstruct the sheet's motion. Everything drawn afterwards depth-tests against
  that buffer, and a sheet nearer than the water or the player rejected them.
- **The fix.** Sheets are static in world space, so their vector is a pure camera
  reprojection of their own interpolated world position, which the vertex stage
  already carries. The motion pass now targets the `object_mv` attachment, tests
  depth and never writes it, and publishes that vector: `current + motion =
  previous` in render pixels with y down, the convention the shared pass reads. The
  sheet stops occluding anything, and the vector is the sheet's own rather than the
  wall's behind it — which is what the depth write was there to buy, at the cost of
  the hole.
- **What it costs.** The upscalers lose the sheet's depth, because there is none:
  their depth input at a sheet pixel is the background's again. The motion is
  right, which is what frame generation interpolates with, and the depth of
  translucent blended content was always the background's before B-25.
- **Where it runs.** In `Renderer::draw_models`, immediately after the main view's
  colour pass, before the add-ons, the water and the weather draw and before the
  mirror or refraction view rewrites the frame block and the bucket order. The pass
  needs the frame's unjittered view-projection, which `draw_models` now receives.
- **The history.** `Draw::draw_motion` keeps the view-projection the previous
  motion pass used and falls back to the current one on the first frame, so a load
  or a cut publishes the zero motion of a still sheet instead of a jump.
- **Verification.** `motion_publishes_the_sheets_own_vector_and_never_a_surface`
  renders an opaque sheet, then the same sheet through a moved camera, then the
  same sheet at 20% opacity, and asserts on the attachment's bytes: owned with a
  zero vector, owned with a non-zero vector, unclaimed — and that the depth buffer
  is byte-for-byte still the clear value in all three cases. On hardware: a torch
  in front of water with no hole in it, and no sliding of the glow against the wall
  behind it as the camera translates.

## B-27 — DLSS evaluates fail with `FAIL_InvalidParameter` (open)

- **Step:** enable DLSS Super Resolution in the viewer.
- **Symptom:** `DLSS failed, rendering at native resolution instead:
  NVSDK_NGX_D3D12_EvaluateFeature(DLSS) failed (0xbad00005 FAIL_InvalidParameter)`,
  once per attempt, and `[upscaler] upscaling=DLSS Super Resolution is NOT
  running, world rendered 3440x1369 (native)`.
- **What works:** NGX initialises, `SuperSampling.Available` reads `1`,
  `CreateFeature` answers `Success` with `FeatureInitResult Success`, and the four
  resources are created at the sizes the feature was built for (colour
  `B8G8R8A8_UNORM` 2293x913, depth `R32_FLOAT` 2293x913, motion vectors
  `R16G16_FLOAT` 2293x913, output `B8G8R8A8_UNORM` 3440x1369).
- **What NGX itself says** — its own log, dumped by this viewer on the first
  failure:
  `[NGXDLAA::EvaluateFeature:788] error: could not find Color parameter`.
- **Cause.** The feature reads its four resources out of the
  `NVSDK_NGX_Parameter` map with `Get(const char *, ID3D12Resource **)`. A pointer
  written through a *different* `Set` overload is stored under a different type
  tag, and that read then reports the key as missing — exactly the message above,
  and exactly the `FAIL_InvalidParameter` the caller sees. `nvsdk_ngx_params.h`
  declares the overload order (eight numeric `Set`s, then `ID3D11Resource *`,
  `ID3D12Resource *`, `void *`), and this viewer addressed the resource ones by
  that declaration. The integer half of that order demonstrably holds on this
  core; the pointer half does not.
- **The fix, as implemented.** The slot is *measured with the feature*, because
  the feature is the only thing whose opinion matters: the four resources are
  written through the declared slot and evaluated, and on
  `FAIL_InvalidParameter` the same four pointers are written through the next
  candidate slot and evaluated again (`src/dlss/imp.rs`, the ladder in `record`).
  A rejected evaluate validates before it records anything, so the command list
  carries no half-applied state between attempts. The winning slot is remembered
  for the session, so the ladder is paid once, and each attempt logs one line:
  `[DLSS] resource parameters reach the feature through Set slot N; the declared
  slot M was rejected` or `[DLSS] evaluate rejected the resource parameters
  written through Set slot N (…)`. The map also reports its own writability once
  per session (`[NGX] parameter map: integer write round trip works / does not
  work`, and which pointer slots a null probe round-trips).
- **Second candidate, changed in the same pass.** The feature map is now a map
  from `NVSDK_NGX_D3D12_AllocateParameters`, and the capability map is kept only
  for `SuperSampling.Available` and the optimal-settings callback. `nvsdk_ngx.h`
  documents the capability map as usable "in the same ways" but recommends the
  allocator, and the allocator is what NVIDIA's own samples create and evaluate
  with. Both changes are in the same run on purpose: the log distinguishes them
  (`[NGX] parameter map: NGX's own, from …AllocateParameters` versus
  `…GetCapabilityParameters`), and either one alone would have cost a session.
- **What a run has to settle.** Whether one of the three slots is accepted. If
  the log shows `integer write round trip does not work`, the map itself is the
  problem rather than the slot, and the next step is to stop evaluating through
  the capability map (`NVSDK_NGX_D3D12_GetCapabilityParameters`) and use a map
  from `NVSDK_NGX_D3D12_AllocateParameters` for `CreateFeature`/`EvaluateFeature`,
  keeping the capability map only for `SuperSampling.Available` and the optimal
  settings callback — the header documents the capability map as usable "in the
  same ways" but recommends the allocator. If all three slots are rejected with
  the integer round trip working, the ladder needs a fourth candidate (the
  remainder of the vtable) and the NGX log dump will name the resource it could
  not find.
