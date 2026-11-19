# Experimental Vulkan temporal reconstruction

The normal Lighter path is unchanged: guide-aware resolve, optional built-in
history/filter and optional spatial FSR 1 EASU. `PW_RAY_RECONSTRUCTION=fsr` or
`rr` requests an **experimental** temporal provider for Lighter's Software or
full-ray Vulkan renderer. It is off by default, does not alter the settings
file, and does not use or replace Native's DX12 vendor/FG setup. Windows and
Linux here refer to the optional Vulkan bridge support, not to a claim that
both SDKs are distributed for both operating systems.

A request is not proof of admission. The log and Lighter status name the
provider **after** a successful evaluation submission. A headless coverage
sidecar records `ray_reconstruction=portable` if it didn't run. A missing
runtime, unsupported adapter, incompatible settings/extent or failed
provider command uses portable ray resolve; it doesn't turn ray tracing off.
Genuine Vulkan device loss still takes the viewer's normal recovery path.

## Requirements and setup

Use a **physical** Vulkan 1.2+ adapter, fixed ray resolution, one sample per
pixel, no Reference or Rasterized primary visibility. Use **Reload World**
after choosing Software or Hardware RT. `WGPU_BACKEND` still overrides backend
selection; use `vulkan` for this experiment. Hybrid, automatic resolution,
Reference and multisample frames use the portable route. Changing between
eligible and ineligible settings starts fresh history. The portable denoiser,
progressive accumulation, EASU and screen DI/GI do not run **in addition to**
the SDK. Do not infer a quality or speed win from selecting these options.

### AMD temporal FSR (Windows x64)

1. From AMD's [FidelityFX SDK v2.4.0 signed Vulkan binaries](https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/tree/v2.4.0/Kits/FidelityFX/signedbin),
   obtain **`amd_fidelityfx_upscaler_vk.dll`** (36,135,696 bytes). The viewer
   checks SHA-256 `4a7e895af6e0b615d40a84d59eb8c2b1054ad91cdd0c739567940270d51bdab7`.
   This is the Vulkan upscaler's exported API, not the DX12 DLL in `sdk/` or
   the Vulkan frame-generation/runtime loader DLL. A working Windows Vulkan
   loader and the DLL's MSVC runtime dependencies are needed.
2. In the PowerShell that launches Explorer, set:

   ```powershell
   $env:PW_RAY_RECONSTRUCTION = 'fsr'
   $env:PW_RAY_FSR_VK_DLL = 'C:\sdk\ffx-v2.4.0\amd_fidelityfx_upscaler_vk.dll'
   $env:WGPU_BACKEND = 'vulkan'
   ```

3. Start with 50% rays. The FFX API is queried with the *selected* Vulkan
   device context. The viewer can select FSR 3.1.5 or 2.3.4 when that version
   is reported by the binary; it uses the returned ID and verifies the active
   context. It does not silently select an ML provider. The version and ID go
   to the launch log. There is no Linux FFX binary packaged or assumed here.

### NVIDIA DLSS Ray Reconstruction (Windows or Linux x64)

Use the NVIDIA RTX driver/NGX runtime for the selected GPU, and the release
feature binary and loader library from [NVIDIA DLSS v310.9.16](https://github.com/NVIDIA/DLSS/tree/v310.9.16/lib).
The `pwe_ray_rr` bridge in [tools/ray-reconstruction](../../tools/ray-reconstruction)
uses the official headers and SDK loader. The viewer requires bridge ABI v2;
rebuild any older bridge. **Do not use the feature binary as
an SDK loader or set `PW_RAY_RR_BRIDGE` to it.** Explorer does not fetch these files, and the SDK libraries and bridge are
**not** in the viewer ZIP.
Follow the upstream SDK terms. Use trusted builds and explicit paths.

| Platform | Link when building the bridge | Place in the SDK directory |
|---|---|---|
| Windows | `Windows_x86_64/x64/nvsdk_ngx_s.lib` | `Windows_x86_64/rel/nvngx_dlssd.dll` (SHA-256 `b49435ae346f227feafe7bbe937d88277ea0ee7323975ec06488f377fdb46cee`) |
| Linux | `Linux_x86_64/libnvsdk_ngx.a` | `Linux_x86_64/rel/libnvidia-ngx-dlssd.so.310.9.16` (SHA-256 `a5d59de18841f15779039dda567a15dd723ba737915f489271fdb5f109fbfb96`) and a `libnvidia-ngx-dlssd.so` symlink to it |

Build once with CMake, a Vulkan SDK (headers/loader development package),
and MSVC x64 or a Linux x64 C++17 compiler:

```sh
cmake -S tools/ray-reconstruction -B target/ray-rr \
  -DNGX_LOADER=/absolute/path/to/libnvsdk_ngx.a -DCMAKE_BUILD_TYPE=Release
cmake --build target/ray-rr --config Release
export PW_RAY_RECONSTRUCTION=rr
export PW_RAY_RR_BRIDGE="$PWD/target/ray-rr/libpwe_ray_rr.so"
export PW_RAY_RR_SDK_DIR=/absolute/path/to/dlssd-release
export WGPU_BACKEND=vulkan
```

On Windows, use the x64 generator/toolchain and the `.lib` above; set the
same variables with `$env:` and use the resulting `pwe_ray_rr.dll` (typically
in `target/ray-rr/Release`). On Linux, the NVIDIA driver must provide
`libnvidia-ngx.so.1`. The bridge queries the RR feature's instance and device
extension lists **before** creation, checks feature and capability support,
and queries optimal inputs for that output size. An arbitrary ray percentage
may not be a supported optimal input. Try an even output extent at 50%, or
100% (DLAA) within the image budget. Rays are not silently resized to a
provider's nearest quality tier. Even valid extents can be rejected by the
SDK or memory budget. A resize with a different supported extent recreates
its feature after polling the previous completion fence.

## What goes to the SDK

- One Halton sequence owns the ray-camera subpixel offset. Trace the jittered
  **ray** viewport, send that offset in ray pixels, and measure motion with
  unjittered cameras. Pick resolution and camera lens before tracing. Time
  delta comes from the host clock (fixed 16 ms warm-up in headless audit;
  zero-delta captures use a nominal frame interval).
- Use **raw linear ray radiance**, finite current reverse-Z and RG32F
  **current → previous** motion in render pixels, with +Y down. Reproject
  static hits by position and sky by direction. Use last accepted local XYZ,
  triangle barycentrics and placement rows for weighted/attached moving
  geometry. Repacking by itself doesn't change ownership. A matching depth
  at the previous sample is necessary, not a full surface identity certificate.
- Missed reprojections, new/unsupported draws, layered and transmissive hits,
  and changed sampled animation dependencies reject history conservatively.
  Keep distinct FSR reactivity and transparency/composition masks. For RR,
  use linear diffuse and specular reflectivity, packed world normals with
  linear roughness, actual first specular-hit distance when available, and
  row-major world/view and view/clip matrices. Sky/specular miss distance is
  65504. A non-specular sample is not a specular hit; when a required guide
  can't be produced, present the portable current color at that pixel.
  Do not repurpose FSR masks as RR's optional signed responsivity mask.
- Inputs and the returned HDR color are same-device Vulkan images. Record
  native work on the same graphics/compute queue, with explicit transitions
  to shader-read for inputs and general/UAV for output. Restore wgpu-owned
  layouts before composition. Poll the native fence; don't wait for it on
  each live draw. Commit the previous scene pose and swap input pages only
  after the SDK command buffer has been accepted by the queue. A dispatch
  failure abandons that extent and resumes the portable path. Its first
  fallback draw may reuse the already traced jittered sample; subsequent
  draws are portable. Cuts, major lighting changes, a scene publication or
  extent/lens changes reset history rather than blending across the event.
- Always write nearest **current reverse-Z** during composition. Neither
  SDK provides the viewer's depth, tone mapping, HUD, map views, or frame
  generation. The added input/output textures and local depth/rejection
  pages have a 192 MiB logical cap per extent, separate from the existing
  trace-image cap and from driver/SDK allocations.
  This is **not** a whole-viewer VRAM budget or a measured cost on RTX 3080.

## Validation and remaining gates

The compile-only [FFX ABI check](../../tools/ray-reconstruction/check_ffx_abi.cpp)
uses the upstream v2.4.0 header tree without flattening it:

```sh
ffx=/path/to/FidelityFX-SDK/Kits/FidelityFX
c++ -std=c++17 -fsyntax-only -I"$ffx/api/include" -I"$ffx/upscalers/include" \
  tools/ray-reconstruction/check_ffx_abi.cpp
```

Rust tests cover ABI sizes, continuity, identity remapping, and WGSL/SPIR-V. The ignored Vulkan probes run a deterministic
native output write in place of a vendor call; they are tests of trace inputs,
barriers, history and composition, **not** of FSR/RR output quality.

Local tests on llvmpipe/Mesa 25.0.7 passed two extents on Software BVH
and the Vulkan ray-query route. At 32² rays both checked camera/sky motion,
static geometry, rigid instances, weighted poses, 59 revealed samples and
880 valid prior-depth samples. They also checked current depth, camera and
scene resets, plus a reflected segment with a measured 10-unit hit distance
and a layered counterpart that must fall back. The FSR input/failure probe
checked distinct masks and bit-exact portable color/depth after rejection.
Software probes ran with driver defaults; the local ray-query probe used
`LP_NATIVE_VECTOR_WIDTH=128 LP_NUM_THREADS=1` to limit JIT memory. That
setting is not a viewer recommendation or a physical-GPU result.

On a physical GPU with one SDK configured as above, run:

```sh
cargo test --locked --bin perfect-world-explorer lighter_temporal_vendor_dispatch -- --ignored --nocapture
```

Set `PW_TEST_RAY_QUERY=1` for the Hardware trace control. This test must not
pass via portable fallback. Keep its adapter/driver, chosen provider version,
validation messages, fixed-size and resize output results with the test data.
The fixture checks finite non-black radiance in the SDK image itself, not
only the composited result, which can contain portable fallback pixels.
Follow with device-recovery/windowed tests, then matched `--gpu-preview-at`
captures with `PW_PREVIEW_COVERAGE=1`, `PW_PREVIEW_WARMUP=48`, and the same
map, extent, camera, clock and 1-spp settings, separately for `off` and the SDK.
Check a still view and camera/actor movement, thin geometry, wet/layered
materials and newly revealed pixels; compare to a converged reference as
well as to the portable branch. Record active `ray_reconstruction` in the
sidecars and collect [normal-present travel](travel.md) including VRAM, not
only synchronized readback timings. The [RT-17 matrix](retail-matrix.md)
clears this opt-in and remains a separate portable-renderer baseline.

**Still open:** actual FFX or DLSSD create/evaluate/resize on the supported
physical drivers, per-surface quality and fallback boundaries, physical
performance, and RT-17. A Linux bridge linked to the official loader and an
export/ABI audit of the Windows files do not establish a successful vendor
dispatch.
