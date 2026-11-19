# 6 — Matched temporal previews (physical run blocked)

**P/V: absent.** [No matching Windows/RTX or retail client is attached](01-sdk.md).
There are no submitted SDK PNGs or converged retail Reference frames for
still, path, actor, thin, wet, layered or reveal; do not substitute a
portable screenshot or a stand-in for either SDK.

The audit uses 48 warm-up steps of 16 ms and a zero-delta capture. An
optional `PW_PREVIEW_PATH_PLAN` in the [travel format](../../travel.md)
advances one pose per warm-up frame inside the **same prebuilt terrain
block**. Its header must match the map/hour/warm-up and the final frame
index and pose must be 48 and the `--gpu-preview-at` pose. The sidecar
records the plan SHA-256. This is not a streamed-window travel test.

`PW_PREVIEW_REFERENCE=1` plus `PW_PREVIEW_REFERENCE_SPP=32|64|128` is a
headless-only refinement control, at full resolution and 8 bounces;
interactive Reference remains unchanged. A preview at 32 spp alone is not
convergence evidence. Keep the map, pose sequence, extent, clock and ROIs
identical and test refinement against a predeclared tolerance. The higher
budgets can be expensive and need a physical run to establish reliability.

`tools/lighter_temporal_compare.py --help` checks the matched schema-3
sidecars, decoded RGBA8 hashes, Windows RTX 3080 Vulkan adapter, exact active
provider and intended ray settings, and reports per-region RGB8 mean/max
error vs portable and the higher-budget Reference. It fails a silent
fallback and reports any Reference refinement threshold violation. These
are encoded RGB differences, not optical or temporal quality verdicts.
**S:** seven Python receipt/region tests passed (`06-captures/`).

**S retry on `99f7564`:** Software produced both PNGs at 960×720 with
48 warm-up frames, 768 ms animation, active `portable` and the path receipt
`22faa3d4f5d1bf1e7360e6ca98dbd16cf0c6b9a5494370b9d5715228c3058311`.
The final PNGs byte-match one another (`06e71efd…`). This tests path parsing,
matching and final capture on a synthetic view, not SDK temporal quality or
Reference convergence. See `06-captures/path/` and `06-captures/still/`.
