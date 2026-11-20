# RAM-first follow-up: lossless terrain paging

## What changed

The previous optimizations reduced texture duplication. This follow-up removes a different kind of residency: **the entire world's CPU height and normal grids**. It is integrated into loading, terrain meshing, far-mesh construction, navigation, CPU preview and PNG export, not an optional prototype.

### Method: byte-plane XOR terrain pages

1. Decode and validate one original T2BK block, as before. Keep the existing grid cells and discard the duplicated edge row/column exactly as the original loader did.
2. Split each f32 height into its four original byte planes and the existing packed normal into three signed-byte planes. XOR each plane against its preceding byte within the same row. This exposes correlated float exponent/normal bytes to a fast zlib encoder **without quantizing or changing any bits**.
3. Append the compressed block to a disk backing store. There is no full-world height/normal allocation, including during startup. Only source-file/block parsing buffers and the small block index remain in memory.
4. Terrain, navigation and streaming share one LRU decoded hot set, default **2 MiB**. A cursor pins its current page and avoids cache locking while repeatedly reading that page. Neighboring heights and normals come from the same decoded page.
5. Evict unused pages and decode them again when revisited. Eviction changes I/O work, not geometry, resolution, lighting or scene content. The initialization sweep's hot set is explicitly released after GPU upload.

This is a RAM/disk/CPU tradeoff, **not a hard whole-application RAM cap**. A slow or busy disk can introduce streaming stalls. OS file-cache pages remain reclaimable and can appear in memory monitoring; the hot-set counter is not total process RSS.

## Expected allocation reduction (arithmetic, not a laptop benchmark)

For a 4096×5632 grid:

| CPU data | Previous storage | New storage |
| --- | --- | --- |
| Heights | 88 MiB, plus potential Arc conversion copy | Paged; shared hot set below |
| Packed normals | 66 MiB | Paged; shared hot set below |
| Combined decoded terrain | **154 MiB retained** | **2 MiB hot-set budget**, including decoded page object/Arc overhead |
| Page index | None | Approximately **1.03 MiB** for 22,528 blocks (48-byte records) |
| Active cursors | None | One approximately 7 KiB page per cursor can remain pinned outside the hot set |

Hash-map bookkeeping, source-file parsing, short-lived compression/decompression buffers, filesystem cache and all other systems are additional costs. The new method removes the large grids from both startup and steady-state ownership; it does not claim that the full viewer uses 3 MiB or that any particular 6 GB machine is now guaranteed to fit.

Additional RAM changes:

- Static-scenery decoded texture LRU target: **32 MiB instead of 256 MiB**, with the existing environment override retained. The possible budget difference is 224 MiB per cache, **not a measured saving**. Actual residency depends on the route and textures in use. Active-batch textures stay pinned even above budget so no content is omitted or downscaled.
- Near-terrain concatenation pre-reserves exact vertex/index counts instead of growing the second CPU mesh geometrically.
- PNG export streams a single RGB row. A 4096×5632 RGB export no longer needs its previous **66 MiB** output buffer; the explicit RGB row is 12 KiB. PNG compression buffers and terrain reads still have costs.

## Cache location and safety

- Default: `cache/terrain-v1/`, relative to the viewer's working directory.
- `PW_CACHE_DIR` overrides the root; `terrain-v1` is appended. The directory must be writable and the disk must have free space.
- `PW_TERRAIN_RAM_MIB` selects a 1–256 MiB decoded hot-set budget; default 2. Smaller budgets prioritize RAM at the expense of extra I/O.
- `PW_TEX_BUDGET_MIB` selects the existing scenery texture budget; new default 32.
- Cache content includes dimensions, format version, block IDs and compressed source values in its SHA-256 identity. Existing objects are stream-hash-verified before reuse. Each decoded page is also checked against its expected size and SHA-256, in addition to zlib validation.
- Incomplete/duplicate/malformed grids are rejected. Partial builds are not published as completed cache objects. Cache corruption is reported, never turned into replacement terrain; the next startup reconstructs the cache from client sources. Mesh initialization/streaming propagate cache errors rather than accepting damaged geometry.
- Cache files are immutable after publication. Concurrent sessions do not overwrite a valid cache in use. If Windows prevents replacing a corrupt old cache object, the verified new session file is used instead.
- Client files are not modified. The derived cache is ignored by Git. Delete it **with the viewer closed** to reclaim disk space; it is rebuildable. Do not distribute derived client data.
- Source validation/cache construction still run at startup. This implementation does not skip the source parse or promise faster warm loading. Historical content-addressed versions can remain on disk until the folder is cleared.

## Verification

New viewer unit tests cover:

- Exact f32 bit round trips, including negative zero and nonfinite bit patterns in the codec, and signed-normal byte round trips. The production loader still rejects nonfinite source heights.
- Every cell of a multiblock synthetic grid under repeated forced eviction, out-of-order source blocks and shared height/normal views.
- Cursor reuse without repeated page reads.
- Cache identity reuse, source changes and corrupt-cache reconstruction.
- Rejection of damaged individual pages instead of silent geometry substitution.
- Concurrent readers and evictions without exceeding the owned hot-set budget.
- Incomplete grids, duplicate blocks, out-of-range blocks and cleanup of abandoned builds.
- Streaming PNG RGB output checked through PNG decoding.

`terrain-memory-check` is an asset-free diagnostic using the production store and exact source-value comparisons. CI runs both a small stress fixture and `--full-world` (4096×5632), with all far-grid sample positions, edge crossings, revisits, explicit eviction, a bounded peak counter and post-sweep trimming. The full-world source is **synthetic**, not the user's client or a physical-laptop measurement.

Consult PR #22 for the exact-head CI result and Windows build artifact. Existing terrain/shader/model regressions remain required. No new GPU-quality reduction was introduced.

## Changed files

- `src/terrain_store.rs`: lossless codec, disk builder, content identity/integrity, bounded shared hot set, cursors and unit tests.
- `src/main.rs`: block-at-a-time loading and row-streamed PNG export/test.
- `src/gpu.rs`, `src/terrain_gpu.rs`: paged near/far/grass queries and post-initialization cache trim.
- `src/navigation.rs`, `src/viewer.rs`, `src/bin/navigation-survey.rs`: shared paged queries and compatible small diagnostic fixtures.
- `src/terrain_residency.rs`: exact-count mesh allocation.
- `src/objects.rs`: RAM-first decoded texture budget.
- `src/bin/terrain-memory-check.rs`, `.github/workflows/pr-checks.yml`: reproducible allocation/correctness stress checks.
- `.gitignore`, `README.md`, `Findings/README.md`, this report: private cache exclusion and usage/verification documentation.
