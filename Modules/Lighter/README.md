# Lighter

Lighter is an optional replacement for the **main world renderer**. It does not
use a raster color, depth, shadow map, or reflection texture to find ray hits.
The native renderer is the default. The interface and map views still use the
viewer; post-processing for bloom, sharpen and display gamma can consume the
ray-traced scene and its reverse-Z depth.

## Use it

1. Open **Escape → Modules → Lighter**. Select a ray backend and restart
   Explorer. This first restart lets the module retain CPU geometry before the
   normal renderer releases its upload sources.
2. **Software RT** uses a stackless BVH in a GPU compute shader. It does not
   require RT cores. **Hardware RT** requires a physical Vulkan 1.2+ GPU and a
   driver exposing acceleration structures and `VK_KHR_ray_query`. It builds
   BLAS/TLAS and dispatches native ray queries on the same device as the viewer.
   On Windows select Hardware and restart to use Vulkan instead of D3D12.
3. Adjust ray resolution, samples per pixel and bounces. At zero bounces you
   still get camera rays, local lighting and optional shadow rays. Water
   transmission, glossy reflections and indirect lighting require at least
   one bounce. The separate **Water reflections** switch on the World tab still
   gates water mirrors. Without a bounce, the authored ambient approximates
   indirect light. The **Edge-aware denoiser**, **Progressive sampling while
   still** and **Soft sun shadows** are on by default and can be toggled
   independently. Leave the camera still to accumulate lighting samples.
4. **Off** returns to the native world renderer. An unsupported or failed
   selected backend reports an error rather than silently drawing native or
   switching to the other ray backend.

For a source-tree build, set `PW_MODULES_DIR` to the folder containing the
installed manifests (the repository's `Modules` directory). Settings live in
`PerfectWorldViewer/lighter.ini` under `%LOCALAPPDATA%`, `$XDG_CONFIG_HOME`, or
`~/.config`. If the Vulkan selection cannot open a window on your system, set
`mode=off` in that file to recover the original backend.

## What is traced

- The resident terrain mesh and layer masks, plus a lower-detail mesh of the
  native far heightfield when terrain cutoff is off. Lighter omits overlapping
  far triangles and stitches the coarse exterior to the actual near boundary
  vertices, including non-flat edges. Distant interiors remain simplified.
- BMD buildings, SpeedTree geometry, instanced grass, posed world models,
  world effect sheets and posed effect meshes. Walker and Populator poses are
  included if active. Secondary rays can hit objects outside the camera
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
  **Soft sun shadows** restores a single directional shadow ray. Point lights
  still have sharp shadows. The native world passes do not composite on top.

### Static scenery residency

Lighter consumes BMD meshes and source textures from the native archive
resolver, not from the raster atlas. It retains the native local ornaments and
also requests BMDs within **768 world units horizontally** of the installed
terrain block center. Selection does not depend on camera direction. The
original-game terrain cutoff still clips placement residency to the installed
terrain window. Raster map views and the native renderer use their existing
scenery selection.

The extension admits no more than 512 candidates, nearest first, with separate
16 MiB budgets for packed geometry (including conservative BVH overhead) and
additional texture tiles. Available adapter storage can reduce it further; at
insertion, no more than half of the remaining texture-bank room goes to the
extension. Source data for unchanged resident BMDs is reused on the CPU. This
is not whole-map residency or a guarantee against off-screen omissions: bounds
are based on placement origins, not receiver/light reach or model bounds, and
the static acceleration structures are still rebuilt for a new packet. The
**Lighter** menu reports local and extended placements, range/cutoff exclusions,
budget deferrals, unavailable models and missing texture references.

For the remaining work across terrain, models, textures, vegetation, water,
lighting and verification, see the [full ray-tracing checklist](TODO.md).

### Denoising and convergence

- Both backends write linear radiance, reverse-Z depth, and primary hit depth,
  normal and albedo guides. A 3×3 joint bilateral filter smooths compatible
  surfaces, and guide-aware upscaling avoids mixing across depth or normal
  discontinuities. Mixed-coverage and translucent pixels are not averaged with
  their neighbors. Sky, emissive and water pixels bypass the spatial filter.
  Depth always comes from the closest current camera sample, never from a
  neighboring pixel or an older frame.
- The interactive defaults are one sample, one secondary bounce and 50%
  ray resolution per axis. Progressive sampling varies the light transport
  sequence without jittering the single-sample depth or cutout pattern.
  It averages the first ten compatible samples, then uses 10% new radiance
  and 90% history. Tiny single-sample view changes reproject the primary
  hit into the previous view; depth, normal and albedo must agree. Sky,
  water and blended or mixed pixels do not reproject through view changes.
  Movement of more than one ray pixel from the last reset's frustum (or a
  material change to near/far depth), changed resolution, backend, lighting,
  off-camera poses or moving lights reset history. Water-wave and scrolling
  inputs invalidate only pixels whose paths used them. Roof drift is allowed
  up to a quarter texel before resetting roof-dependent history.
- On a moving view or with animated poses/lights, the spatial filter still
  works, but there may be little usable history. Raising samples per pixel can
  help at a higher ray cost. The requested ray resolution is an upper bound:
  at large sizes it can be reduced to stay within adapter storage limits and
  a 256 MiB budget shared by the trace image, two alternating history images
  and the spatial filter output. Composition uses guide-aware bilinear color
  (ordinary bilinear with the filter disabled) and nearest current depth.

### Light transport and its limits

This is genuine ray-traced visibility in both modes. Software intersects a
compute BVH; Hardware uses Vulkan ray queries. The two modes share the shading
code. They are not two different lighting models.

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
  shoreline mask reduces the interface reflection. The first water split is
  evaluated on both sides; later interfaces sample one lobe to bound ray counts.

This is still a bounded, low-sample renderer for legacy assets, not a reference
path tracer. Lights retain authored colors/ranges rather than calibrated radiant
intensities. Panoramas are not HDR light probes. Gloss is a normalized
power-cosine approximation with scalar reflectance, not a full microfacet BRDF
with independently sampled analytic highlights. Air fog is linear, and water
is an open height surface rather than a closed volume. There is no caustic
solver.

### Coverage and work per frame

Both backends test material visibility and alpha before accepting an
intersection. Transparent holes don't shift the start of the ray past another
surface. Solid shadows stop at the first covered blocker. Texture tiles have
isolated, ray-footprint-filtered mips with alpha-weighted color; distant cutouts
sample the fraction of passing texels using a fixed per-pixel pattern, not a
new seed each frame. Blended sheets sort by distance, using authored
layer/priority for coincident GFX cards; the two halves of a card blend once.

Point lights are indexed by their world-space influence, not a list of the 48
nearest the camera. Static lights upload with the resident world, and a light
pulse doesn't rebuild the Vulkan acceleration structures. Unchanged moving
packets skip upload; unchanged geometry skips Vulkan BLAS/TLAS builds.

Lighter is a different lighting model, not pixel-for-pixel retail emulation.
Texture tiles and distant terrain are simplified. Native terrain lightmaps,
vegetation wind and camera-facing leaf behavior, cloud detail, rain/snow and
shoreline wave overlays, lens flares and some legacy effect/skin blend and
no-depth-test modes are not reproduced.
The native whole-world BMD preload, temporal upscalers and frame generation
are paused while Lighter renders. Existing saved choices for those features
are unchanged; Lighter uses the separate bounded BMD region described above.

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
