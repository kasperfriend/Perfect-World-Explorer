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
