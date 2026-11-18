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
