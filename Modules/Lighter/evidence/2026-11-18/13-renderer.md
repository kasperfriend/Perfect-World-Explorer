# 13 — Renderer controls stay opt-in

No [physical RTX 3080 execution](01-sdk.md) was available. Logical byte
budgets, sampled allocation requirements and lavapipe captures are not
retail VRAM or normal-present frame time.

| Gate | Verified scope (S) | Remaining on the target |
| --- | --- | --- |
| Paced uploads/static BLAS | CPU Vulkan publication fixtures keep the old resident snapshot until its replacement is ready. Hardware recorded eight old-scene redraws, five copy batches (max 8,192 B) and 18 BLAS builds in three batches. | Resident-crossing tails, publication counts and whole-viewer driver heap. |
| Sampled BC | Opt-in BC1 terrain plus compatible authored **BC5 UNORM RG normals** and **BC7 sRGB cutout/opacity** static BMD tiles. Test details below. | Matched sampled-vs-buffer allocation, full-view speed and heap A/B on the installed client. |
| DI/GI reuse | Four seeds × eight moving poses, 43² trace: DI repeat variance 131.41 → 155.90; DI+GI 562.86 → 583.50; mean-image RMS 4.944/4.888 RGB8 vs finite controls. Reuse cost more on both routes. | Fast retail flight, animated occlusion/reveal, and convergence-to-Reference comparisons of GI bias, with separated seeds and matched timings. |
| Hybrid primary | A forced unavailable-raster test with no direct light or bounce returned identical fallback color `[89,51,25,255]` and reverse-Z `0.32275134`. No repeatable raster-inclusive gain. | Matched Hardware–Hybrid–Hardware–Hybrid travel, counting raster plus traversal and rejecting silent fallback. |

## Sampled BC admission and readback

`PW_RAY_BC_TEXTURES=1` applies to Vulkan **Hardware** only. Each supported
format gets a separate 256²-image array with block-aligned mip rectangles.
BC5 samples RG as linear data; shader reconstruction of the normal's Z
follows filtering. BC7 hardware linearizes RGB but keeps alpha linear;
filtering premultiplies the taps and unassociates the result. Authored mips,
wrap/clamp and explicit fractional-LOD weights are retained. Alpha-test
coverage can still read the associated decoded mip tree. Materials that
need generated/reduced mips, are not compatible with the stored channels,
or arrive after the immutable prefix was assembled can use decoded tiles.
The prepacked material prefix is capped to the smaller of 8 MiB and 1/8 of
remaining texture-bank bytes. Unsupported device/format, failed allocation,
or a changed bank identity uses the buffer fallback; fallback allocation
cannot be counted as an optimization. Changed bank identity disables atlas
access without freeing the images, so their charged bytes remain reported.

The small Vulkan compute fixture ran on `d6c5c37`: **16 NPOT mip/wrap/clamp
samples**, maximum absolute linear RGBA difference **0.0000002**. It uses
real buffer uploads, BC image copies, sampled-image descriptors and shader
filtering. Corrupting only the decoded BC7 level confirms the sampled path
is actually accessed. It charges **592 B** for the mixed logical bank and
**131,072 B** for the allocated image arrays. It is not a full-frame ray
traversal success, nor a memory or speed win. Earlier full-ray attempts on
this CPU driver exited 137/139 during cold compilation; the old BC1
interop passed on hosted stock Mesa and failed in the local narrow run.
Individual receipts are in `13-renderer/` in the [bundle](14-delivery.md).
The repeated reuse test on `31bfeef` reports synchronized draw/readback p50
for DI of **68.04 → 94.79 ms** (Software) and **43.07 → 65.45 ms** (CPU query),
and for DI+GI **90.66 → 136.34** and **74.61 → 92.64 ms** respectively. That
extra readback-bound evidence also supplies no reason to enable reuse.

For a target A/B, run the same portable Hardware capture/travel with
`PW_RAY_COMPACT_TERRAIN=1` in both and `PW_RAY_BC_TEXTURES=1` only in B.
Confirm nonzero sampled counts (per format for material controls), comparable
settings/poses/coverage, and no fallback. Record `bc_image_allocated_bytes`,
render allocation logs, current AS/trace/filter spans and synchronized plus
normal-present tails. Whole-viewer heap is a separate driver estimate, not
the sum of tile payloads. If the admissions change, report those changes
rather than attributing the entire delta to filtering. Keep **Hybrid,
sampled BC and DI/GI reuse off by default** until physical measurements
justify changing policy. See [performance](../../performance.md),
[budgets](../../gpu-budget.md) and [travel](../../travel.md).
