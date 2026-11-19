# A4 — Raster primary contract (S)

`PW_PREVIEW_HYBRID_PRIMARY=1` (preview) and the world-reload Hybrid setting
remain opt-in. The Hardware raster entry point now enforces the same closed
camera ray interval as the full-ray entry point. It rasterizes the actual
packet at one sample per pixel: cutout coverage uses the ray acceptance rules,
and a solid water or glass interface supplies the first hit only. Its
reflection, refraction and subsequent boundary queries stay in ray transport.
Ordered transparent sheets, authored GFX overlays and depth-writing effects
remain ordered ray work, not flattened into an opaque raster depth. Missing
raster resources, unsupported extents and multiple camera samples take the
full-ray path. Destination-color/Warp exclusions are unchanged.

Travel adds `primary_solid_queries_traced` and `primary_solid_queries_skipped`
per drawn ray frame. They count first-solid query slots (trace extent × spp,
only if the packet contains the solid mask), not ray-box tests, shading rays
or ordered effect queries. They are assigned from the executed route, not the
requested flag; `lighter_raster_primary_parity` checks both branches.

The [hosted Linux job at `0b4b485`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44568900822/job/135718210202)
passed its `Trace Lighter test scenes on software Vulkan` step, whose
fail-on-error loop explicitly runs `lighter_raster_primary_parity`. The
fixture compares 19², 25² and 43² full-ray/Hybrid RGB8 (≤1 channel level)
and reverse-Z (<1e-5) with an alpha hole, a sheet and moving geometry.
It also checks water, terrain and effect/pose controls plus forced allocation
failure/MSAA fallback. No physical/retail glass or timing proof follows.

Local sandbox trials did not finish Hybrid: narrow-vector exited 139;
both default-width trials (one with one llvmpipe thread) exited 137 after
logging a full-ray draw. Cause not established. Those raw logs are kept in
`cache/lighter-perf/S-A4/parity*.log` and
`cache/lighter-perf/S-final/hybrid-default-vector-1thread.log`.
