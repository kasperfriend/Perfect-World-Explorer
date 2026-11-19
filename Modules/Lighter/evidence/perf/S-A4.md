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
