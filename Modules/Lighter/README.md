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
   camera rays, local lighting and optional shadow rays. Water transmission,
   mirror rays and indirect lighting require at least one bounce. The separate
   **Water reflections** switch on the World tab still gates water mirrors.
   Without a bounce, the authored ambient approximates indirect light.
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
  refractions are new world-space rays. Snell refraction and unpolarized
  dielectric Fresnel handle both sides of the interface, including total
  internal reflection. The baked scenery placement flags gate water views.
- The client's sky panorama and roof textures are sampled by ray direction.
  The sun, static scene point lights, and point lights published by GFX owners
  can all cast occlusion rays. Optional indirect rays contribute diffuse
  lighting. The native world passes do not composite on top.

### Light transport and its limits

This is genuine ray-traced visibility in both modes. Software intersects a
compute BVH; Hardware uses Vulkan ray queries. The two modes share the shading
code. They are not two different lighting models.

- Diffuse bounces sample a cosine-weighted hemisphere. They carry linear albedo,
  not a fixed brightness multiplier, and replace the ambient approximation on
  that surface. Baked vertex lighting is not treated as diffuse albedo and
  bounced again. Reflective materials share their energy between a matte lobe
  and an ideal mirror lobe; they don't get full diffuse light plus a mirror.
- The geometric face determines which side can receive shadows or emit a
  secondary ray. Interpolated and water normal-map normals steer shading but do
  not move the geometry or let light shine through a back face.
- Underwater paths lose light exponentially with distance traveled. The
  authored fog range and hue approximate extinction and in-scattering; shallow
  bottoms show through because the underwater path is short, not because a
  shoreline mask reduces the interface reflection. The first water split is
  evaluated on both sides; later interfaces sample one lobe to bound ray counts.

This is still a bounded, low-sample renderer for legacy assets, not a reference
path tracer. The sun and point lights have hard visibility and retain authored
colors/ranges rather than calibrated radiant intensities. The panoramas are not
HDR light probes, reflectance is a scalar from the old material, air fog is
linear, and water is an open height surface rather than a closed volume. There
is no roughness-based specular model, caustic solver, or temporal denoiser.

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
ray-query SPIR-V. The hardware path uses Naga 26 to keep candidate and
committed hits distinct without changing the viewer's wgpu version. Pixel tests
read back *composited* color and depth. They cover stacked and coincident
cutouts, small camera moves, blended sheets, sun and point shadows, and water
visibility gates. Analytic pixel cases check Lambertian sky lighting, baked
colors, diffuse/mirror energy, back-face light rejection, Fresnel both above
and below water, total internal reflection, and distance-based extinction.
The tests verify that unchanged geometry does not rebuild acceleration
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
