# Lighter — full ray-tracing roadmap

Lighter already traces camera, shadow, reflection, refraction and bounded indirect rays in world space. Software BVH and Vulkan ray queries share the same scene and shading. This checklist tracks three different goals: **scene coverage** (a ray can hit the asset), **client fidelity** (the hit has the right geometry, material and behavior), and **reference transport** (light arrives by the right paths and converges). Finishing one does not establish the others. HUD, menus, lens effects and the final display transform do not need to be ray traced.

Status: `[x]` means implemented and verified at the scope on that line; unchecked items remain open. Synthetic software Vulkan and physical GPU evidence must be reported separately.

## Ordered implementation

Complete and validate each numbered step before starting the next. The detailed
RT checklist below retains its original IDs and earlier completed foundations;
it does not set execution order.

### Phase 0 — Performance

1. [x] Remove live GPU completion waits in the world/RT paths, use later-frame callbacks with a three-slot readback ring, and verify a windowed run/resize and the explicit headless PNG. See [the caller audit and synthetic regression results](performance-waits.md).
2. [x] Cache BLAS by stable chunk and geometry identity; admit/evict with residency. Update posed positions in place and refit their BLAS. Dirty TLAS only for instance membership/transforms, and rebuild software BVH topology only when the set changes. Camera-only movement in a static scene must trigger zero rebuilds. See [the synthetic residency/refit results](performance.md#resident-acceleration-structures-step-2).
3. [x] Retain buffers, texture storage and bind groups across frames. Grow to `max(needed, capacity * 2)` within adapter limits, preserve contents when needed, and otherwise write in place. Log zero Lighter-owned resource creation in steady frames. See [the synthetic reuse test](performance.md#resident-gpu-storage-step-3).
4. [x] Audit `gpu.rs`, `lighter/scenery.rs` and `lighter/host.rs` clones. Borrow read-only data, share lifetimes with `Arc`, and use cached asset IDs. See [the 16/64/256-chunk timing sweep](performance.md#streaming-copy-audit-step-4).
5. [x] Default to 1 spp, 1 bounce and half ray resolution. Reproject and blend about 0.1 new / 0.9 history with a camera-movement reset; bilinear or edge-aware upscale in presentation. Higher quality is opt-in. See [the reprojection checks](performance.md#interactive-reprojection-step-5).

### Phase 1 — Coverage

6. [x] **RT-01b:** Build a stable `ChunkId`/bounds/asset manifest from the loaded world. Warn on conservative placement bounds. Rank by distance, light paths and secondary-ray reach; budget/evict by rank and retain matching BLAS. Check joins and teleports for gaps or duplicates. See [the manifest tests](performance.md#ranked-scenery-manifest-step-6).
7. [x] **RT-02b:** Compare coarse/native heights on a grid (mean/max, fail beyond about 0.5 world units), and silhouettes at 1–3° grazing incidence. Distinguish cutoff from failed loads and save results on at least two real maps. Verified for standalone heightfields and synthetic live seam geometry. See [S38/S40 results and remaining viewer checks](terrain-fidelity-2026-11-18.md).
8. [x] **RT-03:** Use native poses and bone-relative child instances; first-person hiding applies only to primary rays. Software and CPU Vulkan verified hook movement, TLAS/BLAS updates and an off-camera NPC shadow. Count unsupported payloads in SKI files seen by each feed. See [fixture evidence and retail census limits](scene-coverage.md).
9. [x] **RT-04:** Count loaded/skipped/failed terrain, trees, buildings, water, models and effects, with missing-package, texture-fallback, unsupported and budget reasons. Verified against saved fixture counts and two v12 retail map captures. See [the inventories and their boundaries](scene-coverage.md); the full scene mix remains in RT-17.

### Phase 2 — Client fidelity

10. [x] **RT-05:** Preserve authored mip counts and levels. Sample color as sRGB, coverage and data as linear; choose explicit LOD by footprint and 8×/16× grazing anisotropy. Report `mip_fallback` separately and test brick-wall shimmer. See [the mip and grazing-pan checks](texture-filtering.md).
11. [x] **RT-06:** Carry authored PBR/material channels, sidedness, opacity and transmission. Distinguish baked vs. relit buildings, terrain and grass. Carry posed face/eye secondary stages and UV scrolling; validate day/night. See [the archive and pixel checks](material-fidelity.md).
12. [x] **RT-07:** Match authored SpeedTree wind and grass fades; keep two-sided transmissive leaves in secondary rays independently of camera facing. Test noon dappled light. See [the ray and motion fixtures](vegetation.md).
13. [x] **RT-08:** Build per-body water BLAS/AABBs and wet/dry shoreline geometry. Document shading-only and geometric normal layers, initialize submerged media (Step 17), and compare shoreline/reflections to native. Verified at fixture scope; see [water geometry, materials and remaining retail checks](water-fidelity.md).
14. [x] **RT-09:** Match native GFX order and animation, project decal boxes onto hits, document lens/screen distortions in `present.wgsl`, and derive ray-shadowed weather from authored clouds/rain/snow. See [fixture coverage and limitations](effects-weather.md).

### Phase 3 — Transport

15. [x] **RT-10:** Honor static-light flags; sample finite sphere/disk emitters and accumulate shadow-ray transmission. Test tinted glass between emitter and receiver. See [the finite-light and transmission fixtures](direct-light.md).
16. [x] **RT-11:** Add `bsdf.wgsl` with energy-conserving GGX/Lambert, colored metal/dielectric Fresnel, Snell/TIR and backface handling. Compare reflected to incident energy across angles. See [the GGX furnace and interface tests](bsdf.md).
17. [x] **RT-12:** Track air/water per segment, apply authored RGB Beer–Lambert absorption, and separate specular caustics. Match native exponential fog or document linear. Test submerged sky refraction and tint. See [segmented media and the flat-interface sun check](media.md).
18. [x] **RT-13:** Importance-sample sky CDF and emissive triangles with power weighting and balance MIS. Reference mode uses full samples/bounces and no temporal reuse. Check analytic diffuse-box brightness. See [the estimator and measured error](reference.md).

### Phase 4 — Production

19. [x] **RT-15:** Log GPU build/traversal/filter timestamps each second; use an 8 GB RTX 3080 default budget, low-priority eviction and 128 recent texture tiles with background page uploads. Use an independent build queue if supported, otherwise early submit with only necessary fencing. Document backend caps. See [timings, queue and memory limits](gpu-budget.md).
20. [x] **RT-16:** Classify post effects by dependency on the ray world; keep native minimap/full map. Reconstruct rays before HDR post; leave unsupported vendor upscalers/FG paused. Recover the device and current chunks after DeviceLost; test resize and backend switching. See [ownership and recovery scope](presentation.md).
21. [ ] **RT-17:** Capture matching cameras on dense city, forest, open distance, water, cave/instance and effects-heavy real maps, in Native/Software/Hardware under day/night/rain. Save dated images, Step 9 counts, per-region mean pixel error, driver/GPU/settings. Keep synthetic regressions separate from performance evidence. See [the matrix, capture recipe and remaining gates](retail-matrix.md).

## P0 — Scene coverage and residency

- [x] **RT-01a: Extend static BMD residency beyond the native ornament load distance.** Both backends use the bounded, direction-independent extension and retain the native renderer's residency and map views. Archive-backed pixel tests cover cutouts, shadow, water reflection/refraction, visibility flags, eviction and reload. See [implementation and limits](README.md#static-scenery-residency).
- [x] **RT-01b: Index the whole map for bounded static residency.** Use decoded bounds or warned placement estimates; rank by receiver, light and secondary-ray reach. Retain matching BLAS and report out-of-range, budget and unavailable assets. Synthetic join and teleport tests pass; full-map visual verification remains in RT-17.
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
