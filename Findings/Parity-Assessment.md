> **Current status correction — 2 November 2026:** Use the [full eight-phase reassessment](Eight-phase-reassessment-2026-11-02.md) and [unsupported-path register](Unsupported-client-paths.md) for current decisions. The material below preserves historical milestones and may contain superseded absence claims. Old percentages are not current completion scores. Ordinary animated world models/scene-light submission, recovered postprocessing/gamma, per-material fog and clipped water/edge passes now exist; none of that establishes full phase parity.

**Navigation update:** The viewer now uses all 54 main-world client targets and 88 original map tiles, with a live overhead-map option and click teleport. Client landmark/tile/height checks found no horizontal coordinate flip. Local-light research advanced, but lighting rendering was not changed; hierarchy and material behavior still need verification.

**Latest iteration:** Denser distant terrain and client-derived grass noise/history were added, with synthetic GPU regressions. Existing tree, water and lighting shaders were preserved. The remaining lighting gaps below have not been closed or measured against the running game; no percentage-point parity gain is claimed.

# Perfect World Explorer — renderer parity assessment

**Baseline: 1 November 2026. Target: the supplied Perfect World 1.4.2 client, maximum visual settings, all relevant world-rendering content, no gameplay.**

**Update — grass patch transforms and experimental wind:** Client executable analysis now informs RANMAR-based patch jitter, yaw and scale variation, and the optional GPU wind uses the reconstructed per-vertex softness weight. The wave itself is approximate and off by default. No in-game visual comparison or overall percentage gain has been established.

**Previous update — client grass distance fade:** The grass GPU pass now uses the retail vertex shader’s per-vertex squared-distance alpha, per-type sight range and alpha cutout behavior. Synthetic GPU coverage matches a CPU reference, including lightmapped grass, but grass placement, wind and image-level parity remain unverified. The World setting can restore the previous visibility.

**Update — experimental regional tree lighting:** World settings can now opt into region-authored day/night ambient and directional colors and direction. The default remains unchanged to preserve the approved tree appearance. After a user report of overexposure and oversaturation, the experiment now uses bounded illumination and a restrained regional color tint instead of applying a branch shader’s 2× operation to all foliage. This is still an approximation, not the original SpeedTree materials; real-client comparison and user visual confirmation remain outstanding, and no overall parity gain has been measured.

**Update — client day/night schedule:** The world-clock blend curve is now recovered and used for baked terrain, grass and BMD lighting and regional fog. Isolated real-asset GPU arithmetic tests pass at twelve clock times. Sky and tree shader lighting remain approximations, and no matched-client visual comparison has established a quantitative parity gain.

**Update — regional fog transitions:** The new build uses the client’s authored transition duration (5,000 ms in all 99 surveyed world boxes) and linear day/night endpoint interpolation. The prior percentages remain unchanged: no matched-client visual measurement establishes a +10-point overall gain.

## How to read the percentages

These are **engineering estimates, not measurements of pixel similarity or a verified completion metric**. The weights and scores are judgments based on the implemented code, client-file surveys, reverse engineering, synthetic GPU tests, and your reports that the current build works and trees look good. There is not yet a matched-camera, matched-time visual comparison against the running client. Consequently, confidence in all visual-parity percentages is low; the target for every row is 100%. More categories or client behavior may still be discovered.

The 15 rendering categories below have illustrative weights totaling 100%. Their weighted score is approximately **44%**, rounded to **about 45%**, with a wide uncertainty of at least ±15 percentage points. It is a planning aid, not evidence that any particular screenshot is 45% correct. A 100% claim requires the validation at the end of this document.

| Rendering area | Weight | Estimated current / target | What exists | What remains for 100% |
|---|---:|---:|---|---|
| Terrain geometry, height and LOD | 10% | 75% / 100% | 88 main-world subterrains; real heights, grid and local detail; whole-world coarse terrain at 4-grid spacing, with packed GPU vertices. | Original LOD distances, transitions, seams, distant detail, culling and validation across the world and other maps. |
| Terrain materials, masks and texture filtering | 7% | 65% / 100% | 162 client textures, 4,521 masks; batch shader layer structure, native-resolution default, gradients and anisotropy. | Exact sampler, color space, render states, layer edge behavior, compression, every material path and real-client image comparisons. |
| Static scenery geometry and placement | 9% | 75% / 100% | 16,037 placements and real BMD geometry; whole-world streaming; reversed-Z and experimental back-face culling. | Verify transforms, per-mesh culling flags, original draw order, missing meshes/textures and reported buildings against game. |
| Static scenery materials and alpha | 5% | 40% / 100% | Original texture paths, RGBA atlases, cutouts, dual baked vertex colors and MODULATE2X approximation; Plume texture alpha restored. | Original material state, blend/cull flags, alpha thresholds, sorting, texture stages, special shader/material variants and image tests. |
| Lighting, day/night and local lights | 12% | 35% / 100% | Real terrain day/night lightmaps; grass lightmap approximation; BMD baked arrays; region fog data; 1,993 scene-light records now parsed. Real BMD blend tested on GPU. | Sky/sun/ambient/directional lighting, region overlap order and interpolation, scene-light query and affected classes, dynamic lights, shadows, color-space and sampler states. Scene lights are not yet applied. |
| SpeedTree geometry, motion and LOD | 9% | 60% / 100% | Real DLL-derived geometry for 59 types and 3,938 placements; three LODs and sampled leaf rocking. User reports trees look nice and the revised experimental regional lighting works perfectly. | Live wind/time behavior, exact per-instance motion and LOD/fades, billboards, material/light states and client comparison. Avoid regressing verified leaf geometry. |
| Grass and other ground foliage | 7% | 40% / 100% | Real area masks and MOXB types; GPU instancing, client textures, client-derived patch jitter, yaw and size variation, distance-fade/cutout formula, estimated terrain lightmap use and mips, optional client-derived noise/history and approximate spring wind. | Exact terrain placement, client wind timing and filter validation, lighting coordinates, draw state, density/LOD, streaming and in-game comparison. |
| Water surface, reflection and refraction | 10% | 45% / 100% | 107 water areas and masks, animated client normal maps, water shader equations, approximate reflection/refraction, depth ordering. | Actual client multipass targets, clipping, shoreline edges, Fresnel/specular, all constants, underwater interactions, transparency and visual validation. |
| Sky, weather and celestial motion | 6% | 30% / 100% | Client sky textures and seam mitigation; approximate time-dependent panorama. | Exact geometry/projection, regional skies, sun/moon, cloud movement, weather, timing and original transitions. |
| Animated EC models and other model classes | 7% | 5% / 100% | Client dependency chains partially inspected. | Implement ECM/SMD/SKI/BON meshes, animation, materials, transforms and lighting; currently 73 EC model placements not rendered. |
| Effects and particles | 5% | 0% / 100% | 2,275 effect placements identified. | Parse and render effect assets, particles, animation, blending, lighting and timing. |
| Shadows and special lighting passes | 4% | 10% / 100% | Some shadowing is baked into source lightmaps and vertex colors. | Original dynamic and projected shadows, caster/receiver behavior, scene-specific passes and shadow-map settings. |
| Bloom and other postprocessing | 4% | 35% / 100% | Optional bloom with adjustable strength and estimated client equations; underwater distortion uses recovered procedural wave. | Original constants, buffers, blur/composite order, other postprocessing, gamma and real-client comparison. |
| Fog, atmosphere and underwater | 3% | 45% / 100% | 99 main-world region boxes and day/night underwater/air fog values; procedural underwater bump and approximate fog pass. | Exact region selection and transition timing, blending, sky/water ordering, distance scaling, underwater color/visibility and comparison. |
| Visibility, depth, streaming and map scope | 2% | 60% / 100% | Main-world terrain, static objects and trees; whole-world mode; reversed-Z and atomic scenery switch. | Client culling/occlusion, accurate far LOD, smooth streaming, all regions/instances, memory optimization and visual tests at boundaries. |

## Important boundaries

- **Main world only:** dungeons, instances and other maps are not established as complete. The percentages are not a promise of coverage outside the main world.
- **Scene-light parser is a foundation, not a lighting port.** The 1,993 records load and validate, but are not yet rendered; static BMDs already carry baked light and must not be blindly double-lit.
- **Tree geometry has been user-verified as visually good**, not proven 100% identical across all 59 types, LODs, times and weather.
- **Water, fog and bloom contain recovered pieces plus approximations.** A passing shader arithmetic test does not validate the complete render pipeline.
- **Building culling is experimental.** The per-mesh original override remains unresolved; disable it in World settings if intended surfaces disappear.
- Music, minimap, launcher, controls and other UI are useful Explorer features but are not included in the graphics-renderer weighted score. Music and minimap still have the limitations documented in `TODO.md`.

## Evidence supporting the estimates

1. Client files and the PE32 executable have been inspected; full game assets are not redistributed with the program.
2. Reassembled `litmodels.pck` matches the original Drive MD5. All 16,335 world BMDs, 40,579 meshes and 7,802,464 vertices decode. A real BMD's day/night colors pass an isolated GPU blend test; this is not proof of final scene appearance.
3. Actual terrain day/night lightmap pairs, shader arithmetic, some water/fog paths, grass instancing and tree LOD have isolated tests. Tests on a software Vulkan GPU establish code behavior, not equivalence to the live client.
4. You have reported that trees look good and the latest model-facing release works. This is valuable usability evidence, but not a systematic matched-scene comparison.

## Requirements to call a subsystem 100%

- Recover the corresponding client assets, execution path, parameters, materials, render states and transitions, including exceptional cases.
- Compare against captures from the actual client at matched camera position, resolution, settings, time of day, weather and scene loading; cover representative regions, near/far LODs, underwater and day/night transitions.
- Track measurable image differences and diagnose remaining mismatches; include temporal comparisons for animation.
- Run regression tests on Windows and a real GPU without breaking previously verified locations. Include failure cases, long travel, repeated settings changes and both local/whole-world modes.
- Update the scores only when evidence justifies the change. Coverage counts, successful parsing and subjective implementation estimates are not themselves visual-parity measurements.

## Next lighting work

1. Recover the complete spatial-cell scene-light query, second dynamic-light list and time-dependent scaling; identify which render classes use it.
2. Recover the real time-of-day factor and regional ambient/directional/light colors and their transitions; connect them to trees and applicable models without double-lighting baked scenery.
3. Verify BMD per-mesh state, lightmap UV mapping, D3D9 sampler/sRGB/gamma behavior and shader constants.
4. Capture matched day, night, dawn, region boundaries and local-light examples; use those references to correct fog, sky, terrain, trees and model response together.
