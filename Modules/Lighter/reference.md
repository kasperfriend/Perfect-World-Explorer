# Reference lighting (Step 18)

Enable **Reference** in Lighter or `PW_PREVIEW_REFERENCE=1` in a headless
capture. The preset uses 32 samples, up to eight continuations and 100% ray
resolution subject to the same image storage cap. It enables GGX/Lambert,
visibility and relighting and disables the denoiser, history and the terminal
ambient approximation. Switching it off restores the interactive settings.

A 64×32 equal-solid-angle grid of the authored roof and panoramas supplies an
RGB lighting lookup and a luminance CDF for each sky set. This is a compact,
undrifted proxy for diffuse/rough illumination. Primary and delta paths still
see the full animated sky. Crossfades mix both PDFs and colors; a 1% uniform
floor keeps the proposal defined even in black cells. The analytic fallback
uses a uniform sphere. The display sun disc is excluded from these lighting
samples because the directional sun is tested separately.

Opaque base-channel emitters in each packet get an area × approximate emitted
power CDF, using world-space area, sides, authored emission and the last mip's
mean linear color. The connection samples the actual triangle UV and vertex
tint, using the same top-level filter as a reference continuation hit. A
triangle-area PDF converts to solid angle with distance squared / emitter
cosine. The resident and moving distributions mix by their total power.

One sky and one triangle proposal each compete with the BSDF continuation via
balance MIS. A terminal hit uses weight one for the proposals because there
is no BSDF continuation there. Delta glass/water scatter resets that competing
PDF; a straight probe does not bend or pass through a delta surface.

Scope: at most 8,192 registered triangles per packet; unregistered emitters
(including transparent GFX, cutouts and special multi-stage materials) may
still contribute through a hit. Legacy ordered layers are not a participating
medium or part of the MIS visibility model. This is a bounded offline-quality
integrator, not a ground truth for all client blend/fog/weather behavior.
