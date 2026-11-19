# A9 — Adaptive GPU timing and specialization (S)

Hardware automatic ray resolution now uses one fresh Vulkan timestamp
sample after the fence, with no `map_async` and a 50–100% scale controller.
`PW_RAY_ADAPTIVE_TIMESTAMPS=1` selects a 16.6 ms controller target when
Automatic ray resolution is already enabled. Its measurement is raster
visibility + ray traversal, not total present time; this does not promise
16.6 ms cadence. Shader extent/allocation limits still take precedence.
SDK and fixed half-size game profile runs do not adapt. Readback profiling
is confined to explicit captures (`PW_LIGHTER_TRAVEL_SYNC=1`, headless
capture, or `PW_RAY_CAPTURE_TIMINGS=1`); live Vulkan querying uses no wait.

Naga override tests lower media, reference, emissive and transmission
variants into SPIR-V, including a combined specialization. Those stripped
variants are not selected for real worlds by default, nor without a content
admission rule. Existing bounds checks and Single Path's compensated
roulette after bounce 2 remain; full Reference/progressive and the stable
primary-water two-leg exception stay available. No matched Win32 frame
pacing/quality result or adaptive world-raster scaling is established.
