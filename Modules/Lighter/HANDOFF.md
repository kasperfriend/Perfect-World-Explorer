# Lighter performance handoff — PR #51

## On the Windows RTX 3080 PC

1. Set the primary display to **2560×1440** and close other GPU-heavy apps.
   Use PowerShell 7 x64 on the local desktop.
2. Download and **extract** [Lighter-Windows-Perf-Kit](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44568900822/artifacts/14552639482)
   (sign in to GitHub if asked). The viewer, strict SDK smoke binary, script
   and a07 path are from `0b4b48527aab9dcd030565df0129622734051c07`.
   Archive SHA-256: `eed462478aefc0da3a7bb428e4e3e3f75f8c6e5e9426fdefbb4456f695d98372`.
   The kit does **not** install software or include optional reconstruction SDKs.
3. Run the script (adjust the kit path if needed):

   ```powershell
   $kit = "$HOME\Downloads\Lighter-Windows-Perf-Kit"
   pwsh -NoProfile -File "$kit\run-perf-session.ps1" -ClientRoot 'E:\pw142'
   ```

   If you have the already-pinned SDKs, add `-FsrDll 'C:\…\amd_fidelityfx_upscaler_vk.dll'`
   and/or `-RrBridge 'C:\…\pwe_ray_rr.dll' -RrSdkDirectory 'C:\…\rr-feature'`.
   See [operator notes](../../tools/windows/README.md) and the
   [SDK versions and bridge setup](temporal-reconstruction.md). Missing or
   wrong SDK inputs stay unavailable; they do not prevent portable collection.
   If the originally failing startup used RR or FSR, use `-StartupProvider rr`
   or `fsr` with that SDK so the two starts test the implicated route. A failed
   SDK startup triggers separately named single-skip runs. `-StartupOnly`
   collects just the startup evidence if that is the immediate priority.
4. Let the windows open and close automatically. Do not move the camera,
   change settings, or put the PC to sleep. There is a **60-second pause**
   after the first start and a separate **≥10-minute** memory run.
5. Upload **the ZIP and its `.sha256`** printed by the script, and include
   **one sentence about what you saw** (for example, “Both starts reached the
   world; saw a hitch at the water approach.”). Upload partial/failing captures
   too. Do not upload `scratch`, game files or SDK DLLs. The ZIP includes local
   paths, driver and event text; review before sharing outside the PR.

This is the actual replacement-process startup code on a fixed 1440p window.
It is not a test of an existing parent window retaining focus or of the OS
reclaiming VRAM after a TDR. Native DX12 is a separate control. The matched
Vulkan route set is 7 × 2 separate travel policies; synchronized values are
not used for normal FPS. A vendor fallback is preserved in the failed run
log, not passed as the requested route. If the RR 50% travel extent is rejected,
retain that run and, if practical, use a **separate** `-RayPercent 45` session.

## What is already verified (S only)

| Area | Code | S receipt |
|---|---|---|
| Step 0 | `52b9712` | [Plan](PERF-PLAN.md) recorded before renderer edits |
| A1–A3 | `30a4cc1`, `7275d14`, `b7e5bdc`, cache round trip in `0b4b485` | ABI guards, portable bisections, ordinal watchdog, staged first draw, disk reopen, two injected teardowns |
| A4–A7 | `c9432d6`, `8930ad6`, `7e02b21`, `68e4b6a`, shader fixes in `e195fa3`/`e18f10a` | Hosted synthetic Hybrid parity suite, exact-tail controls and shader lowering; isolated SVGF RMSE/motion |
| A8–A10 | `6402d63`, `b339de9`, `e195fa3` | SDK fail-closed negatives, controller/override tests, raw allocation ceiling and staged compact-option draw |
| B–C | `e18f10a`, `0b4b485` | Exact kit source; checked bundle/schema, manifest/driver, failure preservation and timing/memory thresholds. 33 Python `test_lighter_*` tests. |

The final local run passed 1,350 Rust tests (39 ignored, two pre-existing
CPU-driver GFX failures filtered) and 33 Python tests. The unchanged Native
960×720 PNG controls matched with SDK flags off, FSR and RR. [Hosted checks on `0b4b485`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44568900822)
passed the Linux fixture suite (including Hybrid) and the Windows build and
failed-preflight packaging test. The latter ran without retail input, not on
the target GPU. See [results and receipt limits](PERF-RESULTS.md) and
[per-change evidence](evidence/perf/); the workspace archive
`cache/Lighter-Perf-S.zip` collects S receipts, not the Windows binaries.
Neither a hosted build nor CPU Vulkan is an RTX 3080 startup or frame-time result.

## Still open before changing defaults

- Reproduce the two Windows startup symptoms under the original settings,
  or record that they did not recur. Compare stage, exit code, event module/
  offset and dedicated-memory timeline. A skip removing a symptom narrows the
  suspect stage; it does not prove a root cause. Require ten consecutive clean
  starts on the intended route before promotion.
- Validate 600 matched normal presents, 600 synchronized presents per route,
  streaming commits and the ≥600 s process-heap trace. Compare candidate to
  full rays and to bare Hybrid; frame rate, GPU stages and memory are separate
  gates. Check the explicit strict Software/Hardware SDK smokes; do not count
  rows that fall back. This session does not fill any missing RT-17 cells.
- Verify Hybrid color/depth and motion on matched retail glass, water and
  ordered effects. The hosted synthetic parity suite does not prove retail quality. General secondary mesh
  LOD/culls/merges, separate reflection-resolution/counter work, distant raster
  shadows and integrated effect-plane SVGF remain pending. Adapter timings
  cannot establish visual parity. The isolated filter, exact proxy and game
  profile remain experimental.
- Use the analyzer from the [results page](PERF-RESULTS.md) with the trusted
  kit receipt. Keep raw logs and unsuccessful runs; attach the resulting table
  and reviewer notes to PR #51. Leave it open and unmerged.
