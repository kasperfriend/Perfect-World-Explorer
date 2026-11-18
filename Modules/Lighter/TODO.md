# Lighter — full ray-tracing roadmap

Lighter already traces camera, shadow, reflection, refraction and bounded indirect rays in world space. Software BVH and Vulkan ray queries share the same scene and shading. This checklist tracks three different goals: **scene coverage** (a ray can hit the asset), **client fidelity** (the hit has the right geometry, material and behavior), and **reference transport** (light arrives by the right paths and converges). Finishing one does not establish the others. HUD, menus, lens effects and the final display transform do not need to be ray traced.

Status: `[x]` means implemented and verified at the scope on that line; unchecked items remain open. Synthetic software Vulkan and physical GPU evidence must be reported separately.

## Ordered implementation — Phase 0: performance first

The scene, fidelity and transport sections below retain their original RT item
numbers. They do not take priority over the numbered implementation workflow.
Complete each performance step before working on the next one; the first
coverage step after these five is RT-01b (Step 6).

1. [ ] Remove live GPU completion waits, use later-frame callbacks with a bounded readback ring, and verify the viewer and explicit headless PNG. See [the caller audit and regressions](performance-waits.md).
2. [ ] Cache static BLAS by stable chunk/geometry identity, refit posed BLAS in place, and dirty TLAS/software BVH only when their inputs change. Verify zero builds on camera-only movement in a static scene.
3. [ ] Retain buffers, textures and bind groups across frames. Grow capacity geometrically and update in place. Verify no steady-state resource creation.
4. [ ] Audit renderer/scene clones; borrow immutable data, share lifetimes with `Arc`, and use cached asset IDs. Check frame cost as chunk count grows.
5. [ ] Default to 1 spp, 1 bounce and half ray resolution. Reproject and blend history with a camera-cut reset, upscale on presentation and keep higher quality opt-in.

## P0 — Scene coverage and residency

- [x] **RT-01a: Extend static BMD residency beyond the native ornament load distance.** Both backends use the bounded, direction-independent extension and retain the native renderer's residency and map views. Archive-backed pixel tests cover cutouts, shadow, water reflection/refraction, visibility flags, eviction and reload. See [implementation and limits](README.md#static-scenery-residency).
- [ ] **RT-01b: Scale static coverage to entire maps.** Build a world manifest with representative bounds rather than just placement origins; prioritize chunks by receiver, light and secondary-ray reach. Reuse resident geometry/BLAS across streams, add distant geometry or conservative proxies, and report unloaded/over-budget/unsupported assets separately. Test joins across chunk, map and teleport boundaries.
- [x] **RT-02a: Stitch the coarse far grid to the resident terrain perimeter.** The ray mesh shares the actual near edge heights without overlapping terrain. Non-flat edge fixtures check all four sides; grazing-ray pixel tests check depth and hit order from both sides of the join.
- [ ] **RT-02b: Validate and improve distant terrain fidelity.** Quantify the coarse far mesh against native heights and silhouettes on actual maps, including streaming and grazing views. Include terrain outside the camera frustum and distinguish intentional cutoff from missing data.
- [ ] **RT-03: Cover posed world, Walker and Populator models.** Give world animated models and attached effects a ray-aware feed; keep first-person-hidden geometry in appropriate secondary queries; preserve pose, attachments and stable identity. Inventory unsupported morph/supple/muscle payloads, critter classes, missing packages and any intentionally excluded asset types. Test off-camera animation, owner transitions and streaming.
- [ ] **RT-04: Account for scene coverage.** Produce an auditable per-map inventory by geometry class and failure reason (including asset/texture fallbacks). Add counters and representative real-client captures; a populated AS alone is not evidence that every authored asset was loaded.

## P1 — Assets and client fidelity

- [ ] **RT-05: Represent textures completely.** Preserve authored mip chains and source resolutions subject to paging; use linear-light, ray-footprint and grazing-angle filtering. Separate color, coverage and linear data textures. Track resolution/mip fallback rather than conflating it with a missing texture.
- [ ] **RT-06: Expand material semantics.** Carry authored albedo, normals/tangents, specular color and roughness, emissive, opacity, sidedness and transmission where present. Include posed face/eyes/mouth and secondary texture stages, source blend/depth modes and scrolling surfaces. Separate baked light from reflectance; explicitly choose legacy baked appearance or physical relighting for buildings, terrain and grass area lightmaps. Validate at day and night.
- [ ] **RT-07: Reproduce vegetation behavior.** Match SpeedTree branches/fronds/leaves, wind, rocking and LOD transitions, grass instance movement/fade, area lightmaps and authored coverage. Verify secondary silhouettes and transmitted light; do not make leaves disappear because they face the main camera.
- [ ] **RT-08: Cover water geometry and authored effects.** Trace separate static water meshes, per-body extents/exits and shoreline geometry/overlays where appropriate. Reproduce native simple-water art and document which normal layers only affect shading and which change the intersection. Check different bodies, wet/dry cells, edges, shallow water, submerged cameras and water-purpose visibility flags at later bounces.
- [ ] **RT-09: Complete world GFX and weather.** Preserve ordered depth/alpha/additive/other supported blends, decal placement, animation, lights and the intended behavior of billboards in secondary views. Decide when `Warp` is a display distortion versus optical transmission. Cover authored cloud, rain and snow inputs. Keep purely screen/lens effects in a documented presentation stage.

## P2 — Lighting and reference-quality transport

- [ ] **RT-10: Improve direct visibility and emitters.** Honor applicable static-light flags and dynamic owner conditions. Add finite local emitters, fractional/colored visibility through transmissive surfaces and consistent cutout behavior. Verify penumbra, emitter distance, shadow precision and illumination outside the main frustum.
- [ ] **RT-11: Represent reflection and transmission physically.** Add energy-consistent metal and dielectric BSDFs, roughness and colored Fresnel, glass and thin-surface transmission. Verify both sides, total internal reflection, multi-interface paths and reflection/transmission energy across angles.
- [ ] **RT-12: Trace media and submerged lighting.** Track water and air media per segment, with absorption/scattering from appropriate fields. Connect light through water (including refracted paths); solve and validate caustics separately. Replace linear air fog only where it can be matched or intentionally changed; verify shore, underwater exits, cloud and volume shadows.
- [ ] **RT-13: Converge indirect and emissive light.** Importance-sample sky and textured emissive geometry, combine BSDF and light sampling without double counting, and provide a reference quality mode without the interactive bounce/ambient approximations. Validate against analytic energy checks and a converged reference. Keep an HDR scene-to-display path distinct from legacy post effects.

## P3 — Production quality and verification

- [ ] **RT-14: Stabilize moving frames.** Reproject using geometry/pose motion; reject disocclusion, changing lights/materials and incompatible secondary hits. Verify noise and ghosting during camera travel, wave/scroll animation, moving NPCs and thin foliage. Keep reverse-Z depth current and unfiltered.
- [ ] **RT-15: Bound work and memory.** Profile CPU gather/decode and GPU build/traversal/filter work separately. Reuse/refit BLAS and TLAS, improve texture paging and fence/queue scheduling, and track time and VRAM on representative maps/devices. Document quality caps and failures; don't hide missing geometry behind a backend fallback.
- [ ] **RT-16: Integrate presentation.** Audit minimap/full-map, Librarian, frame generation/upscalers and HDR post separately. Preserve native/off behavior and device recovery; expose which features are intentionally independent of the ray-rendered world.
- [ ] **RT-17: Verify on real maps and physical GPUs.** Exercise dense city, forest, open distance, water, caves/instances and effect-heavy areas across day/night and weather. Show the **same** assets in primary, shadow, reflection and refraction paths across terrain/stream boundaries. Compare both tracers and the native renderer. Record reproducible settings, coverage deltas, frame-time distributions, memory and device/driver info; keep CI's synthetic assertions as regression guards, not as performance certification.
