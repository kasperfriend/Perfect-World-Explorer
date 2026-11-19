# 14 — PR checks and evidence delivery

[PR #51](https://github.com/kasperfriend/Perfect-World-Explorer/pull/51) stays
open. Implementation and current fixture revision:
[`31bfeef6ff2778e5f95713643af95001cc0c76a6`](https://github.com/kasperfriend/Perfect-World-Explorer/commit/31bfeef6ff2778e5f95713643af95001cc0c76a6).
The follow-up started at `5a81c22`. The [acceptance checklist](../../REMAINING.md)
checks only Step 5. Labels: **S** synthetic, CPU Vulkan or stand-in; **P**
portable/Native on the specified physical host; **V** actual SDK dispatch
on that host. There are **no P or V runs** in this delivery.

| Check | Outcome and scope |
| --- | --- |
| [PR checks `44523578638`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44523578638) | Linux and Windows passed on `31bfeef`. Linux includes all binary targets, Rust tests, format, CPU Vulkan parity/pacing/BC/fallback, and separate normal, motion and synchronized Xvfb paths. Windows linked the pinned RR bridge with MSVC, built the x64 viewer and verified the uploaded package layout and checksum on the runner. No hardware SDK evaluation. |
| [Windows package `14529826834`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44523578638/artifacts/14529826834) | Hosted audit passed. GitHub reports SHA-256 `daf13ad82d3e3ba9612f8a095ebc9d2ee7fc50987aa5c0cae6d2441b9d14da79`. Signed download returned EOF here; not independently extracted in this workspace. |
| [Linux live travel artifact `14529426969`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44523578638/artifacts/14529426969) | Uploaded S receipts and 30 motion PNGs; local signed download returned EOF. Raw Linux job log transfer also returned EOF; job, step and artifact metadata retained. |
| `lighter-retail.yml` dispatch `423141928` | Retry at `31bfeef` refused (HTTP 403, integration permission). No new run ID. |
| [Earlier CPU subset `44424106472`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44424106472) / [artifact `14483069898`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44424106472/artifacts/14483069898) | Nine Native/Software pairs at `7c57586`; a07 lightmaps off. Historical numbers only. Retry of the non-expired artifact returned EOF. No local reinspection or latest-code shoreline run. |

The older [red run `44520590942`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44520590942)
at `67121c8` failed the Linux all-target type check (E0282) and the Windows
viewer build. It is superseded, not a successful receipt. The earlier
`44519481366` ABI link check passed before cancellation; the complete check
is now the green run above.

## Local bundle

`cache/lighter-remaining-2026-11-18.zip` collects the evidence available in
this workspace. `SHA256SUMS.txt` inside hashes each payload; validate it after
extraction with `sha256sum -c SHA256SUMS.txt`, or use `Get-FileHash -Algorithm
SHA256` on Windows. The ZIP digest is in the adjacent `.zip.sha256` receipt.
`14-delivery/` contains the code revision, CI metadata, sanitized transfer
errors, local suite outcomes and build logs. Signed URLs, raw client inputs,
SDK binaries and Windows executables are not bundled.

- **S local suite on `31bfeef`:** 1,332 unit tests passed (30 ignored, two
  known local GFX driver tests excluded), plus the targeted CPU Vulkan
  tests and day/night T2LM binding fixtures. The narrowed lavapipe sampled
  BC fixture exited 139 locally; the hosted stock-driver fixture passed.
- **S comparisons:** all five Native/Software/full-ray PNG controls match
  prior bytes; retained path/still Software PNGs are tooling checks. The
  a07-record PNGs are from synthetic geometry, not retail screenshots.
- **Pinned offline data:** checksums, format/census output and height-grid
  reports, not raw files or render verdicts. `a01` fails the selected
  far-stride tolerance. `a02` height is flat. Forsaken World S38/S40 do not
  qualify as Perfect World retail inputs.

## What still blocks acceptance

Run the physical suite on **Windows x64 / NVIDIA GeForce RTX 3080 10 GB**
with the NGX runtime, validation layers, the pinned optional SDKs and the
matching `E:\pw142` install. Start with [provider smoke](01-sdk.md) and
[window/recovery](04-window.md), then save matched [previews](06-captures.md),
[motion](07-motion.md) and [timings](08-performance.md). Complete the
verified-camera matrix with exhaustive Native inventories: **0/54** physical
cells are complete today. No evidence here enables Hybrid, sampled BC, or
DI/GI reuse by default. Separately, an integration with `actions:write` or a
maintainer with workflow-dispatch access can rerun the optional CPU subset
on the updated lightmaps-on manifest. That run still would not close a
physical cell.
