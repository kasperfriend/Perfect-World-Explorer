# Lighter performance verification

## Resident acceleration structures (Step 2)

- Near terrain uses resident terrain-block keys, far terrain uses its existing
  patch coordinates, and baked ornaments share stable 128-unit cells. A
  geometry fingerprint covers indexed positions, not texture or packet
  addresses. Assets are ordered by placement ID inside a scenery cell so
  promotion to the native load radius alone does not change the BLAS.
- Posed actor meshes use the world slot, Walker part or Populator spawn, plus
  index topology. The independent effect batch can change size without
  invalidating the actor BLAS. Eviction drops cached trees/structures; bounded
  residency does not keep all previously visited chunks alive.
- Software retains leaf membership and split/escape topology, refits bounds
  bottom-up, and remaps references when packing a new window. Vulkan builds
  posed BLAS with `ALLOW_UPDATE`, updates in place, and retains unmodified
  device BLAS across world revisions. A moved instance or changed BLAS bound
  can require a TLAS **update**. A TLAS **build** is reserved for membership
  changes. Shade-only changes with the same masks and opacity do neither.
- [2026-11-18 Linux run 44392550077](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44392550077/job/135113823843): the Software and CPU Vulkan ray-query pixel tests passed. They probe unchanged builds while panning, tint changes, offsets after reordering, entering/evicted geometry, and a pose that leaves its old bounds and hits at the new location. Standalone BVH tests also checked unchanged leaf references/escapes after refit. These are synthetic checks, not physical GPU timing or real-map validation.

## Resident GPU storage (Step 3)

[2026-11-18 CI run 44393517467](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44393517467)
passed on Windows and Linux. Linux ran `exercise_resource_reuse` in both the
Software BVH and CPU Vulkan ray-query pixel suites. Earlier texels remained
readable after a 256×256 tile append; color and reverse-Z depth also matched
through `27 → 21 → 25 → 27` with accumulation and filtering enabled. After
warm-up the Lighter-owned buffer, binding and acceleration-structure
creation counters did not change. The texture bank is buffer-backed, not a
collection of separate GPU image objects.

`TimingFrame` reports the same counters about once per second in the viewer.
They exclude the swapchain, host post/UI targets, and driver/wgpu internals
such as queue-write staging. Growth may allocate; the 256 MiB pixel storage
budget was split into three retained trace/history/filter capacities in this
run. Reprojection adds a second history buffer within the same total budget.
The run verified reuse, not a performance gain or a physical-GPU run.

## Streaming copy audit (Step 4)

- `host.rs`: move the tree placement list after the native upload, borrow
  the tree mesh while mapping its material IDs, and stitch only referenced
  far vertices. Immutable base texels and terrain masks are sealed separately
  and shared by `Arc`; a stream writes its scenery/animation suffix only.
  Both tracers upload that suffix if their buffer still has the same prefix.
- `scenery.rs`: snapshots hold `Arc<Asset>` and geometry and decoded tiles
  are retained across block crossings. The texture cache returns the
  already-resolved content ID with the texture in one lookup. `Objects::get`
  uses the placement's cached model key instead of recreating it on every
  visit.
- `gpu.rs`: move the LOD vector; borrow grass indices, texture names, map
  path and ornaments; resolve each local scenery texture name once per
  rebuild and pack equal-size atlas entries deterministically. The remaining
  world/scene handle clones share ownership across workers. The raster and
  ray formats still require separate packed vertices/indices and atlas
  uploads; those are not identical copies that can be shared by reference.

`exercise_chunk_scaling` sweeps 16/64/256 synthetic chunks on each tracer at
96×96, measuring median cached CPU packing (5 iterations) and synchronized
frame time (9 warmed draws, fixed targets, no readback within the timings).
The suite checks color/depth and warmed creation counters at each size. A
second test replaces a same-size texel segment and appends within existing
capacity on both tracers. [2026-11-18 Linux run 44395084914](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44395084914/job/135122736000)
passed both ray suites, the streamed viewer, terrain, and production shader
checks. Times are milliseconds on that runner:

| Chunks | Triangles | Software pack | Software frame | CPU Vulkan pack | CPU Vulkan frame |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 16 | 512 | 0.421 | 1.274 | 0.406 | 1.004 |
| 64 | 2,048 | 1.653 | 1.384 | 1.578 | 1.147 |
| 256 | 8,192 | 6.661 | 1.434 | 6.305 | 1.065 |

The test keeps much of the larger scene off-screen. Cached packing grew with
geometry count; the warmed frame changed much less at this fixed ray count.
These are not map-specific before/after or physical-GPU results.

## Interactive reprojection (Step 5)

[2026-11-18 Linux run 44396201690](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44396201690/job/135126621971)
passed on Software and CPU Vulkan ray queries. The tests check the 1 spp / one
bounce / 50% defaults, the exponential 10% new-sample weight after ten samples,
and unchanged current-frame depth. A four-pixel shader fixture shifts history
by one texel, accepts two neighbors with matching guides, rejects a mismatched
depth and rejects a viewport exit. Camera cuts reset history, including gradual
drift beyond one ray pixel from the last reset. The lighting fixture also
checks camera movement out of a penumbra, changes to an off-camera caster and
light-only edits. The two ping-pong history buffers and both directions of
postprocessor bindings are retained after warm-up. These are synthetic tests.

## Ranked scenery manifest (Step 6)

[2026-11-18 Linux run 44397497506](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44397497506/job/135131211562)
passed Software, CPU Vulkan and the streamed-viewer tests. `scenery_manifest`
unit tests check required-placement deduplication, cutoff, receiver distance,
static-light ranking, and learned bounds crossing owner cells after teleports.
The archive-backed join fixture puts one facade on each side of x=128. It
checks the exact join and both sides for color and depth, asserts four total
triangles and two stable owner IDs, and verifies local/extra promotion does
not rebuild the same BLAS. A teleport evicts both, and the return restores the
same pixels. The fixture covers synthetic assets, not full retail map captures.

Steps 5 and 6 also passed the Windows standalone build and portable upload,
re-download and layout audit in those same runs. This packaging verification
is not a Windows ray-rendering or performance test.

## Interactive Hardware RT pass

This pass keeps ordinary primary and secondary queries against actual
world-space terrain, scenery, water, vegetation and actors. The default
Native launch, explicit same-map reload, and Software fallback route are
unchanged; Hardware does not silently become Software. Reference bypasses
the diffuse distance cap and the approximate grass bend. It still benefits
from exact batching, uploads and traversal changes.

| # | Work removed | Scope / trade-off |
|---:|---|---|
| 1 | **No duplicate CPU geometry BVHs in Vulkan packets** | Hardware builds its triangle BLAS/TLAS on the device. CPU packing skips software geometry bounds, checks each position once and retains the point-light BVH, materials, indices and transforms. Software still builds its stackless geometry trees. |
| 2 | **Dirty-range moving geometry and instance uploads** | Compare with the last submitted bytes, copy only changed ranges, and coalesce highly fragmented updates. Growth still copies the complete payload; retain Vulkan visibility barriers and fence ownership. `moving_copy_bytes` and `instance_copy_bytes` identify the sampled transfer costs. |
| 3 | **Instance identical rigid model parts across owners** | Matching bind-space vertices/topology/materials share one packet mesh. Different material/visibility records can still share the same native BLAS when shape matches. Weighted skins remain truly posed. Static BMD chunk reuse is existing behavior, not a new optimization. |
| 4 | **Animate grass in instance transforms** | Independently scaled and yawed two-height cards can share one rest BLAS. The bounded affine shortcut avoids per-blade vertex copies and rebuild/refit work; retain the exact path for larger errors. |
| 5 | **Split tree canopy geometry by ray purpose** | Share stem/frond, camera leaf and secondary proxy templates per type/LOD. Keep the camera and proxy triangles out of each other's queries; omit the unused level at the endpoints of the Bayer transition. Separate masks can add TLAS entries where both card types exist. |
| 6 | **Terminate shadow traversal on an accepted blocker** | Classify each distinct material once. Only clear legs walk tinted leaf/cloud/thin-glass layers; emitter endpoint and two-leg underwater visibility remain real queries. Empty sheet passes are skipped; GFX without overlays stops at the solid/depth-write limit. |
| 7 | **Scalar alpha filtering for ray candidates** | Filter alpha directly, not unpacked RGB, keeping the same threshold, area-mip, anisotropic footprint, wrap/clamp and alpha holes. |
| 8 | **Reuse projected UV footprints across texture stages** | Compute geometry/UV derivatives once for albedo, normal and face stages or for both animated wave maps; different texture sizes still choose their own mip and taps. |
| 9 | **Check terrain masks before fetching layer colors** | Composite top-down using the equivalent weights, skipping zero-weight layers and everything below an opaque mask. Keep baked light and authored paint. |
| 10 | **Limit only dry diffuse bounce distance** | Default 256 world units, configurable up to full range. More distant diffuse paths use sky instead of distant geometry. Primary, shadow and specular/water rays keep their range. |

### Visual regressions covered

- A foliage fragment that passed coverage no longer acts like glass. A
  genuinely transparent texel still shows sky at sky depth, and leaves still
  tint direct shadows.
- The shared frame snapshot now keeps the material light vector separate
  from celestial billboard data. By default, Lighter uses a stable map
  palette evaluated at the current hour; translating the camera into another
  regional record does not dim global surface/sky light. **Follow local
  region lighting** restores intentional local recoloring. Ray air fog uses
  its own coefficients rather than swapping with the Native camera medium.
- Coplanar wet exports no longer stop extinction at an ownership edge.
  The fixtures compare split and unsplit pools through the same geometry,
  including a camera exactly at the join, bitmap-backed cells and sun rays
  crossing the seam. They also check that a real dry gap or another water
  height does not become wet. A separate fixture puts the stitched near/far
  terrain join under a single water owner and checks a known extinction and
  unchanged reverse-Z on both sides and exactly at the edge.

### Measurements and limits

`lighter_vulkan_interop_on_cpu` reports packet bytes, CPU pack time and
warmed synchronized frame time for the matched 96×96 static sweep. At
8,192 triangles the Hardware packet is **702,648 bytes** rather than the
**879,252 bytes** of the corresponding packet with Software BVHs. The
spread of CPU Vulkan frame times on this small sweep is not an RTX 3080 FPS
estimate. It has no forest, animated models, or water in the timed frames.

The affine wind fixture keeps **1 BLAS template for 64 independently bent
placements**, versus 64 bent copies plus the rest template on the old path.
The sparse upload fixture changes two 1 KiB pages in a 1 MiB payload and
emits **2 KiB**, not 1 MiB, of copies. Entirely changed payloads still need
copies. Shader and traversal costs depend on the scene mix, driver, ray
resolution and enabled effects. For physical A/B, use the same map, camera
and hour, and hold resolution, ray settings, view distance and weather fixed.
Use the capture tooling in [retail validation](retail-matrix.md) to retain
GPU build/traversal samples and wall times separately. The synthetic
submerged check is not a reproduction of the reported retail seam; retail
screenshots and the physical GPU comparison are still needed.
