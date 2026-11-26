# Hi-Z visibility and GPU performance audit

**Scope:** PR #58, branch `perf/gpu-full-map-effects-100-more`. This is a code-based audit of the renderer and its pinned API surface as of 2026-11-26. It does not change the target branch or merge the PR.

## Renderer facts verified in source

- `Cargo.toml` pins the `wgpu 0.19` API line; `Cargo.lock` resolves `wgpu 0.19.4`.
- The scene depth attachment is `Depth32Float`, created with one sample and `RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC`. Main opaque depth clears to **0.0** and writes with `Greater`; scenery uses `GreaterEqual`. The camera matrix is a reversed-depth perspective matrix: its near plane maps toward 1 and far plane toward 0 in WebGPU's [0, 1] depth range.
- There is no MSAA depth resolve in this path: scene, reflection and refraction depth targets are single-sampled.
- `src/hiz.rs` builds an `R32Float` minimum-reduction pyramid. Clear depth (0), non-finite values and out-of-range values become zero, so uncertainty propagates toward coarser mips instead of being lost. Odd right/bottom edges are included in the final destination texel. The tree shader selects a mip from projected rectangle size, pads its screen rectangle, samples every overlapped cell, and culls only when every cell is strictly in front of the candidate by a depth bias.
- Candidate tree bounds are expanded using authored tree height; invalid projections, near-plane intersections, uncertain depth and out-of-range clips are fail-open. The production path uses GPU compute/indirect args and does not read results back to the CPU. The readback in `src/bin/hiz-gpu-check.rs` is test-only.
- Main, reflected, and each refraction view invoke the opaque renderer with their own depth target, projection matrix, clip plane and optional scissor. They reuse the pyramid storage sequentially, rebuilding it from the active view. Minimap/full-map explicitly pass `use_hiz = false`.
- The depth copy used for water-edge reconstruction is intentionally taken immediately after terrain; it remains terrain-only even after the opaque scenery pass.
- SDR curtains use a blended pipeline with depth writes disabled. Water, particles, and dynamic receiving-model effects are scheduled separately and are not valid static occluders.

## Hi-Z integration change in this PR

Previously, `draw_opaque` generated the hierarchy immediately after terrain, before drawing static opaque scenery. That meant mountains/buildings/world objects could not contribute depth when the tree compute shader tested its candidates.

The revised order is:

1. Render sky/terrain and clear the current view's depth.
2. Preserve the terrain-only depth copy when water needs it.
3. Render static opaque scenery (local and whole-world pages) using the same view frustum, water clip and scissor.
4. Build this view's Hi-Z pyramid and run the GPU tree visibility/LOD compaction in the same command buffer.
5. Draw far trees and grass, then draw blended SDR scenery.

This makes terrain plus depth-writing opaque scenery valid occluders for the far-tree candidates without drawing the static scenery twice. Alpha-cutout fragments remain governed by the existing opaque shader's discard behavior. Transparent effects and water are excluded. The existing frustum/LOD fallback remains when Hi-Z allocation is unavailable; minimap remains disabled.

**Coverage boundary:** Hi-Z currently rejects far-tree placements. The world-page and model paths still use their existing CPU-side frustum/clip/screen-size visibility logic; this change does not claim GPU Hi-Z rejection for every arbitrary world-object draw. Those are the next worthwhile candidates, but need separate per-page command compaction and material-safe batching.

## Additional performance technologies

### A. GPU-driven rendering and indirect submission

**Already present**
- `src/far_trees_gpu.rs` uploads immutable tree placements/groups once, computes per-frame visibility and LOD on the GPU, atomically compacts instances, and writes indirect instance counts. It uses multi-draw indexed indirect when enabled and a per-batch indirect fallback otherwise.
- `src/terrain_gpu.rs` uploads per-view visible terrain runs into reusable indirect argument buffers and uses multi-draw when supported. CPU still computes the run list and writes the arguments.
- `src/world_objects.rs` stores pages, placement AABBs and 32-command chunks. Large static world lists can now use a compute culler that writes one zero/one-instance indirect record per stable command. It preserves global page order and page-local MDI runs; small/all-inside workloads and adapters without multi-draw retain CPU page/chunk/placement culling.
- Near-world vertex/index buffers and several indirect scratch buffers are capacity-reused rather than recreated for every small update.

**Implemented in this PR follow-up:** large whole-world command lists (64+ commands) now have an optional GPU compute culler when `MULTI_DRAW_INDIRECT` is available and the workload is not covered by the all-inside fast path. Immutable per-view bounds and index spans are uploaded once and retained; each view refreshes its own frustum/clip/screen-size constants every frame, then writes zero/one instance counts into persistent indirect records. Opaque and SDR lists have separate scratch state, page/atlas bind transitions and global command ordering are retained, and CPU hierarchy culling remains the fallback for small workloads, unsupported adapters and all-inside opaque views. The culler deliberately performs current-view frustum/clip/size checks only; it does not reuse stale depth or claim static-world Hi-Z rejection.

**Next step:** profile the new compute path against the CPU hierarchy on large worlds, then consider GPU Hi-Z rejection for static world commands only if a safe current-view depth source exists before their submission. Keep the CPU path for unsupported adapters and diagnostic views.

The pinned `wgpu 0.19.4` source already defines `MULTI_DRAW_INDIRECT_COUNT`, but this project currently requests `MULTI_DRAW_INDIRECT` and `INDIRECT_FIRST_INSTANCE`, not the count feature. The count variant is documented for DX12 and Vulkan 1.2 (or its extension), so it must be feature-gated rather than assumed. A count-based path is most useful after a compute pass actually compacts visible world commands; enabling it alone will not reduce the current CPU culling cost.

### B. Clustered / meshlet visibility

The current asset path produces page/placement ranges and AABBs, not meshlets. There is no mesh-shader/task-shader feature in the pinned `wgpu 0.19.4` feature enum. A renderer upgrade solely for mesh shaders would be a disruptive dependency/backend change.

A compatible intermediate step is to divide large static placements into small indexed clusters during CPU asset conversion, retain per-cluster AABBs (and material/alpha flags), then compute-cull those clusters and write indirect commands. Start with coarse placement clusters where the current AABBs already exist; only build finer clusters for large meshes shown by profiling to dominate vertex work. Cone/backface rejection must be disabled for two-sided or alpha-sensitive materials unless their semantics are known.

### C. Temporal visibility reuse

The tree path currently rebuilds visibility for every Hi-Z-enabled view and does not reuse a previous-frame occluded result. This is conservative and avoids stale depth causing newly exposed geometry to disappear after a camera turn, teleport, water-plane change or streaming update.

For future static world chunks, cache **visible** results freely as a draw-safe fallback; do not reuse an old **occluded** bit as proof of current occlusion. Any more aggressive reuse needs a fresh current-view test or a conservative motion/geometry bound, plus invalidation on camera cuts, view/projection changes, water clip/scissor changes, world-page replacement and dynamic-object movement.

### D. Rendering and bandwidth

- The pass boundary before Hi-Z is required: the depth attachment must be finished before the compute shader samples it. The revised order reuses the color+depth scenery pass; it does not add a second full scenery depth prepass. Avoid introducing a duplicate depth prepass without GPU timestamp evidence that saved fragment overdraw exceeds its extra vertex/alpha-test work.
- The hierarchy is rebuilt per view because the depth contents and projection differ. This costs compute bandwidth for each reflection/refraction view; scissoring limits rendered geometry, while clear pixels intentionally reduce culling rather than create false positives.
- Keep blended water/effects/SDR out of the occluder set. The existing post-water effect scheduling and no-depth-write SDR pipeline preserve this separation.
- Terrain upload already attempts native BC texture uploads when supported and otherwise retains decoded source resolution/mips. Keep compression capability-gated; do not trade away texture quality or authored mips for a synthetic benchmark.
- Add GPU timestamp measurements around terrain, opaque scenery, hierarchy generation, tree compute and water subviews before tuning mip resolution, dispatch granularity or a depth prepass. Existing frame-stage measurements should not be mistaken for GPU execution time.

### E. Asset loading and streaming

- `src/direct_storage.rs` is an optional Windows DirectStorage **range-read** accelerator with ordinary positioned-I/O fallback. Its FFI ranges target CPU memory; `src/pck.rs` then performs zlib inflation through `flate2` on the CPU. This is not GPU decompression.
- `src/world_page_cache.rs` persists converted world pages with format/version identity and SHA-256 verification, reads batches sequentially, and parallelizes page deserialization across CPU workers. This is already a strong warm-start cache for expensive legacy conversion.
- `src/client_region_stream.rs` uses a bounded FIFO admission budget for newly exposed scene blocks. `src/terrain_gpu.rs` parallelizes several decode/packing jobs and batches GPU uploads.
- DirectStorage's public samples demonstrate GPU decompression paths for supported compressed payloads (including GDeflate); the existing PCK payloads use zlib, so enabling a GPU decompressor does not make them compatible. A worthwhile future experiment would be an offline conversion/cache format with independently compressed chunks, explicit checksums and a GPU-decompressible codec, while keeping the current PCK reader and CPU zlib fallback authoritative. Do not replace the current cache until cold/warm load timings show a benefit.

### F. Modern hardware features in the pinned API

The API check below uses the upstream `v0.19.4` source, matching this repository's lockfile:

- `MULTI_DRAW_INDIRECT` is already requested when supported by the adapter. The upstream feature declaration lists DX12, Vulkan and Metal Apple3+/Mac1+ (Metal may emulate it).
- `MULTI_DRAW_INDIRECT_COUNT` exists in 0.19.4 but is not requested by this app; upstream lists DX12 and Vulkan 1.2 / `VK_KHR_draw_indirect_count`.
- `TEXTURE_BINDING_ARRAY` and `BUFFER_BINDING_ARRAY` exist as native-only options. Texture arrays list DX12/Vulkan/Metal; buffer arrays list DX12/Vulkan. These can reduce bind-group churn only after assets are grouped into stable, bounded descriptor sets and all fallbacks are retained.
- Non-uniform sampled-texture/storage-buffer array indexing exists, but backend support is narrower and adapter-feature dependent. It should be a capability tier, not a baseline assumption.
- Subgroup operations and mesh/task shaders are not exposed in this pinned 0.19.4 feature API. Do not write WGSL that assumes them. Re-evaluate these only as part of a tested `wgpu` upgrade.
- The adapter's supported feature set is intersected with the application's request during device creation; all optional features need a clean fallback.

References: [wgpu 0.19.4 feature declarations](https://github.com/gfx-rs/wgpu/blob/v0.19.4/wgpu-types/src/lib.rs), [DirectStorage samples and GPU decompression overview](https://github.com/microsoft/DirectStorage).

## Recommended order after this PR's Hi-Z pass

1. Profile the new per-view Hi-Z cost and far-tree rejection counts on a large world with several water views.
2. Add GPU coarse-culling for static world-page placements/chunks, gated by compute + indirect capabilities; preserve the CPU fallback.
3. Add finer meshlet-like indexed clusters only for measured high-cost large placements.
4. Rework PCK payloads for GPU decompression only if a compatible independent-chunk codec and measurable CPU/decompression bottleneck justify it.
5. Consider temporal reuse only for conservative positive visibility; never allow stale negative visibility to hide newly exposed geometry.
