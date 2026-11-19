# GPU timing and residency envelope (Step 19)

Lighter samples traversal and filter timestamps at most once per second. The
Software BVH is packed/refit on the CPU, so there is no GPU AS build to time.
Vulkan separately reports the GPU BLAS/TLAS build/refit and traversal. The raw
query pool is only read after the trace fence signals; the wgpu query resolve
is mapped asynchronously after submission. A pending measurement is skipped,
not waited for. Unsupported queries are omitted. The numbers are stage samples,
not synchronized frame-time percentiles.

The raw Vulkan allocator's default ceiling is **8 GiB** or the largest
reported device-local heap, whichever is smaller. It charges Vulkan buffer
memory requirements for BLAS/TLAS, scratch, uploads and staging, and the
shared image, including transient growth. Departed BLAS retire before new chunks
claim that space. This is a conservative Lighter allocation ceiling, not an
estimate of free VRAM or a claim about the whole viewer. Software stays under
its stricter per-binding, packed mesh and image ceilings. The viewer requests
at most 256 MiB per storage binding (or the adapter's limit); the texel bank is
capped at 128 MiB or the device's storage limit, whichever is smaller. Trace,
two history buffers and filter each reserve at most 64 MiB.
The Vulkan device's primitive, instance, address and alignment limits also apply.

Terrain blend masks store one byte per texel instead of four. With the same
32-pixel ceiling and mip chain, the payload is 75% smaller (the small per-tile
header remains); blend weights and filtering are unchanged. If the material
tables and masks still exceed their share of the budget, the loader chooses a
lower common ceiling before decoding them, keeping every layer. Tree textures
also get a common mip ceiling when their chains exceed their budget, preserving
placements and cutout modes. Terrain colours and optional baked lightmaps
scale their budgets down on smaller adapter bindings.

The streamed suffix is shared by grass, local and extended ornaments, and
moving assets. Grass cannot take more than a quarter of the available room at
its admission stage. Local ornaments are admitted before the extension, with
up to 16 MiB or a quarter of their starting room reserved for moving assets.
When an optional grass material or BMD placement cannot fit, its instances
are counted in the inventory as budget-limited. If essential ray data cannot
fit, the live viewer falls back to Native; it does not present an empty ray
frame. Device loss still triggers device recovery, not a render-mode fallback.

The streaming loader keeps 128 recently used decoded BMD tiles on top of the
live set. Higher-ranked placements and local assets are touched last. Lower
rank optional owners are omitted first when geometry or texel admission is
exhausted; their coverage rows retain the budget reason. Static texture
suffixes upload in pages of at most 4 MiB, leaving the shared prefix intact.
The loader handles archive decode; device page copies are asynchronous. A ray
packet publishes atomically with its uploads, not with partially readable
texture pages. This is not shader-driven demand paging.

The wgpu device exposes one created Vulkan queue. A separately enumerated
queue family cannot be used without provisioning it when opening the device.
Uploads and AS work are submitted with the trace before composition; explicit
transfer/build/read barriers and later-frame fence polling cover only the
resources that are reused. No extra queue idle is inserted on steady frames.

## Validation (2026-11-18)

On Mesa 25.0.7 CPU Vulkan, 22 Lighter units plus both pixel suites and streamed
regression passed. For example, the streamed Software run sampled 72.329 ms
traversal and 3.288 ms filtering; the Vulkan fixture logged 0.150 ms build and
55.886 ms traversal for one sample and exactly 0.000 ms build when unchanged.
The measured raw allocator ceiling was 4,131,278,848 B on this CPU device. The
unit tests check transient overrun/release, timestamp wrap, page offsets and
128-tile retention. These results do not characterize RTX 3080 performance.
