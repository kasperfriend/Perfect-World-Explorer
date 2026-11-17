# Third-Party Notices

Perfect World Explorer is licensed **GPL-2.0-or-later** (see [LICENSE](LICENSE)).

This file covers the GPU-upscaling and frame-generation SDK technologies that
Settings → Graphics can use. The runtime DLLs of **all three** vendors are
vendored in `sdk/` under the licenses below (AMD FidelityFX, Intel XeSS/XeLL,
NVIDIA DLSS/NGX) and are staged into every viewer package; each DLL's license
text ships beside it. See
[README](README.md#upscaling-and-frame-generation-optional) and
[docs/blockers.md](docs/blockers.md).

Versions below are the ones this build was documented against at the time of
writing. They were **not** downloaded, compiled against or executed in the
build environment. When you download an SDK you get whatever version the
vendor serves; check the license file that comes **inside** your download —
it, not this page, is authoritative for your copy.

## How to supply the SDKs

1. Obtain the SDK from the vendor (links below) and accept its license.
2. Copy the relevant DLLs into **one** of:
   - `sdk\` next to `Perfect-World-Explorer.exe`,
   - the folder named by the `PW_UPSCALER_SDK_DIR` environment variable,
   - the folder set in `viewer-settings.txt` as `upscale_sdk_dir=<path>`
     (the Graphics tab has no text box for it yet; edit the file while the
     viewer is closed). Restart the viewer afterwards.
3. Run `perfect-world-explorer --diagnose-upscale --sdk-dir <path>`
   and `perfect-world-explorer --diagnose-framegen --sdk-dir <path>`
   to see what was detected.
4. `.gitignore` ignores `*.dll` and `/sdk/`, then un-ignores exactly the
   vendored sets described below (AMD FidelityFX, Intel XeSS/XeSS-FG/XeLL and
   NVIDIA `nvngx_dlss.dll` / `nvngx_dlssg.dll`) plus the headers and licenses
   that go with them. Nothing else under `sdk/` can be committed — in
   particular no Streamline `sl.*.dll`.
   `tools/package-windows.ps1` stages only those allowlisted files, and
   `tools/verify-vendor-sdks.py` fails if the allowlist, the probe in
   `src/main.rs` and the loaders in `src/xess/`, `src/fsr/` and `src/dlss/`
   ever disagree.

Expected file names probed (names vary between SDK releases; the diagnostics
print exactly which names were found):

| SDK component | Probed file name(s) |
|---|---|
| DLSS Super Resolution | `nvngx_dlss.dll` |
| DLSS Frame Generation | `nvngx_dlssg.dll` |
| NVIDIA Streamline (host) | `sl.interposer.dll` |
| AMD FSR API loader (DX12) | `amd_fidelityfx_loader_dx12.dll` |
| AMD FSR upscaler (FSR Upscaling 4 / FSR 3 / FSR 2) | `amd_fidelityfx_upscaler_dx12.dll` |
| AMD FSR Frame Generation (FG 4 / FG Swapchain 4 / FSR 3 FG) | `amd_fidelityfx_framegeneration_dx12.dll` |
| Intel XeSS | `libxess.dll` |
| Intel XeSS Frame Generation | `libxess_fg.dll` |
| Intel XeLL (low latency, mandatory for XeSS Frame Generation) | `libxell.dll` |

## NVIDIA DLSS Super Resolution / DLSS Frame Generation

- **What it is:** NVIDIA's temporal neural upscalers and frame interpolator
  for RTX GPUs. DLSS Super Resolution raises internal render resolution; DLAA
  runs DLSS at native resolution; DLSS Frame Generation synthesises a
  presented frame between each rendered frame.
- **License:** proprietary, not open source. The unmodified runtime DLLs are
  redistributed under the **NVIDIA RTX SDKs License**, the text of which is
  vendored verbatim at [`sdk/NVIDIA_DLSS-SDK-LICENSE.txt`](sdk/NVIDIA_DLSS-SDK-LICENSE.txt)
  and shipped inside every package next to the binaries.
- **Redistribution:** both DLLs are taken unmodified from the official
  [NVIDIA/DLSS](https://github.com/NVIDIA/DLSS) repository, directory
  `lib/Windows_x86_64/rel/`, and are staged into the package by
  `tools/package-windows.ps1` together with that license text. No other NVIDIA
  binary is redistributed: Streamline (`sl.*.dll`) and every other `nvngx*.dll`
  are still refused by both the ignore rules and the packaging script.
- **Provenance and integrity:** recorded in
  [`tools/verify-vendor-sdks.py`](tools/verify-vendor-sdks.py), which checks
  the size, the SHA-256 and the export table of each DLL on every CI run:

  | File | Source | Size | SHA-256 |
  |---|---|---|---|
  | `nvngx_dlss.dll` | `NVIDIA/DLSS` `lib/Windows_x86_64/rel/` (git blob `23c4672e…`) | 58,956,912 B | `d00b5b0ab29c3ca101b70c3a5ecb3a01b7ef33766a3383a28a18623083e6123e` |
  | `nvngx_dlssg.dll` | `NVIDIA/DLSS` `lib/Windows_x86_64/rel/` (git blob `71b8070c…`) | 7,506,544 B | `8d59d49b36410dd6b62fef2503a5871d347415bd85d943fa0848563a95dd50c7` |

  The matching SDK headers (`nvsdk_ngx*.h`, NGX API version `0x15` = 1.5.0)
  are vendored under `sdk/include/nvidia/` for review; the viewer loads the two
  DLLs directly with `LoadLibraryW` and uses NGX's snippet ABI, so no NVIDIA
  SDK library is linked in.
- **Attribution:** "NVIDIA", "DLSS", "DLSS Super Resolution", "DLSS Frame
  Generation", "DLAA" and the NVIDIA logo are trademarks and/or registered
  trademarks of NVIDIA Corporation. "DLSS" technology is powered by NVIDIA RTX.
- **Where to get it:** <https://github.com/NVIDIA/DLSS> (the same repository the
  vendored files came from); the registered-developer SDK page remains
  <https://developer.nvidia.com/rtx/dlss>.

## AMD FidelityFX SDK — FSR (upscaling) and FSR Frame Generation

- **What it is:** AMD's open-source upscaler and frame-interpolation
  technology, part of the AMD FSR SDK. One DX12 upscaler DLL provides AMD
  FSR Upscaling 4 (4.1.2), FSR 3.1.6 and FSR 2; one DX12 frame-generation
  DLL provides FSR Frame Generation 4.0.2, FSR Frame Generation (Swapchain)
  4.0.0 and FSR 3 frame generation (3.1.7). Works on AMD, NVIDIA and Intel
  GPUs. (FSR 1 is EASU/RCAS shaders, not a DLL; it is not part of this SDK — see the FSR 1 notice below.)
- **License:** AMD FSR SDK v2.4.0 (`Kits/FidelityFX/docs/license.md`).
  The signed binaries (`signedbin/*.dll`) are expressly excepted from the
  SDK's binary-only terms and are covered by the following MIT-style
  permission, reproduced verbatim by this notice:

  "Copyright (C) Advanced Micro Devices, Inc. All rights reserved.
  Permission is hereby granted, free of charge, to any person obtaining a
  copy of this software and associated documentation files (the "Software"),
  to deal in the Software without restriction, including without limitation
  the rights to use, copy, modify, merge, publish, distribute, sublicense,
  and/or sell copies of the Software, and to permit persons to whom the
  Software is furnished to do so, subject to the following condition:
  The above copyright notice and this permission notice shall be included
  in all copies or substantial portions of the Software."

  (The remainder of the SDK — samples, tools, documentation and source files
  not listed in the SDK's exception list — is redistributed **binary-only**,
  with no reverse engineering/decompilation/disassembly, and this notice
  reproduces those restrictions: they do not apply to the signed binaries
  shipped here.) The full SDK license text ships in this repository as
  `sdk/AMD_FidelityFX-SDK-2.4.0-LICENSE.md`.
- **Redistribution:** **INCLUDED.** The following signed binaries are
  committed to this repository under `sdk/` and are staged into every
  release package produced by `tools/package-windows.ps1`:

  | File | SHA256 | Size |
  |---|---|---|
  | `sdk/amd_fidelityfx_loader_dx12.dll` | `e2191d3c0828be45eb8affd4e3ec0378f2293adc345b772a7170edcf3b021ecb` | 39,184 bytes |
  | `sdk/amd_fidelityfx_upscaler_dx12.dll` | `aad4fccccb29084a655f9a4f9f613bd22c8c03fb4a0f8840148cc6543b4ccace` | 57,664,272 bytes |
  | `sdk/amd_fidelityfx_framegeneration_dx12.dll` | `e6843252f58b0341a0d80f6a20bc0d840643ee95bc48c22cac08745344577bb6` | 29,679,376 bytes |

  They are signed by AMD and are MIT-style licensed, so committing and
  redistributing them with this notice is permitted. The loader DLL is
  required at runtime; the effect DLLs are dispatched through it. The Intel
  and NVIDIA runtime DLLs are vendored too, each under its own license (see
  the sections below), and the packaging script stages exactly those files.
- **Attribution:** "FidelityFX", "FSR", "FidelityFX Super Resolution" are
  trademarks of Advanced Micro Devices, Inc.
- **Where to get it:** <https://gpuopen.com/fidelityfx-sdk/> and
  <https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK> (minimal
  packaged release, `v2.4.0.zip`). The DLLs live under
  `Kits/FidelityFX/signedbin/` inside the SDK archive.
- **Version documented against:** AMD FSR SDK v2.4.0 (FSR Upscaling 4.1.2,
  FSR 3.1.6 upscaling, FSR Frame Generation 4.0.2 / Swapchain 4.0.0 / FSR 3
  FG 3.1.7). Not verified on hardware in this build environment. Earlier
  SDK 1.x DLL names (`ffx_*.dll`) are deprecated by AMD and are no longer
  probed.

## AMD FidelityFX Super Resolution 1 (EASU + RCAS)

- **What it is:** AMD's FSR 1 spatial upscaler. This build ships a scalar
  WGSL port of the EASU (edge-adaptive spatial upsampler) and RCAS (robust
  contrast-adaptive sharpening) passes in `src/fsr1.wgsl` and
  `src/fsr1_gpu.rs`. It is **not** part of the FidelityFX SDK and needs no
  vendor binary; the Graphics tab lists it as "FSR 1 (EASU + RCAS)".
- **License:** MIT License.
  Derived from GPUOpen-Effects/FidelityFX-FSR, `ffx-fsr/ffx_fsr1.h`.
  Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.
  The copyright, permission and warranty notice in that header are
  reproduced here:

  "Permission is hereby granted, free of charge, to any person obtaining a
  copy of this software and associated documentation files (the
  "Software"), to deal in the Software without restriction, including
  without limitation the rights to use, copy, modify, merge, publish,
  distribute, sublicense, and/or sell copies of the Software, and to permit
  persons to whom the Software is furnished to do so, subject to the
  following conditions:
  The above copyright notice and this permission notice shall be included
  in all copies or substantial portions of the Software."

  The scalar (non-gather4) formulation, the WGSL syntax and the integration
  are this project's work; the algorithm, constants and structure follow
  the AMD reference closely enough to be considered a derivative work.
- **Attribution:** "FidelityFX Super Resolution" is a trademark of Advanced
  Micro Devices, Inc.
- **Where to get it:** <https://gpuopen.com/fidelityfx-fsr/> and
  <https://github.com/GPUOpen-Effects/FidelityFX-FSR>.
- **Version documented against:** FSR 1.0 (ffx_fsr1.h, 2021). Not verified
  on hardware in this build environment.

## Intel XeSS / XeSS Frame Generation

- **What it is:** Intel's temporal upscaler and frame interpolator. Native on
  Intel Arc (XMX matrix cores); runs on other GPUs with Shader Model 6.4
  through a DP4a fallback path. XeSS Frame Generation (`libxess_fg.dll`)
  creates its own proxy swapchain for the window, paces real and interpolated
  frames, and requires a XeLL (Intel Xe Low Latency, `libxell.dll`) context on
  every GPU — Intel or otherwise — which the application must create, mark
  and sleep per frame.
- **License:** Intel Simplified Software License (October 2022).
  The full license text is vendored in this repository as
  `sdk/Intel_XeSS-SDK-3.0.2-LICENSE.txt` and permits redistribution with the
  notice preserved. (The previously referenced MIT `LICENSE` file no longer
  exists in the SDK v3.0.2 release; the Intel Simplified Software License
  governs the SDK.)
- **Redistribution:** permitted by the Intel Simplified Software License with
  the notice preserved, and **the three binaries are committed to this
  repository** under `sdk/` (they are the same files Intel publishes in the
  `XeSS_SDK_3.0.2.zip` release, unmodified — the license forbids any
  modification). The copyright notice and terms travel with them as
  `sdk/Intel_XeSS-SDK-3.0.2-LICENSE.txt`, and the SDK's third-party terms as
  `sdk/Intel_XeSS-SDK-3.0.2-third-party-programs.txt`.
  `tools/package-windows.ps1` stages all three into every package's `sdk/`
  folder next to the executable.

  | File | Bytes | SHA-256 |
  |---|---|---|
  | `sdk/libxess.dll` | 77,795,704 | `251659dd84a3e84de67c886a4186e01f3eca49b00641906fe38bb6b807e5d5b7` |
  | `sdk/libxess_fg.dll` | 22,957,432 | `ec5e0c65e075570c6ede72618bb666d0be0c2e10b2ea9762c0fe8cb8e375ab27` |
  | `sdk/libxell.dll` | 415,368 | `d2030dcd694fda8f2ec7e044b13e6db8f0b56d4ba9113a5efad334e3f3ded8c7` |

  `python3 tools/verify-vendor-sdks.py` re-checks those hashes, the PE export
  tables (every symbol the viewer resolves with `GetProcAddress`) and the
  numeric constants against the vendored headers.
- **Attribution:** "XeSS", "XeLL" and "Intel Arc" are trademarks of Intel
  Corporation. Intel XeSS SR technology.
- **Where it comes from:** the official `v3.0.2` release of the XeSS SDK —
  <https://github.com/intel/xess/releases/tag/v3.0.2> (asset `XeSS_SDK_3.0.2.zip`;
  the `bin/` folder of the same tree is served as
  <https://codeload.github.com/intel/xess/tar.gz/refs/tags/v3.0.2>). To update,
  copy `libxess.dll`, `libxess_fg.dll` and `libxell.dll` from that `bin/` folder
  into `sdk/` and refresh the hashes above (see `sdk/SDK_GOES_HERE.txt`).
  Intel's XeSS topic page:
  <https://www.intel.com/content/www/us/en/developer/topic/technology/xess.html>.
- **Version documented against:** Intel XeSS SDK **3.0.2** (XeSS Super
  Resolution, XeSS Frame Generation, XeLL). Unverified in this build
  environment — the CI runner has no GPU. XeSS Frame Generation is only
  exposed with XeSS upscaling or none — cross-vendor frame-generation
  pairings are not documented by Intel and are not offered.

## Everything else

- The viewer's own code: GPL-2.0-or-later. This project is not affiliated
  with or endorsed by NVIDIA, AMD, Intel or Perfect World.
- Rust dependencies: see `Cargo.lock`; most are permissively licensed
  (MIT/Apache-2.0/BSD/Zlib). Build-time tooling (DXC/V-slim toolchain if you
  add one for a native port) is not part of this repository.
- The Windows CI runner uses Mesa lavapipe for headless GPU checks; it is not
  distributed with the viewer.
- The game client, its DLLs, archives, extracted assets and derived caches
  are **not** included in, or redistributable as part of, this project.
- `Modules/Walker` and `Modules/Populator` are part of this project (GPL-2.0).
