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
budget is split into three retained trace/history/filter capacities. This
verified reuse, not a performance gain or a physical-GPU run.

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
capacity on both tracers. CI results pending; this is a synthetic scaling
probe, not a map-specific before/after benchmark.
