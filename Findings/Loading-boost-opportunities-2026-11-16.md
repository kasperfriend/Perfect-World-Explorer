# Loading acceleration opportunities — 16 November 2026

## Goal and constraint

"Instant everywhere" is not a defensible cold-start promise for this viewer: a first run must read game assets from the user's storage, validate and decode proprietary formats, allocate/upload GPU resources, and the viewer currently builds a substantial portion of the world before rendering. The safe goal is (1) a quickly interactive first view, (2) low-latency warm starts and nearby streaming, and (3) bounded background work without visual/content loss. Cold and warm paths must be measured separately on the supplied client and target hardware.

## Candidate boosts, ordered by expected value / risk

| # | Opportunity | Cold start | World/flight | Risks / decision |
|---|---|---:|---:|---|
| 1 | Phase timers and counters for source hashing, terrain decode/cache, object/model decode, CPU texture work, GPU uploads, and first usable frame | Enables all | Enables all | **Do first.** No benchmark claim without these. |
| 2 | Asynchronous, overlapped DirectStorage requests with bounded outstanding work; coalesce/submit ranges in batches where caller data permits | High for archive-heavy load | High for asset streaming | **Implement a bounded concurrent system-memory queue.** Current one-request/one-submit/one-wait call path serializes DS reads; fix that before claiming it helps. |
| 3 | Give the renderer a usable terrain/sky frame before optional scenery, water/grass/tree/model preloads finish; load those behind the frame loop with cancellation and visible progress | Very high perceived | Moderate | **Candidate for the next architectural phase.** Requires splitting `Terrain` construction/renderer initialization and preserving first-frame invariants; don't fake a completed world. |
| 4 | Prioritized spatial streaming: camera-forward and visible blocks first, prefetch the next flight corridor, cancel/deprioritize work behind the camera | Low initial | Very high | **Partly present** (nearby residency, worker queues). Audit queue priorities/cancellation before replacing. |
| 5 | Parallel archive range reads and independent asset decompression with bounded queues/backpressure | High | High | Several existing loaders already use worker threads; audit per-archive serialization and avoid unbounded CPU/RAM expansion. |
| 6 | Persistent decoded/converted asset cache (models, meshes, mip chains, parsed scene records), content-addressed and versioned | Medium cold after first run; high warm | High on revisits | **Good follow-up.** Needs cache schema, corruption handling, identity rules, cleanup and disk budget. Don't cache GPU handles. |
| 7 | Avoid repeated terrain cold-path I/O: source identity currently fully hashes every terrain source before warm-cache reuse; build a safe, verifiable source inventory or background verification path | Medium warm | Medium | Current full hashes protect against stale cache. Metadata-only identity is unsafe; retain content verification unless stronger filesystem identity and revalidation design is proven. |
| 8 | Avoid unnecessary PCK directory scans/handle opens; share read-only archive handles and batch nearby entries | Medium | High | Keep archive bounds, path checks, and independent-reader semantics. Measure before broad refactor. |
| 9 | Stream GPU uploads over frames with resource budgets and first-view priority | High perceived | High | Existing whole-world upload is frame-budgeted and nearby residency is windowed; extend carefully to startup resources. |
| 10 | D3D12 GPU decompression through DirectStorage 1.4 Zstd | Potentially high only after content conversion | Potentially high | Retail PCK entries are zlib, not Zstd; 1.4 cannot GPU-decompress those as-is. Requires a lossless offline transcode/cache plus access to the same D3D12 device as `wgpu`. **Do not relabel current zlib data as Zstd or claim this path is active.** |
| 11 | GACL conditioning, archive repacking and aligned/chunked asset containers | Medium after cooking | High streaming throughput | Requires offline content cooker, extra disk footprint, hashes/versioning and validation. Not a direct fix for unconverted user data. |
| 12 | Shader/pipeline/texture warmup and native pipeline cache | Medium | Medium | Driver/backend dependent; wgpu version and platform support must be checked. Warmup itself can lengthen first load if indiscriminate. |
| 13 | Memory mapping / `FILE_FLAG_NO_BUFFERING`, storage-specific tuning, larger reads | Unknown | Unknown | Not inherently faster; alignment, page-cache effects and HDD/SSD variance. Benchmark candidates only. |
| 14 | Reduce render quality/resolution, omit assets, quantize geometry, or shrink texture/mip fidelity | Fast but lossy | Fast but lossy | **Reject as a default acceleration strategy.** Existing fidelity and user quality settings must remain intact. |

## Work selected for this pass

1. Fix the SDK-enabled Windows build path and stage the redistributable runtime/licenses correctly. Run `43978558371` confirms the standalone build passes when MSVC is initialized for that step. The later audio fixture test's diagnostic echo exposed that Cargo's custom build script ran without `cl.exe` on PATH in that separate step. The workflow now exports the x64 MSVC environment for every subsequent Windows step, in addition to printing useful test diagnostics.
2. Change DirectStorage from one globally serialized queue to per-calling-thread queues and completion events, so independent asset-loader threads can overlap I/O while keeping error records isolated. Keep safe fallback and error recovery.
3. Package x64 runtime DLLs beside the viewer when present and preserve the package license/notices; otherwise explicitly retain ordinary file-I/O fallback.
4. Add a loading-window DirectStorage throughput graph backed by successful read counters, with explicit unavailable/fallback states so a flat graph cannot be mistaken for active I/O.
5. Bound terrain block decode work-ahead to two worker waves; the previous unbounded result channel and reorder map could retain many decoded blocks behind one slow early block. Preserve ordered builder insertion and the one-thread path.
6. Confirm both ordinary Windows CI and Linux tests. SDK/hardware throughput still needs a user's machine.

CI run `44034878945` passes on both Linux and Windows with the official pinned NuGet x64 runtime DLLs committed at `sdk/DirectStorage/1.4.0-preview2-2606.904/native/bin/x64/`: `dstorage.dll` (207,200 bytes; SHA-256 `ac4a39ce3d6301d1449a86b54e6f6190fcf93b9dfd92ecf8dcb143ee75e38fdb`) and `dstoragecore.dll` (2,517,304 bytes; SHA-256 `4e92a4ba8a1851281cfc003ef833b64c9e56b7a6a667e30eae4a004a4b41f02a`). Both are PE x64 (`0x8664`); the portable artifact audit requires both beside the EXE plus package notices. No hardware throughput result is claimed.

The progressive-first-frame split, priority streaming scheduler, and persistent asset-cache proposal remain separate follow-ups because each changes ownership/lifetimes and needs client-asset measurements. No amount of these software changes guarantees an instant cold read from arbitrary storage or hardware.
