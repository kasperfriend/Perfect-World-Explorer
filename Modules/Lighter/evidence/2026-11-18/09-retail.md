# 9 — Missing physical RT-17 cells

**P: 0 of 54 required renderer cells.** In the grid below each missing
entry comprises **Native, Software and Hardware** at the declared
condition. Every entry shares the [exact host/client blocker](01-sdk.md):
no attached Windows/RTX 3080 execution and no runnable `E:\pw142` client.
There is also no confirmed matching city, forest or open-distance camera
in the pinned CPU manifest. Do not infer any of those classes from an
instance capture or from generic screenspace regions.

| Scene class | Day (N/S/H) | Night (N/S/H) | Rain (N/S/H) |
| --- | --- | --- | --- |
| City | All missing | All missing | All missing |
| Forest | All missing | All missing | All missing |
| Open distance | All missing | All missing | All missing |
| Water | All missing | All missing | All missing |
| Cave / instance | All missing | All missing | All missing |
| Effects-heavy | All missing | All missing | All missing |

The [optional CPU run `44534622432`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44534622432)
passed 11 Native/Software pairs on `349b681`: 960×720 pictures, 240×180
rays, three synchronized draw/readbacks, and a fixed animation clock.
Unlike [the earlier nine-pair run](../../retail-matrix.md#completed-cpu-vulkan-matrix--run-started-2026-11-18-utc),
`a07` lightmaps were on and the manifest checked both dry and wet eye
positions. It did not run the Hardware path or fill any physical cell.

The [new CPU run `44538569462`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44538569462)
passed **12 matched** pairs on `d6c5c37`, including day/night/rain shoreline,
above-water and submerged captures, and a submerged Rain control. Both
routes loaded the authored rain mask from the pinned texture package.
See the [CPU results](../../retail-matrix.md#lightmaps-on-cpu-rerun) for
per-region/whole-image scope. The physical count is still **0/54**.

The runner verifies the actual ray extent, poses, decoded image hashes,
coverage hashes, exclusive reasons and trusted client inputs. Native can
now supply an accountable `main_streamed_v1` inventory; historical summary
counters or schema-2 sidecars do not suffice. This inventory describes main
scene submission, not water subpass raster visibility, complete world asset
ownership, or successful shaded pixels. Each route still requires verified
camera evidence and the exact physical target. `--require-complete` checks
all 54 declared cells, 50% ray extent and at least three timings per cell.

`09-retail/physical-missing.json` lists each missing route and its blocker.
The follow-up workflow emits `capture-summary.json` and compact per-map
notices for validated cases (including late a07 views) and retains partial
receipts on failure. See [CPU results and reproduction](../../retail-matrix.md)
and the [delivery record](14-delivery.md). Numeric region errors are not
verdicts about the intended subject when the PNGs have not been inspected.
