# Agent-lists review — every item against real code and real client source

Date: 2026-11-09. Branch `arena/f7121b76-perfect-world-explorer` (PR 35).
Method per instruction: "Do not trust findings, dig in the real code and
source." Every verdict below cites the client source line it rests on, read
from the Angelica source dump through the Drive connector this session
(`A3DGFXEx.h`, `A3DGFXEx.cpp`, `A3DGFXElement.h`, `A3DGFXLightning.cpp`,
`A3DGFXLightningEx.cpp`, `APerlinNoise1D.h`, `APerlinNoiseBase.cpp`). Items
whose source is not in the dump are reported, not guessed.

## 1. The priority symptoms

### 1.1 Waterfall curtain animation

Two causes, both fixed and both grounded:

* **The `.gfx` flipbook path** was already the client's law (P3, §63): one
  cell per `TexInterval`, `TileMode` wrap-vs-clamp per
  `A3DGFXRenderSlot.cpp:547-549`. Regression-tested.
* **The BMD curtain sheets do not animate through the flipbook at all** —
  they are `.sdr` shader materials whose UVs scroll in the vertex shader.
  PR 36's direct copy (`SdrUvAnim`, cherry-picked as `0ccc7a1 4f9437d
  c80a06f`) carries the scroll: `kind += 0.02` on the sheet kind and
  `sdr_v = -0.5 * grass_wind.x` on the seven scrolling shaders incl.
  `物体/texture3/瀑布.dds` and `瀑布细长.dds` (V −0.5). The user confirmed
  this copy fixes the Archosaur beams as well.

### 1.2 Waterfall texture/effect transparency

* **NOTEX white fallback** — `A3DGFXTexMan` binds a white placeholder when a
  texture fails; the render pass used to treat a failed bucket as *transparent*
  (killing the sheet). Fixed (`84d4df0`), comments corrected, and now proven
  at the pixel level: `a_failed_texture_renders_the_same_pixel_as_white`
  renders a failed texture and an explicit 1×1 white sheet and asserts the
  pixels match (`7416bb0`).
* **Grid foot fades** — `A3DGridDecalEx` gives every lattice vertex its own
  `dwColor`; the waterfall's `88caff` rows (alpha 0) must fade across the
  cell, not switch whole cells off. `EffectQuad.vertex_colors` +
  `emit_grid` per-corner law + GPU interpolation, tests
  `a_grid_cell_draws_with_one_transparent_corner` and
  `quad_vertex_colours_interpolate_across_the_cell` (`efd8ac2`).
* **Stale placeholder bindings** — a bucket that bound the fallback kept it
  after the texture arrived. `Bucket.bound_fallback` + `binding_for()` +
  rebind-on-late-load + test (`49544c1`).

### 1.3 Sunrays

`MatchGrnd: 0` is respected as instructed: the authored placement height is
used directly; the ground law (`ground_parent`) only engages when `MatchGrnd`
is set AND (`GroundHeight` or near-ground). Verified keys in
`A3DGFXElement.h`: `ID_GFXOP_GROUND_NORMAL 1010`, `ID_GFXOP_GROUND_HEIGHT
1022`, `UseGroundNormal()/UseGroundHeight()`; root matrices
`GetGrndNormalTM()/GetGrndHeightTM()` in `A3DGFXEx.h`.

### 1.4 Archosaur third beam

Settled at §63 and unchanged: the ring law (`A3DGFXRing`: nSect 36, radii via
`tan(Pitch)`, ONE strip) cannot produce a third beam; the observation traces
to an emitter mis-map/neighbour. PR 36's direct copy is pulled.

## 2. List-2 items

| # | Item | Verdict | Evidence |
|---|------|---------|----------|
| 1 | Stale fallback bindings | **fixed** `49544c1` | `binding_for()` rebuilds; `a_late_texture_replaces_the_placeholder_binding` |
| 2 | Grid per-corner colours | **fixed** `efd8ac2` | `A3DGridDecalEx` per-vertex `dwColor`; two tests incl. rendered interpolation |
| 3 | PlaySpeed docs contradiction | **fixed** `00d2097` | `A3DGFXEx.h`: `CalcActualSpeed() { m_fActualSpeed = m_fDefPlaySpeed * m_fPlaySpeed; }` — the PRODUCT is the client law. Both docs now quote it; test pins 0.4 × {0, 1, 2} = {0, 0.4, 0.8} + `clear_play_speed` |
| 4 | White-fallback pixel proof | **added** `7416bb0` | `a_failed_texture_renders_the_same_pixel_as_white` |
| 5 | Trace-or-report gaps | **done** | §3 below |

Directing constraints honoured: `flipbook_uv()` untouched for the curtain;
no global scale/alpha/rotation changes; the PlaySpeed formula is unchanged
(only docs + tests, as instructed).

## 3. Trace-or-report (every gap traced to `A3DGFXElement.h` / `A3DGFXEx.h`)

* **Warp (`水门_瀑布` `Warp: 1`)** — `ID_GFXOP_WARP 1014`. The client adds
  flagged elements to a warp queue (`SetAddedToWarpQueue`) and renders them
  through `DrawToBuffer(A3DViewport*, A3DWARPVERTEX*/A3DTLWARPVERTEX*, …)`,
  i.e. a separate screen-space warped pass, not an ordinary quad. The
  render-man source consuming the queue is not in the dump, so the exact
  distortion is **reported, not guessed**; the player parses the flag, skips
  the element and counts it in `stats.warp` (logged).
* **ScaleNoOff** — `ID_GFXOP_PARSYS_SCALE_NO_OFFSET 105` (V30 "add scale no
  offset flag"); the decal-side equivalents `ID_GFXOP_DECAL_NO_WID_SCALE
  104` / `NO_HEI_SCALE 105`. Parsed.
* **DummyEle** — `ID_GFXOP_IS_DUMMY 1012` + `ID_GFXOP_DUMMY_ELE 1013`;
  `SetDummyEle/GetDummyEle/GetDummyMatrix/SetDummyColor/SetDummyScale` and
  `ID_GFXOP_CONTAINER_USE_GFX_SCALE_WHENDUMMY 104` (V94): a dummy element is
  a matrix/colour/scale anchor its children read. Parsed.
* **StrokeAmp** — `ID_GFXOP_BOLT_AMP 107`; the whole bolt table maps 1:1 to
  our `Lightning` struct (DEVIATION..CIRCLES = 100..112).
* **Ribbon/wave/fractal enable flags** — the client's `ID_PARAM_*` param
  table: `IsParamEnable/EnableParam/UpdateParam` (verified in
  `A3DGFXLightning.cpp`) mark endpoints a host driver may reposition at
  runtime (`m_bOutParamChanged` rebuilds the path). Our player has no host
  driver, so the flags are stored and inert — no visual consequence.
* **MatchSurface** — `ID_GFXOP_DECAL_MATCH_SURFACE 109` + `YAWEFFECT_GRNDNORM
  110` + `SURFACE_USE_PARENT_DIR 111` (V54-57, V86 "match ground surface use
  gfx dir property"). Parsed.
* **ModelRef alpha compare** — `ID_GFXOP_MODEL_ALPHA_CMP 103` (V29). Parsed.
* **2DRender / 2DBackLayer** — root properties (V50); `A3DGFXEx.h`:
  `Set2DRender(bool b) { m_b2DRender = b; m_bZTestEnable = !b; }` — **2D
  render forces Z-test off**. Root fields exist; the 2D pass is out of scope
  for the world renderer.
* **Camera shake host path** — `A3DGFXEx.h` carries `m_pShakeNoise
  (A3DGFXCtrlNoiseBase*)`, `m_pShakeCamera (IShakeCamera*)`, `ShakeAffector`,
  `GetShakeOffset()`; `A3DGFXEx.cpp` attenuates by distance
  (`_shake_attenu_dist = 30.0f`). Our `camera_shake()` + ctrl-112 root shake
  match; the host implements the offset, the element supplies the noise.

## 4. Open D-items after source review

* **D1 lightning Interval — settled.** `A3DGFXLightning::TickAnimation`
  uses `m_nInterval` raw (`Interval: %d`, constructor default 100,
  `ID_GFXOP_LN_INTERVAL` assigns with no clamp): there is **no 50 ms floor**
  in the client. Our `max(1)` guards are division safety only; authored
  behaviour unchanged. Bonus: `BuildPath` re-randomizes through
  `m_NoiseCtrl.GetNoise().GetValue(fStart + i*fSeg, …)` with
  `fStart = a_Random(0, 1022)` each interval — the 112 noise base is the
  bolt generator again.
* **D5 noise range — settled.** `APerlinNoiseBase::RandFloat` is
  `(RandInteger() % 2001 - 1000) / 1000` — a uniform in **[−1, 1] exactly**,
  matching our fbm basis; the range flag is replaced with the citation
  (`7416bb0`).
* **D2 (177 ECWLD raw bytes), D3 (sunray brightness over time), D4
  (MatchGrnd wording)** stand as recorded in `World-effects-2026-11-09.md`;
  D4's wording follows the user's instruction (MatchGrnd: 0 keeps the
  authored height) and `ground_parent` implements exactly that.

## 5. Version history recovered (`A3DGFXEx.cpp`, verbatim)

V17 default play speed · V22 match ground · V24 shake camera · V29 model
alpha compare · V30 scale-no-offset · V32 dummy ele · V34 tile mode · V44
curve move dir · V50 2D render · V54-57 match surface · V59 GridDecalEx · V67
LightningEx · V73 lightning filter · V74 `m_bResetWhenResumeLoop` (verified in
`A3DGFXLightningEx::ResumeLoop`: pre-74 files do not reset) · V75
`m_dwTexTimeTotal` · V81 ClTrans alpha-only · V88 sound multiple paths · V90
shake period/distance · V100-101 grid decal scale/rot/ground options.
`GFX_CUR_VERSION 103`.
