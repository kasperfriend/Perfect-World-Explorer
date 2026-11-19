# Ray vegetation (Step 12)

The ray feed takes the same loaded SpeedTree LOD records and frame inputs as
native. Bark/frond meshes keep the decoded triangles and alpha references.
Leaf cards use the SpeedTree corner/rotation data and the native 16-second
sample loop, including its wrap blend and the current wind-strength input.
Fixed-world leaves are shared across purposes. A camera-facing leaf has one
view card and a separate fixed, sky-facing card for secondary rays. That
proxy is an approximation: it sees both sides, shares the same texture and
rocking, and doesn't rotate with the camera. It isn't an extra leaf for the
same ray. The native SpeedTree code does not supply an optical transmission
coefficient; Lighter uses a bounded RGB transmission based on the leaf
pigment. Cutout holes pass light without tint, and trunks remain blockers. An
accepted leaf occludes the background in camera and other image paths, rather
than forwarding light from the sky straight through the leaf.

LOD selection uses the native 3D distance, LOD-0 height and detail scale.
Both levels in a transition use complementary 4×4 thresholds; a missing or
budgeted partner keeps full coverage at the retained level. Trees share stem, camera-card and secondary-proxy templates by type/LOD.
The acceleration-instance masks keep camera cards out of secondary rays and
proxies out of primary rays. Camera motion only changes the templates that
track the view; bark/fronds are not copied into every animated proxy build.
Near either end of a blend, the level that can't win any Bayer threshold is
not submitted. A change in selected LOD membership can
still rebuild a TLAS. The dither is in ray-pixel space, so reduced ray
resolution is not the same as a native full-resolution dissolve.

Grass uses the native instance yaw, scale, area center, 80-slot world-space
wind history and per-vertex bend weights. Fade is interpolated from unbent
vertices for the main view; changing the viewer's range never makes an
otherwise-present off-camera grass shadow disappear. Grass uses the same
ray area lightmap and authored lower mip chains as before. A blade whose bend weights are affine in height reuses one rest mesh and
uses a sheared instance transform per placement, preserving original
normals. Interactive Hardware also does this for complex blades when the
maximum predicted vertex error is at most 0.02 world units. Otherwise the
existing exact bend path is used, with the 4 MiB extra geometry budget;
further blades keep rigid coverage and emit a diagnostic. The budget is
an admission estimate, not a measured VRAM cap.

## Validation scope (2026-11-18)

Commit `0d20f8e`: Software BVH, CPU Vulkan ray queries and the streamed-world
pixel suite passed locally on llvmpipe / Mesa 25.0.7, with
`GALLIVM_PERF=nopt` to avoid long cold LLVM optimization. This is not a
physical ray-query GPU timing or canopy comparison.

- Unit fixtures check the sampled phase/loop, unchanged secondary card when
  the view rotates, moving secondary corners with wind, and world-space grass
  bend after yaw and scaling. Naga validates both ray shaders.
- Ray fixtures cover noon cutout holes, two-sided tinted leaf attenuation, a
  primary-only card that must not shadow, 8/8 complementary LOD coverage at
  coincident depth, and a grass blade crossing its native fade threshold.
- The two previous retail captures (`a01`/`a02`) have no authored vegetation.
  The matching v12 `world` EC records contain 3,938 tree placements across 59
  tree types and 2,181 distinct grass area references across 220 grass types.
  Those are source counts, **not** loaded ray coverage or a rendered forest.
  A complete client and matching SpeedTree cache are needed for the forest
  comparison, including visual checks of canopy silhouettes and dense wind
  budgets on a real GPU.
