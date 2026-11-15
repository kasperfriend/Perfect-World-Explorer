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
| DLSS Super Resolution | **Implemented, runtime-unverified.** NVIDIA NGX `NVSDK_NGX_Feature_SuperSampling` from the vendored `nvngx_dlss.dll`: reversed-Z depth (`DLSS.Feature.Create.Flags = DepthInverted`), LDR sRGB input, Halton(2,3) jitter passed both as the projection shear and as `Jitter.Offset.X/Y`, and the shared reversed-Z depth-reprojection motion vectors. NGX has no parameter allocator in the shipped DLLs, so the viewer supplies the `NVSDK_NGX_Parameter` table itself (`src/dlss/params.rs`) and drives the DLL over NGX's snippet ABI. `DLAA` is the Native AA entry. Any init/create/evaluate failure logs the named `NVSDK_NGX_Result` and falls back to bilinear for the session. | Vendored NVIDIA DLSS/NGX (`nvngx_dlss.dll`, included in `sdk/`) + Windows/DX12 + an RTX GPU. |
| XeSS | **Implemented, runtime-unverified.** Intel XeSS Super Resolution (SDK 3.0.2, `libxess.dll`) as a temporal `xessD3D12Execute` on a native DX12 command list: reversed-Z depth, LDR sRGB input, Halton(2,3) jitter, and the shared reversed-Z depth-reprojection motion-vector pass. Any loader/dispatch failure logs and falls back to bilinear for the session. | Vendored Intel XeSS SDK 3.0.2 (`libxess.dll`, included in `sdk/`) + Windows/DX12 + SM 6.4 GPU. |

**Native AA** is a quality entry, not a provider: `upscale_quality=nativeaa`
renders at exactly the window resolution (factor 1.0) and dispatches the
provider with `renderSize == upscaleSize`, which is how FSR 3.1 exposes
temporal antialiasing without upscaling. It is offered for FSR 3.1 only — the
other providers either have their own name for the same thing (DLSS Ultra
Quality = DLAA) or refuse a 1.0× ratio. A persisted `nativeaa` choice degrades
to `quality` when the upscaler is changed to one that cannot dispatch it
(`crate::sanitise`). The FFX sharpening pass is a slider too: **FSR
sharpening** (`fsr3_sharpness`, 0 = off, 1 = maximum, default 0.75) is passed
straight through as `ffxDispatchDescUpscale.sharpness` with
`enableSharpening = sharpness > 0`, and applies to FSR 3.1/4 and to Native AA
alike. The value is read every frame, so dragging the slider takes effect
without recreating the context.

**Jitter and colour, fixed in this build.** All three temporal providers share
one jitter helper (`crate::fsr::jittered_clip_vp`): the Halton offset lands as a
constant NDC offset, by putting the offset in the `w` column of the
view-projection (`clip.xy += offset * clip.w`) — which is what "jitter the
projection matrix" means in all three vendor guides. Adding a clip-space
translation instead (`T * VP`) divides the on-screen displacement by `clip.w`, so
near geometry moves several pixels while the horizon barely moves; no temporal
accumulator can converge on that, and it showed up as blur and ghosting from
DLSS and XeSS. The same raw pixel offsets still go to `ffxDispatchDescUpscale.
jitterOffset`, `xessExecute` and NGX `Jitter.Offset`. Separately, the composite
pass that writes the FFX result into the sRGB world target now decodes first
(`fs_decode`, `src/fsr/mv.rs`): copying sRGB bytes into an sRGB render target
encoded them a second time (mid grey 0.5 landed at 0.735), which is exactly the
washed-out, flat look FSR had next to FSR 1.

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
| FSR Frame Generation | **Implemented, runtime-unverified.** FFX swapchain (`FrameGenerationSwapChainForHwndDX12`) takes over the window through the shared hand-over (`src/fsr/swapchain.rs`, see `docs/blockers.md` B-10); keeps the UI in the composition by default so the interpolator never blends HUD pixels (`PW_FSRFG_UI_COMPOSITION=0` restores the separate native-resolution UI texture, which the proxy composites onto real *and* generated frames but which FFX tone-maps with the scene); `ffxDispatchDescFrameGeneration` runs on the swapchain's interpolation command list for `numGeneratedFrames = 1`, i.e. 2x. **Independent of the upscaler — works with FSR 1, FSR 2/3/4, the DLSS/XeSS fallback, or Upscaling Off entirely.** The FG context creates with the mandatory `ffxCreateContextDescFrameGenerationVersion` plus the DX12 backend descriptor, and retries the older advertised FG providers before giving up; FFX diagnostic messages are piped to stderr (`[FSR ...]`). Failure restores the standard present path for the session. | Vendored AMD SDK 2.4.0 (included) + Windows/DX12 + any real GPU. |
| DLSS Frame Generation | **Implemented, runtime-unverified.** `NVSDK_NGX_Feature_FrameGeneration` from the vendored `nvngx_dlssg.dll`, driven without Streamline: the viewer owns a flip-model swapchain for the window (shared hand-over, `src/fsr/swapchain.rs`), copies the composition into the current backbuffer, then evaluates the feature once per generated frame — `DLSSG.MultiFrameCount` = n, `DLSSG.MultiFrameIndex` = 1..n, `DLSSG.BackbufferFrameID` monotonic — with each generated frame writing into its own backbuffer (`desc.BufferCount = n + 2`), and finally presents the real frame and every generated frame with `Present(1, 0)`. Presenting the generated frames with sync interval 0 instead lets the next real frame land on the same vblank, which coalesces the group away and reads as stutter. n = 1 on Ada (`DLSSG.MultiFrameCountMax` = 1, the 2x path); **DLSS Multi Frame Generation** (n > 1) needs Blackwell — see *Frame multiplier* below. Depth and the shared reprojected motion vectors are fed at internal resolution, the unwarped camera matrices as `ClipToPrevClip`/`PrevClipToClip`. **Independent of the upscaler — works with DLSS SR, FSR, XeSS or Upscaling Off, but needs an RTX 40-series or newer GPU** (the setting is gated on the GPU name). The UI is interpolated with the scene, so no UI mode can drop the HUD. Any init/create/evaluate failure logs the named `NVSDK_NGX_Result` and restores the standard present path for the session. | Vendored NVIDIA DLSS/NGX (`nvngx_dlssg.dll`, included in `sdk/`) + Windows/DX12 + an RTX 40-series or newer GPU. |
| XeSS Frame Generation | **Implemented, runtime-unverified.** XeSS-FG proxy swapchain (`xefgSwapChainD3D12InitFromSwapChainDesc`) takes over the window through the same hand-over, exactly like the FSR path. It paces real and interpolated frames, takes the Frame multiplier through `XefgD3D12InitParams.maxInterpolatedFrames` (clamped to the `maxSupportedInterpolations` the SDK reports) and confirms it with `xefgSwapChainSetNumInterpolatedFrames`, and, by default, the UI is drawn into the composition so the model interpolates it together with the scene (Intel's first recommendation: UI composition disabled); `PW_XEFG_UI_COMPOSITION=1` switches to a separate native-resolution premultiplied-alpha UI texture (`HUDLESS_UITEXTURE`) that the proxy composites onto real *and* generated frames, keeping the UI sharp and never interpolated. **Documented pairing: XeSS upscaling or none.** Failure restores the standard present path for the session. | Vendored Intel XeSS SDK 3.0.2 (`libxess_fg.dll` **and** `libxell.dll`, included in `sdk/`) + Windows/DX12 + SM 6.4 GPU. |

### Frame multiplier (x2 / x3 / x4 / x6)

`Graphics → Frame multiplier` is how many frames are presented per rendered
frame: the real frame plus `multiplier - 1` generated ones. It is a separate
setting because the vendors genuinely differ, and nothing is unlocked behind a
runtime's back — an entry that the hardware cannot do is greyed out with the
reason on screen.

| Multiplier | FSR Frame Generation | DLSS Frame Generation | XeSS Frame Generation |
|---|---|---|---|
| x2 | **yes** (`numGeneratedFrames = 1`) | **yes** (RTX 40 series and later) | **yes** |
| x3 | no — greyed out | **yes** on Blackwell (RTX 50 series) | requested; the SDK clamps to the device maximum |
| x4 | no — greyed out | **yes** on Blackwell | requested; clamped |
| x6 | no — greyed out | **yes** on Blackwell (5 generated frames) | requested; clamped |

* **DLSS** reads `DLSSG.MultiFrameCountMax` from the GPU architecture: the
  runtime reports **1 on Ada**, so a 40-series card cannot interpolate more than
  the single frame that 2x produces, and up to **5 on Blackwell**. Unlocking it
  on Ada would mean patching `nvngx_dlssg.dll`, which this project does not do;
  the option says so instead.
* **XeSS** has `xefgSwapChainSetNumInterpolatedFrames`, and
  `xefgSwapChainD3D12GetProperties` reports `maxSupportedInterpolations`. The
  viewer asks for the setting, clamps to that maximum, and logs both numbers to
  `session.log`, because Intel refuses an out-of-range count rather than
  clamping it.
* **FSR** frame generation generates exactly one frame per present
  (`ffxDispatchDescFrameGeneration.numGeneratedFrames`; `outputs[4]` exists in
  the header but the shipped runtime only fills one).

Changing the multiplier rebuilds the active frame generator (the count is part
of its construction), and the whole group is paced onto consecutive vsync
intervals rather than being queued at once.

XeLL (`libxell.dll`) is mandatory for XeSS-FG on every GPU — Intel Arc included — and the
viewer creates the context itself, enables low-latency mode (`xellSetSleepMode`) and delivers
the simulation / render-submit / present markers each frame; the XeLL context is destroyed
after the XeSS-FG context, as the SDK requires.

Depth and motion vectors are always tagged as `XEFG_SWAPCHAIN_RV_ONLY_NOW` on the viewer's own
command list, so XeSS-FG records its copies there and they execute on the app's direct queue
right after the writes that produced the textures (with `RV_UNTIL_NEXT_PRESENT` the SDK could
read the live textures during `Present` instead). With `PW_XEFG_UI_COMPOSITION=1` the same
applies to the two composition inputs — the UI texture *and* the HUD-less colour, which that
mode requires (`XEFG_SWAPCHAIN_RES_HUDLESS_COLOR`); tagging only the UI texture leaves XeSS-FG
with no colour to interpolate and no UI to composite, which reads on screen as "the whole HUD
vanished" while the scene keeps rendering. `PW_XEFG_UI_MODE=<0..5>` selects the composition
mode, and the ready line in `session.log` names the mode, the UI target and whether generation
is on.

The proxy swapchain is created **without**
`DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT`, even though the shared hand-over prefers
it for FSR: XeSS-FG presents two frames per app Present, and with the flag set the proxy is
capped at one queued frame unless its owner also calls `SetMaximumFrameLatency`, which leaves
the generated frame nowhere to go. `PW_XEFG_ENABLED=0` keeps the proxy as the presenter but
turns generation off — one run with it separates "the generated frames are broken" from
"presenting through the proxy is broken".


## Latency

A third, **completely separate** choice next to Upscaling and Frame generation.
It answers "who paces this frame", not "how is it upscaled", so it works with
any upscaler (including Off) and with any frame generator.

| Entry (Graphics tab) | Status | Gate |
|---|---|---|
| VSync | **Works** — the default, and what every build before this setting did. The display paces presents (`PresentMode::Fifo`); no latency library is involved. | Always available. |
| Reflex | **Implemented, runtime-unverified.** NVIDIA's Reflex algorithm without Streamline: Streamline is never vendored (it takes the swapchain over through `sl.interposer`, which the frame generators already own), but what Reflex changes for a vsync'd presenter is an ordering rule — keep one frame in flight and start recording the next one only just before the display needs it. `crate::latency::ReflexPacer` implements exactly that: it seeds the frame period at 60 Hz, learns the real cadence from the present timestamps (slow exponential average), and sleeps the CPU until `budget - min(3 ms, budget/4)` after the previous present. Presentation is untouched (`Fifo`, sync interval 1), so this lowers input-to-photon latency without tearing, frame drops or a swapchain change, and it works with every upscaler and every frame generator (including none). The mark shows `Reflex pacing - N frames, X ms budget`. | Always available (any real GPU; no SDK, no vendor check, nothing to fail). |
| XeLL | **Implemented, runtime-unverified.** Intel Xe Low Latency (`libxell.dll`): `xellSleep` holds the CPU back until the GPU queue is nearly empty (render-queue reduction), and the simulation / render-submit / present markers are delivered every frame so the library can line the end of rendering up with the display's refresh while keeping the wait short. Present stays vsync'd with sync interval 1 — Intel requires exactly that while low-latency mode is on — so this lowers latency *relative to* VSync rather than replacing it. The viewer creates the context itself (`src/latency.rs`), waits for an idle device before `xellSetSleepMode` (SDK requirement), keeps the marker edges ordered, and drops the context when the setting stops asking for it. Any failure logs, shows the reason on the mark, and leaves the vsync'd present path untouched. | Vendored Intel XeSS SDK 3.0.2 (`libxell.dll`, included in `sdk/`) + Windows/DX12 + a real GPU. XeLL is **selectable on every vendor** — the library, `xellSleep` and the per-frame markers work on any GPU, and the note under the combo says so. **On a non-Intel GPU, though, Intel only turns that into actual latency reduction while XeSS Frame Generation is active**, so on NVIDIA/AMD expect the markers to run without a measurable gain unless Frame generation = XeSS Frame Generation (where the frame generator owns the context). |

Ownership: XeSS Frame Generation needs a XeLL context of its own and refuses to
initialise its proxy swapchain without one, so with `Frame generation = XeSS
Frame Generation` the frame generator owns it and the viewer does not create a
second context (`crate::latency_owner`). That combination shows
`XeLL via XeSS FG (latency reduction)` on the mark. With any other frame
generator — or none — the viewer's own session runs.

## On-screen mark

While an upscaler, frame generation or XeLL is active, the viewer draws a small
non-interactive mark (top-left) showing the live path: the FFX provider
actually selected (e.g. `FSR 4.1.2`), `FSR 1 (EASU + RCAS)`, `XeSS (<quality>)`,
`FG x2` / `XeSS FG x2` with the pipeline's finished-frame counter (the
multiplier shown is the one the frame generator was built with, so a clamped
XeSS request still reads honestly), and `XeLL <version> - N frames paced` (or
`XeLL via XeSS FG`, or `Reflex pacing - N frames, X ms budget`) for the latency
setting. When a path
fails and the fallback engaged, the mark shows a red line (with the reason,
truncated) saying so instead of vanishing.

## Settings tokens

`upscaling=off|dlss|fsr1|fsr2|fsr3|fsr4|xess` ·
`upscale_quality=nativeaa|ultraquality|quality|balanced|performance|ultraperformance` ·
`fsr1_sharpness=<0..2 stops>` ·
`fsr3_sharpness=<0..1>` (FFX sharpening for FSR 3.1/4 and Native AA) ·
`frame_generation=off|dlss|fsr|xess` ·
`frame_multiplier=2|3|4|6` (also accepted: `x2`, `2x`, …) ·
`latency=vsync|reflex|xell` ·
`upscale_sdk_dir=<path>` (FFX / XeSS SDKs; empty = env var or `<exe>\sdk\`).

## Verifying the vendor binaries

`python3 tools/verify-vendor-sdks.py` checks the vendored DLLs without a GPU:
their SHA-256, that every symbol the loader resolves with `GetProcAddress` is
really exported by the real DLL, that the FFI constants equal the vendored
headers, and that the probe, the loaders, the packaging script and the ignore
rules all name the same files. Run it after any SDK update.

See also: [README](README.md#upscaling-and-frame-generation-optional),
[docs/blockers.md](docs/blockers.md), [docs/dx12-port-notes.md](docs/dx12-port-notes.md),
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
