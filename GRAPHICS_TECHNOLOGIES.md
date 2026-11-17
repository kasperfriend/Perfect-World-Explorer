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
| DLSS Super Resolution | **Implemented, runtime-unverified.** NVIDIA NGX `NVSDK_NGX_Feature_SuperSampling` from the vendored `nvngx_dlss.dll`: reversed-Z depth (`DLSS.Feature.Create.Flags = DepthInverted`), LDR sRGB input, Halton(2,3) jitter passed both as the projection shear and as `Jitter.Offset.X/Y`, and the shared reversed-Z depth-reprojection motion vectors. The vendor DLL is an NGX *snippet*, driven by NGX **Core** — the `_nvngx.dll`/`nvngx.dll` that ships with the display driver and lives in the `DriverStore` since driver 471.11: the viewer initialises Core once per device (`NVSDK_NGX_D3D12_Init_with_ProjectID`, falling back to `…Init`; `…Init_Ext` is the Core-to-snippet entry point and answers `0xbad00002` when an application calls it, which is why the viewer never does) and creates both features through it, with the viewer's `sdk/` folder first in `PathListInfo` so the vendored build is the one that runs. Core is looked for in the DriverStore first, then next to the executable and in the viewer's `sdk/` folders, then in `System32` (a copy left there belongs to a pre-471.11 driver and answers `0xbad0000c FAIL_OutOfDate` to everything newer), and **every** module found is tried in that order, so a stale copy cannot block a good one; the vendored snippet itself is the last resort; the parameter map is Core's (`NVSDK_NGX_D3D12_AllocateParameters` + `NVSDK_NGX_Parameter_Set*`), with the viewer's own `NVSDK_NGX_Parameter` table (`src/dlss/params.rs`) as the fallback. NGX's log lines go into `session.log` and `SuperSampling.Available` is logged after init. `DLAA` is the Native AA entry. Any init/create/evaluate failure logs the named `NVSDK_NGX_Result` and falls back to bilinear for the session. | Vendored NVIDIA DLSS/NGX (`nvngx_dlss.dll`, included in `sdk/`) + Windows/DX12 + an RTX GPU. |
| XeSS | **Implemented, runtime-unverified.** Intel XeSS Super Resolution (SDK 3.0.2, `libxess.dll`) as a temporal `xessD3D12Execute` on a native DX12 command list: reversed-Z depth, LDR sRGB input, Halton(2,3) jitter, and the shared reversed-Z depth-reprojection motion-vector pass. Any loader/dispatch failure logs and falls back to bilinear for the session. | Vendored Intel XeSS SDK 3.0.2 (`libxess.dll`, included in `sdk/`) + Windows/DX12 + SM 6.4 GPU. |

### Quality modes: one table per provider

The providers do **not** share a quality list, and the same word does not mean
the same thing in two of them, so the matrix lives in one place —
`src/modes.rs` — and the UI, the CLI (`--diagnose`), the render-extent helper
and both vendor `create` paths read it. Each row carries the provider's own
name for the mode, the render ratio it asks for, and the vendor enum value:

| Mode | DLSS (NGX `PerfQualityValue`) | FSR 3.1 / 4 (FFX `qualityMode`) | FSR 2 | FSR 1 (EASU) | XeSS (`qualitySetting`) |
| --- | --- | --- | --- | --- | --- |
| Native AA | **DLAA** `5`, 1.0× | **Native AA** `0`, 1.0× | — | — | **Native AA** `106`, 1.0× |
| Ultra Quality | — | — | — | — | **Ultra Quality** `104`, 1.3× |
| Quality | `2`, 1.5× | `1`, 1.5× | `1`, 1.5× | 1.5× | `103`, 1.5× |
| Balanced | `1`, 1.7× | `2`, 1.7× | `2`, 1.7× | 1.7× | `102`, 1.7× |
| Performance | `0` (`MaxPerf`), 2.0× | `3`, 2.0× | `3`, 2.0× | 2.0× | `101`, 2.0× |
| Ultra Performance | `3`, 3.0× | `4`, 3.0× | `4`, 3.0× | 3.0× | `100`, 3.0× |

The four dashes are not gaps in the viewer:

* **DLSS has no Ultra Quality.** `NVSDK_NGX_PerfQuality_Value` declares value 4,
  but the installed core (32.0.16.2114) answers `CreateFeature` with
  `0xbad00010 FAIL_UnsupportedParameter` for it, and the optimal-settings
  callback answers `0x0` (no render size) — measured, `docs/blockers.md` B-28.
  DLSS's native-resolution mode is DLAA, which is `upscale_quality=nativeaa` on
  DLSS.
* **The FFX upscaler has no Ultra Quality preset.** `ffx_upscale.h` (SDK 2.4.0)
  runs Native AA 1.0×, Quality 1.5×, Balanced 1.7×, Performance 2.0×, Ultra
  Performance 3.0×.
* **FSR 1's EASU is spatial**, so a 1.0× dispatch would be a no-op, and **FSR 2
  predates `FFX_UPSCALE_QUALITY_MODE_NATIVEAA`** (a 3.1/4.0 provider mode).
* **XeSS 1.x had no Ultra Performance**, which is why it used to be hidden; the
  vendored 3.0.2 runtime documents it (`XESS_QUALITY_SETTING_ULTRA_PERFORMANCE`,
  3.0×) and the viewer offers it.

Three rules follow from the table, and they are what keep a mode from silently
becoming another one:

1. `modes::require` is what `DlssUpscaler::new` and `XessUpscaler::new` call. A
   mode the table does not carry for that provider is an error naming the reason
   (`… the NGX core rejects NVSDK_NGX_PerfQuality_Value 4 with 0xbad00010 …`),
   never a fold into a neighbouring mode — XeSS "Native AA" used to become
   `XESS_QUALITY_SETTING_ULTRA_QUALITY` at a 1.0× render, and XeSS "Ultra
   Performance" used to render at Performance's 2.0×.
2. A persisted mode the selected provider does not offer degrades to the
   **nearest advertised ratio** (`modes::nearest_available`) — once, at the top
   of the frame, before the extent and the create call — so `ultraquality` saved
   with XeSS becomes Quality under DLSS rather than a 1.0× render that fails.
3. `--diagnose` prints every provider's rows with the vendor value and lists the
   modes it does not offer, with the reason.

**Sharpening is a per-upscaler slider, for every provider.** A temporal
upscaler reconstructs detail from jittered history rather than inventing it
with a spatial filter, so its output is softer than a Lanczos/EASU upscale, and
which knob sharpens it back is vendor-specific — that is why each upscaler has
its own setting instead of one shared one:

| Upscaler | Setting | Where it is applied | Default |
| --- | --- | --- | --- |
| FSR 1 | `fsr1_sharpness` (0–2 stops, lower = sharper) | RCAS in `src/fsr1.wgsl` | 0.2 |
| FSR 2 / 3.1 / 4 | `fsr3_sharpness` (0 = off, 1 = maximum) | `ffxDispatchDescUpscale.sharpness` with `enableSharpening = sharpness > 0` | 0.75 |
| DLSS | `dlss_sharpness` (0 = off, 1 = maximum) | the viewer's own pass, in `DlssUpscaler::composite` (same filter as XeSS) | 0.35 |
| XeSS | `xess_sharpness` (0 = off, 1 = maximum) | the viewer's own RCAS-style pass while the XeSS result is copied into the composition | 0.35 |

DLSS used to be handed `Sharpness = 0`, i.e. *no* sharpening at all, which is
what made Quality look blurry next to FSR: FSR arrives with RCAS at 0.75 and
DLSS arrived with nothing. Setting `NVSDK_NGX_Parameter_Sharpness` per
*evaluate* did not change anything on screen, though, and that is a real
limitation rather than a mystery: every shipped NGX wrapper writes `Sharpness`
in the **create** parameter map (`NVSDK_NGX_D3D12_DlssCreateParams`,
`nvsdk_ngx_helpers_d3d.h`), so an evaluate-time value is simply not read. The
feature flag that used to enable NGX's own sharpening is deprecated as well
(`nvsdk_ngx_defs.h:296`, "Sharpness is not supported"). So the DLSS slider now
runs the *same* pass as XeSS, after `EvaluateFeature`, inside
`DlssUpscaler::composite` — predictable, live, and impossible to apply twice
(NGX is still handed `Sharpness = 0`).

XeSS has no sharpening control at all: Intel's SDK 3.0.2 (`sdk/include/xess/xess.h`)
exposes `xessD3D12CreateContext`, `xessD3D12Init`, `xessD3D12Execute` and the
jitter/logging helpers, and nothing else. Its slider therefore runs our own
pass — an unsharp filter against the 4-neighbour average, clamped to the local
neighbourhood minimum/maximum so even 1 cannot ring or halo — inside
`FsrComposite`, in linear light (after the sRGB decode) when the composition
target is sRGB. FSR and DLSS pass 0 to that pass, because their own runtime
already sharpened; sharpening twice would double the halos.

Every one of these values is read every frame, so dragging a slider takes
effect without recreating the context. The filter's gain is the slider value
times 1.5 (so 1.0 is a strong but still halo-free sharpen) and it is applied in
linear light whenever the composition target is sRGB.

**A provider that is not running does not get to shrink the world.** The render
extent comes from the *support* table (`crate::render_extent`), which keeps
answering "1280×720" for a quality preset even when the provider itself failed
to initialise. The fallback in that case is `RenderScale::blit`, a bilinear
stretch — so a failed DLSS/XeSS used to be *worse* than Upscaling Off: 720p
bilinearly stretched to 1080p instead of a native 1080p render. At a 1.0× ratio
(DLAA / Ultra Quality) that stretch is the identity, which is why "DLAA" and
"Upscaling Off" were indistinguishable. The frame loop now asks
`RenderScale::input_extent` and, when no provider is actually alive (FSR 1
counts only while `crate::fsr1_eligible`), renders the world at the native
extent. A provider that failed once is retried three times, three seconds
apart, before the failure becomes final.

**Jitter and colour, fixed in this build.** All three temporal providers share
one jitter helper (`crate::fsr::jittered_clip_vp`): the Halton offset lands as a
constant NDC offset, by putting the offset in the `w` column of the
view-projection (`clip.xy += offset * clip.w`) — which is what "jitter the
projection matrix" means in all three vendor guides. Adding a clip-space
translation instead (`T * VP`) divides the on-screen displacement by `clip.w`, so
near geometry moves several pixels while the horizon barely moves; no temporal
accumulator can converge on that, and it showed up as blur and ghosting from
DLSS and XeSS. The same raw pixel offsets still go to `ffxDispatchDescUpscale.
jitterOffset`, `xessExecute` and NGX `Jitter.Offset`. FSR 2/3/4 takes its
sequence from FFX itself (`GetJitterOffset`, resets on a camera cut), DLSS and
XeSS use the in-tree 16-phase Halton(2,3) sequence, and every provider applies
it to the world matrix *and* its own evaluate call from the same value. The
frame loop keeps that value in one place (`frame_jitter` in `src/gpu.rs`), so
frame generation sees the jitter the world was actually rendered with no matter
which upscaler is active — FSR-FG used to be handed `(0, 0)` while DLSS or XeSS
jittered the world, which made the interpolator correct for a jitter that was
not there. Separately, the composite
pass that writes the FFX result into the sRGB world target now decodes first
(`fs_decode`, `src/fsr/mv.rs`): copying sRGB bytes into an sRGB render target
encoded them a second time (mid grey 0.5 landed at 0.735), which is exactly the
washed-out, flat look FSR had next to FSR 1.

**DLSS parameters that describe the buffers, not a preference.** Four NGX
values have to match the buffers the viewer actually uploads, and getting any of
them wrong shows up as softness or ghosting rather than as an error:

* `NVSDK_NGX_DLSS_Feature_Flags_MVLowRes` (create flag) — the motion vectors are
  written at *render* resolution (`FsrMotionVectorPass` runs over the render
  extent and subtracts previous/current UVs in render pixels), not at display
  resolution. DLSS defaults to display-resolution vectors, so without this flag
  it read 1.5× more motion than there was at Quality and reconstructed detail
  from badly matched history.
* `MV.Scale.X/Y = 1.0` — because the vectors are already in pixels; NGX's own
  helper sets the same default when the field is zero.
* `FrameTimeDeltaInMsec` — measured per evaluate call
  (`DlssUpscaler::record`). NGX uses it to decide how far it may trust history
  and how hard to denoise/anti-alias, so it must be the real frame time, not a
  constant.
* `Jitter.Offset.X/Y` in render pixels (same value that jitters the world
  matrix, `nvsdk_ngx_helpers_d3d.h`: "Jitter offset must be in input/render
  pixel space"), and the in-tree Halton(2,3) pattern lengthened with the
  reconstruction ratio (`ceil(8 · ratio²)`, clamped to 16…64) so that a 1.5×
  or 3× reconstruction still sees distinct sub-pixel phases instead of repeating
  the same 16.

FSR 1 and the FFX providers are independent implementations; picking
`FSR 1` never touches FFX, and picking `FSR 2/3/4` never loads the FSR 1
pipelines. Legacy `viewer-settings.txt` files with `upscaling=fsr` and
`fsr_version=` migrate on read: `1|2|3|4 → fsr1|fsr2|fsr3|fsr4`,
absent/`0` → `fsr4` (the old "newest" default). The old sub-selector is gone
from the tab.

**Colour space: what the upscaler is told it is reading.** The world target
holds sRGB-encoded bytes (mid grey is 0.5 in the *encoded* domain), and every
provider is configured for exactly that: FSR creates with
`FFX_UPSCALE_ENABLE_NON_LINEAR_COLORSPACE` and dispatches with
`FFX_UPSCALE_FLAG_NON_LINEAR_COLOR_SRGB`, XeSS with
`XESS_INIT_FLAG_LDR_INPUT_COLOR` ("disable tonemapping for input and output"),
NGX with the LDR `SuperSampling` feature and `IsHDR` clear. All three build
their own SRV from the resource *description*, so the description of the colour
input has to be the plain UNORM twin: describing it as `_UNORM_SRGB` made the
hardware decode the bytes to linear *before* the upscaler ran, and the provider
then performed its own perceptual→linear step for the bytes it had been told
were perceptual. XeSS showed it as a world noticeably darker than the bilinear
path while the HUD (composited later, outside the provider) stayed correct.
The declaration and the description now agree — UNORM resource, perceptual
flags — and the upscaler's output is likewise read as sRGB by the composite
pass, which decodes once (`fs_decode`).

**Camera: one lens for the matrices and the providers.** The projection is
built with `perspective_rh(70°, aspect, far, CAMERA_NEAR)` (reversed-Z: near→1,
far→0), and the same three numbers are handed to the providers instead of being
repeated as literals: `crate::fsr::CameraLens` (`near`, `far`,
`fov_y_radians`) comes from `Camera::lens()` and reaches
`ffxDispatchDescUpscale.cameraNear/cameraFar/cameraFovAngleVertical` and the
`FrameGenerationPrepareV2` descriptor. The values used to be
`1.0 / 16000.0 / 70°` regardless of the live far plane, so every provider
linearised depth against a matrix that did not exist; FFX uses those numbers to
turn the sampled reversed-Z depth into view-space distance, and a mismatch skews
every depth-derivative and disocclusion test — blur towards the horizon,
ghosting on silhouette edges. `viewSpaceToMetersFactor` stays 1.0: the viewer's
world unit *is* a metre.

## Frame generation

| Entry (Graphics tab) | Status | Gate |
|---|---|---|
| Off | **Works.** | Always available. |
| FSR Frame Generation | **Implemented, runtime-unverified.** FFX swapchain (`FrameGenerationSwapChainForHwndDX12`) takes over the window through the shared hand-over (`src/fsr/swapchain.rs`, see `docs/blockers.md` B-10); keeps the HUD out of the interpolated image by handing FFX `HUDLessColor` (see *The HUD is never interpolated* below), `PW_FSRFG_UI_COMPOSITION=1` restoring the old in-composition path; `ffxDispatchDescFrameGeneration` runs on the swapchain's interpolation command list for `numGeneratedFrames = 1`, i.e. 2x. **Independent of the upscaler — works with FSR 1, FSR 2/3/4, the DLSS/XeSS fallback, or Upscaling Off entirely.** The FG context creates with the mandatory `ffxCreateContextDescFrameGenerationVersion` plus the DX12 backend descriptor, and retries the older advertised FG providers before giving up; FFX diagnostic messages are piped to stderr (`[FSR ...]`). Failure restores the standard present path for the session. | Vendored AMD SDK 2.4.0 (included) + Windows/DX12 + any real GPU. |
| DLSS Frame Generation | **Implemented, runtime-unverified.** `NVSDK_NGX_Feature_FrameGeneration` from the vendored `nvngx_dlssg.dll`, created through the driver's NGX Core (same bootstrap and module search as DLSS SR) and driven without Streamline: the viewer owns a flip-model swapchain for the window (shared hand-over, `src/fsr/swapchain.rs`), copies the composition into the current backbuffer, then evaluates the feature once per generated frame — `DLSSG.MultiFrameCount` = n, `DLSSG.MultiFrameIndex` = 1..n, `DLSSG.BackbufferFrameID` monotonic — with each generated frame writing into its own backbuffer (`desc.BufferCount = n + 2`), and finally presents the real frame and every generated frame with `Present(1, 0)`. Presenting the generated frames with sync interval 0 instead lets the next real frame land on the same vblank, which coalesces the group away and reads as stutter. n = 1 on Ada (`DLSSG.MultiFrameCountMax` = 1, the 2x path); **DLSS Multi Frame Generation** (n > 1) needs Blackwell — see *Frame multiplier* below. Depth and the shared reprojected motion vectors are fed at internal resolution, the unwarped camera matrices as `ClipToPrevClip`/`PrevClipToClip`. **Independent of the upscaler — works with DLSS SR, FSR, XeSS or Upscaling Off, but needs an RTX 40-series or newer GPU** (the setting is gated on the GPU name). The HUD is separated from the interpolated image: the scene goes to NGX as `DLSSG.HUDLess`, the HUD alone as `DLSSG.UI`, and the runtime recomposites the UI onto generated frames (User Interface Recomposition). Any init/create/evaluate failure logs the named `NVSDK_NGX_Result` and restores the standard present path for the session. | Vendored NVIDIA DLSS/NGX (`nvngx_dlssg.dll`, included in `sdk/`) + Windows/DX12 + an RTX 40-series or newer GPU. |
| XeSS Frame Generation | **Implemented, runtime-unverified.** XeSS-FG proxy swapchain (`xefgSwapChainD3D12InitFromSwapChainDesc`) takes over the window through the same hand-over, exactly like the FSR path. It paces real and interpolated frames, takes the Frame multiplier through `XefgD3D12InitParams.maxInterpolatedFrames` (clamped to the `maxSupportedInterpolations` the SDK reports) and confirms it with `xefgSwapChainSetNumInterpolatedFrames`, and the UI is kept out of the interpolated image by default: `XEFG_SWAPCHAIN_UI_MODE_BACKBUFFER_HUDLESS` tags the HUD-less colour and lets the proxy extract the UI from the presented frame (`PW_XEFG_UI_COMPOSITION=0` interpolates everything, `PW_XEFG_UI_MODE=3|5` uses the separate premultiplied-alpha UI texture instead). **Documented pairing: XeSS upscaling or none.** Failure restores the standard present path for the session. | Vendored Intel XeSS SDK 3.0.2 (`libxess_fg.dll` **and** `libxell.dll`, included in `sdk/`) + Windows/DX12 + SM 6.4 GPU. |

**DLSS-G gets the whole parameter set, not a subset.** Three of them decide
whether the interpolated frame lines up with the real ones. `DLSSG.JitterOffsetX/Y`
is *clip space* jitter (`2 * jx / width`, `-2 * jy / height` for the y-down pixel
offsets the world was rendered with) and the matrices are deliberately
unjittered, so leaving the offset at 0,0 — which is what the viewer used to do —
leaves the generated frame sampling a sub-pixel position the real frame never
used. `DLSSG.MvecScale` "specifies scale factors used to normalize motion
vectors (so the values are in [-1,1] range)": the reprojection pass writes
render-resolution *pixels*, so the factor is `1 / renderWidth, 1 / renderHeight`,
the same convention `DLSS.MV.Scale` uses for Super Resolution; feeding `1.0`
told the runtime the vectors were already normalised and it interpolated as if
the camera had not moved. The camera basis (`CameraPos/Up/Right/Fwd`) and lens
(`CameraNear/Far/FOV/AspectRatio`) are filled from the same `CameraLens` and
`Camera` the matrices came from, so the runtime can separate camera motion from
object motion. `DLSSG.Reset` is set on the first frame *and* on every camera cut
(`prepare`'s `reset`), not just the first. On the vector pass, all four
providers now zero their motion vectors on a reset frame: the matrices on either
side of a cut describe unrelated views, so reprojecting through them yields
garbage that the upscaler would otherwise blend into its first post-cut frame.

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


## The HUD is never interpolated

All three frame generators support the same contract, and the viewer now uses it
in all three: **the scene and the UI are separate inputs, and the presented frame
is the scene plus the UI.**

* The world is rendered into the generator's composition target, which contains
  no HUD pixels at all.
* `prepare` copies that composition into a second surface, and the HUD pass draws
  egui over the copy (`Load`, not `Clear`). That surface is the presented frame
  and the only thing that reaches the backbuffer.
* The composition itself is handed to the provider as its HUD-less input:
  `ffxConfigureDescFrameGeneration.HUDLessColor`, `DLSSG.HUDLess` (plus
  `DLSSG.UI`, the HUD alone, for User Interface Recomposition) and
  `XEFG_SWAPCHAIN_RES_HUDLESS_COLOR` with
  `XEFG_SWAPCHAIN_UI_MODE_BACKBUFFER_HUDLESS`.

The interpolator therefore only ever sees scene pixels, and the HUD is
recomposited onto generated frames from the real frame's own pixels — never
blended by the vendor's own UI pass, which is what washed HUD colours out when
the UI was registered as a separate resource (FSR's `RegisterUiResource`), and
never interpolated, which is what smeared it when it was part of the
composition.

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
`upscale_quality=nativeaa|ultraquality|quality|balanced|performance|ultraperformance` (which of these a provider offers is the table above) ·
`fsr1_sharpness=<0..2 stops>` ·
`fsr3_sharpness=<0..1>` (FFX sharpening for FSR 2/3.1/4 and Native AA) ·
`dlss_sharpness=<0..1>` (NGX `Sharpness`; 0 = the raw, softer DLSS look) ·
`xess_sharpness=<0..1>` (our own pass, XeSS has no control of its own) ·
`frame_generation=off|dlss|fsr|xess` ·
`frame_multiplier=2|3|4|6` (also accepted: `x2`, `2x`, …) ·
`latency=vsync|reflex|xell` ·
`upscale_sdk_dir=<path>` (FFX / XeSS SDKs; empty = env var or `<exe>\sdk\`).

## Verifying the vendor binaries

`python3 tools/verify-vendor-sdks.py` checks the vendored DLLs without a GPU:
their SHA-256, that every symbol the loader resolves with `GetProcAddress` is
really exported by the real DLL, that the FFI constants equal the vendored
headers, and that the probe, the loaders, the packaging script and the ignore
rules all name the same files. It also checks the quality-mode values in
`src/modes.rs` — NGX's `PerfQualityValue`, the FFX upscaler's `qualityMode` and
XeSS's `qualitySetting` — against their headers, because a wrong one there is a
mode that renders at another mode's ratio rather than a compile error. It runs
in CI on every pull request.

See also: [README](README.md#upscaling-and-frame-generation-optional),
[docs/blockers.md](docs/blockers.md), [docs/dx12-port-notes.md](docs/dx12-port-notes.md),
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
