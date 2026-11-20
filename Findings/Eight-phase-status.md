# Eight-phase implementation status

Updated 2 November 2026. The original requested requirements are retained below.
Unchecked means **partial or not implemented**, not absent by default.

P5.03 scope decision (4 November 2026): at the user’s direction, the sun/moon point is closed as **Implemented for the recovered disc path, with the screen-space flare chain documented as the remaining limitation** rather than claimed as native halo parity. The user's own render check of the delivered build (correct sky, `sunshine.bmp` glow on the sun, no moon halo) confirmed the behaviour the recovery predicts; the retail moon halo lives in the `CECSunMoon::Init` flare chain, which stays documented in section 3.2 of the P5 report and unimplemented.

P4.07 scope decision (3 November 2026): at the user’s direction, native client capture is skipped and the item is closed as **Implemented for the best-supported viewer path with limitations documented**, not as proof of exact native parity. The original parity gaps remain explicit in the Phase 4-C report and are deferred, not claimed solved. Checked test/preservation/package/policy tasks mean their stated deliverable exists, NOT that their parent phase has native parity.
Requirement-level acceptance checks: [Phase 1 completion checks](Phase-1-completion-checks.md). Existing partial
implementations are documented in the research logs; no phase is complete by inference.

Execution gate: **Phase 1 open**. See [Phase 1 audit](Phase-1-audit-2026-11-02.md).
No phase is fully complete. The [full 76-point reassessment](Eight-phase-reassessment-2026-11-02.md) records implemented portions, evidence and specific gaps for every requirement. The [unsupported-path register](Unsupported-client-paths.md) remains nonempty.


## Phase 1 — Complete the lighting and display pipeline

- [x] P1.01 — Trace the client’s complete scene-light selection path, including the static grid, dynamic candidates, priority rules, range checks, flags and day/night scaling.
- [x] P1.02 — Identify every object class that receives scene lights and reproduce its light assignment, attenuation, color, ambient and diffuse calculations.
- [x] P1.03 — Integrate those lights into the appropriate model shaders without adding them a second time to already-baked building or terrain lighting.
- [x] P1.04 — Finish the client’s regional ambient, directional and sun/moon lighting calculations, including region overlap priority, transitions and time-of-day behavior.
- [x] P1.05 — Connect the recovered sun-direction curve only where the client actually uses it for material lighting.
- [x] P1.06 — Complete terrain and grass lightmap selection, coordinates, blending, sampler states and shader arithmetic.
- [x] P1.07 — Finish baked-model day/night color interpolation, material flags, alpha behavior and any client-specific light overrides.
- [x] P1.08 — Recover the remaining bloom constants, texture coordinates, render-target formats, effect transitions and pass order.
- [x] P1.09 — Complete the gamma chain: client settings, fullscreen versus windowed behavior, color conversions, output encoding and interaction with postprocessing.
- [x] P1.10 — Replace the approximate screen-space fog with the client’s applicable per-pass fog behavior, including regional transitions and underwater conditions.
- [x] P1.11 — Keep the approved tree appearance intact while tracing its actual lighting separately; replace the fallback only when the complete tree material path is available.
- [x] P1.12 — Add automated tests for light selection, transitions, shader arithmetic, color-space conversions and postprocess passes.

## Phase 2 — Complete terrain and ground materials

Nine work points (re-cut 2 November 2026; the Phase 2 scope is unchanged, only its
boundaries are now non-overlapping and independently verifiable). Execution order
and ownership: [Phase 2 plan and agent prompts](Phase-2-plan-and-agent-prompts.md).

- [x] P2.01 — Reproduce terrain block and area residency and draw selection: which T2MK areas and height blocks are resident for a camera position, which are submitted, how the streaming window grows and evicts, and explicit failures for missing or malformed blocks.
- [x] P2.02 — Reproduce terrain resolution levels and their transitions: the local full-resolution mesh, the distant coarse mesh, the client's own level thresholds and hysteresis, and the geometry and vertex path used at each level.
- [x] P2.03 — Replace approximate terrain seam stitching with the client's own edge and neighbour rules, including block boundaries and boundaries between resolution levels.
- [x] P2.04 — Complete every terrain shader variant and pass state: opaque and alpha surfaces, near and far vertex paths, stage setups, cull/depth/blend/alpha-test state, and the hardware fallback paths the client actually uses.
- [x] P2.05 — Complete terrain lightmap selection and arithmetic: T2LM/T2LM1 pair choice, tile addressing and clamp/wrap behavior, day/night blend, encoded-space modulation, and how the result combines with regional light.
- [x] P2.06 — Verify and implement every terrain material layer: layer axes and projections, ordering, per-area counts, mask formats and resolutions, and the world-to-texture mapping for each authored area.
- [x] P2.07 — Match terrain texture sourcing and sampling: source dimensions, mip chain generation, addressing modes, anisotropic filtering, per-material sampler settings and every source format in the archive.
- [ ] P2.08 — Implement terrain specials and detail surfaces: holes and visibility flags, special surface cases found in client data, and maximum-quality road and city detail including Archosaur's high-resolution surfaces.
- [ ] P2.09 — Keep terrain consistent across views (main, reflection, clipped refraction, minimap and full-world map) with one snapshot per frame, and keep boundary, shader-variant, layer-selection and near/far LOD-continuity regressions running in CI.

## Phase 3 — Complete buildings, rocks and other static models

Nine work points (re-cut 2 November 2026; the Phase 3 scope is unchanged, only its
boundaries are now non-overlapping and independently verifiable). Execution order
and ownership: [Phase 3 plan and agent prompts](Phase-3-plan-and-agent-prompts.md).

- [x] P3.01 — Finish BMD container and format coverage: every model and mesh version, chunk layout, transforms, texture references, baked arrays, per-mesh and per-material flags, with explicit errors for unsupported versions and truncation.
- [ ] P3.02 — Reproduce every static-model material and texture-stage state: stage setups, alpha test, blending, transparency, material colors, two-sided flags and the baked-light combination, as a recovered table rather than per-asset special cases.
- [ ] P3.03 — Handle missing, shared and animated texture references with the client's original lookup rules, page sharing and fallbacks, and explicit dependency errors.
- [ ] P3.04 — Trace and implement the client's actual culling, winding and two-sided-material rules instead of a global approximation, including mirrored or negative-scale placements.
- [ ] P3.05 — Identify and reproduce the real causes of building Z-fighting and overlap: depth bias, draw order, duplicate placements and overlapping geometry behavior.
- [x] P3.06 — Reproduce static-model visibility, distance selection, LOD and regional loading and eviction.
- [x] P3.07 — Complete reflection, fog, minimap and map rendering for every supported static-model material.
- [ ] P3.08 — Bound atlas residency so full-resolution assets do not all have to stay resident: packing, cross-page deduplication, eviction and streaming.
- [x] P3.09 — Keep regression coverage for model versions and truncation, culling and sidedness, texture stages, depth conflicts and day/night baked lighting running in CI.

## Phase 4 — Complete SpeedTree and grass

- [ ] P4.01 — Recover and reproduce the client’s SpeedTree material and lighting paths for branches, fronds, leaves and billboards.
- [ ] P4.02 — Complete the original runtime’s geometry, LOD thresholds, transitions and billboard selection without reverting to camera-facing sprites where real geometry should be used.
- [ ] P4.03 — Replace the sampled leaf-motion approximation with the client’s actual runtime animation behavior where technically possible.
- [ ] P4.04 — Reproduce the client’s wind modes, timing, camera inputs and per-tree parameters; do not invent movement for components the client leaves static.
- [ ] P4.05 — Complete grass placement, terrain height alignment, density, type selection, patch visibility and distance LOD.
- [x] P4.06 — Finish the client’s grass wind history, shader constants, fading, alpha tests and material behavior.
- [x] P4.07 — Implement the best-supported vegetation material path from available evidence and document deferred native-only parity gaps (user-approved scope closeout; not a native-parity claim).
- [x] P4.08 — Keep existing approved tree rendering available until the replacement path is complete.
- [x] P4.09 — Add tests for tree LOD boundaries, foliage animation, grass placement, wind, alpha coverage and lighting.

## Phase 5 — Complete sky, weather and atmospheric environment

- [x] P5.01 — Reproduce the client’s sky geometry, texture layers, panorama mapping and seam handling.
  - Recovered and harness-verified: curtain 70 vertices/288 indices (`0x79a330`, stream `+0x584`), cap 145/792 (`0x79a810`, `+0x588`), the cylinder/sphere projections, the `Textures\Sky\%s` texture layers, and hard joins (duplicated column at azimuth 90°, `z`-mirrored second draw at 0°/180°) in `src/sky_geometry.rs`; no crossfade remains. The decoded `shaders.pck` holds no sky program and neither draw path selects one, so the 38.16°–72.5° band its buffers leave empty is a property of the client's own meshes; `Region::CapEdgeGap` reports it. See `Findings/P5-sky-geometry-daynight-celestial.md`.
- [x] P5.02 — Implement the actual day/night sky blending and time-dependent texture selection.
  - Recovered: `CECSunMoon::Update` `0x4b60e0` (elevations, `+0x74` set selector, `+0x70` night strength), switch length `0x4b6250` (2,400,000/1,800,000 halved), timer `0x79b150`/`0x79a040`/`0x79b180`; replaced the smoothstep schedule with the linear timed switch in `src/sky_time.rs`. The set mapping is decoded too: `TransSky` `0x79ac30` prefixes `Textures\Sky\` to the three configured paths, `element_client.cfg [DefEnv]` names `clear` for all three and `autofamilyconfigs/sky.lst` lists the named sets, so the shipped default keeps one image and changes the lighting. The viewer's `night2` night set stays as a stated deviation until the `+0x70` lighting consumer exists.
- [x] P5.03 — Complete sun, moon, halos and related celestial geometry, positioning, visibility and blending.
  - Recovered and implemented: billboard render `0x4b3e00`–`0x4b41ee` (distance 100.0, half extent 1.0 → 1.146°, per-object elevation gate, shared azimuth, `fade << 24` alpha) in `src/sky_celestial.rs` with explicit failures and a World-tab status line; the `sun.sdr`/`moon.sdr`/`sunshine.sdr` effect files give texture × vertex-diffuse × texture-factor; the sun's stage-1 factor `(1 - [+0x70]) * 5.0` (×0.8 on the night set) is reproduced, tested and consumed as `SkyFrame.sun.w` scaling an additive `sunshine.bmp` quad. The moon's `.w` is 0 because `0x4b4049` halves its `+0x48` colour and scales nothing, so the moon carries no shine quad and `moonflare.bmp` is held for the flare chain. Closed with the documented limitation: the discs inherit the pass blend state (the celestial render issues no `SetRenderState` call), and the `CECSunMoon::Init` flare pipeline (three buffers, flare-alpha accumulation target, `ps/solarflare_14.txt`, `flares.bmp`/`sunflare.bmp`/`moonflare.bmp`) remains documented in `Findings/P5-sky-geometry-daynight-celestial.md` §3.2 rather than implemented. Render check 4 November 2026 matches the recovery: correct sky, sun `sunshine.bmp` glow, no moon halo.
- [ ] P5.04 — Connect region-authored sky settings and transition behavior to the world environment.
- [ ] P5.05 — Implement client weather visuals and their timing, including clouds, precipitation and other atmospheric effects present in the supplied world.
  - Partial runtime now exists: `world.cld` groups feed atlas-textured cloud billboards, and EC-BSD flags select rain/snow particles advanced by the recovered strict 33 ms clock. It is not native parity: cloud topology/sizing and particle randomness, lifetimes, wind, collision and transitions remain approximations; do not mark complete. See `Findings/Phase-5B-Region-Weather-Fog-report.md`.
- [ ] P5.06 — Complete regional fog, atmospheric colors and visibility behavior across normal, underwater and reflected views.
- [x] P5.07 — Reproduce environment transitions when crossing region boundaries or changing the time slider.
- [ ] P5.08 — Ensure sky and weather behave correctly in reflections and do not shift incorrectly with camera altitude.
  - Sky reflection/altitude invariance is implemented; cloud and precipitation passes are wired into reflection and above-water refraction, but their camera-centered precipitation model and native cloud geometry are not parity-validated.
- [x] P5.09 — Add tests for time boundaries, region transitions, celestial placement and sky seams.

## Phase 6 — Complete water and underwater rendering

- [ ] P6.01 — Trace the client’s water renderer and implement its original render targets, geometry, shaders and pass sequence.
- [ ] P6.02 — Reproduce water material selection, area masks, heights, shoreline behavior and visibility.
- [ ] P6.03 — Implement the original wave animation, normal maps, scrolling and time-dependent parameters.
- [ ] P6.04 — Complete reflection and refraction using the client’s camera transforms, clipping, depth handling and texture coordinates.
- [ ] P6.05 — Replace approximate reflection and refraction safeguards with the recovered client rules.
- [ ] P6.06 — Match water transparency, fresnel or equivalent material calculations, highlights, color and interaction with regional lighting.
- [ ] P6.07 — Complete underwater entry detection, distortion, fog, color and transition behavior in the correct order.
- [ ] P6.08 — Handle multiple visible water heights efficiently without losing their individual reflections.
- [x] P6.09 — Add tests for water masks, multiple heights, reflected cameras, depth ordering and underwater transitions.

## Phase 7 — Add animated world models and visual effects

- [ ] P7.01 — Finish parsing and rendering the client’s EC/animated world models, including their geometry, materials, skeletons and animation data.
- [ ] P7.02 — Implement their animation playback, timing, transforms, LOD and visibility.
- [ ] P7.03 — Connect the scene-light system from Phase 1 to the model classes confirmed to receive it.
- [ ] P7.04 — Implement world effects and their dependent resources: particles, sprites, trails, animated textures and other client effect types. *(7 November: the animated graph now plays for every world-owned root — `gfx_owner::Manager::effect_quads` into `world_models_gpu::World`'s own `gfx_effect_gpu::Draw`, in the world receiving-model pass, behind a World-tab switch that defaults on. What is left under this item is the sound (170), model (160) and curve (190) element types and the authored fade/stop phase.)*
- [ ] P7.05 — Reproduce effect attachment points, timing, blending, depth behavior and region-based activation.
- [x] P7.06 — Add any remaining world-object formats or specialized shaders discovered during the client-file census.
- [ ] P7.07 — Integrate animated models and effects into normal views, reflections and fog as the client does.
- [x] P7.08 — Keep gameplay systems out of scope: render world content without adding combat, quests, networking or player simulation.
- [x] P7.09 — Add tests for animation sampling, effect lifetimes, resource loading and material blending.

## Phase 8 — Full-world integration, optimization and delivery

- [ ] P8.01 — Complete a census of world files, object types, materials and shaders; resolve remaining unsupported or silently skipped assets.
- [ ] P8.02 — Implement client-equivalent visibility and streaming so the entire world remains explorable without loading every full-resolution texture into GPU memory at once.
- [ ] P8.03 — Optimize GPU memory, draw batching, culling, texture residency and background loading while retaining maximum-quality assets.
- [ ] P8.04 — Complete the realistic full-world map and minimap so their scenery, orientation and loading behavior remain consistent with the 3D world.
- [ ] P8.05 — Finish zone music and environmental audio selection, transitions, volume behavior and remaining client audio formats.
- [ ] P8.06 — Preserve and polish the launcher, remembered game-folder selection, loading screen, controls, settings, teleport list and map interaction.
- [ ] P8.07 — Ensure all settings are persistent, have clear defaults and apply consistently to the main view and relevant secondary passes.
- [x] P8.08 — Expand automated coverage across parsers, shaders, render passes, world streaming, navigation, audio and Windows packaging.
- [x] P8.09 — Produce the single Windows executable with the required embedded components, plus the complete source.
- [x] P8.10 — Maintain an explicit list of any unsupported client paths; 100% parity is reached only when that list is empty, not by assigning an estimated percentage.
