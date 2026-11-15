# DX12 port notes — renderer map before the port

Status at the time of writing: **the renderer has not been compiled or run in
this working environment.** There is no Rust toolchain (`cargo`/`rustc`), no
GPU, and no Windows machine here. Everything in §1–§5 was verified by reading
the source only. See `docs/blockers.md`. The PR's CI *is* the build/test
authority for this branch: both jobs (Linux — rustfmt, `cargo check --bins`,
unit tests, lavapipe GPU checks; Windows — `cargo build --release` + package
audit) passed on HEAD `a75149f` (run 43616119506, 2026-11-14).

Branch: `arena/4d8f7e24-perfect-world-explorer` (Arena pins one branch per
session; the requested `feature/dx12-upscaling-framegen` branch was not
created — the PR is opened from this branch instead). Base commit:
`52858e844f39f839c7174421ba4d038bac627ebc` (`main`).

## 0. Current graphics backend — verified answer

**The backend is `wgpu` 0.19, not a native D3D11/D3D12/Vulkan renderer.**

Evidence:

- `Cargo.toml` depends on `wgpu = "0.19"`, `winit = "0.29"`, `egui-wgpu = "0.27"`.
- `src/gpu.rs` (≈5,372 lines) creates `wgpu::Instance::new(wgpu::InstanceDescriptor {
  backends: wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::all()), .. })`
  (three sites: windowed viewer, the offline preloader path, and the module
  preview path share the same pattern) and requests an adapter with
  `power_preference: HighPerformance` (or `WGPU_ADAPTER_NAME` override).
- All shaders are WGSL (`wgpu::ShaderSource::Wgsl`, 47 module
  creations across `src/`). wgpu-hal translates them (naga → HLSL →
  DXIL/DXBC via DXC, or SPIR-V → driver for Vulkan) at runtime.
- `src/render_scale.rs` documents the Offscreen-target + bilinear blit path.
- CI (`.github/workflows/pr-checks.yml`) runs the GPU checks headless on Linux
  with Mesa lavapipe (`WGPU_BACKEND=vulkan`, `VK_ICD_FILENAMES=...lvp_icd...`).
  On Windows, `Backends::all()` lets wgpu-hal pick between DX12 and Vulkan per
  driver. The repository contains **no D3D12, D3D11, Vulkan or OpenGL API
  calls of its own**; `grep -i "d3d12\|d3d11\|vk::" src/` finds only prose.

Consequences:

1. "Porting to DX12" with zero behaviour change means: on Windows, request the
   DX12 backend explicitly (and fail or fall back loudly rather than silently
   rendering on Vulkan), and keep everything else. wgpu-hal's DX12 path runs
   the existing WGSL through naga → HLSL → DXC; every previous pass then
   executes on the D3D12 API without rewriting a single shader.
2. A *native* pass-by-pass DX12 rewrite (own `IDXGIFactory7`, swapchain,
   descriptor heaps, root signatures, pipeline states, command lists, fences,
   barriers, upload path) is a multi-thousand-line change across every
   `src/*_gpu.rs` module **and** the ~90 `src/bin/*-gpu-check` diagnostics that
   exercise them. It cannot be compiled, run or verified in this environment
   (no toolchain, no Windows, no GPU, no vendor SDKs downloadable), so it is
   recorded as a blocker rather than faked. See `docs/blockers.md`.
3. On Linux/CI the checks keep running on Vulkan (lavapipe); pinning DX12 is
   `cfg(windows)`-gated.

This branch makes the DX12 backend the explicit default on Windows (§6) and
documents what a native port would require (§7).

## 1. Process map (frame flow)

```
EventLoop → RedrawRequested
  ├─ window resize / resolution-scale change → realloc scaled targets
  ├─ Renderer::render(...)               (src/gpu.rs, the big frame command)
  │    ├─ pre-pass: terrain shadow? (none; depth only)
  │    ├─ passes into offscreen WorldTarget (render_scale extent or native)
  │    ├─ water reflection pass (refraction/reflection targets)
  │    ├─ water main pass, shore/edge passes
  │    ├─ sky passes (sky_geometry/sky_gpu + weather_gpu + cloud_gpu)
  │    └─ post chain (post_gpu: bloom, sharpen, underwater, gamma ramp)
  ├─ RenderScale::blit                   (bilinear upscale into composition view)
  ├─ UI pass                             (egui → composition pass, LoadOp::Load)
  ├─ DisplayGamma (fullscreen ramp, fullscreen-only)   (display_gamma.rs)
  └─ PresentEncode (UNORM swapchain fallback)          (present_encode.rs)
```

`render_scale` keeps the 3D world at 10–200 % of the window (default 100 %)
and blits into the full-resolution composition view; the UI stays at native
resolution. When the upscaling work lands, this blit is the Off path that is
reused as the fallback if a provider cannot initialise.

## 2. Passes and where they live (grep-verified)

File → WGSL modules → `begin_render_pass` sites → role

| File | WGSL | passes | Role |
|---|---|---|---|
| `src/gpu.rs` | 1 | 5 | Renderer core: world target, scenery/main composition, UI composition, minimap/full-map views, module add-on passes (Walker/Populator draw through this file's pass machinery) |
| `src/terrain_gpu.rs` | 1 | 0* | Layered terrain material atlas, masks, day/night lightmaps, far terrain geometry; pipelines shared |
| `src/sky_gpu.rs` | 1 | 0* | Sky dome/panorama, sun/moon discs, seam handling |
| `src/model_receiver_gpu.rs` | 1 | 0* | Scene-light receivers for animated EC models |
| `src/water_gpu.rs` | 2 | 3 | Water surface shading, reflections/refraction targets, shoreline waves |
| `src/simple_water_gpu.rs` | 1 | 1 | Fallback/simple water surface (terrain-only depth) |
| `src/shore_gpu.rs` | 1 | 2 | Shoreline edge alpha, foam |
| `src/weather_gpu.rs` | 1 | 1 | Weather particles (rain/snow) |
| `src/cloud_gpu.rs` | 2 | 1 | Cloud surface (`.cld`) |
| `src/far_trees_gpu.rs` | 2 | 0* | SpeedTree LOD compute + indirect draw, impostor atlas |
| `src/grass_instances.rs` | 1 | 0* | Grass instance compute |
| `src/gfx_effect_gpu.rs` | 1 | 1 | Authored ECWLD/ECM world effects (quads) |
| `src/gfx_decal_gpu.rs` | 1 | 1 | Static decals |
| `src/post_gpu.rs` | 1 | 6 | Bloom (mono/low-glow modes), sharpen, underwater distortion, glow chain |
| `src/display_gamma.rs` | 1 | 1 | Whole-frame client gamma ramp (fullscreen only) |
| `src/present_encode.rs` | 1 | 1 | UNORM-swapchain encode fallback |
| `src/render_scale.rs` | 1 | 1 | Scaled world target + bilinear upscale blit |
| `src/effect_preview.rs` | 1 | 1 | Headless effect-preview harness |
| `src/model_draw_gpu.rs` / `model_texture_gpu.rs` / `model_receiver_gpu.rs` | 0 | pipelines | Static-model draw validation, shared model textures/pipelines |
| `src/world_models_gpu.rs` | (via gpu.rs uniform path) | in gpu.rs | EC world models, overlays, Walker/Populator hookup |
| `src/modules/walker/mod.rs`, `src/modules/populator/mod.rs` | 0 | 1 each | Add-on draw entry points (module passes) |
| `src/modules/librarian/preview3d.rs` | 0 | 2 | Character-designer 3D preview |

\* passes are recorded in the shared command encoder in `gpu.rs`.

Pass-count totals (grep `begin_render_pass`): ≈33 sites. WGSL modules ≈47
(production + diagnostics). Every shipped pass is one of the above; there is no
pass in the source that this table does not cover.

## 3. Resources held across frames

- **World target** (scaled or native): sRGB + linear-alias view; depth
  (`Depth32Float`-class target, checked at `resize_world_targets`); shared by
  the terrain/scenery/model/water passes for occlusion.
- **Water reflection target** (rendered-water view of the world from the
  mirrored camera), **refraction target** (copy of the composed scene colour +
  depth used for shoreline/underwater distortion), **water edge alpha** target.
- **Post chain**: scene-chain intermediate, bloom mips/luminance chain,
  optional underwater distortion target (`post_gpu.rs`).
- **Scaled target** (`render_scale`): sRGB texture at the scaled extent,
  bilinear-blitted into the native composition view; UI loads with
  `LoadOp::Load` and stays native.
- **Terrain**: BC1/BC2 material atlas (4096²), layer masks, lightmaps, far
  geometry; residency driven by `terrain_residency.rs` (paged) or
  `Resident` (all-to-VRAM). `cache/terrain-v1/` XOR-plane disk cache; hot-set
  defaults to 32 MiB (`PW_TERRAIN_RAM_MIB`, 1–256 MiB). The README documents a
  "2 MiB hot set" — that figure is historical (the README predates the
  32 MiB default); the operative value in `src/terrain_store.rs` is
  `DEFAULT_BUDGET = 32 MiB`. Nothing about the cache format or hot-set
  behaviour is changed by this work.
- **Model textures**: shared BC pages, whole-world ≈5.4 GiB sources.
- **Uniform buffer** (`gpu.rs::Uniform`): one 4×4 matrix, direction, clip and
  bounds vectors, regional lighting, 80 grass-history rows, fog range/colour.
  All passes read it through bind-group 0/1; none of it is per-draw.

## 4. Settings keys (persisted, `view_settings.rs` — unchanged by this work unless listed)

Version 1 text file (`%LOCALAPPDATA%\PerfectWorldViewer\viewer-settings.txt`).
Keys parsed today: `radius, terrain_cutoff, tree_lod_scale, scenery_quality,
camera_speed, boost, sensitivity, far, time_of_day, time_pass_enabled,
time_pass_speed, all_to_vram, whole_world, water_reflections, world_effects,
atmospheric_fog, bloom, bloom_strength, bloom_mode, bloom_tint,
bloom_brightness_fade, bloom_opacity_fade, client_gamma, gamma_setting,
terrain_lightmaps, model_backface_culling, music_enabled, music_volume,
ambience_enabled, sound_volume, effects_enabled, effects_volume, fullscreen,
show_zone_entry, show_minimap, minimap_range, tree_wind_strength,
client_tree_wind_preview, regional_tree_lighting, grass_distance_fade,
grass_wind_enabled, grass_wind_amplitude, streamed_regions, area_grass_lightmaps,
vegetation_authored_mips, celestial_discs, celestial_brightness,
ignore_celestial_flags, cloud_detail, sharpen, resolution_scale, world_map_style,
minimap_style, minimap_shape, weather_override, sky_override, effect_shaft,
effect_distance, logging_mask, logging_dir`.

New keys added by this work (§6): `upscaling`, `upscale_quality`,
`frame_generation`, `upscale_sdk_dir`, `latency`. Defaults keep behaviour
identical; an old file without them decodes to the same values as before plus
the new defaults (see the migration test in `view_settings.rs`). `latency`
defaults to `vsync`, which is exactly the pre-existing present path (wgpu's
default `PresentMode::Fifo`), so adding the key changes nothing until the user
picks `xell`.

## 5. Keep-unchanged invariants (checked, not aspirational)

- Terrain cache format `cache/terrain-v1`: untouched (`src/terrain_store.rs`
  not modified by this work).
- Settings keys and meanings: untouched; only added keys.
- Module IDs (`walker`, `populator`), `Modules/…/module.ini` layout,
  `PW_MODULES_DIR` override: untouched.
- Release ZIP structure (`Perfect World Explorer/` top folder, exe + `Modules/`
  + `SHA256SUMS.txt`): preserved; `THIRD_PARTY_NOTICES.md` is added to it.
- The 2 MiB-era hot-set behaviour — the code today pages a 32 MiB decoded
  budget; this work does not change either number.
- Known-approximate systems stay approximate: water, sky, terrain, fog,
  animated-EC coverage. No "fixes" smuggled in with the renderer work.

## 6. What this branch changes

1. **DX12 on Windows by default.** `gpu.rs`'s three instance-creation sites
   request `Backends::DX12` on Windows (still honouring `WGPU_BACKEND` for
   diagnostics, falling back to `Backends::all()` if DX12 adapter creation
   fails, with a printed warning). Non-Windows unchanged. `adapter_info` is
   retained so the Graphics tab can state the real backend/GPU/driver.
   This is *not* a native DX12 rewrite — see §0 and `docs/blockers.md`.
2. **Settings → Graphics → Upscaling** (`Off` / DLSS Super Resolution /
   FSR 1 / FSR 2 / FSR 3.1 / FSR 4.1 / XeSS) and **Frame generation**
   (`Off` / DLSS Frame Generation / FSR Frame Generation / XeSS Frame
   Generation), independent selectors, disabled options with a visible reason,
   Resolution scale slider active only when Upscaling is Off. FSR 1 is the
   in-tree spatial EASU + RCAS pass (`src/fsr1.wgsl`, `src/fsr1_gpu.rs`);
   FSR 2/3/4 select the FFX provider major (2 → 2.3.4, 3 → 3.1.6, 4 → 4.1.2)
   in the unified upscaler DLL. FSR 4 additionally requires an RDNA 4
   (RX 9000 series) GPU, gated by adapter vendor/name; FSR 1 has no SDK
   requirement at all. Frame generation works with any upscaler or Off
   for the FSR path (it has its own depth-reprojection motion-vector pass);
   XeSS Frame Generation is offered only with XeSS upscaling or Off, as the
   vendor documents. While upscaling
   or frame generation is active, a small on-screen mark (top-left) shows
   the upscaler version and the FG frame counter (or a red fallback note
   when a path failed). This module (the former `src/upscaling.rs`, now merged
   into the viewer) holds the state
   machine, jitter sequence, per-provider recommended internal resolutions,
   the pairing table, and `probe_sdk`, which probes a user-chosen directory
   (Settings → `upscale_sdk_dir` or `PW_UPSCALER_SDK_DIR` or `<exe>\sdk\`)
   for the documented DLL names without loading any of them. NVIDIA SDK
   binaries are not shipped or committed; the AMD FSR SDK 2.4.0 signed DX12
   binaries **and** the Intel XeSS SDK 3.0.2 binaries (`libxess.dll`,
   `libxess_fg.dll`, `libxell.dll`) **are** vendored under `sdk/` at the user's
   request (MIT-style license for AMD; Intel Simplified Software License for
   Intel, both notices reproduced, SHA-256 recorded in
   `THIRD_PARTY_NOTICES.md`; packaging stages them). AMD FSR
   can and does dispatch from this wgpu 0.19.x renderer: the FFX upscaler runs
   on its own native D3D12 command list chained to the wgpu device/queue
   (`wgpu::Device::as_hal`; see `src/fsr.rs` and `docs/blockers.md` B-3/B-5).
   **FSR frame generation** (`FrameGenerationSwapChainForHwndDX12`) is wired
   the same way: the viewer drops its wgpu surface on enable, FFX creates its
   own proxy `IDXGISwapChain4` for the window, the UI is drawn into a separate
   native-resolution texture that the swapchain composites onto real and
   generated frames, and `ffxDispatchDescFrameGeneration` runs on the
   interpolation command list (`src/fsr/fg.rs`). Release destroys the FFX
   contexts and hands the window back to a recreated wgpu surface. Both frame
   generation paths share the hand-over in `src/fsr/swapchain.rs`: DXGI allows
   one flip-model swapchain per window, and dropping `wgpu::Surface` does not
   retire the frames that still reference the old swapchain, so the hand-over
   drains the device and retries a real `CreateSwapChainForHwnd` probe — with a
   matrix of descriptions (buffer count, alpha mode, scaling, tearing flag) as
   the fallback — until DXGI accepts one. The accepted description, a factory
   parented to the device's adapter and a real windowed fullscreen description
   are what the SDK is given (see B-10). Runtime unverified — it has not run on
   hardware yet (see B-7). **XeSS Super Resolution**
   and **XeSS Frame Generation** are wired the same way (`src/xess*.rs`): XeSS-SR
   dispatches `xessD3D12Execute` on its own native command list, and XeSS-FG
   creates its own proxy swapchain from a flip-model swapchain description,
   owning an XeLL low-latency context (mandatory on every GPU) with the
   required markers each frame; the UI is drawn into a separate
   premultiplied-alpha surface tagged in `HUDLESS_UITEXTURE` mode. The Intel
   DLLs are user-supplied and never committed (`sdk/SDK_GOES_HERE.txt`). DLSS
   cannot dispatch and still renders through the fallback path. When a provider
   is selected but its SDK cannot be used, the world renders through the
   existing fallback path at the provider's recommended internal resolution —
   the UI says so, and no temporal accumulation is faked (see
   `docs/blockers.md` B-3).
3. **Diagnostics**: `perfect-world-explorer --diagnose-upscale` and
   `perfect-world-explorer --diagnose-framegen` (flags on the viewer binary)
   print the support/pairing matrix and the
   SDK probe results; they need no client and no GPU (they degrade to a
   report with exit 0 when no adapter or SDK is present; `--require-sdk` /
   `--require-gpu` make them fail in an SDK/GPU-equipped environment, exit 4/5).
4. **Packaging/notices**: `THIRD_PARTY_NOTICES.md`, README license section,
   `tools/package-windows.ps1` (notices staged, optional MIT-SDK allowlist via
   `-SdkDirectory`, SHA256SUMS regenerated over the final list, NVIDIA proprietary
   DLLs never staged).

## 6a. FFX bring-up fixes (2026-11-15, after the first hardware run)

Hardware result on the first DX12 build: FSR 1 (in-tree) works; FSR 2 / 3.1 /
4.1 fell back to bilinear; FSR frame generation fell back to standard present.
Root causes fixed in `src/fsr/imp.rs` and `src/fsr/fg.rs`:

1. **FSR upscaler: the create chain never linked
   `ffxCreateContextDescUpscaleVersion`.** SDK 2.x is explicit that this
   descriptor must be attached to `ffxCreateContextDescUpscale` ("As of SDK
   2.1 it is necessary to create and fill in an ffxCreateContextDescUpscaleVersion
   ... and link this in the header.pNext field"). Every FFX FSR provider
   therefore failed at `ffxCreateContext`. The chain is now
   `upscale -> [override ->] upscaleVersion -> backend`, in all cases.
2. **FSR frame generation: the backend descriptor was dropped from the
   chain.** The FG create built `fg_desc -> backend` but then overwrote
   `fg_desc.p_next` with the version-override descriptor when a provider had
   been selected, so `MustCreateBackend` found no DX12 backend and
   `ffxCreateContext` failed. The chain is now
   `fg_desc -> [override ->] fgVersion -> backend`, in all cases.
3. **Zero provider ids poisoned the provider selection.** the two-step
   `GetVersions` query passed the returned count back as the array capacity
   while also leaving the scratch zero, so a fill beyond the written count
   would be read as id 0 (and 0 decodes to major 0). The capacity is now kept
   explicitly, `count` is clamped to it, and zero ids are filtered everywhere.
4. **Raw/native resource barriers disagreed with wgpu's tracked states.**
   The FSR depth native is written by the MV pass render pass (wgpu leaves it
   in `RENDER_TARGET`) but was transitioned out of `COPY_DEST`; the FSR output
   native (FFX UAV) was created in `COMMON` and returned to `COMMON`, while
   wgpu composites from `PIXEL_SHADER_RESOURCE` (and the first composite
   transitions `COMMON -> PIXEL|NON_PIXEL_SHADER_RESOURCE`, leaving the actual
   state at 384 from frame 1 on). Both protocols now match wgpu's tracked
   states exactly; the output native is created directly in the combined read
   state. The FG UI surface is parked in `PIXEL_COMPUTE_READ` for the FFX read
   during `Present` and restored to `RENDER_TARGET` before the next egui
   pass (in the next frame's `prepare` list, to avoid a second list).
5. **No synchronization before allocator reset.** `record()`/`prepare()`/
   `present()` reset the shared command allocator with no fence, which is
   invalid D3D12 while the GPU still executes the previous list. Both FFX
   pipelines now carry a fence+event: signal after every native list
   submission, bounded wait (5 s) before each allocator reset and before
   context teardown.
6. **Failures were invisible.** The FFX global-debug message callback is now
   registered once per loader, so FFX prints its errors/warnings to stderr
   (`[FSR ...]`); create/dispatch return codes are surfaced (FG create in
   particular used to vanish into the fallback), and the jitter-phase and
   prepare failures print their codes.
7. **No retry across providers.** The create paths now walk every advertised
   provider id newest-first (e.g. FSR 4.1.2 -> 3.1.6 -> 2.3.4, FG 4.x ->
   3.1.x) before falling back, because the SDK's `IsSupported` never rejects
   an unsupported GPU — the failure only shows up at context creation.
   The on-screen mark shows the provider version FFX actually created
   (`FG 3.1.7 x2 - 1234 frames`), matching the FSR badge.

Still runtime-unverified until the next hardware run; see `docs/blockers.md`.
If FSR still falls back, the `[FSR]` stderr lines now name the exact provider
and return code, and the retry chain will have already walked down the list —
please capture stderr and the on-screen mark and report them.

## 7. What a native DX12 port would require (not done — blocked)

- Device/swapchain: `ID3D12Device`, flip-model `IDXGISwapChain3`
  (`DXGI_SWAP_EFFECT_FLIP_DISCARD`), `R8G8B8A8_UNORM` + sRGB variant,
  `ALLOW_TEARING` for borderless.
- Descriptor heaps: CBV/SRV/UAV + DSV + sampler budgets from the pass map
  above; per-frame bindless table if the terrain/model atlases stay resident.
- Root signatures: one per pass family (terrain, scenery, water, sky, post,
  UI); the `Uniform` struct maps to a single root CBV.
- PSOs: ~one per (pass, shader entry) above; depth-stencil variants for the
  water/shore edge passes.
- Command lists/queues + `ID3D12Fence` with per-frame command allocators;
  resource barriers for the reflection/refraction reads (depth → SRV),
  render_scale target transition and post-chain ping-pong.
- Upload path: `UploadHeap` textures or `WriteToSubresource` for the BC atlas
  pages; `terrain_upload.rs` logic ports unchanged at the CPU level.
- Shaders: the 47 WGSL modules → HLSL source compiled with DXC to DXIL
  (shader model 6.0+) — or keep naga's WGSL→HLSL output checked in as build
  input. Semantics to preserve: sRGB write-encode boundaries
  (`present_encode`), the single gamma ramp, fog applied per pass,
  `grass_history` ring layout, terrain lightmap UV conventions.
- Verification: every `src/bin/*-gpu-check.rs` rerun on a Windows GPU
  (water-pass, simple-water, post, display-gamma, terrain-*, sky-*, tree-*,
  grass-*, model-*, scenery-*), plus the lavapipe CI suite on Vulkan.
- Upscaler prerequisites the native port must add for real DLSS/FSR/XeSS
  integration: a motion-vector target (RG16Float half-float with the
  same projection convention), depth in the SDK-required plane (DLSS: non-opaque
  reversed-Z optional; FSR2/XeSS: standard D32_FLOAT fine), Halton jitter in the
  projection for the chosen provider sequence, exposure (DLSS requires it —
  the viewer currently has no exposure value; fixed 1.0 is legal input),
  depth-in-meter ratio and near/far matching the camera, UI excluded from
  interpolation (FG), and present pacing that keeps the UI input rate.

None of the above was attempted beyond §6, for the reasons in
`docs/blockers.md`.
