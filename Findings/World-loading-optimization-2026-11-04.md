# World loading and rendering optimization — 4 November 2026

## Scope and guarantees

This change targets avoidable allocations and repeated work, not a new low-quality preset. It keeps existing source resolution, authored terrain mip chains, scene content, animation clocks, material lighting, and far-mesh triangles. The intentional appearance changes are terrain color wrapping, crack repairs, and sun-glow alignment.

**No client assets or 6 GB physical laptop were available for an end-to-end measurement in this session.** Consequently, this is not a measured claim that the user's 5.6 GB startup now fits a particular RAM budget, nor a promise of identical pixels on every driver or support for every PC. BC hardware decoding can differ slightly in rounding from the software decoder. The visible fixes still need comparison against the user's affected locations/screenshots.

## Loading and memory

- Terrain color sources retain their validated DDS payloads rather than eagerly decoding every authored mip of every world texture to RGBA. Non-BC adapters and unusual source layouts decode **one texture at a time** during upload. CPU preview sampling memoizes sources it actually uses.
- Full-size masks retain their original zlib payloads after validating their decompressed size. GPU upload expands a temporary mask, copies it into the current atlas page, and drops the temporary; it does not populate the CPU preview cache. The optional 128px mode keeps its existing downsampling behavior.
- BC-capable adapters upload square, complete-chain DXT1/DXT3 sources directly into BC1/BC2 arrays grouped by native resolution and format. Non-square, incomplete-chain and other formats retain the RGBA fallback. No recompression is performed. Tiny authored mips use the required physical BC block extents.
- Empty fallback arrays use tiny dummy allocations instead of full-size empty layers. Array layer limits are checked per actual array, not against the aggregate texture count.
- The unused full-world RGBA vertex-color grid was removed. Color sections are still decompressed and validated per block; neither existing renderer consumed the retained grid, so this does not remove an existing rendered effect.
- Once uploaded, CPU tree geometry/motion/image data and water image frames are released rather than carried into the streaming worker.
- Ordinary model DDS textures are shared across placements by resolved archive path. GPU resource ownership is reference-counted; the old additional unbounded CPU cache of texture file bytes is gone.
- Immutable ordinary-model opaque/alpha pipelines are shared by their full pipeline configuration. Per-placement meshes, uniforms, playback and lighting remain independent.
- Parsed external animation track sets are shared by normalized archive path across ECM variants; every skeleton still validates its own joint binding.
- Whole-world static scenery now keeps one immutable atlas slot per texture content across **all** geometry chunks, not just within a chunk. Once a texture atlas is uploaded, later meshes bind that same reference-counted GPU material instead of allocating/blitting/uploading another copy. Native pixel dimensions, source RGBA, all eight atlas mip levels, UV scaling and per-placement visibility are retained. A separate original model/mesh draw sequence preserves coplanar depth-tie winners even when storage chunks differ. Source textures are not recompressed or downscaled.
- Whole-world CPU construction retains one active pixel atlas and a bounded 32-placement batch of reused-atlas geometry, with the existing one-message channel providing backpressure. Completed atlas pixels are not held in a world-sized CPU collection. Logs now distinguish geometry chunks from unique atlas count and report exact atlas bytes including mip levels.
- Grass retains validated original compressed bitmaps, not every cell ID and randomized transform in the world. Nearby-area decoding feeds a constant-space, deterministic 16×16 patch iterator. It uses the same seed, consumes the same five random samples through empty cells, preserves patch/cell ordering, and emits bit-identical jitter, yaw, size and wind selectors. Empty patches never initialize an RNG; there is no whole-world transform generation during startup.
- Grass and water BSData references are grouped by source file, each file is read once, and only one source file is retained at a time. The original area/draw order is restored after parsing. This removes the previous all-files decoded cache without multiplying disk reads.
- Compressed PCK members decode directly from a bounded file reader instead of first allocating the complete compressed member beside its output.
- Loading logs print packed CPU terrain bytes versus equivalent eager decoded bytes, plus native-BC/fallback texture counts, to support measurements on the actual client.

### Allocation arithmetic, not an RSS benchmark

- The documented 4096 × 5632 world previously allocated **88 MiB** for the unused RGBA grid, plus a possible transient copy when converting it to `Arc`.
- Existing repository corpus evidence reports 4,521 full-size masks totaling **282.6 MiB expanded**, while all 88 source T2MK files together total approximately **76 MiB**, including headers and other records. Keeping original payloads removes the expanded-mask residency during normal-quality startup; the exact packed-mask total is printed for the supplied client. The GPU mask atlas itself is still uncompressed R8.
- BC1 blocks use roughly **one eighth** of RGBA8 storage; BC2 uses roughly **one quarter**, apart from tiny mip/block and allocation rounding. These ratios apply to eligible color textures, **not total application memory**.
- Historical corpus grass counts (1,496,155 live cells) imply **45.66 MiB** for eager u32 IDs plus 28-byte transforms, before vector growth and temporary patch maps. The new compressed source total is printed at startup; it must be measured rather than assumed. More importantly, startup no longer randomizes every patch in every area.
- Whole-world atlas savings depend on cross-chunk reuse. Each previously duplicated 4096² atlas costs **85.33 MiB** with eight mip levels; a shared geometry chunk adds **zero** atlas pixels or mip allocations. Unique textures and geometry still have genuine costs. No claim is made that every prior 5.4 GiB corpus page was duplicated, or that all of that storage disappears.
- Model texture/pipeline/track savings depend on how many placements/variants reuse each resource. The synthetic regression checks two placements sharing one texture allocation and the same two base/alpha pipeline variants, with independent lighting.

## CPU/GPU work

- Static, stopped, paused, or otherwise unchanged model samples reuse their evaluated skeleton pose and deformed meshes. Clocks continue ticking as before; changed samples still evaluate normally, and attached lights use the retained correct pose.
- Unchanged geometry and secondary water passes update model constants without re-preparing, copying, or re-uploading identical vertex arrays. A geometry change or vertex-fog change takes the fully validated geometry update path.
- The persistent far terrain retains **every original triangle** but groups indices into bounded patches. The main/fog view frustum-culls those patches and merges adjacent visible index ranges. Maps and secondary views retain their complete submissions, not the main camera's culled list.
- Release builds use thin LTO, one codegen unit and stripped debug information. This trades longer builds for whole-program optimization and a smaller distributed executable; debug/test profiles are unchanged.

## Visual repairs

1. **Neighbor-dependent cracks:** terrain residency keys now include all four edge stitch cases, not only the block's own LOD. A neighbor-level or perimeter change invalidates the affected mesh. Level-zero blocks also release previously cached geometry.
2. **Near/far holes:** the near window's outer ring matches the persistent far mesh's four-cell grid. Far-mesh exclusion uses the actual uploaded window, including the native window shift at world borders, rather than a separately clamped camera square.
3. **Reflected ground patterns:** color layers wrap rather than mirror. The mirrored color lookup introduced a reflected 2×2 motif in repeating detail. Authored mip data and gradients remain intact; packed mask/lightmap lookups stay clamped. This is a production viewer color-layer correction, not a rewrite of the historical native-state transcription in `terrain_sampling.rs`.
4. **Sun red glow:** the glow lookup accounts for the bitmap's bright-core center relative to the sun disc's bright-core center and the two billboard scales. It ignores dim asymmetric flare tails. The offset is in billboard UV coordinates, so it follows camera pitch and reflected views without a fixed screen-space nudge. This corrects asset-centering within the existing approximate shine quad; it does not implement the original flare chain.
5. Maps no longer reuse the main camera's near-terrain cull list.

## Changed files

| Files | Purpose |
| --- | --- |
| `Cargo.toml` | Release optimization profile. |
| `src/main.rs` | Remove unused world RGBA grid; retain block color validation; register upload module. |
| `src/material.rs` | Compact terrain sources, temporary decoding, CPU-preview memoization, source-size logs and mask tests. |
| `src/terrain_upload.rs` (new) | Native source grouping and exact authored BC upload; shared production color-sampler factory; format/size/fallback tests. |
| `src/terrain_gpu.rs` | Compressed native arrays, fallback uploads, correct wrapping, bounded far patches, limits and triangle/bounds tests. |
| `src/terrain_lod.rs` | Explicit viewer near/far boundary stitching and all-level tests. |
| `src/terrain_residency.rs` | Stitch-aware invalidation, disabled-block eviction and regressions. |
| `src/gpu.rs` | Stitch integration, actual-window bounds, far visibility runs, map visibility fix, upload-source release. |
| `src/model_asset.rs`, `src/world_models.rs` | Shared parsed animation tracks. |
| `src/model_draw_gpu.rs`, `src/world_models_gpu.rs` | Shared textures/pipelines, retained samples and constants-only pass updates. |
| `src/pck.rs` | Bounded streaming decompression and member-size regression. |
| `src/sky_gpu.rs` | Glow/disc bright-core alignment and synthetic offset test. |
| `src/world_objects.rs` | Global immutable atlas-slot/GPU-material sharing; original draw order; exact allocation accounting and CPU regressions. |
| `src/grass.rs`, `src/grass_random.rs` | Compact compressed masks, nearby deterministic placement iterator and bit-exact comparisons; one-file source residency. |
| `src/water.rs` | One-file BSData residency with original ordering restored. |
| `src/bin/world-atlas-gpu-check.rs` (new) | Production shared-atlas upload/draw comparisons across mips and reflected bindings, including observable submission order. |
| `src/bin/world-preload-check.rs` | Unique atlas/geometry counts and exact source-allocation diagnostics. |
| `.github/workflows/pr-checks.yml`, `tools/static-model-gpu-programs.txt` | Require the new production GPU regression in CI. |
| `src/bin/model-draw-gpu-check.rs` | Shared GPU-resource/independent-constant pixel regression. |
| `src/bin/terrain-view-gpu-check.rs` | Updated production layout, native BC1/BC2 tiny mip readback, and asymmetric color-wrap pixels across positive/negative U/V using the production sampler. |
| `src/bin/phase5-sky-transition-gpu-check.rs` | Updated uniform layout; off-center green-disc/red-glow fixture rendered through production helper/shader across pitch and reflected views. |
| `README.md`, `Findings/README.md`, this report | Usage, scope, resource arithmetic and verification limitations. |

## Verification

Local checks executed:

- `tools/check-phase1-core.sh`: **299 tests passed**.
- Standalone `rustc --test src/terrain_lod.rs`: **7 tests passed**, including all 81 edge-case combinations at every level and the near/far boundary test.
- Standalone `rustc --test src/grass_random.rs`: **3 tests passed**, including borrowed and owned packed iterators compared field-for-field against the prior eager implementation over dense, sparse and empty masks, partial patches and multiple areas.
- Formatting of changed Rust files with rustfmt; `git diff --check`.

PR CI also runs all binary type checks, viewer unit tests, the production shader/GPU regressions under Mesa software Vulkan, terrain per-view checks, and the Windows executable build. Consult PR #22 for the final checks on its exact head commit; an earlier green step is not a substitute for the head result. Software-adapter tests prove validation/contracts, not physical integrated-GPU performance or game-image parity.

## Remaining limits and reproducible hardware follow-up

The optional **whole-world static-scenery preload** no longer duplicates native-resolution texture pages across geometry chunks. The historical roughly 5.4 GiB atlas figure describes the old per-chunk allocator, not a measured result for the new allocator. **The complete world still retains all unique texture atlases and geometry**: this is lossless deduplication and bounded CPU construction, not an absolute whole-application GPU/RAM budget or full spatial GPU residency system. Nearby streaming remains the default. Full terrain masks/lightmaps, height/normal grids, model assets, water targets and driver allocations also have real nonzero costs. A physical 6 GB run is still necessary to establish the supported settings and peak memory.

On the affected laptop, compare release builds before/after using the same client, resolution, settings and camera route. Record peak process private bytes/working set through startup, shared/dedicated GPU memory, load duration, steady-state frame time, and allocations while flying/teleporting. Record the new grass compressed-residency and whole-world unique-atlas byte counters as well. Check ground detail, changing LOD boundaries, all world edges, noon/dusk sun alignment and water reflections. Keep the source-size log lines and note whether BC upload was enabled. No new on-disk asset cache is introduced; client files are not modified.
