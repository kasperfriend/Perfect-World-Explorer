# A7 — Effect-plane filter prototype and blue noise (S)

`PW_RAY_BLUE_NOISE=1` with the game profile uses a build-generated,
16×16 toroidal void-and-cluster rank tile for soft sun and short diffuse
bounce. Phase rotation affects stochastic transport, not coverage/camera
rays. The generator checks that each rank occurs exactly once.

`svgf.wgsl` is deliberately a separate reflection-RGB/sun-visibility
prototype, not a filter on full-frame composited radiance. It reprojects
with raster motion, checks depth, normal and material ID, keeps first and
second moments and applies five depth/normal/variance-stopped à-trous
steps. The isolated GPU fixture compares to a known signal expectation and
checks translated, invalid-motion and changed-material history slots.

Still open: integrate separate reflection/sun signals and true raster
material/motion histories into Hybrid, compare to converged retail-frame
references and measure moving-camera ghosting against the current filter.
Neither the prototype nor the noise tile proves production denoiser quality.

On llvmpipe Mesa 25.0.7 (LLVM 19.1.7), the 32×16 isolated fixture reduced
linear reflection and sun-visibility RMSE from 0.180000 to 0.035973 after
reprojection and five iterations. A one-pixel translation reused valid
history; off-screen and changed-ID samples reset exactly. The current
renderer’s moving-camera ghosting comparison remains open (this was not
that renderer or a reference render of a retail scene).
