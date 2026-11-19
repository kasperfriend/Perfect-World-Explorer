# Lighter

[PR #51 acceptance checklist and dated evidence](REMAINING.md). Hardware
SDK evaluation and retail performance require the specified Windows RTX
host; CPU and build receipts cover narrower scopes.

Lighter is an optional replacement for the **main world renderer**. By default
its visibility comes from rays, not the Native renderer's raster targets.
Hardware also offers an experimental raster *primary visibility* pass using
Lighter's own geometry. Shadows, reflections and refraction remain rays.
The Native renderer is the default. The interface and map views still use the
viewer; post-processing for bloom, sharpen and display gamma can consume the
ray-traced scene and its reverse-Z depth.

## Use it

1. Explorer starts with **Native** so a saved ray setting cannot hold up the
   first window. Your saved choice is still selected in **Escape → Modules →
   Lighter**. Choose **World renderer** and click **Reload World** to apply it
   to the same map, camera and time. The old world stays open with progress
   and **Cancel Reload**. If the replacement stops making first-frame progress
   for two minutes, fails or is cancelled, Explorer keeps the current world
   and does not overwrite the saved mode.
2. **Software RT** uses a stackless BVH in a GPU compute shader. It does not
   require RT cores. **Hardware RT** requires a physical Vulkan 1.2+ GPU and a
   driver exposing acceleration structures and `VK_KHR_ray_query`. It builds
   BLAS/TLAS and dispatches native ray queries on the same device as the viewer.
   On Windows, interactive Software RT also tries Vulkan first to avoid the
   legacy D3D12 shader compiler; if it cannot open a Vulkan adapter/device it
   tries D3D12. Native still prefers D3D12. An explicit `WGPU_BACKEND` overrides
   the default API choice.
3. Adjust ray resolution, samples per pixel and bounces. At zero bounces you
   still get camera rays, local lighting and optional shadow rays. Water
   transmission, glossy reflections and indirect lighting require at least
   one bounce. The separate **Water reflections** switch on the World tab still
   gates water mirrors. Without a bounce, the authored ambient approximates
   indirect light. The **Edge-aware denoiser**, **Progressive sampling while
   still** and **Soft sun shadows** are on by default and can be toggled
   independently. Leave the camera still to accumulate lighting samples.
   **Diffuse bounce distance** defaults to 256 world units. Only indirect
   diffuse rays in air are shortened; primary visibility, direct shadows,
   mirrors and refraction keep their range. Select **Full view distance**
   to compare, or enable **Reference** for the full-range reference transport.
   With **Follow local region lighting** off (default), the sun, ambient and
   sky palette follow the map and clock, not the local box under the camera.
   Regional fog and weather still apply. See the [optimized renderer](performance.md#interactive-hardware-rt-pass).
   **Single-path transport** (default) usually samples one continuation at
   glass/water and one local light in dense lighting. At 1 spp it still
   splits the first water interface, preserving stable reflection and
   transmission even with accumulation turned off. Turning Single-path off
   also gives a deterministic first split at glass.
   **FSR 1 spatial upscale (EASU)** is opt-in. Below full ray resolution it
   reconstructs color from the filtered ray image, without changing current
   depth or enabling any vendor temporal SDK. The separate, launch-only
   [experimental Vulkan FSR and DLSS-RR paths](temporal-reconstruction.md)
   require explicit SDK setup and are not part of the default renderer.
   **Automatic ray resolution** is opt-in and targets completed ray and
   reconstruction GPU time, not total frame time; the slider seeds its
   50–100% range. Start with a fixed scale when comparing renderers.
   To try **Rasterized primary visibility**, select Hardware, enable it and
   click **Reload World**. It requires one sample per pixel, is off by default,
   and falls back to full primary rays if the raster target is unavailable.
   Its extra render pass does not guarantee a speedup.
4. Select **Off** and reload to leave Lighter off and save Native as the
   preference. In a running ray world, an unavailable backend or an unfittable
   scene falls back to Native with the reason in the HUD and Lighter menu. It
   neither rewrites your selected mode nor switches to the other ray backend.
   A healthy streamed scene or a changed ray setting can retry it. An explicit
   ray reload must produce a ray frame before replacing the old viewer, and
   explicit headless ray captures never save a Native substitute.

For a source-tree build, set `PW_MODULES_DIR` to the folder containing the
installed manifests (the repository's `Modules` directory). Settings live in
`PerfectWorldViewer/lighter.ini` under `%LOCALAPPDATA%`, `$XDG_CONFIG_HOME`, or
`~/.config`. During Windows startup use **Cancel startup** to quit a stalled
load. If **Reload World** fails, use **Copy details** in the error dialog. An
unexpected exit includes its code and the last startup stage Explorer saw.
`0xc0000005` is a Windows access violation; it does not identify the failing
module or imply a graphics device loss. The `Viewer startup:` and `Ray adapter`
lines for the replacement PID in `pwe-logs/session.log` (or your configured
log folder) give more context, including the adapter and driver. If Windows
recorded an Application Error for that attempt, its faulting module name and
exception offset can help narrow the crash.

## What is traced

- The resident terrain mesh and layer masks, plus a lower-detail mesh of the
  native far heightfield when terrain cutoff is off. Lighter omits overlapping
  far triangles and stitches to the installed near boundary in either LOD
  direction. On smaller maps, the ray feed compares regular grids and can
  refine only the tiles that need it within the index budget. Large maps can
  keep the coarser stride-16 fallback, whose fidelity is not guaranteed.
  See the [PW 1.4.2 height, grazing, and seam audit](evidence/2026-11-18/11-terrain.md).
- BMD buildings, SpeedTree geometry, instanced grass, posed world models,
  world effect sheets and posed effect meshes. Walker and Populator poses are
  included if active. Rigid SKI parts follow their bone or hook as instances;
  a rigid attachment moving with its parent keeps the same BLAS. Walker's
  first-person head/face mask applies to camera rays, not shadow or bounce
  rays. Authored visibility (e.g. hiding a weapon while riding) still applies.
  Secondary rays can hit objects outside the camera
  frustum **within the resident scene**. BMDs can also be resident beyond the
  native ornament load distance (see below).
- Native water cells and their animated normal textures. Reflections and
  refractions are new world-space rays. Snell refraction and unpolarized
  dielectric Fresnel handle both sides of the interface, including total
  internal reflection. The baked scenery placement flags gate water views.
- The client's sky panorama and roof textures are sampled by ray direction.
  The sun, static scene point lights, and point lights published by GFX owners
  can all cast occlusion rays. Sun shadows sample a disc with an angular radius
  of 0.27°, producing penumbras that widen with blocker distance. Disabling
  **Soft sun shadows** restores a single directional sun shadow ray. Local
  emitters sample finite shapes, and explicit thin glass tints visibility.
  See [direct lighting](direct-light.md). The native world passes do not
  composite on top.

Morph, supple and muscle skin records are not decoded. The Lighter settings
show separate per-feed counts for unsupported SKI types **encountered** on
the current map, deduplicated by path within each feed. Missing packages
and skins never inspected cannot be inferred from these counts. Animated GFX
still uses the shared effect-sheet feed; its placement is evaluated by the
native owner/hook player, not by the rigid SKI instance path.

### Static scenery residency

Lighter consumes BMD meshes and source textures from the native archive
resolver, not from the raster atlas. The map's BMD manifest assigns each
ornament a stable **128-world-unit owner cell**. Decoded vertices produce
exact bounds even if they run into another cell; an undecoded placement starts
with an estimated ±64-unit box and a warning. Very large or offset assets can
extend past that estimate until first decoded. Terrain has separate near-block
and far-patch keys with bounds from the heightfield.

Native local ornaments have priority, nearest to the installed block first.
When a local placement cannot fit, it is counted as budget-limited and omitted
whole, not rendered with incomplete material data. Extra placements are ranked by
distance to the installed block, distance to the local receiver region, and
whether they overlap a static point-light path to that region. The secondary
reach is **768 world units beyond the receiver region**, with a coarse corridor
for each of up to 32 nearby static lights. The corridors are ranking hints,
not visibility tests. Sun and runtime lights are not used for ranking.
View direction and motion within one block do not change the rank. Terrain
cutoff still checks placement anchors against the installed window. Raster
map views and the native renderer use their existing scenery selection.

The extension considers up to 512 ranked placements, with separate 16 MiB
budgets for packed geometry (including conservative BVH overhead) and extra
texture tiles. Available adapter storage can reduce it further. Admission
reserves texture space for moving actors and effects; the extension can use
at most half of the room still free after local admission. See the
[GPU budget](gpu-budget.md) for the terrain-mask and vegetation limits.
Unchanged resident chunk geometry retains matching software trees and Vulkan
BLAS across streams; evicted geometry does not accumulate in the cache. The
**Lighter** menu shows catalog and resident chunks, estimated bounds, local
and extended placements, range/cutoff exclusions, budget deferrals, unavailable
models and missing texture references. A whole-map manifest does not mean
that all of the geometry on the map is resident at once.

For the remaining work across terrain, models, textures, vegetation, water,
lighting and verification, see the [full ray-tracing checklist](TODO.md).

### Denoising and convergence

- Both backends write half-float radiance and luminance variance, compact
  guides, and **FP32** reverse-Z and linear guide depth (28 bytes/pixel).
  A variance-guided à-trous filter uses up to five spaced 3×3 passes with a
  single scratch buffer. Wider passes require accumulated, noisy guides.
  Guide-aware upscaling respects depth/normal discontinuities. Mixed or
  translucent guides, sky, emissive surfaces and water bypass spatial
  filtering. Depth remains the closest **current** camera sample.
- The defaults are one sample, one secondary bounce and 50% scale per axis.
  Progressive sampling varies transport without moving the one-sample camera
  ray or cutout pattern. It blends compatible history with variance-adaptive
  weight and a 32-sample count limit. A small single-sample camera change can
  reproject a matching owner, depth, normal and albedo. Sky, water and blended
  or mixed pixels do not reproject across view changes. Camera cuts, resized
  output, changed static scene revision or changed lighting reset history.
  Moving geometry, waves and scrolling invalidate affected paths. Roof drift
  may advance by a quarter texel before invalidating roof-dependent history.
- On a moving view or with animated poses/lights, the spatial filter still
  works, but there may be little usable history. Raising samples per pixel can
  help at a higher ray cost. The requested ray resolution is an upper bound:
  at large sizes it can be reduced to stay within adapter storage limits and
  a 256 MiB budget shared by the trace image, two alternating history images
  and the spatial filter output. Composition uses guide-aware bilinear color
  (ordinary bilinear with the filter disabled) and nearest current depth.

### Light transport and its limits

Software intersects a compute BVH; Hardware uses Vulkan ray queries. In the
optional Hybrid path only primary solids use raster visibility. Both modes
share shading and the same world-space secondary visibility rules.

- Diffuse bounces sample a cosine-weighted hemisphere. They carry linear albedo,
  not a fixed brightness multiplier, and replace the ambient approximation on
  that surface. Baked vertex lighting is not treated as diffuse albedo and
  bounced again. Reflective materials share their energy between a matte lobe
  and a reflected lobe; they don't get full diffuse light plus a mirror.
  Authored specular strength and power on opaque BMDs, posed skins and eligible
  GFX skins control reflection strength and lobe width. Higher power gives a
  narrower reflection. GFX skins must be opaque, use standard alpha blend and
  test/write depth; translucent and additive effects keep their sheet order.
- The geometric face determines which side can receive shadows or emit a
  secondary ray. Interpolated and water normal-map normals steer shading but do
  not move the geometry or let light shine through a back face.
- Underwater paths lose light exponentially with distance traveled. The
  authored fog range and hue approximate extinction and in-scattering; shallow
  bottoms show through because the underwater path is short, not because a
  shoreline mask reduces the interface reflection. Single-path transmission
  retains the radiance Jacobian and tint; total internal reflection still
  reflects. Disable Single-path for the legacy first split, including in
  Reference. Moving water may not accumulate enough samples to hide noise.
  See [per-body geometry and authored water materials](water-fidelity.md).

This is still a bounded, low-sample renderer for legacy assets, not a reference
path tracer. Lights retain authored colors/ranges rather than calibrated radiant
intensities. Panoramas are not HDR light probes. Gloss is a normalized
power-cosine approximation with scalar reflectance, unless **GGX / Lambert materials** is enabled. That option uses one
energy-bounded microfacet/diffuse model for direct and bounced light; see
[scattering and explicit glass](bsdf.md). Air fog is linear, and water
is an open height surface rather than a closed volume. There is no caustic
solver.

### Coverage and work per frame

Both backends test material visibility and alpha before accepting an
intersection. Transparent holes don't shift the start of the ray past another
surface. Opaque shadows stop at the first covered blocker; covered leaves transmit
a bounded, pigment-tinted fraction. Texture tiles have
isolated, ray-footprint-filtered mips with alpha-weighted color; distant cutouts
sample the fraction of passing texels using a fixed per-pixel pattern, not a
new seed each frame. Ordinary sheets sort by distance; GFX preserves its
authored draw bracket independently of distance, testing opaque and earlier
GFX depth. The two halves of a card blend once.

Point lights are indexed by their world-space influence, not a list of the 48
nearest the camera. Static lights upload with the resident world, and a light
pulse doesn't rebuild the Vulkan acceleration structures. Unchanged moving
packets skip upload; unchanged geometry skips Vulkan BLAS/TLAS builds.

Lighter is a different lighting model, not pixel-for-pixel retail emulation.
Texture tiles and distant terrain are simplified. Vegetation uses sampled motion and independent secondary cards; see
[vegetation motion and LOD scope](vegetation.md). Ground projections, cloud-shadow proxies and ray-lit view precipitation
use shared authored inputs. See [world effects and weather](effects-weather.md)
for remaining lens/Warp and blend-factor limits.
The native whole-world BMD preload, external upscalers and frame generation
are paused while Lighter renders. Existing saved choices are unchanged;
Lighter resolves its own ray image before scene post and uses the separate
bounded BMD region described above. See [presentation and device recovery](presentation.md).

### Baked and relit materials

Lighter keeps authored day/night appearance by default. Building vertex
lighting uses the baked BMD colors (including the color-only and texture ×2
stages), and the terrain and grass paths can use the matching map's T2LM
pairs. A missing night map reuses day for terrain only. Grass needs both
maps; it selects the area-center tile and clamps to its edge when the World
setting is on. These are bounded ray lightmap copies (up to 64 px per tile,
12 MiB total), not the full native lightmap atlas.

**Relight baked scenery** uses the terrain/grass diffuse textures and BMD
texture/material diffuse instead of the baked colors and lightmaps. Sun,
local lights and indirect rays then shade that reflectance; they do not
multiply an already-lit vertex color or lightmap. For headless comparisons
use `PW_PREVIEW_HOUR=12` or `0` and `PW_PREVIEW_RELIGHT_BAKED=0` or `1`.

SKI parts carry diffuse/ambient/emissive coefficients, colored specular and
legacy power, sidedness and texture/material opacity. Authored face, brow,
eye, mouth, moustache and body stages use the posed mesh's own texture paths;
the eye/lip gloss stage is applied after local lighting. Body-mask alpha is
not coverage. Optional BMD NRF references resolve through mounted `shaders.pck` or loose
files. Lighter interprets their RGB as tangent normals when relighting;
this interpretation is checked against a fixture, not reverse-engineered
for all retail NRF encodings. They do not move the intersection geometry.
Known `.sdr.` scale and scroll constants apply to both hit coverage and color.

These are legacy material channels, not invented metallic/roughness maps.
Authored power drives the pre-GGX glossy lobe. Transparent model sheets carry
scalar transmittance from alpha; tinted shadow transmission and measured
BSDF energy are separate transport work. Unsupported blend/depth modes do not
become physically equivalent merely because their geometry is traced.

### Ray texture filtering

The texture bank retains authored DDS levels for terrain, vegetation, scenery,
animated models/effects, sky and water normals when they fit. An oversized
source uses the first authored level that fits and keeps its contiguous tail;
short authored chains clamp to their last resident level. Single-level images
and sources with no usable authored level under the cap get generated mips.
The common terrain cap targets 48 MiB, with a 512-pixel maximum side for color,
128 for water normals and 32 for the terrain blend masks. The shared bank is
limited to 128 MiB (also subject to the device's buffer limits). This isn't
a virtual texture system or a promise to retain every original top level.

RGB is decoded from sRGB before each filter tap and associated with linear
alpha to avoid transparent color fringes. Masks, normals and alpha aren't
sRGB-decoded. Generated cutout levels store passing area; authored cutouts
retain their original alpha levels and test against the material reference.
Grass uses its original image chain independent of the native atlas layout.

**Ray texture anisotropy** defaults to 8×; 1× uses isotropic trilinear and 16×
spends more samples along the projected footprint's long axis to retain detail
across it. Primary and secondary rays use that same filter. For headless
comparisons set `PW_PREVIEW_ANISOTROPY=1|8|16`. Capture metadata records
`mip_authored_levels`, `mip_generated`, `mip_reduced` and `mip_fallback` (the
union of generated and reduced **tiles**, counted once per admitted tile).
These are separate from missing textures. Masks contribute to generated counts.

## Capture geometry coverage

Set `PW_PREVIEW_COVERAGE=1` for a headless preview. Explorer writes the PNG and
its `coverage.txt` sidecar; Software and Hardware also write ray inventory to
`coverage.csv`. `PW_PREVIEW_LIGHTER=off` captures the matched Native baseline.
A failed selected map is not silently replaced with `world`. The [coverage guide](scene-coverage.md)
explains the inventory and [retail matrix](retail-matrix.md) explains the
matched settings, regional comparisons and device gates.

## Verification

See [GPU completion and readback](performance-waits.md) for the Phase 0 wait
audit, F12 world captures, and the asynchronous window/resize regressions.
See [residency and performance checks](performance.md) for acceleration and
resource reuse, the streaming copy audit, and the synthetic timing sweep.

The viewer tests validate both WGSL paths and inspect the emitted Vulkan
ray-query SPIR-V. The hardware path uses Naga 26 to keep candidate and
committed hits distinct without changing the viewer's wgpu version. Pixel tests
read back *composited* color and depth. They cover stacked and coincident
cutouts, small camera moves, blended sheets, sun and point shadows, and water
visibility gates. Analytic pixel cases check Lambertian sky lighting, baked
colors, diffuse/mirror energy, back-face light rejection, Fresnel both above
and below water, total internal reflection, and distance-based extinction.
Both backends also check spatial noise reduction, sharp depth/normal edges
through upscaling, unchanged depth, analytic sun-disc coverage, progressive
averaging and invalidation, and different reflection widths at equal material
strength. The tests verify that unchanged geometry does not rebuild acceleration
structures. The viewer fixture switches between native and RT and checks
actual native-pass submission, terrain, water cells, trees, authored sky, a
streamed BMD, GFX texture/playback, and static and runtime point-light shadows.
It also checks eviction and that Hardware RT on a CPU ICD fails without
falling back to raster. Both ray pixel suites use the production archive and
streaming code for the same cutout BMD beyond the native load distance: they
check camera visibility, sun shadow, both water paths, independent water flags,
reverse-Z depth, movement into the native region, eviction and reload. They
also check grazing rays from both sides of a stitched, non-flat terrain edge.
These are synthetic scenes, not real-map visual or physical GPU performance
certification. The first frame on software Vulkan may be slow while the driver
compiles the shader.

```sh
# Headless software traversal. Point VK_ICD_FILENAMES at lavapipe for CPU Vulkan.
cargo test --locked --bin perfect-world-explorer lighter_software_pixels -- --ignored
# Real viewer geometry feeds and renderer dispatch on a small synthetic map.
cargo test --locked --bin perfect-world-explorer lighter_streamed_world_pixels -- --ignored
# Native ray queries. Requires Vulkan ray-query hardware; no emulation fallback.
cargo test --locked --bin perfect-world-explorer lighter_hardware_pixels -- --ignored
# Optional: only with a CPU ICD exposing KHR_ray_query. Exercises the Vulkan
# interop, NOT RT cores or the production hardware adapter eligibility rule.
cargo test --locked --bin perfect-world-explorer lighter_vulkan_interop_on_cpu -- --ignored
```

To capture a client map headlessly, set `PW_PREVIEW_LIGHTER=software` (or
`hardware`) when running the normal `--gpu-preview` command. This is a one-run
choice and does not change your saved settings. Ordinary previews remain native.
On a supported GPU, compare the same location on land, over shallow water and
underwater in both modes, with the reflection/shadow switches on and off. Use a
location with authored effects and an off-camera building near the water.

## Reconstruction crash diagnostics (opt-in)

For a repeatable crash with `PW_RAY_RECONSTRUCTION=fsr` or `rr`, first
record the failure with no skip. Then set **one** value of
`PW_RAY_RECONSTRUCTION_SKIP` for a new process:

| Value | Stop before |
| --- | --- |
| `bootstrap` | Any reconstruction-specific path or library lookup. |
| `ffx-loader` | The requested FSR DLL lookup/load. RR is unchanged. |
| `ngx-loader` | The requested RR bridge/feature lookup/load. FSR is unchanged. |
| `raw-device` | Both raw Vulkan HAL imports. The requested library can still load, but the ordinary wgpu device is used. |
| `interop` | Reconstruction images, their raw views, and bind/descriptor setup, **after** opening the provider. The provider's internal descriptor work at context creation is not bypassed. |

Unset the variable to restore normal behavior. An intentional skip never
counts as successful SDK dispatch. Native ignores these flags. See
[the S audit](evidence/perf/S-A1.md) and [the performance plan](PERF-PLAN.md).
