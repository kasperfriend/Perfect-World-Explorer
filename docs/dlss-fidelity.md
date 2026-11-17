# DLSS fidelity — the same feature, the same way

This document answers one question: is the viewer's DLSS the DLSS other games
run, in the same pipeline, under the same contract? It is written so that every
claim can be checked against something **outside this repository** — NVIDIA's
shipped binaries, NVIDIA's headers, and NVIDIA's programming guide — rather than
against the viewer's own behaviour.

What it does *not* claim: that a particular driver on a particular RTX card
returned success for a particular frame. Only a hardware run shows that. §9 says
exactly what such a run has to show and which log lines to read.

Ground truth used throughout:

| Source | Version | Used for |
| --- | --- | --- |
| `NVIDIA/DLSS` `lib/Windows_x86_64/rel/nvngx_dlss.dll` | tag `v310.9.16` | the feature binary we execute |
| `NVIDIA/DLSS` `lib/Windows_x86_64/rel/nvngx_dlssg.dll` | tag `v310.9.16` | the frame-generation binary |
| `NVIDIA/DLSS` `include/nvsdk_ngx*.h` | tag `v310.9.16` | entry points, enums, parameter names and setter types |
| `DLSS_Programming_Guide_Release.pdf` | `v310.9.16` | the pipeline rules (§3.2.2, §3.3.1, §3.5, §3.6.2, §4) |

## 1. The binary is NVIDIA's, unmodified

The viewer never ships a re-implementation and never patches the DLL. The two
files in `sdk/` are byte-identical to NVIDIA's release artefacts:

| File | Bytes | SHA-256 |
| --- | --- | --- |
| `sdk/nvngx_dlss.dll` | 58,956,912 | `d00b5b0ab29c3ca101b70c3a5ecb3a01b7ef33766a3383a28a18623083e6123e` |
| `sdk/nvngx_dlssg.dll` | 7,506,544 | `8d59d49b36410dd6b62fef2503a5871d347415bd85d943fa0848563a95dd50c7` |

Both were compared against
`https://github.com/NVIDIA/DLSS/raw/v310.9.16/lib/Windows_x86_64/rel/…` and match
bit for bit. The same digests are pinned in `tools/verify-vendor-sdks.py`, so a
re-vendored or truncated file fails that check instead of degrading the feature
silently. `src/dlss/loader.rs::log_feature_module` additionally prints the
installed file version and the entry points it exports, because an older DLL left
in `sdk/` by a previous install changes the whole bootstrap.

Which copy runs is deliberate on both sides of the hand-over. NGX **Core** is
taken from the driver — `core()` walks the DriverStore (`_nvngx.dll` since driver
471.11) and rejects a module whose folder does not also hold `nvngx.dll`, so a
stale copy in `System32` or next to the executable cannot be picked up by
accident. The **feature** module is the vendored one, because the viewer gives
Core a feature search path list with its own SDK folder first and the executable
folder second (`feature_search_paths`). That is the same split a shipping game
gets and the same one the public samples rely on.

## 2. The bootstrap is the documented one

Two DLLs, two roles, exactly as in the public samples:

* `nvngx.dll` — NGX Core, supplied by the driver. It owns
  `NVSDK_NGX_D3D12_Init*`, `…CreateFeature`, `…EvaluateFeature`,
  `…ReleaseFeature`, `…Destroy`.
* `nvngx_dlss.dll` — the feature module checked above. It is loaded *by* Core
  when a DGX feature is created; the viewer never calls into it directly.

`NgxApi::load` resolves entry points out of `nvngx.dll`, and it refuses to call a
name it cannot type: every signature in `src/dlss/loader.rs` is the one
`nvsdk_ngx.h` declares, and `NGX_VERSIONS` (`0x15`, `0x0f`, `0x03`) are the
released `NVSDK_NGX_Version` values. `NVSDK_NGX_D3D12_Init_Ext` is *not* the first
choice: the guide documents `NVSDK_NGX_D3D12_Init_with_ProjectID` for an
application without an NVIDIA-issued application id, and that is what runs first
(`engine type = NVSDK_NGX_ENGINE_TYPE_CUSTOM`). A pre-2.1-era spelled name
(`…_Init_ProjectID`) is deliberately absent — it crashed inside the driver and
the modern headers no longer declare it.

Constants, all read out of `nvsdk_ngx_defs.h` at `v310.9.16`:

| Ours | Header | Value |
| --- | --- | --- |
| `NGX_VERSION_API` | `NVSDK_NGX_VERSION_API_MACRO` | `0x0000015` |
| `NGX_FEATURE_SUPER_SAMPLING` | `NVSDK_NGX_Feature_SuperSampling` | `1` |
| `NGX_FEATURE_FRAME_GENERATION` | `NVSDK_NGX_Feature_FrameGeneration` | `11` |
| `NGX_ENGINE_TYPE_CUSTOM` | `NVSDK_NGX_ENGINE_TYPE_CUSTOM` | `0` |
| `NGX_QUALITY_*` | `NVSDK_NGX_PerfQuality_Value` | `0..5` in declaration order |
| `NGX_DLSS_FEATURE_MV_LOW_RES` | `NVSDK_NGX_DLSS_Feature_Flags_MVLowRes` | `1 << 1` |
| `NGX_DLSS_FEATURE_DEPTH_INVERTED` | `NVSDK_NGX_DLSS_Feature_Flags_DepthInverted` | `1 << 3` |

## 3. Create: NVIDIA's parameter list, NVIDIA's types

`DlssUpscaler::new` follows `NGX_D3D12_CREATE_DLSS_EXT`
(`nvsdk_ngx_helpers_d3d.h`) step for step:

1. Ask NGX for the optimal settings and then hand its answer to create
   **unchanged** (`Width/Height/OutWidth/OutHeight` from
   `NVSDK_NGX_D3D12_GetOptimalSettings`), which is what guide §3.2.2 requires
   ("exactly as given"). The viewer's own ratio arithmetic is only the request;
   NGX's answer is what the world is rendered at.
2. `PerfQualityValue` is written with `SetI`, because the helper writes it with
   `SetI`. This is not cosmetic: the integer and unsigned entry points are
   different vtable slots, so a `SetUI` leaves an entry that NGX's `GetI` may not
   see.
3. `DLSS.Feature.Create.Flags = MVLowRes | DepthInverted`, `CreationNodeMask` and
   `VisibilityNodeMask` = 1, `DLSS.Enable.Output.Subrects` = 0 — the helper's own
   set, and the flags are the ones §3.6.2 says a single-GPU LDR engine with
   render-resolution motion vectors and reversed depth must pass.
4. A 32×32 minimum output is refused *before* NGX is asked, because §3.3.1 says a
   smaller output fails with `InvalidParameter` and a minimised window would
   otherwise spend the retry budget on a condition that fixes itself.

The parameter names, setter types and vtable order are not taken on trust. The
NGX DLLs export the feature entry points but **no** parameter allocator, so the
viewer owns the `NVSDK_NGX_Parameter` table (`src/dlss/params.rs`). That table is
an ABI: `NVSDK_NGX_Parameter` is an abstract C++ class in
`nvsdk_ngx_params.h`, so the n-th declared virtual method *is* vtable slot n, and
a call through the wrong slot writes the wrong type into NGX's map. The header's
declaration order and the viewer's slots agree exactly —

| Slot | `nvsdk_ngx_params.h` | `params.rs` |
| --- | --- | --- |
| 1 | `Set(const char *, float)` | `SLOT_SET_F32` |
| 3 | `Set(const char *, unsigned int)` | `SLOT_SET_U32` |
| 4 | `Set(const char *, int)` | `SLOT_SET_I32` |
| 6 | `Set(const char *, ID3D12Resource *)` | `SLOT_SET_D3D12` |
| 7 | `Set(const char *, void *)` | `SLOT_SET_VOID` |
| 11 | `Get(const char *, unsigned int *)` | `SLOT_GET_U32` |
| 15 | `Get(const char *, void **)` | `SLOT_GET_VOID` |

To keep that hand-built table honest:

```sh
python3 tools/verify-ngx-params.py            # offline, vendored table
python3 tools/verify-ngx-params.py --headers DIR   # re-derive it from the headers
```

extracts every `#define NVSDK_NGX_[DLSSG_]Parameter_*` from the `v310.9.16`
headers, every `NVSDK_NGX_Parameter_Set*(Parameters, <name>, …)` call in NVIDIA's
create and evaluate helpers, the `NVSDK_NGX_Parameter` virtual-method order, and
every `set_*(P_*)` in `src/dlss/imp.rs` / `src/dlss/fg.rs` / `src/dlss/params.rs`,
then fails when a name does not exist, a setter type differs, or a slot index is
not where the header puts it. Current result: **74 writes over 73 distinct names,
0 unknown, 0 mistyped, all 7 slots in header order**, in CI (`PR checks and Windows build` → "Verify NGX parameter names and setter
types", offline from `tools/fixtures/ngx-v310.9.16-params.json`).

That check is the reason `PerfQualityValue` is a `SetI` today: it found the one
type mismatch the earlier audit missed.

## 4. Evaluate: what the helper writes, on every frame

`DlssUpscaler::record` mirrors `NGX_D3D12_EVALUATE_DLSS_EXT`:

* **Resources**: `Color`, `Depth`, `MotionVectors` in, `Output` out, each set as a
  `D3D12Resource` on every evaluate.
* **States**: the three inputs are transitioned to
  `NON_PIXEL_SHADER_RESOURCE` (DLSS dispatches compute), the output to
  `UNORDERED_ACCESS`, and all four are restored afterwards, so the viewer's own
  passes never inherit a state NGX chose.
* **Per-frame scalars**: `Jitter.Offset.X/Y` from the jitter the world was
  actually rendered with, `Reset`, `MV.Scale.X/Y` = 1 (the vectors are in pixels),
  `FrameTimeDeltaInMsec`, `DLSS.Render.Subrect.Dimensions.*`.
* **Every extent key the header names** is written on every evaluate — `Width`,
  `Height`, `OutWidth`, `OutHeight` and their `Resource*` twins — because
  `EvaluateFeature` answers `FAIL_InvalidParameter` for a *missing* entry, and
  which entry it considers missing is not visible to the caller.
* `Sharpness` is 0 and `TonemapperType` is 0: the guide states NGX sharpening is
  not supported, so sharpening is the viewer's own pass (§7).

Two failure modes are worth recording because they are exactly what "looks like
DLSS but is not" looks like:

* `0xbad00005 FAIL_InvalidParameter` at create time was the viewer's own render
  extent, not NGX's optimal one, by a few pixels.
* `0xbad00005` at *evaluate* time was `DLSS.Pre.Exposure` left at 0: the feature
  created fine and then never ran, which on screen is "DLSS is on but nothing
  changes". Both are now impossible to miss in the log (`[DLSS] create: …`,
  `[DLSS] feature init result: …`), and an evaluate failure copies NGX's own log
  files into the session log once.

## 5. The data contract

| Input | Guide requirement | Viewer |
| --- | --- | --- |
| Color | LDR: 0..1 in a perceptually linear (sRGB) encoding, `IsHDR` clear; or linear with `IsHDR` set | 8-bit sRGB-encoded world/post color, `IsHDR` clear. The native texture is created with the **UNORM** DXGI description on purpose: NGX builds its SRV from the resource description, so an `_UNORM_SRGB` one would hand DLSS linear pixels while the flag says sRGB-encoded, and decode twice |
| Depth | `DepthInverted` when near is 1 | reversed-Z (`perspective_matrix` passes the far plane as `z_near`), copied into a plain `R32_FLOAT` surface NGX can bind as an SRV |
| Motion vectors | render-resolution pixels, `[0,0]` top-left, `current + mv = previous`, not dilated; `MVLowRes` when they are at render resolution, `MVJittered` when they carry the jitter | `R16G16_FLOAT` at render extent, `(prev_uv - uv) * size` from a depth reprojection against the **unjittered** view-projection; `MVLowRes` set, `MVJittered` clear. Content that writes no depth has no motion of its own — the depth-only coverage passes in `cloud_gpu`, `gfx_effect_gpu` and `weather_gpu` now give clouds, effect sheets and precipitation a depth so their vectors are real, and decals inherit the surface they are painted on (`docs/blockers.md` B-25) |
| Jitter | a projection-matrix jitter, and the same value reported to NGX | NDC shear in the `w` column (`jittered_clip_vp`), reported per evaluate; motion vectors are computed from the unjittered matrices |
| Aspect ratio | display aspect | the projection is built from the *display* extent, so a non-integer render scale does not stretch |
| Mip bias | `log2(renderX / displayX) - 1.0` | implemented, see §6 |
| Output extent | re-create on change | resolved extents are re-checked every frame and the context is rebuilt when either side moves |

## 6. Mip bias (blocker B-24)

The world is drawn below display resolution whenever an upscaler runs, so every
world input must be sampled as if the screen were the display size. The bias is
`log2(renderX / displayX) - 1` while a temporal upscaler is alive, the bare ratio
for the resolution slider on its own, clamped to `-3..=0`, exactly 0 when the
world is drawn at or above the display size without a temporal upscaler, and the
guide's `-1` for Native AA, which is the same feature at a 1.0x ratio
(`src/render_scale.rs::set_mip_bias`, published once per frame from
`src/gpu.rs`). At the reported 2293×960 render extent on a 3440×1440
window that is **-1.585**, `exp2(bias) = 0.334`.

It reaches the shaders through a `lod` lane in the camera uniform (bias,
`exp2(bias)`), and only the atlases that actually have a mip chain use it: the
world material atlas and terrain layers fold it into the supplied gradient, the
tree atlas into the `dpdx`/`dpdy` gradients, and the object atlas through
`textureSampleBias`. Single-mip water/cloud/sky/effect textures and the
post-upscale passes are untouched, and the minimap and reflection cameras keep the
sharpest mips. Unit tests pin the formula; `docs/blockers.md` B-24 records the site
list and what a hardware run still has to show.

## 7. Pipeline position

The order is: world + fog/glow/post at the render extent → DLSS → display-res
composition → the viewer's sharpening → UI. That is "as early in post as
possible" with no display-resolution work upstream of the upscale, which is what
the guide asks for.

Nothing is upscaled *per object*, and nothing can be: DLSS, FSR and XeSS are
screen-space temporal upscalers that reconstruct one frame from one jittered
frame plus history. What is per-object is the **level of detail**: the mip bias
of §6 makes every object's textures resolve at display-resolution detail even
though the object is rasterised at the render extent. Without that bias an
upscaled frame is a sharpening of a genuinely lower-detail image — which is why
it used to look like a plain downscale. The other half of that impression can be
the bilinear fallback: when a provider fails, `render_scale.blit` stretches the
render-extent image, which really is "scale down the whole image", and the log
(`[DLSS]`/`[FSR]`/`[XeSS] … failed`) and the on-screen status mark say so.
UI, text, the minimap and the gamma/encode ramp deliberately stay at native
resolution and are never upscaled.

The guide's HDR rule ("run before tone mapping") applies to linear HDR input. This
engine renders LDR: the color handed to DLSS is already sRGB-encoded and 0..1,
which is the LDR branch of the same rule, and `IsHDR` is clear. If an HDR path is
ever added, `IsHDR` must become 1 and DLSS has to move ahead of the tone mapper —
that is recorded here so the two cannot be changed independently.

Sharpening: the guide says NGX sharpening is unsupported, so `Sharpness` stays 0
and the Graphics tab's slider runs the viewer's own contrast-adaptive pass on the
upscaled image. That is a viewer feature, not part of the DLSS contract, and it is
what keeps a game-provided sharpening setting from double-sharpening.

## 8. Reproduction

```sh
# 1. the binaries are NVIDIA's
python3 tools/verify-vendor-sdks.py
for f in nvngx_dlss.dll nvngx_dlssg.dll; do
  gh api "repos/NVIDIA/DLSS/contents/lib/Windows_x86_64/rel/$f?ref=v310.9.16" \
    -H "Accept: application/vnd.github.raw" | sha256sum
done

# 2. the parameters are NVIDIA's, with NVIDIA's types
python3 tools/verify-ngx-params.py

# 3. with a hardware run, these lines say which path the driver took
grep -E '^\[(DLSS|NGX|DLSSG)\]' session.log
```

Expected on a working run: `[NGX] initialising through … (application data
folder: …)`, `[DLSS] NGX ready (API 0x15, core …)`,
`[DLSS] optimal settings: … -> render WxH`, `[DLSS] create: … flags 0x0a`,
`[DLSS] feature init result: 0x00000001 Success`, and no
`NVSDK_NGX_D3D12_EvaluateFeature(DLSS) failed` line for the life of the session.
A refusal carries its own `NGX_RESULT` name and, on evaluate failures, the tail of
NGX's log files.

## 9. What only a hardware run can prove

* That this driver's `nvngx.dll` accepts `NVSDK_NGX_D3D12_Init_with_ProjectID` and
  creates the feature on the user's card (`[DLSS] create:` /
  `feature init result`).
* That evaluate returns `Success` per frame at the reported extents, and that the
  image changes with the quality setting (i.e. the world really is rendered at
  the smaller extent and the output is the upscaled one).
* That the temporal result is stable: no ghosting on a pan, no shimmer on distant
  foliage at 1280×720 → 3440×1440, and a visibly sharper far field than before
  B-24 (that is the mip bias), while Native AA/100 % is unchanged.
* Frame generation: that `DLSSG` creates and that generated frames appear only
  when the UI/scene split the viewer prepares is accepted.
* Perf: what the frame time actually is.

None of the above is a substitute for the checks in §1–§4 — a run can only prove
that NVIDIA's feature accepted NVIDIA's parameters from this pipeline, which is
what the rest of this document establishes independently of any run.

## 10. Honest gaps

* No HDR path, so the HDR branch of the guide's color rule is unused (§7).
* Dynamic resolution is not used: the render extent comes from
  `GetOptimalSettings` per quality and changes only when the window or the setting
  does. The guide allows this; the subrect keys are still written every frame.
* Frame generation's motion-vector dilation, UI recombination and pacing are
  covered by `docs/blockers.md` (B-22), not by this document.
* The parameter table is the viewer's, not NVIDIA's; §3 is what keeps that
  faithful.
