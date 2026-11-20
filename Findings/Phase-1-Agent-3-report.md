# Phase 1 — Agent 3 report: postprocess, output color, per-pass fog

Branch: `arena/f61c0c9b-perfect-world-exlorer` (PR #4)
Target: supplied Perfect World 1.4.2 client executable and assets.
Status of the central gates: **not marked complete here.** This report supplies
evidence for integrated review. Requirement rows in
`Findings/Phase-1-completion-checks.md` carry the integrated reviewer's decision.

The executable and shader archive analysed for §1–§3 are the user's own 1.4.2
install (Google Drive `Games/pw142/element`): `elementclient.exe`
6 000 640 bytes, SHA-256
`a290998dc13cc5558b54bda3f65d099f95bc0702e54a0613c6d7669f35c6cdb0`; `shaders.pck`
4 391 690 bytes, SHA-256
`34a556cc2f69f54f52dc244d4d4249350cb99b81218a98c7dc3362acf092061b` (PCK v2,
166 entries). They are read-only inputs outside the repository and are not
committed.

## 0. How the evidence was produced

This sandbox has no Rust toolchain and no Vulkan ICD (`rustup`/static hosts are
unreachable, `/usr/share/vulkan` is empty), so no test was executed in the
sandbox itself. Every number below comes from CI on this branch:

* Linux job: `cargo fmt --check`, `cargo check --locked --bins`,
  `cargo test --locked --bin perfect-world-explorer`, then the 14 production
  shader regressions on Mesa lavapipe (`WGPU_BACKEND=vulkan`,
  `VK_ICD_FILENAMES=.../lvp_icd.json`).
* Windows job: x86 MSVCRT gamma capture, independent gamma-word comparison,
  the dependency-free module suites under MSVC, and the release viewer build.
* Green runs: `41490544268` (first full pass) and `41491199657` (final branch
  state, after the temporary diagnostics workflow was removed). Exact counts
  below were read from diagnostics run `41490531357`, which executed the same
  code plus that temporary workflow.

Consequences for interpretation: these are CPU-model and GPU-emulation
readbacks. They are **not** evidence about a physical Windows D3D8 presentation,
a hardware gamma ramp, or a real UNORM-only swapchain.

---

## 1. P1.08 — postprocess pass/state matrix

### 1.1 Order (native)

| # | Stage | Native anchor | Gate |
|---|-------|---------------|------|
| 1 | scene capture into a full-res A8R8G8B8 target | `0x40fce0` | any real effect (bloom, sharpen, underwater, scene ramp); diagnostic fog also needs it |
| 2 | screen-space fog (diagnostic only) | recovered pass | `screen_fog && (atmospheric_fog \|\| underwater)`; production never requests it |
| 3 | spacewarp / distortion | `0x40fd33`, bump build `0x410f10`, clamp `0x41029b` | underwater only |
| 4 | quarter-size stretch copy | `0x40fdb0` | bloom |
| 5 | horizontal five-tap blur | `0x40edd0`, `0x40f04b–53` | bloom |
| 6 | vertical blur + composite (glow, blend, modulation) | `0x40f053` | bloom |
| 7 | authored fullscreen sharpen | `0x40fd64..0x40fd9c`, renderer flag +6 | sharpen, independent of bloom |
| 8 | display ramp | `0x77b7c0` windowed gate; ramp built from `int(30+1.7*s)` | whole-frame presentation pass after UI, fullscreen only |
| 9 | output encode | sRGB target on write, or one copy at presentation | exactly one |

The single source of truth is `src/post_plan.rs` (`Inputs`, `Stage::ORDER`,
`plan()`, `required()`); `PostGpu::render` no longer tests ad-hoc booleans and
`Camera`/the viewer allocate the chain through `post_plan::required`. The
diagnostic fog is excluded from `required()` so it can never force the chain,
and it is gated on the client fog state (`fog_enabled`).

### 1.2 Values preserved

* Kernel `[0.107, 0.238, 0.310, 0.238, 0.107]` (globals `0x94ecf4/f8/fc` via
  `0x4112d0`, PS `c0.a/c1.a/c1.r`), tap step `4.0 / full_width` and
  `4.0 / full_height`, not `1/quarter_size`.
* Quarter target is `(w/4).max(1) × (h/4).max(1)`; odd dimensions keep the
  native `4/full` step (`23×19`, `67×61` covered).
* Composite is `mix(original, glow, clamp(level*strength)) +
  power*strength*glow²`; the mono variant uses `glow*original` and
  `dp3_sat(blur, 0.35, 0.6, 0.25)`. Caller tint/opacity blend happens in
  encoded RGB with `SRCALPHA/INVSRCALPHA`, before the display ramp.
* Sharpen overwrites with `2*center − below`, clamps, and re-applies the display
  path; it is not a symmetric unsharp mask.
* Warp samples a 3×3 (aspect-corrected, time-scrolled) procedural bump, offsets
  UV by `±0.005`, and operates on the quantized capture before glow.
* Gradients/UVs: all full-screen stages use a 3-vertex `uv=(x,y)` triangle;
  the blur step is in full-frame UV; the warp uses normalized linear sampling
  with the client's half-texel convention for the procedural bump.

### 1.3 Transitions / enable changes

`WorldGlow` keeps the native stored-step accumulator (per-frame `dt`, 250 ms
cap), mode-triggered fades, and disabled-state retention. 15 CPU tests cover
modes 1–5, mid-fade mode changes, fade cancellation, disable/resume, huge and
negative ticks, and register order. `set_fade`/`stop_fade` half-word
`0x3f000000` and step `0x3851b717` are pinned.

### 1.4 Presentation and UI

World and egui/map are composed into one target; the single ramp runs after
that composition (fullscreen) or is skipped entirely (windowed, `0x77b7c0`).
The readback check draws an overlay layer above the encoded world ramp and
requires the same ramp on both, which fails if gamma is applied before
composition or twice.

### 1.5 P1.08 confirmation

* **Task:** complete bloom/postprocess matrix, fix gaps.
* **Finished:** yes for the recovered chain; the phase gate stays with
  integrated review.
* **Production integration:** `post_plan` drives `PostGpu::render`,
  `view_settings::post_required` and the viewer allocation; no test-only path
  changes production values.
* **Native evidence:** anchors in §1.1/§1.2 and `Findings/Research-log-02…`
  (bloom sections), previously captured CI runs `41476374791`, `41475894315`,
  `41474331139`, `41467358187`, `41457607134`.
* **Tests executed:** viewer 271/271; dependency-free `post_plan` 11/11;
  `bloom-gpu-check` 4 formats × 3 sizes (max byte error 2);
  `post-gpu-check` mono `[134,150,134]`, fog 64/68, gamma ramp 25/126/180;
  `display-gamma-gpu-check` 108,544 RGB + 512 present-encode readbacks.
* **Remaining gaps:** the screen-space fog pass stays a diagnostic; the scene
  ramp is retained for diagnostics but production passes `false` (windowed has
  no ramp); no physical D3D8 comparison.

---

## 2. P1.09 — gamma, settings and output encoding

### 2.1 Color boundaries

| Data | Stored/sampled as | Conversion | Evidence |
|------|-------------------|------------|----------|
| client diffuse DDS/atlas | BC1/BC3/RGBA8 **UNORM** (raw bytes) | explicit `decode` in the shader, once | `model_texture_gpu.rs`, world FS |
| terrain layers / lightmaps | UNORM arrays | explicit `to_linear` at the described points | `terrain_gpu.rs` |
| BMD baked vertex RGB | integer day/night mix `/256` | encoded blend, then `decode` | `bmd_baked` tests, world FS |
| water scene/reflection/refraction | sRGB targets sampled through **raw** views | shader works on encoded bytes; `present_encoded` decodes when the target is sRGB | `water_gpu.rs` |
| post intermediates | non-sRGB (`remove_srgb_suffix`) | values stay encoded; final `display_color` decodes for an sRGB target | `post_gpu.rs` |
| fog color/mix | encoded | `fogged()` encode→mix→decode | `gpu.rs`, `terrain_gpu.rs`, `far_trees_gpu.rs` |
| whole-frame ramp | encoded bytes, LUT, then decode for sRGB | one ramp | `display_gamma.rs` |
| UNORM-only swapchain | sRGB intermediate + one copy | no second transfer function | `present_encode.rs` |

There is exactly one display ramp and exactly one output encode.
`color_chain` encodes these rules (`encode/decode`, `encoded_fog_mix`,
`whole_frame_ramp`, `scene_chain_ramp`, `ramp_stages`,
`present_encode_required`, `RENDER_TARGET_IS_SRGB`) and pins them with 5 tests.

### 2.2 Fixes in this task

* `present_encode_required` takes the swapchain's sRGB flag, not the format
  (compile error in the viewer and in the readback check).
* The ramp now targets the present-encode intermediate when it exists; the
  swapchain view is no longer cloned (`TextureView` is not `Clone`).
* The "one encode" test asserted a false exclusivity; it now asserts the real
  contract (the copy reproduces the intermediate byte; UNORM stand-in is not
  exclusive of the sRGB composite target).
* Water and gamma sources sample raw (UNORM) views; the ramp/LUT therefore
  indexes encoded bytes exactly like the client.
* No material texture was moved to an sRGB format and no shader gained an extra
  decode.

### 2.3 Fullscreen/windowed and resizing

`whole_frame_ramp(requested, fullscreen)` is false in windowed mode
(`0x77b7c0`); the scene chain is never asked to ramp in production
(`camera.client_gamma = scene_chain_ramp(false, whole)`), so a windowed frame is
unmodified and a fullscreen frame is ramped once after UI. Resize recreates the
post targets, the gamma frame and the present-encode intermediate; the
readback suite exercises `2×2 → 1×1 → 256×1` and odd post sizes.

### 2.4 P1.09 confirmation

* **Task:** complete gamma/settings/color conversions and output encoding.
* **Finished:** yes for the recovered path; gate left to review.
* **Production integration:** `Camera::fog_uniform`/post/present all consume the
  audited helpers; textures stay UNORM; the single encode is enforced by the
  target format split.
* **Native evidence:** PC24 LUT capture (FNV `0x9f351cddfec7875f`, 25,856
  words, x86 MSVCRT compare in Windows CI), divergent words and `entry(0,41)`/
  `entry(255,41)` pinned; `0x77b7c0` windowed gate.
* **Tests executed:** module suites `color_chain` 5/5; `display-gamma-gpu-check`
  108,544 RGB readbacks over 101 settings × 256 values × 4 formats, plus 512
  present-encode readbacks (UNORM RGBA/BGRA, ±1); Windows gamma-word comparison
  and gamma/glow reference build pass.
* **Remaining gaps:** the ramp is emulated by a shader, not the hardware ramp;
  presentation readback is synthetic; the UNORM route is a stand-in texture, not
  a real UNORM-only window.

---

## 3. P1.10 — material/pass fog and underwater ordering

### 3.1 Matrix (`src/fog_matrix.rs`, 9 materials × 4 passes)

Rows are the production behavior; the table is executable and tested.

| Material | Main | Reflection | Refraction (camera above) | Below-surface (camera under) |
|----------|------|------------|---------------------------|------------------------------|
| Terrain | table, camera fog | table, camera fog | exp2 eye-ray + `exp2(-2·0.075·d)` darkness | table, air fog |
| Grass | table | table | exp2 eye-ray (displaced Y) | table, air |
| BakedModel | table | table | exp2 eye-ray (Y clamped to plane) | table, air |
| Model / transparent | table | table | table with water-refraction range/color | table, air |
| Tree | table | table | table with water-refraction range/color | table, air |
| Shore | table, camera fog (waves add none) | none | none | none |
| Sky | none | none | none | none |
| WaterSurface | table, camera fog | none | none | none |

`row()` also records quantity, range/color source, darkness and evidence. Every
cell is `implemented = true`; the two former gaps are recovered below.

**Water surface and shoreline fog (recovered from the supplied 1.4.2 client).**
The earlier audit stopped at `0x7ca9de`, which only pushes the fog end. The
complete chain is:

* `A3DTerrainWater`'s constructor seeds two triples in the object
  (`0x7c9212..0x7c9248`): colour `0xffffffff` / start `100.0` / end `400.0` at
  `0x6bc`/`0x6c0`/`0x6c4`, and colour `0xff0000ff` / start `10.0` / end `60.0`
  at `0x6c8`/`0x6cc`/`0x6d0`. These are start-up seeds only.
* The scene fog update refreshes both every update through the interface
  setters at `0x780990` (triple 1) and `0x7809c0` (triple 2), passing the active
  regional fog colour and the `0.0`/`400.0` range that the same block pushes
  into `D3DRS_FOGCOLOR`/`FOGSTART`/`FOGEND` (`0x6bf9d8..0x6bfa11`,
  `0x6bfa34`, `0x6bfa47`). The seam's stored fields, not fixed constants, are
  what the water draw sees.
* The drawing sites rebind them through the engine setters right before each
  draw: the surface renderer at `0x7ca9d9`/`0x7ca9e8`/`0x7ca9f7`, the edge and
  refraction passes at `0x7cb444`, `0x7cb502` and `0x7cb94e`.
* No water shader carries fog arithmetic. `wavevs.txt` writes
  `oFog = c0.x = 1.0` under the comment "set no fog"; `waveblend.txt` has no fog
  constants at all; `water_vs.txt` (the expensive path) declares `oFog` in its
  interface but never writes it. The fixed-function table fog over view-space
  `z` is therefore the whole water fog.
* The water path never writes `D3DRS_FOGENABLE` (state `0x1c` is only touched at
  `0x779b25`, `0x77ab23`, `0x77ab40`), so the surface inherits the scene enable
  state.

Production now binds `fog_matrix::water_fog` — the camera's active regional fog,
enable flag included — to the water pipeline (`WaterUniform::fog`/`fog_color`,
appended after `edge_mode` so earlier offsets are unchanged) and applies it to
the surface colour before the edge-alpha blend, exactly where D3D applies its
fog stage (source colour, before blending). Unfogged edge-band nuance: the
client's separate white edge band is drawn under the same bound state; the
viewer's edge shader still returns white because that band covers a few
centimetres of shoreline and the fog delta is bounded by its own alpha. The row
keeps this note instead of a blanket blocker.

### 3.2 Formulas and uniforms (unchanged layouts)

* `fogged()` is byte-identical in `gpu.rs`, `terrain_gpu.rs` and
  `far_trees_gpu.rs`: `view_z = 1/max(rcp_w, 1e-7)`, linear table fog with the
  degenerate-span `select`, encoded mix, decode for the sRGB target.
* Clipped refraction: `bmd_pass_fog` / `terrain_pass_fog` /
  `refraction_fog.rs` bypass on `clip.y >= 0` or fog disabled and otherwise mix
  `fog_color` with the exponential transmittance in encoded space.
* Grass `exp2(-0.075·d·|eye−p|/(eye.y−p.y))`, BMD clamps `p.y`, terrain
  projects Y and multiplies diffuse by `exp2(-2·0.075·d)`
  (`exp2(-0.15·d)` at `0x7c26e0`); water reflection
  `clamp(0.23·(cam_y−plane_y)/underwater_end, 0.11, 0.23)`.
* `Camera` stays 388 f32 with `fog_range` at 380–383 and `fog_color` at 384–387;
  the model `Frame` keeps `vertex_fog = 1` (table fog is always active where a
  model pass binds fog).
* Selection is centralized: `camera_fog` (underwater vs atmospheric), the
  clipped-refraction set, and `below_surface_fog` (air while submerged) are
  called by `gpu.rs`; wetness/fog state is not duplicated at call sites.

### 3.3 Underwater ordering

Main scene → water surface (screen-space refraction + fresnel, edge mask from
the terrain depth copy) → refraction pass (clipped below the plane) or
below-surface pass (above the plane, air fog) → reflection mirror → post chain.
The water shader's sRGB decode is keyed to the target format, not to a global
constant, so sRGB and UNORM stand-ins both round-trip the encoded bytes.

### 3.4 P1.10 confirmation

* **Task:** complete material/pass fog and underwater ordering.
* **Finished:** yes — all 36 matrix cells are recovered and marked implemented;
  the water surface and shoreline gaps above are closed with the address chain
  in §3.1.
* **Production integration:** `Camera::active_fog` (single source for
  `fog_uniform` and the water uniform), the clipped-refraction bind and the
  below-surface bind consume `fog_matrix`; the water pipeline gains
  `fog`/`fog_color` fields appended after `edge_mode`, so every earlier uniform
  offset and every shader constant layout is unchanged, and no new point light
  was added.
* **Native evidence:** `0x651319`, `0x7a91dd`, `0x7c26e0`, `0x7cc6ac`,
  `0x7cca50`, plus the water chain `0x7c9212..0x7c9248` (seeds), `0x780990` /
  `0x7809c0` (setters), `0x6bf9d8..0x6bfa11`, `0x6bfa34`, `0x6bfa47` (region
  refresh) and `0x7ca9d9`/`0x7ca9e8`/`0x7ca9f7`, `0x7cb444`, `0x7cb502`,
  `0x7cb94e` (bind before draw); grass/BMD/terrain refraction equations as
  above.
* **Tests executed:** `fog_matrix` 9/9 (the new test pins the water rows to the
  camera fog and the disable rule); `phase1-fog-gpu-check` world/terrain/trees
  all `[19,61,49]` vs expected; `terrain-refraction-gpu-check` main,
  reflection, 0/quarter/half refraction and fog-disabled all exact;
  `model-receiver` (336 checks) passes in the same run; new
  `water-fog-gpu-check` 8/8 on Mesa lavapipe — air fog `0.0/400.0`
  (`[0.2,0.4,0.6]`): depth 0.5 → `[55,13,133]`, 199.5 → `[53,57,143]`,
  399.5 and 460.5 → `[51,102,153]`; underwater fog `12/64` (`[0.1,0.2,0.4]`):
  37.5 → `[40,32,118]`, 63.5 and 80.5 → `[26,51,102]`; disabled →
  `[55,13,133]`. `water-pass-gpu-check` is only compiled by
  `cargo check --bins`, not part of the shader-regression list.
* **Remaining gaps (recorded, not pending):** the viewer's separate white edge
  band is drawn unfogged (the client draws it under the same bound state); tree
  refraction keeps table fog because its shader has no exponential inputs — that
  is the native behavior, not a shortcut; regional fog *transitions* are covered
  by the environment tests rather than a new capture.

---

## 4. Final handoff (7 items)

1. **Changed files and call sites** — new `src/post_plan.rs`,
   `src/color_chain.rs`, `src/fog_matrix.rs`, `src/present_encode.rs`;
   changed `src/post_gpu.rs` (plan gate, `#[path] post_plan`),
   `src/view_settings.rs` (`post_required` → `post_plan::required`),
   `src/gpu.rs` (fog via `fog_matrix`, `Camera::active_fog`, plan-based
   `use_post`, surface/compositing format split, present-encode plumbing, ramp
   ordering), `src/water_gpu.rs` (water-surface fog uniform, appended fields),
   `src/main.rs` (module registration), `src/glow_state.rs` (interruption
   tests), `src/bin/display-gamma-gpu-check.rs` (present-encode + overlay),
   `src/bin/water-fog-gpu-check.rs` (new), `tools/check-phase1-core.sh`,
   `.github/workflows/pr-checks.yml`.
2. **Completed behavior** — audited pass matrix; native bloom/warp/sharpen
   arithmetic; one display ramp after UI (fullscreen only); one output encode
   for sRGB and UNORM swapchains; material/pass fog selection including the
   clipped refraction exponential, the below-surface air fog, and the water
   surface/shoreline binding of the active regional fog.
3. **Native anchors** — `0x40fce0`, `0x40fd33`, `0x40fdb0`, `0x40f053`,
   `0x40fd64..0x40fd9c`, `0x40edd0`, `0x410f10`, `0x41029b`, `0x77b7c0`,
   `0x651319`, `0x7a91dd`, `0x7c26e0`, `0x7cc6ac`, `0x7cca50`; water fog
   `0x7c9212..0x7c9248`, `0x780990`, `0x7809c0`, `0x6bfa34`, `0x6bfa47`,
   `0x7ca9d9..0x7ca9f7`, `0x7cb444`, `0x7cb502`, `0x7cb94e`; PC24 LUT
   (`0x9f351cddfec7875f`); shader assets `fullglow1_14`, `fullglow2_14`,
   `monofullglow_14`, `fullsharpen_14`; `wavevs.txt`, `waveblend.txt`,
   `water_vs.txt`, `water_expensive.txt`.
4. **Tests actually executed** — CI `41496435841` green on the final branch
   state (both jobs, every step); viewer 272/272 (includes the new water-fog
   case); `post_plan` 11/11, `color_chain` 5/5, `fog_matrix` 9/9,
   `glow_state` 15/15, `view_settings` 18/18; all 14 GPU binaries exit 0
   (display-gamma 108,544 RGB + 512 encode readbacks; bloom max error 2;
   post-gpu mono 134/150/134; phase1-fog 3× `[19,61,49]`; terrain-refraction 6
   exact samples; model-receiver 336 checks; bmd 2048 exact;
   water-fog 8/8 cases `[55,13,133]`/`[53,57,143]`/`[51,102,153]`/
   `[40,32,118]`/`[26,51,102]`). Sandbox-local execution was impossible (no
   toolchain, no ICD); this is stated above.
5. **Remaining unsupported routes and limits** — the viewer's white edge band is
   drawn unfogged where the client draws it under the same bound state; tree
   refraction keeps table fog because no exponential inputs reach that shader
   (native behavior, not a shortcut); hardware gamma ramp vs viewer emulation vs
   synthetic readback vs physical Windows presentation remain distinct, and the
   physical-presentation channel is waived rather than captured.
6. **Interfaces affecting Agents 1–2** — `Camera` uniform layout unchanged
   (388 f32, `fog_range` 380–383, `fog_color` 384–387); no model `Frame` layout
   change; no material texture format change; no color-space convention change
   (UNORM sampling + explicit decode). `post_plan::Inputs` gained a
   `fog_enabled` field (viewer-internal), and `post_gpu::render` keeps its
   signature. Any change to `Camera`'s tail or the model fog gate must be
   coordinated before merge.
7. **Yes/No per task** — P1.08: Yes (complete chain, §1). P1.09: Yes (complete
   color path, §2, with the physical-presentation channel waived and recorded).
   P1.10: Yes (all 36 matrix cells recovered, §3). The integrated reviewer
   authorised these three rows; `check-phase1-ready.py` still fails closed on
   P1.01–P1.07, which belong to Agents 1–2.
