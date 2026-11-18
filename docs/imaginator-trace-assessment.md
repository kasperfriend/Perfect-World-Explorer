# Full-trace status and temporal reconstruction requirements

This describes the implementation, not a claim of full world parity.

* Full traced image replaces the scene with radiance from primary and secondary
  rays. CPU uses the module BVH; Windows hardware mode attempts DXR inline
  RayQuery. A failed or pending trace presents a blank scene, not a raster fallback.
  The usual world raster pass and its temporal upscaling and frame generation are
  bypassed in this mode. The UI and final fullscreen presentation still use
  ordinary raster/compute operations; these are not world-geometry rendering.
* The captured scene is incomplete: terrain is captured around the initial camera
  with a triangle budget; scenery arrives in pages, also under that budget;
  terrain colour is a bounded baked preview; trees are static capture. Skinned
  actors, grass, clouds, weather and effect geometry are not captured. Streamed
  texture atlases have a 256 MiB limit; a missing atlas falls back to flat colour.
* CPU ray traversal uses foliage and captured scenery alpha cutouts. DXR builds
  opaque geometry and uses constant per-triangle material colours, so foliage
  cutouts and per-hit texture detail are not represented. Both paths are ray
  tracing, but neither currently reproduces the entire existing world.
* The traced image is bilinearly scaled by the compositor. FSR/XeSS/DLSS temporal
  upscalers and their frame generators were gated because their existing inputs
  are raster depth and motion; those buffers are stale when the world raster is
  skipped. Restoring them correctly needs traced device-depth, motion vectors
  (including animated objects and disocclusions), compatible jitter and history
  resets, and a GPU integration for each provider. This is not supplied by
  upscaling the final image alone.
* Full-trace alpha now preserves primary ray distance (mean over hits; -1 for
  sky) for both CPU and DXR. It is **not** projected device-depth and is not yet
  connected to an upscaler or frame generator. Camera and object motion remain
  unimplemented for the traced scene.

Hardware shader compilation and CI do not establish runtime behavior on an RTX
GPU. Visual/world coverage and DXR execution still require a runtime check.
