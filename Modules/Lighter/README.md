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
3. Adjust ray resolution, samples and bounces. At zero bounces you still get
   camera rays and optional shadow rays. Water transmission, mirror rays and
   indirect lighting require at least one bounce. The separate **Water
   reflections** switch on the World tab still gates water mirrors.
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
  native far heightfield when terrain cutoff is off.
- Streamed BMD buildings, SpeedTree geometry, instanced grass, posed world
  models, world effect sheets and posed effect meshes. Walker and Populator
  poses are included if active. Secondary rays can hit objects outside the
  camera frustum **within the streamed scene**.
- Native water cells and their animated normal textures. Reflections and
  refractions are new world-space rays. Water uses Fresnel blending, total
  internal reflection, underwater fog/transmission and shallow-edge attenuation
  from a terrain ray. The baked scenery placement flags gate water views.
- The client's sky panorama and roof textures are sampled by ray direction.
  The sun, static scene point lights, and point lights published by GFX owners
  can all cast occlusion rays. Optional indirect rays contribute diffuse
  lighting. The native world passes do not composite on top.

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
The whole-world BMD preload, temporal upscalers and frame generation are paused
while Lighter renders. Existing saved choices for those features are unchanged.

## Verification

The viewer tests validate both WGSL paths and inspect the emitted Vulkan
ray-query SPIR-V. The hardware path uses Naga 25 for safe hit queries after
traversal without changing the viewer's wgpu version. The pixel tests read
back *composited* color and depth. The controlled scene covers stacked and
coincident cutouts, minified coverage and small camera moves, blended sheets,
local-light and sun shadows, and water reflection/refraction gates. It checks
that identical packets and light-only updates don't rebuild acceleration
structures. The viewer fixture switches between native and RT and checks
actual native-pass submission, terrain, water cells, trees, authored sky, a
streamed BMD, GFX texture/playback, and static and runtime point-light shadows.
It also checks eviction and that Hardware RT on a CPU ICD fails without
falling back to raster. These are synthetic scenes, not real-map visual or
physical GPU performance certification. The first frame on software Vulkan
may be slow while the driver compiles the shader.

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
