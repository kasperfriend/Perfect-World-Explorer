> **Current status correction — 2 November 2026:** Use the [full eight-phase reassessment](Eight-phase-reassessment-2026-11-02.md) and [unsupported-path register](Unsupported-client-paths.md) for current decisions. The material below preserves historical milestones and may contain superseded absence claims. Old percentages are not current completion scores. Ordinary animated world models/scene-light submission, recovered postprocessing/gamma, per-material fog and clipped water/edge passes now exist; none of that establishes full phase parity.

# Water 100% parity

Acceptance: port the supplied PW 1.4.2 client's water behavior using its executable, shaders and real assets, across the world, above and below the surface. Preserve approved trees. The aim is a Windows executable and matching source, with visual comparison against the live game.

1. **Inventory:** Recover all water modes, asset formats, texture/filter settings, pass order, target formats/dimensions and visibility rules from the client. Verify against actual data, without assuming unavailable assets.
2. **Surface and reflections:** Port real geometry/masks, time-dependent normal textures, distortion, Fresnel, color space, clipping and sky/world reflections for every visible elevation.
3. **Refraction and edges:** Port per-plane refraction and underwater fog, actual shoreline edge mask target/pass, depth/occlusion and correct compositing across modes.
4. **Authored shoreline waves:** Load all versions, create original geometry, interpolate authored day/night colors, load original mipmapped texture, reproduce the three animated layers, render states and distance fading.
5. **Underwater:** Recover and implement below-water rendering, refraction, fog and postprocessing/transitions, including entry/exit and changing regions.
6. **World integration and completion:** Cover all world water records and quality settings; ensure no regressions to trees, scenery or controls. Verify code, GPU behavior and Windows build against recovered client evidence, then provide the executable and matching source. Live-game comparison is the user's task.

## Recovered water shader sources (from shaders.pck)

The client's `shaders.pck` is readable, so the water pixel/vertex shaders are
now source evidence rather than recalled behaviour. Extracted with
`tools/pck_lib.py`:

| entry | mode | key constants |
|---|---|---|
| `ps/water_expensive.txt` | reflected + refracted surface | `c1=(0.25,-0.15,0,0)`, `c3=1`, `c4=(0.95,0.95,0.95,1)`, `c2=(0,0,0,1)`, `c0.a` = program minimum reflect alpha |
| `ps/water_simple.txt` | fixed-function-ish surface | `c1=(0.2,0.65,0.65,0.8)`, `c2=(0,0.25,0.35,1)`, `c3=(0.8,0.8,0.8,1)` |
| `ps/water_cheap.txt` | cheap quality | `c1=(0.25,0.2,0,0.6)`, `c3=(0.85,...)`, `c4=1`, `c5=(0.5,...)`, `c2=(0,0,0,1)` |
| `ps/water_expensive_s.txt` | expensive + specular LUT | `c2=(1,-1,1,1)`, `c3=c4=(0.85,...)`, `c6=(0.3,0.3,0.3,1)` specular, `c0.r/g` = look-up linear transform |
| `ps/waterrefract.txt` | below the surface | see the refract register list |
| `vs/water_vs.txt` | surface vertex stage | `c4` = eye, `c30-33` = view matrix for the projected `oT2`, `oT3` = `eye - v0` |

Two details that change arithmetic rather than appearance:

* **The Fresnel lerp weight is not the quintic.** All three surface shaders
  finish with `lrp r0.a, r0.a, c2.a, c1.a`, and `c2.a` is `1.0` in every
  variant, so the weight is

  ```text
  weight  = c1.a + (1 - c1.a) * quintic
  quintic = (1 - cos)^5 = ((1-cos)^2)^2 * (1-cos)
  ```

  The identity term differs by mode: `water_simple` uses `c1.a = 0.8` and
  `water_cheap` uses `c1.a = 0.6` (both literal `def` constants, so they are
  unaffected by the program's `c0.a`), while `water_expensive` writes
  `lrp r0.a, r0.a, c2.a, c0.a` and takes the program's minimum reflect alpha
  there instead. The viewer's cheap/simple path must therefore use the literal
  per-mode constant, not the camera-derived `c0.a`, and its expensive path uses
  `min_reflect + (1-min_reflect)*pow(1-facing,5)`.
* **Specular differs per variant, and the base form's term is zero.** The base
  shader does `mul_x4 r1.rgb, v1, weight` on the interpolated secondary RGB
  input; `water_vs.txt` writes `oD1 = c5.wwww = (0,0,0,0)`, so on that path the
  term vanishes and the viewer correctly omits it. `water_expensive_s.txt`
  instead computes a real specular: `dp3 r3, r0_bx2, -r1_bx2` (reflect vector
  against the normal, unsaturated), reflects with `mad r3, r3_x2, r1_bx2,
  r0_bx2`, saturates against the light direction `c5`, then linear-transforms
  with `add r3, r3, c0.g` / `mul_x8 r3, r3, c0.r` and samples a Fresnel
  look-up table `t3` whose green channel scales the sun colour `c6`. That
  variant is not implemented by the viewer; omitting it is correct for the
  base pipeline but is a separate mode, not the same shader.
* The normal map is used **unnormalised** (`mov r0.x, r1_bx2.x` /
  `mov r0.y, r1_bx2.z`), which the viewer already documents; `water_cheap.txt`
  instead keeps `mov r0.r, r1.r` / `mov r0.g, r1.b`, so the cheap mode does not
  swizzle the same way.

Remaining open items above are unchanged. This table closes the "recover the
water shader constants" gate for the four surface/refraction pixel shaders; it
does not close target sizing, pass order or authored shoreline waves.

### Shader pack coverage audit

`tools/survey-shader-pack.py` cross-references every `shaders\...` path in
`elementclient.exe` against the pack. Current result: **52 of 60 literal
references resolve**; the pack has 166 entries.

The eight unresolvable names are not a pack defect — the shipped client asks
for them and has no fallback, so those paths are inert in the 1.4.2 build:

| missing program | client evidence |
|---|---|
| `ps/water_alpha_simple.txt` | `A3DTerrainWater::SetWaterType, failed to load water_alpha_simple.txt` |
| `ps/simple_alpha_edge_ps.txt`, `vs/simple_alpha_edge_vs.txt` | edge-mask pair, named after `textures\water\waternormal\water_normal%02d.dds` |
| `ps/water_areas_rt_ps.txt`, `vs/water_areas_rt_vs.txt` | area render-target pass (the "areas" path, distinct from the surface pass) |
| `ps/t2_tex_merger.txt`, `ps/t2_tm_lm.txt` | terrain pass, referenced next to `Shaders\vs\terrain_vs.txt` |
| `vs/stereo_vs.txt` | stereo/palette path, referenced next to `gfx_pallete_vs.txt` |

Two consequences for this checklist: the simple-water and area-RT programs the
viewer approximates cannot be matched against shipped source (there is none),
and the real edge-mask shaders being absent explains why `0x7ccfb0`'s edge pass
had to be reconstructed from depth — the original pair simply was not shipped
in this client build.

`treebranch.txt`/`treebranchvs.txt` exist in the pack but are referenced by
nothing in the executable (no `treebranch` string at all), and no tree `.sdr`
exists either, so they are unused leftovers and cannot be treated as the tree
material the client actually runs. Tree draws remain open.

## Open issues
The edge mask is currently a terrain-depth approximation, underwater surface/refraction paths are incomplete, some water mode/target semantics remain unidentified, and shoreline noise/global RNG phase and complete client asset coverage have not been proven. The current build is experimental and these issues remain open.

### Latest edge-pass implementation (not yet parity-certified)
- The client disassembly places `0x7ccfb0` after the refraction scene and before the water surface; its state wrappers disable RGB writes, blending, lighting and fog. The viewer now rasterizes a terrain-height ramp into refraction alpha, preserving RGB and reusing scene depth, then samples that alpha at undisplaced projected coordinates for the expensive-water edge term.
- Both UNORM and sRGB offscreen GPU checks verify alpha ramp, preservation of refraction RGB, and rejection behind nearer depth. The old terrain-depth reconstruction remains the non-reflection fallback.
- **Still open:** prove the client's exact terrain-area selection, depth/write and clear-state behavior and geometry/culling against the disassembly; resolve low-quality water and underwater pass/transitions, RNG phase, integrated runtime and remaining acceptance gates. This is not a claim of 100% parity.

### Below-surface continuation
- Recovered the distinct `waterrefract.txt` path and added an above-plane clipped air-fogged refraction scene with an unmirrored sky; synthetic GPU checks cover steep and grazing below-water angles on UNORM and sRGB.
- Matched strict area boundary rejection and surveyed all 107 records: their center Y matches surface height, while 18 bounding-box pairs overlap. Client first-match ordering is still unknown.
- Open: scroll and cheap quality shaders, original target size/clear/depth details, register precision, below-surface fog, and original overlap/load order. Live appearance and final integrated Windows behavior remain unverified. No 100% claim.

### Area-class and underwater texture continuation
- Preserved the authored area-class flag and separated batches. Three of 107 real areas use the client's alternate fixed-function material; a provisional implementation was added in the later entry below.
- Ported quantized 256² underwater bump-map arithmetic with bilinear sampling. A gradient GPU readback agrees with the reference calculation.
- Full parity is **not complete**. In particular, alternate area material, quality/scroll modes, original exact pass states and target sampling remain open.

### Alternate-area material implementation
The renderer now routes flagged areas to a separate depth-read-only encoded-blend material and loads the client's 30 L-water texture frames and mips. Synthetic GPU tests cover its fixed-function color arithmetic and frame change in both target formats. This is **not complete parity**: dynamic water light factor, fog, quality/variant modes, ordering and real-texture validation are still open; the client PCK exceeds the connector's 100 MiB download limit, though the application can read it from the installed client.
