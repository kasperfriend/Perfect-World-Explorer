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

## Transport, hybrid visibility and noise follow-up

Native is still the first renderer at launch. **Reload World** also stages
changes to the experimental raster-primary pipeline, so its initial compile is
in the watched replacement, not the working window. Software keeps full
primary rays. Hardware keeps full rays by default and uses them for Reference,
multiple samples, or failed optional raster attachment admission.

| Checklist area | Status and scope |
|---|---|
| Shadow traversal | **Already present:** early-out on the first accepted opaque blocker, with a separate ordered colored-transmission query. Hardware uses inline ray queries, not a closest-hit shader. **Added:** skip unnecessary hit decoding on blocker commits; reuse binary hard-sun visibility only for a stationary, matching one-sample snapshot. |
| Ray count and noise | **Added:** sample one Fresnel lobe at every dielectric, with tint and radiance `eta²` on transmission and probability compensation for roulette from depth two. Keep deterministic first split as the control. Soft-sun primary samples use a shifted Sobol sequence; stochastic radiance does not jitter single-sample primary depth or cutout coverage. |
| Direct proposals | **Already present:** back-face rejection, range/influence culling, and Reference sky and power-weighted mesh proposals. **Added:** resample one of the eligible local proposals with inverse-PMF weighting when shadows and more than four local sources are active. No temporal/spatial light reservoirs. |
| Primary visibility | **Experimental:** packet-driven reverse-Z raster pass writes instance, triangle, hit distance and barycentrics. Same material acceptance, including cutouts, and the same posed vertices/transforms. Frustum culling and contiguous batches only affect the raster pass, not secondary or shadow BVHs. Sheets still use ordered ray visibility. Raster coverage at subpixel edges and exactly coplanar solid ties is not guaranteed to match a ray query. |
| History and filtering | **Added:** 28 B/pixel instead of 32 B (12.5% less at equal extent), FP32 depth, packed guides, half RGB/log variance, owner/depth checks, bounded variance-adaptive history. Reject moving-path, water-wave and scrolling dependencies selectively; reset for changed light/decal records or scene revision. Two local à-trous passes on a fresh image; three wider passes when usable history exists. Reuse dead trace output and one scratch, with no extra full-resolution history. |
| Software BVH | **Added:** 2–4 child stackless layout with 12-bin SAH for larger partitions, and sparse leaf/ancestor refits with a dense fallback. No change to the WGSL escape ABI. Immediate CPU partition cost can increase; no hardware BLAS layout claim. |
| Sky and medium | **Alternative:** bilinearly reuse the precomputed sky table only on interactive diffuse misses; primary, mirrors and refraction retain the authored filtered art. No screen-space GI cache. Water keeps exact per-leg exponential extinction (with a zero-length fast path), not a new fog LUT. |
| Texture and streaming work | **Existing/partial:** authored mips, ray-side packed mask bytes and budgets, rank/reuse of decoded scenery, dirty upload ranges and retired ASes. Ray material texels are still decoded into a storage bank; no BC1/3/5/7 hardware-sampled ray texture array. No new residency cap or loader-thread packet pacing here: mask admission and packet assembly still happen during install. |
| Pipeline startup | **Added:** validate and compact individual Vulkan entry points; best-effort bounded driver cache keyed by adapter/driver UUID. No guaranteed cache hit or physical cold-start reduction. |
| Upscaling / SDKs | **Alternative:** built-in guide-aware composition and opt-in 50–100% adaptive ray scale with hysteresis. The bundled FidelityFX integration is DX12, and the NVIDIA Ray Reconstruction headers do not supply an integrated Vulkan runtime path. Neither FSR nor DLSS RR handles ray-world reconstruction here. |
| Measurement | **Added:** nonblocking AS/raster/trace, reprojection, denoise and upscale/compose timing; normal-present 1% low; tracked ray allocations and optional driver heap usage. See [the limits and 7 GB warning](gpu-budget.md). A physical RTX 3080 FPS, tail latency or whole-viewer VRAM target is not verified. |

### Reproduce the matched synthetic capture

Use the same Vulkan ICD throughout. The Linux-only test explicitly admits a
CPU ray-query adapter for the fixture, which production still rejects for
Hardware mode. This is not a retail map or a GPU performance claim.

```sh
WGPU_BACKEND=vulkan GALLIVM_PERF=nopt PW_SMALL_TREE_ATLAS=1 \
PW_PREVIEW_WEATHER=Clear PW_PREVIEW_TIMED_FRAMES=9 \
PW_SAVE_LIGHTER_MATCHED=target/lighter-matched-fixture \
  cargo test --locked --bin perfect-world-explorer lighter_matched_fixture_capture -- --ignored --nocapture
```

For actual GPU measurements, use `--profile physical --hybrid-ab` with the
[retail capture matrix](retail-matrix.md). Inspect the matched color and depth
controls first; render-stage intervals may vary by driver. Compare Hybrid's
**raster plus traversal** cost and complete frame time, not the traversal
marker alone. Keep frame-time tail and memory measurements separate from
headless image readback.

### Matched local results

Pinned `19afdaf` (capture-only test scaffolding) and this follow-up ran on
**llvmpipe, Mesa 25.0.7 / LLVM 19.1.7**, with `GALLIVM_PERF=nopt`. All
captures used the same synthetic streamed-map camera, noon/clear light,
four 16 ms warm-ups, a frozen clock, 960×720 output and 480×360 internal
rays. Each timing is from nine warmed draws **including synchronized
readback**. Adaptive scale and progressive sampling were off. The follow-up
retains the default Single-path and filter changes, so the before/after rows
measure the combined change, not just the addition of raster visibility.

| Path | `19afdaf` p50 / p95 (ms) | Follow-up p50 / p95 (ms) |
|---|---:|---:|
| Native | 153.2 / 243.9 | 146.1 / 160.9 |
| Software BVH | 3242.7 / 3369.4 | 3230.5 / 3324.4 |
| Full Vulkan rays | 2184.1 / 2314.8 | 2229.0 / 2299.0 |
| Hybrid Vulkan | — | 2206.3 / 2315.6 |

Native's decoded RGBA8 hash was identical. Full rays vs Hybrid averaged
**0.00020 RGB8 levels** of difference over the frame (119 of 691,200 pixels
changed; maximum channel delta 38). The subpixel raster parity fixture checks
sampled interiors, not exact silhouette coverage. It passed at 19², 25² and
43² with a maximum reverse-Z error of `3e-7`, then exercised water ownership,
terrain joins, ordered sheets, off-screen posed shadows and simulated raster
allocation fallback/retry.

Hybrid's warmed combined raw GPU-stage median was **2094.8 ms** vs **2118.1
ms** for full rays on this CPU driver. The driver attributed nearly all of
Hybrid's combined time to the interval ending at the raster marker, so this
is not evidence of near-zero traversal cost. The wall-time difference is
small, p95 is slightly worse, and the raster attachments cost an additional
3,456,000 tracked bytes here. There is no clear win that warrants enabling
Hybrid by default. At equal ray extent, full-ray Vulkan input/AS/output
allocations went from 6,040,440 to 5,349,240 B, exactly the reduction in the
raw output stride. The wgpu resources and total GPU residency are separate.

The 96², zero-bounce, filter-off full-ray chunk sweep was faster on this ICD:
43.5→39.5 ms (512 triangles), 45.3→41.6 ms (2,048), and 50.8→42.6 ms (8,192).
The larger streamed control did **not** show an end-to-end full-ray speedup.
Neither run establishes RTX 3080 performance.

In the soft-sun fixture both tracers gave 0.382 penumbra visibility (analytic
0.388) and reduced spatial variance from 0.2456 to 0.0020 without brightening
the deep shadow. Separate unfiltered 65² tests matched the split/full-light
means for Fresnel, roulette and local-light resampling within 0.0025 per
linear RGB channel; they also checked unchanged primary depth. The stationary
sun fixture verifies cache bits survive filtering, a displaced shadow does
not reuse old history, unaffected pixels keep history, and colored glass
still traces RGB transmittance.
