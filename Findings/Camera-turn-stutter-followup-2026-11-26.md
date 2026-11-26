# Camera-turn stutter follow-up (2026-11-26)

## Source-level findings

This follow-up is limited to the full-world renderer on PR #58. It was prompted by
heavy stutter when rotating the camera with the whole-world preload resident.

1. The production whole-world GPU culler wrote `instance_count = 0` for
   invisible commands, but retained the full original indirect command array and
   every original page run. The draw pass therefore still submitted off-screen
   page runs on every view. It also dispatched a second compute pass for the SDR
   list even when no SDR content was present. The GPU path did not compact the
   command list, so it added compute work without removing the driver's command
   submission work.
2. The far-tree Hi-Z pass was enabled for main, reflection, and refraction views.
   Every such view rebuilt the full depth pyramid and ran the tree occlusion
   shader over every resident placement. Reflection/refraction are already
   restricted to a conservative water-surface scissor, making a full Hi-Z
   hierarchy and per-tree projected-rectangle sampling poor value there.
3. The main-view tree occlusion shader selected a mip whose projected rectangle
   could still cover up to 4x4 Hi-Z cells. That allowed as many as 16 texture
   loads for a single tree candidate.

## Changes

- Production static-world drawing now uses the existing CPU hierarchical
  page/chunk/placement culler, which builds indirect records only for the
  commands it actually keeps. The experimental GPU culling kernel remains
  covered by its software-Vulkan regression test, but is not used by the
  production renderer until it performs actual compaction.
- Reflection/refraction views keep their conservative frustum, water clip,
  and LOD filtering, but no longer build a Hi-Z pyramid or run per-tree
  Hi-Z sampling. Main view Hi-Z remains enabled.
- Main-view Hi-Z sampling now climbs until the projected rectangle is covered by
  at most 2x2 minimum-pyramid cells (at most four loads). Because each selected
  coarse cell stores the minimum depth of its complete source footprint, this
  is conservative: larger footprints can retain an actually hidden tree, but
  cannot turn an uncovered hole into occlusion evidence.
- Far-tree candidates are now prefiltered by 256-unit XZ cells. Each cell's
  AABB includes the authored tree sphere; a cell is uploaded only when that
  conservative bound intersects the view frustum, optional water clip plane,
  and optional resident-world bounds. The GPU still tests each selected tree
  individually, including its exact sphere/frustum and Hi-Z logic.
- Added a regression covering a clear-depth hole inside a rectangle whose
  coverage is tested at a coarser mip.

## Validation boundary

These changes are based on renderer/code-path evidence and conservative CPU
reference tests. A physical-GPU frame-time capture is not available from this
repository session, so no numerical FPS or frame-time gain is claimed. CI and
the software-Vulkan rendering regressions are the validation gate for this pass.

## Receiving-model submission scan

The main/reflection/refraction update path also walked every ECM placement for
each view before drawing. Added a second XZ cell index over world models using
the same 128-unit conservative sphere radius already used by the exact model
frustum test. Each view builds a reusable, source-order-sorted list of only
cells intersecting its frustum and resident bounds; exact per-model residency
and sphere tests still run before drawing. The opaque/alpha submission and alpha
sort now iterate the visible index list instead of the full model slot array.
Pose ticking and hook resolution are intentionally unchanged because attached
effect roots depend on current parent poses.


## Additional frame-path reductions

- The production main-view Hi-Z now starts at half render resolution. Its first
  mip is the minimum of every 2x2 scene-depth footprint (including odd trailing
  rows/columns), so any clear pixel propagates as zero and can only make an
  occluded tree visible. This reduces hierarchy depth-copy/reduction traffic
  and storage by roughly 4x while retaining conservative occlusion.
- Hi-Z mip bind groups are created once with the depth target and reused until
  resize; the previous hot path allocated a bind group for every mip on every
  rendered main view.
- The near/far terrain visibility walk now runs inline instead of launching two
  short-lived scoped threads on every frame. The work is a bounded walk over
  already-resident span arrays, and avoiding thread creation removes needless
  scheduler/context-switch variability from camera turns.
- The lavapipe Hi-Z regression now checks full- and half-resolution hierarchies,
  including odd-edge clear propagation through the real WGSL downsample shader
  and the cached-binding path.

A physical-GPU frame-time capture at the reported 3440x1369 / RTX 3060
configuration is still required to measure the actual FPS gain. CI validates
correctness, not a specific runtime frame rate.

## Static world-model pose work

Most map ECM placements are static. The prior pose update nevertheless scanned
all of them and created up to 16 scoped operating-system threads every frame,
then discovered that their bind-pose sample had not changed. Static props now
receive one bind-pose evaluation while resident; only actual action-playback
slots are visited by the per-frame clock/pose update. Static slots are queued
again if a residency-window change evicts and later re-admits them. Pose
advancement and hook resolution still happen before the main view, so
hook-attached GFX keeps the same frame-consistent parent pose.


- Hook-attached GFX roots whose parent world model has no active action now cache
  the first validated bind-pose hook matrix. Residency is still checked every
  frame, so cutoff eviction still detaches the root and re-entry restores the
  same parent; animated parents continue to resolve their hook from the current
  pose. The owner attachment setter is also idempotent, avoiding repeated writes
  when a cached matrix is unchanged.


- Retained world-model constant updates now derive each mesh's material/frame
  payload once, cache those payloads in a per-Draw scratch vector, then upload
  them in a second pass. Previously every visible mesh performed all material
  lookup and constant derivation twice per frame (once for validation and again
  for writing); this keeps the all-or-nothing validation-before-upload contract
  while removing duplicate work and steady-state allocations.


- Alpha mesh ordering is now reused across rotation-only camera frames when the
  eye position and main-view visible model-slot list are identical. Its sort
  distance depends on the eye's position, not yaw/pitch; a changed eye or a
  changed visible set still rebuilds the exact same order from transformed
  initial bounds. This removes an unnecessary full alpha-candidate walk and
  sort while turning in place without changing transparency output.
