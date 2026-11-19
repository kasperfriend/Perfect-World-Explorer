# A2 — Bounded first-use Hardware preparation (S)

Opt in with `PW_RAY_PACED_STARTUP=1`. A worker lowers WGSL and creates the
cold Hardware (and requested raster-primary) pipelines before the first
swapchain image. The render thread polls it without submitting from the
worker. The worker keeps a strong reference to the device until its state
is published or dropped. Initial static residency then uses the existing
paged upload lane: at most 1 MiB of copies or one static BLAS per submit,
waiting for the prior fence on later polls. Pipeline, BLAS, copy and chunk
completion are reported. Dynamic AS, TLAS and the first trace still use the
first draw; a single indivisible BLAS is not a GPU time guarantee. Actual
per-submit timestamps, when supported, are printed in the startup log.

The watcher tracks a strictly increasing progress ordinal, not a repeating
stage label or arbitrary text. Its independent total-load deadline remains.
The Vulkan cache already validated size, UUID/vendor/device and header;
it now takes `PW_CACHE_DIR/vulkan-cache` when supplied and logs a cache hit.
Without that variable, the prior directory and cache validation stay in use.

S: `first_frame_progress_requires_a_strictly_advancing_ordinal` and the
existing stalled-child watchdog passed (11 handoff unit tests). The CPU
query `lighter_paced_cold_startup` published its initial scene after one
copy and three separate BLAS batches, then drew a non-background depth.
Native's existing 960×720 S PNG matched byte for byte (`1b20187d05a3…`).
Local raw logs: `cache/lighter-perf/S-A2/`.

A CPU queue timestamp does not certify a Windows TDR margin. The sub-1 ms
submit goal, large-mesh/timeline behavior and actual restart cache reuse
still require the player's target log.
