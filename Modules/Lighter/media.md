# Segmented air/water transport (Step 17)

Each path carries the current water export's medium ID, or air. Intersections
with the shared wet surface choose the medium on each transmitted branch;
reflection keeps the incident side. A submerged camera starts in the
nearest wet surface above it. The shader walks that body's packed cell grid
until the ray reaches the top, cutoff, or its first dry cell, so an underwater
ray can illuminate or see a dry wall without applying water absorption over
the rest of the path. This uses the actual wet-cell footprint, not a box
that fills holes in it. Untagged test sheets use an explicit open medium.

The authored underwater fog span maps to 5% Beer–Lambert transmission in
the clearest RGB channel after one span. The normalized linear fog color
biases the RGB extinction coefficients; it also provides a constant
in-scatter color. This is an interpretation of the client's fields, not
measured water absorption/scattering. **Air fog stays the authored linear
ramp**, applied to the air part of each segment. The sky panorama already
includes haze and is not replaced by a second air ramp on a miss.

The **sun-to-water specular link** is separate from the camera BSDF. For an
underwater receiver it computes the inverse Snell direction for the flat
interface, validates a wet exit within the same body, attenuates the water
leg with Fresnel/RGB extinction, and checks occlusion on both sides. Thus a
blocker above the surface can shadow the bed without being visible to the
camera. It does not project an animated caustic texture. This is the
flat-interface case only: wave focusing, general specular caustics, lateral
entry from below a dry cell, overlapping media, and refracted point-light
connections aren't solved. Lateral exit changes medium without inventing a
vertical glass wall at a terrain mask edge.

`ddee31d` passed both Software and CPU Vulkan pixel suites (Mesa 25.0.7),
18 Lighter units, and the streamed-world test. The segmented fixture checks
1.5 units of wet travel followed by one unit of dry fog at exact wall depth,
two units of RGB extinction on the submerged sky ray with an optical exit,
and independent four-unit water-leg attenuation of bed lighting. Adding a
blocker above the flat interface removes the sun term at unchanged bed depth.
These are synthetic tests, not a retail caustic or fog match.
