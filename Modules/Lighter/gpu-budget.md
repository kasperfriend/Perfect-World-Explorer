# GPU timing and residency envelope (Step 19)

Lighter logs GPU stage samples at most once per second per source. The
Software BVH is packed/refit on the CPU, so it has no GPU AS build to time.
Vulkan reports AS build/refit, raster primary (if active), and ray traversal
separately. wgpu reports Software traversal, reprojection, filtering and
upscale/compose. The raw query pool is read only after its fence signals,
and wgpu uses a three-slot timestamp readback ring. Unsupported or pending
queries do not block the viewer. Headless capture GPU stages are separate
from its synchronized draw-plus-readback wall time.

The raw Vulkan allocator's ceiling is **6.5 GB** or the largest reported
local heap, whichever is smaller. It charges Vulkan memory requirements for
BLAS/TLAS, scratch, uploads and staging, the shared image and optional raster
attachments, including transient growth and an incoming resident scene.
The active scene's BLAS remain live until the pending replacement is ready. Where `VK_EXT_memory_budget` is advertised, the menu
also shows the driver's device-local usage estimate and warns at 7 GB. The
raw limit is **not** a hard 7 GB cap on the whole application or the adapter.
Tracked allocations exclude Native scene/UI/swapchain resources and opaque
wgpu/driver allocations. The live cadence reports a rolling 1% low for normal
presents, not headless timings or frame-generation cadence.

Software stays under per-binding, packed mesh and image ceilings. The viewer
requests at most 256 MiB per storage binding (or the adapter's limit); the
texel bank is capped at 128 MiB or the device's storage limit, whichever is
smaller. Trace, two history buffers and filter each reserve at most 64 MiB.
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
exhausted; their coverage rows retain the budget reason. This is not
shader-driven demand paging.

## Sampled texture experiment

`PW_RAY_BC_TEXTURES=1` is opt-in Hardware Vulkan. Track the logical fallback
bank and actual Vulkan image allocation sizes separately. The immutable
BC1/BC5/BC7 images can remain allocated when an unrelated replacement bank
uses buffer decoding; the `bc_image_allocated_bytes` receipt includes them
while sampled tile/page counts go to zero. Physical A/B still needs the
whole-viewer driver-heap estimate and matched draw/flight measurements,
not just a sum of compressed mips. See [Step 13](evidence/2026-11-18/13-renderer.md).

## Paced world crossings

The streaming worker packs the replacement ray scene. The draw thread stages
it in separate static GPU buffers, at most one batch per redraw. A batch has
at most **4 MiB** of packet/texel copies (and at most 32 copy spans); shared
sealed texel bytes are copied device-to-device, not re-uploaded from the
host. The active and pending banks can overlap in memory. Native's separately
paged mask atlas is also kept unpublished until the ray candidate is ready.
A failed admission still publishes Native with an explicit ray error, not a
ray render with partially updated inputs.

Vulkan builds up to **8 new static BLAS** per batch, targeting **32,768
triangles** (one indivisible BLAS can exceed this target). Existing BLAS
with matching geometry and key are retained. A fence retires each batch
before staging or scratch can be reused. The live TLAS is left alone; at
publication the first ray draw assembles its new membership with the latest
moving geometry. TLAS work, dynamic uploads/refits, a cold first scene and
Native's non-mask geometry uploads are **not** split by this change.

The wgpu device exposes one created Vulkan queue. A separate command pool and
fence are **not** an independent queue or concurrent compute. Native/Software
and raw Vulkan work are submitted on the render thread. Draws can continue
on the old buffers between batches; actual present cadence still depends on
shared-queue contention. At a committed crossing the diagnostic records
transfer batches, host and retained bytes, maximum batch size and BLAS
batches. No interactive queue-idle wait is needed to publish.

Synthetic 9×9 crossing on Mesa 25.0.7 CPU Vulkan, with a test-only 8 KiB cap:
**5** copies (27,708 B host, 5,524 B retained), **5** unchanged Software
redraws, and **8** unchanged raw Vulkan redraws including 18 new BLAS across
3 batches. The retained BLAS was not rebuilt. The replacement's RGB and
reverse-Z appeared together; Software and raw Vulkan both passed a cancelled
copy/retry. This checks ordering and bounds, not latency on a physical GPU.

## Validation (2026-11-18)

On Mesa 25.0.7 CPU Vulkan, 22 Lighter units plus both pixel suites and streamed
regression passed. For example, the streamed Software run sampled 72.329 ms
traversal and 3.288 ms filtering; the Vulkan fixture logged 0.150 ms build and
55.886 ms traversal for one sample and exactly 0.000 ms build when unchanged.
The measured raw allocator ceiling was 4,131,278,848 B on this CPU device. The
unit tests check transient overrun/release, timestamp wrap, page offsets and
128-tile retention. These results do not characterize RTX 3080 performance.
