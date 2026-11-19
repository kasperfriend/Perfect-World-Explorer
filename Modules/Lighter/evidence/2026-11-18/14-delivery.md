# 14 — CI, receipts and evidence delivery

[PR #51](https://github.com/kasperfriend/Perfect-World-Explorer/pull/51)
remains **open and unmerged**. This follow-up began at `5a81c22`.
The local regression and pinned CPU retail revision is
[`d6c5c37248fe9e8235e91cba5889797483ab39f4`](https://github.com/kasperfriend/Perfect-World-Explorer/commit/d6c5c37248fe9e8235e91cba5889797483ab39f4).
`94e31a0` changes only the BC fallback test expectation. The [acceptance
checklist](../../REMAINING.md) distinguishes **S** (synthetic, CPU Vulkan or
stand-in), **P** (physical Native/portable) and **V** (physical SDK). There
are **no P/V runs**, regardless of a hosted Windows build's outcome.

| Check | Outcome and scope |
| --- | --- |
| [Earlier PR checks `44534634921`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44534634921) | Both jobs green at `349b681`. The Windows package was built and audited on the runner. [`44523578638`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44523578638) and [`44525009463`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44525009463) are historical green runs. |
| [CPU subset `44534622432`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44534622432) | Eleven matched Native/Software cases on `349b681`, with a07 lightmaps enabled. Signed transfer of artifact `14536516455` returned EOF. Keep runner metadata and known metrics, not an uninspected visual verdict. |
| [PR checks `44538579897`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44538579897) | Windows passed at `d6c5c37`; Linux hit a stale BC1 fixture assertion (charged image bytes remain allocated after sampling falls back). `94e31a0` corrects the assertion without changing the renderer. The red run is not counted as a pass. |
| [PR recheck `44539961277`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44539961277) | **Linux and Windows passed** at `94e31a0`. Linux covered BC1 replacement/fallback, BC5/7 sampling, paced residency, ambient fallback, and separate synthetic Xvfb travel, synchronization and motion. The [Windows x64 package `14539246204`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44539961277/artifacts/14539246204) passed its MSVC bridge link and hosted ZIP audit, not physical SDK dispatch. Artifact SHA-256 `5fa4abd96a995a92f52971a9b0b31147bdb2e42d542b18d407fb6b74b7fa570c`. |
| [CPU subset `44538569462`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44538569462) | CPU Vulkan **passed all 12 matched pairs** at `d6c5c37`. [Artifact `14538576299`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44538569462/artifacts/14538576299), SHA-256 `d242bac9913932f0a32179c89be21c12bc01e9b65d6092d078730d8599c71e87`. Its signed local transfer returned EOF. The run index and nine complete case receipts are retained; the a07 notice hit the API text cap. The workflow now batches JSON under that limit. |

Earlier `lighter-retail.yml` manual dispatch was refused with HTTP 403.
Workflow/manifest changes triggered the separate CPU evidence runs via
branch push. A run aborted by concurrency was not a success. The repo's
self-hosted runner inventory was also forbidden to this integration; that
is not proof that no matching runner exists. See [host access](01-sdk.md).

## Local bundle

`cache/lighter-remaining-2026-11-18.zip` contains the available evidence from
this workspace. The archive manifest `SHA256SUMS.txt` hashes every payload;
verify it after extraction with `sha256sum -c SHA256SUMS.txt`. The adjacent
`.zip.sha256` hashes the ZIP. `14-delivery/` records tested revisions,
terminal job/step results and artifact digests from GitHub, local logs,
and transfer errors when signed downloads are unavailable. Raw client
packs, SDK binaries, Windows executables and signed URLs are not included.

- **S at `d6c5c37`:** 1,339 viewer tests passed locally (33 ignored; two
  known local GFX driver cases excluded) plus the targeted sampler and
  capture fixtures. All 21 Python evidence-tool tests passed. The exact
  five matched Native/Software/CPU-query PNGs passed again; prior portable
  controls and their settings are retained separately.
- **Pinned data and S fixtures:** production far-mesh error and packed rear
  ray geometry on three PW 1.4.2 heightfields, a07 record binding and a
  diagnostic extinction probe, main-stream census, matched Clear/Rain
  controls and sampled BC. The bundle labels proof scope for each result;
  none is a physical parity or performance claim.

## Next run needs the exact host

On **Windows x64 / RTX 3080 10 GB / NVIDIA NGX / `E:\pw142`**, use pinned
SDKs and Vulkan validation. Start with [positive provider smoke](01-sdk.md)
and [window/recovery](04-window.md); then gather [matched previews](06-captures.md),
[motion](07-motion.md) and [seven synchronized timing routes](08-performance.md).
Use the same tested executable, verified cameras and matching client inputs
for the [54-cell matrix](09-retail.md) plus live terrain, water and renderer
controls. These prerequisites, not hosted CI permission alone, block the
remaining application acceptance. Hybrid, sampled BC and DI/GI reuse stay
opt-in.
