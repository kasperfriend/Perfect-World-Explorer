# 13 — Renderer controls stay opt-in

There is no [physical RTX 3080 execution](01-sdk.md) for this pass. Driver-heap
estimates and synchronized/normal-present timing must be collected on that
host. Do not treat logical byte budgets, lavapipe timings or thumbnail
readback cadence as retail VRAM use or FPS.

| Gate | What is supported or measured | Remaining |
| --- | --- | --- |
| Paced copies and static BLAS | Bounded batch allocator and CPU Vulkan publication fixtures; a previous resident snapshot remains visible until its replacement is ready. | Matched live resident-crossing tails, publication counts and driver-heap peak on the specified GPU. |
| Sampled vs buffer BC | Opt-in sampled BC1 terrain on raw Vulkan; nonadmitted pages use the buffer fallback. Native's DDS support is separate. | Matched sampled-vs-buffer physical allocation, whole-viewer heap and timing A/B. Ray BC5 normal and BC7 material sampling are **not implemented** in the sampled terrain atlas; those inputs still decode to the portable bank. |
| DI variance / GI bias | Prior four-seed synthetic pan showed **higher** repeat variance and cost with reuse on both routes. The moving fixture checks local cache rejection; it doesn't quantify converge-to-Reference bias. | Repeat on fast retail flight, animated occlusion and reveal, with fixed ROI, separated seeds, converged Reference and matched timings. |
| Hybrid primary | Prior CPU A/B had inconsistent raster-inclusive performance. The focused unavailable-raster fixture uses an ambient-only white receiver with no direct light or bounce; a forced target refusal falls back to full primary rays with identical color and reverse-Z. | Physical raster-inclusive A/B/A/B on the same captures with normal-present tails and no silent fallback. |

No change to default Hybrid enablement, BC sampling or DI/GI reuse was made.
See [performance and prior results](../../performance.md#still-open),
[pacing and logical limits](../../gpu-budget.md), and the separate
[synchronized and normal-present recipes](../../travel.md).

**Local S on `31bfeef`:** both paced publication suites passed. The narrow
CPU Vulkan Hardware fixture kept the old scene through eight redraws and
published five copy batches (maximum 8,192 B), with 18 static BLAS builds in
three batches. The forced unavailable-raster ambient probe matched
`[89, 51, 25, 255]` and reverse-Z `0.32275134` exactly. The local **sampled BC**
interop fixture exited **139** on the narrow lavapipe process, so there is
no fresh local success or sampled-vs-buffer comparison. Keep this result
separate from [the hosted Linux run](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44523578638/job/135558183606), where the BC,
water and raster parity fixtures passed using the runner's stock Mesa
settings. Raw local logs: `13-renderer/`. Neither run measures a physical GPU.
