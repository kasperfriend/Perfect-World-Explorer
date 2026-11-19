# 1 — SDK inputs and fail-closed smoke (partial)

## Host access blocker (also applies to physical items below)

The attached workspace is **Debian GNU/Linux 12, x86-64**, not the Windows
machine specified for this test. `/dev/dri` and NVIDIA device nodes are absent;
`nvidia-smi`, `pwsh`, and `cl.exe` are absent. The installed test Vulkan ICD is
lavapipe. `E:\pw142` and the checked WSL-style mount paths do not exist. The
GitHub integration returned **HTTP 403 `Resource not accessible by integration`**
for the repository self-hosted runner inventory. This does not establish that
no runner exists. No execution connection to the specified Windows machine
was provided by this session. The repository's `windows-latest` job is a
hosted build, not access to that RTX 3080. Raw probe: `01-sdk/access.log` in the
dated evidence archive. Do not substitute CPU Vulkan, a hosted Windows build,
or evaluator stand-ins for physical results.

## Release receipts

Checked the actual scratch files (not committed) obtained from the official
[AMD v2.4.0 Vulkan release tree](https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/tree/7539ae8b6568ad29ed78a753404351daf3b5cca7/Kits/FidelityFX/signedbin)
and [NVIDIA DLSS v310.9.16](https://github.com/NVIDIA/DLSS/tree/9981c6350dc58c0ce21f024fd65e31e851c31689/lib).
`01-sdk/sdk-sha256.txt` in the evidence archive holds the machine check.

| File | SHA-256 |
| --- | --- |
| `amd_fidelityfx_upscaler_vk.dll` | `4a7e895af6e0b615d40a84d59eb8c2b1054ad91cdd0c739567940270d51bdab7` |
| `nvngx_dlssd.dll` | `b49435ae346f227feafe7bbe937d88277ea0ee7323975ec06488f377fdb46cee` |
| `nvsdk_ngx_s.lib` | `7b9a5239c1145af5f645ab472aa066f9ea79db36ac819562312aae69d65f3765` |

The bridge build now pins the official loader name and SHA-256; passing a
feature DLL to CMake is rejected. FFX and DLSSD feature bytes are checked
before admitting them. GNU x64 CMake linked the ABI-v2 bridge and its five
exports and `pwe_rr_api_version()==2` were checked. The AMD header-only ABI
check also passed. This is a **build check** (`01-sdk/local-abi-build.log`),
not execution of a Windows evaluator. A dedicated MSVC x64 link/export step
in the PR workflow uses pinned headers and loader, with no SDK artifact
upload. Its hosted result is tracked in [delivery](14-delivery.md).

## Negative tests (S)

| Case | Result and evidence in bundle |
| --- | --- |
| Missing DLL, same-length bad hash | Actual pre-load validation rejected each; `01-sdk/guard-tests.log` |
| CPU FSR/RR and non-NVIDIA RR | Adapter admission predicate rejected; `01-sdk/guard-tests.log` |
| Oversized render/display request | Vulkan fixture rejected before context/record, with no active SDK; `01-sdk/lighter_temporal_rejected_extent_is_not_dispatch.log` |
| Failed command / portable fallback | Injected submit failure after command recording on CPU Vulkan; status cleared, vendor gate rejected it, following portable color/depth still bit-exact; `01-sdk/lighter_temporal_fsr_masks_and_fallback.log` |
| Raw black / nonfinite | Vendor smoke's readback checker rejects both at unchanged thresholds; `01-sdk/guard-tests.log` |
| No SDK configuration | The actual ignored vendor smoke exited 101; `01-sdk/no-sdk-smoke.log` |

The inject/readback tests do not execute FSR/DLSSD. Positive smoke on both
routes, driver-side adapter/extent/command refusal on the requested system,
and validation-layer messages remain blocked by host access. No portable or
stand-in output is accepted by the positive vendor assertion.
