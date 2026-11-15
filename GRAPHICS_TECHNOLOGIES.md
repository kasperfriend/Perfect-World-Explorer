# Graphics technologies — what ships, what is gated, what is unverified

The Graphics tab lists every provider, including ones this build cannot run.
Unavailable entries stay visible with the exact reason. Nothing here is
faked: the viewer never substitutes a different algorithm and presents it as
the selected one.

## Upscaling

| Entry (Graphics tab) | Status | Gate |
|---|---|---|
| Off | **Works** (bilinear blit). | Always available. |
| FSR 1 (EASU + RCAS) | **Implemented, runtime-unverified.** In-tree scalar WGSL port of the MIT FSR 1 algorithm (`src/fsr1.wgsl`); no SDK, no FFX context. Renders the world at the provider resolution, then EASU upscales and RCAS sharpens into the composition view. | Any real GPU (disabled on software adapters). |
| FSR 2 | **Implemented, runtime-unverified.** FFX provider **2.3.4** from the unified `amd_fidelityfx_upscaler_dx12.dll`, requested with `ffxOverrideVersion`; at creation the viewer links the mandatory `ffxCreateContextDescUpscaleVersion` and, if the chosen provider refuses to create, retries every other provider the SDK advertises for this GPU (newest first) before falling back to bilinear. Same jitter + reversed-Z depth-reprojection motion-vector pass as FSR 3/4. | Vendored AMD SDK 2.4.0 (included) + Windows/DX12 + any real GPU. |
| FSR 3.1 | **Implemented, runtime-unverified.** FFX provider **3.1.6**, identical path (version-descriptor fix + provider retry chain apply here too). | Vendored AMD SDK 2.4.0 (included) + Windows/DX12 + any real GPU. |
| FSR 4.1 | **Implemented, runtime-unverified.** FFX provider **4.1.2** (ML), identical path. | Vendored AMD SDK 2.4.0 (included) + Windows/DX12 + **AMD Radeon RX 9000 series (RDNA 4)** — gated on vendor/name in `support()`; if the provider refuses at context creation (non-RDNA-4 GPU), the viewer retries the older advertised providers (3.1.6, then 2.3.4) and only then falls back to bilinear, logging each attempt to stderr. |
| DLSS Super Resolution | Not integrated. Renders bilinear at the DLSS-recommended internal resolution. | Requires NVIDIA RTX + SDK (not shipped). |
| XeSS | Not integrated. Renders bilinear at the XeSS-recommended internal resolution. | Requires user-supplied Intel XeSS SDK. |

FSR 1 and the FFX providers are independent implementations; picking
`FSR 1` never touches FFX, and picking `FSR 2/3/4` never loads the FSR 1
pipelines. Legacy `viewer-settings.txt` files with `upscaling=fsr` and
`fsr_version=` migrate on read: `1|2|3|4 → fsr1|fsr2|fsr3|fsr4`,
absent/`0` → `fsr4` (the old "newest" default). The old sub-selector is gone
from the tab.

## Frame generation

| Entry (Graphics tab) | Status | Gate |
|---|---|---|
| Off | **Works.** | Always available. |
| FSR Frame Generation | **Implemented, runtime-unverified.** FFX swapchain (`FrameGenerationSwapChainForHwndDX12`) takes over the window; composites a separate native-resolution UI texture onto real *and* generated frames; `ffxDispatchDescFrameGeneration` runs on the swapchain's interpolation command list. **Independent of the upscaler — works with FSR 1, FSR 2/3/4, the DLSS/XeSS fallback, or Upscaling Off entirely.** The FG context creates with the mandatory `ffxCreateContextDescFrameGenerationVersion` plus the DX12 backend descriptor, and retries the older advertised FG providers before giving up; FFX diagnostic messages are piped to stderr (`[FSR ...]`). Failure restores the standard present path for the session. | Vendored AMD SDK 2.4.0 (included) + Windows/DX12 + any real GPU. |
| DLSS Frame Generation | Not integrated. No frames are synthesised. | Requires NVIDIA RTX 40 + SDK (not shipped). |
| XeSS 2 Frame Generation | Not integrated. | Requires XeSS SDK + XeSS upscaling or none (user-supplied). |

## On-screen mark

While an upscaler or frame generation is active, the viewer draws a small
non-interactive mark (top-left) showing the live path: the FFX provider
actually selected (e.g. `FSR 4.1.2`), `FSR 1 (EASU + RCAS)`, or `FG x2` with
the pipeline's finished-frame counter. When a path fails and the fallback
engaged, the mark shows a red line saying so instead of vanishing.

## Settings tokens

`upscaling=off|dlss|fsr1|fsr2|fsr3|fsr4|xess` ·
`upscale_quality=ultraquality|quality|balanced|performance|ultraperformance` ·
`fsr1_sharpness=<0..2 stops>` ·
`frame_generation=off|dlss|fsr|xess` ·
`upscale_sdk_dir=<path>` (FFX SDKs; empty = env var or `<exe>\sdk\`).

See also: [README](README.md#upscaling-and-frame-generation-optional),
[docs/blockers.md](docs/blockers.md), [docs/dx12-port-notes.md](docs/dx12-port-notes.md),
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
