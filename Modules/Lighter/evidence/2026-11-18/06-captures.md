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
sidecars, decoded RGBA8 hashes, Windows x64 RTX 3080 10 GB Vulkan device, exact active
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

## Ordered follow-up

`PW_PREVIEW_REQUIRE_PROVIDER=portable|fsr|rr` guards **every** draw,
not just the saved image. The sidecar records the expected provider and
checked draw count; the comparator requires at least 49 verified draws
and the actual trace extent. The ordinary preview path is unchanged.
`tools/lighter_temporal_capture.py --help` runs the seven named scenes in
order on each ray route, preserves all attempted variant logs, and binds
the input pins, executable, paths, and comparison receipts by hash. Its
JSON manifest has a `cases` array; each case needs `scene`, `map`, a
five-number `camera`, `hour`, named normalized `regions`,
`reference_mae_limit`, and `camera_evidence` (a retained scout file relative
to the manifest). `weather` defaults to `Clear`. `path` and `reveal` need a
`path` pointing to a moving 0–48 plan in the same terrain block. Scout the
subjects and choose the tolerances before running. The runner checks receipt
consistency, not whether the subject is visibly present or the SDK is authentic.

Example target command (trusted pins must cover every selected map):

```powershell
py tools/lighter_temporal_capture.py --manifest C:\bench\previews.json `
  --input-manifest C:\bench\client-pins.txt --viewer C:\bench\perfect-world-explorer.exe `
  --client E:\pw142 --output C:\bench\06-captures --source-sha $testedCommit
```

**New S evidence:** `06-captures/reference-32-vs-64.json` in the ordered
bundle compares two 96×72, 8-bounce Software fixture PNGs after one 16 ms
warm-up and a final draw each. Both are decoded-hash verified; the whole
RGB8 MAE is **0.58435** (max **29**), center MAE **1.03896**. This smaller
`PW_PREVIEW_SIZE` audit exercises refinement and is **not** the 48-step
retail convergence gate. Requiring FSR with only portable available
failed on the first draw and wrote no image. No P/V preview was recorded.
