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
  `amd_fidelityfx_framegeneration_dx12.dll` (FSR SDK v2.4.0). NVIDIA's DLSS SDK
  is not published on a reachable host and is not available at all.
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
    XeLL low-latency context owned by the FG integration). An active DLSS
    provider still renders through the bilinear path (no temporal accumulation
    is faked for it). The NVIDIA SDK still could not be obtained; the Intel
    DLLs are user-supplied (`sdk/SDK_GOES_HERE.txt`) and are never committed.
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
- **Status:** closed for AMD FSR and Intel XeSS/XeSS-FG/XeLL (binaries vendored,
  exports and constants verified); **open for NVIDIA DLSS/Streamline**, which is
  not published on a reachable host, may not be redistributed, and remains not
  integrated. Hardware verification of the Intel path is tracked in B-2.

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
  the same camera-only MVs; an active DLSS provider renders bilinearly as
  before.
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
