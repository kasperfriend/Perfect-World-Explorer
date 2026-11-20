# Phase 2 — nine-point plan and the three agent briefs

Phase 2 is **Complete terrain and ground materials**. On 2 November 2026 the old
nine points were re-cut into nine non-overlapping work points so that each one is
independently verifiable and can be owned by one agent without sharing a file's
internals. The scope did not change; only the boundaries did. The authoritative
per-point decisions stay in
[Eight-phase-reassessment-2026-11-02.md](Eight-phase-reassessment-2026-11-02.md)
and the checkboxes in [Eight-phase-status.md](Eight-phase-status.md).

| ID | Point | Owner |
|---|---|---|
| P2.01 | Block/area residency and draw selection | Agent 2-A |
| P2.02 | Resolution levels and transitions | Agent 2-A |
| P2.03 | Seams and edge rules | Agent 2-A |
| P2.04 | Shader variants and pass states | Agent 2-B |
| P2.05 | Lightmap selection and arithmetic | Agent 2-B |
| P2.06 | Material layers, masks, mapping | Agent 2-B |
| P2.07 | Texture sourcing, mips, samplers | Agent 2-C |
| P2.08 | Terrain specials and detail surfaces | Agent 2-C |
| P2.09 | Multi-view consistency and CI regressions | Agent 2-C |

## Shared boundaries (all three agents)

* Target is the 1.4.2 executable and assets only. Later client source is research
  material, never a porting source, and no proprietary implementation is copied.
* Terrain and ground materials only. Water, sky, foliage, static models, animated
  models, effects, gameplay, networking and player simulation are other phases.
* No substitute geometry, silent skips or guessed material behavior. Anything
  unrecovered must be an explicit error or an entry in
  [Unsupported-client-paths.md](Unsupported-client-paths.md).
* A green build or a synthetic shader test does not complete a point. Each point
  needs a recovered native rule plus an executable check.
* `src/terrain_gpu.rs` is the shared file: Agent 2-A owns mesh/vertex/index
  construction and the near/far split, Agent 2-B owns pipeline/WGSL/state setup,
  Agent 2-C owns texture/mip/sampler creation and view submission. Announce an edit
  that crosses those lanes instead of formatting or restructuring the other lanes.
* Do not flip Phase 2 rows or phase-level gates. Supply the change plus the
  evidence, and let the integrated review flip them.

## Brief for Agent 2-A — terrain structure (P2.01, P2.02, P2.03)

You own how terrain becomes a set of resident blocks, which resolution each block
is drawn at, and how neighbouring blocks and levels meet without cracks. You do not
own shader math, lightmap arithmetic, layer transforms, texture sampling or view
list management.

Required reading first: `Findings/Eight-phase-reassessment-2026-11-02.md` (Phase 2),
`Findings/Unsupported-client-paths.md`, the terrain sections of
`Findings/Research-log-02-rendering-and-lighting.md` (`Terrain seam mitigation`,
`Terrain refraction geometry, darkness and fog`, `Deferred world-model archive and
terrain upload memory`), `Findings/Roadmap.md` (`Terrain and full-world
visibility`), then the actual source: `src/main.rs` (world stream, `Terrain`),
`src/terrain_gpu.rs` (`TerrainGpu`, `world_mesh`, `grid_axis`),
`src/terrain_seam.rs`, `src/client_region_stream.rs`, `src/gpu.rs`.

Work:

1. **P2.01** Recover from the executable which terrain blocks/areas are loaded and
   submitted for a camera position: the residency window, the growth/eviction rule,
   per-view visibility, and what happens at map edges and for missing blocks.
   Replace the viewer's current always-loaded-then-culled approach with the recovered
   rule, or prove by disassembly that the current rule already matches. Missing or
   malformed blocks must fail explicitly with the block name.
2. **P2.02** Recover the resolution levels the client uses (local full-resolution
   mesh, distant coarse mesh, any intermediate) and the thresholds, hysteresis and
   transition geometry between them. The current near/far split is a viewer
   approximation; state what the client actually keys the switch on and implement it.
   Preserve the existing vertex paths for each level.
3. **P2.03** Replace `terrain_seam.rs`'s explicitly approximate interpolation with
   the client's own edge/neighbour rule, including block boundaries and boundaries
   between levels. If the client does not stitch but instead overlaps, sinks or
   snaps edges, implement that instead of an invented blend.

Deliver: production call sites and changed files; recovered native addresses and the
assets/settings used to validate them; new regressions for residency transitions,
level switching including hysteresis, and boundary seams at block and level edges;
exact commands and results. Add every remaining unsupported case to the register.

Confirmation block (one per point):
`Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps`.

## Brief for Agent 2-B — terrain shading and materials (P2.04, P2.05, P2.06)

You own what the terrain shader does with its inputs: variants, pass state, lightmap
arithmetic, layer/mask selection and world-to-texture mapping. You do not own mesh
construction, LOD switching, texture file decoding or view submission.

Required reading first: `Findings/Eight-phase-reassessment-2026-11-02.md` (Phase 2),
`Findings/Unsupported-client-paths.md`, the lightmap and terrain-shader sections of
`Findings/Research-log-02-rendering-and-lighting.md`, then `src/terrain_gpu.rs`
(WGSL, bind groups, `load_lightmaps`, `neutral_tile`), `src/terrain_lightmaps.rs`,
`src/material.rs`, `src/client_regional_light.rs`, `src/gpu.rs`.

Work:

1. **P2.04** Recover every terrain shader variant and its pass state as a table:
   opaque versus alpha surfaces, near and far vertex paths, stage setups,
   cull/depth/blend/alpha-test values, and the hardware fallback the client uses when
   a capability is missing. Anything the client never draws stays an explicit error.
2. **P2.05** Recover lightmap pair selection (T2LM/T2LM1), tile addressing and
   clamp/wrap behavior, the day/night blend, the encoded-space modulation and how the
   result combines with regional ambient. Replace remaining estimates with the
   recovered constants and arithmetic; keep the distinction between authored
   lightmap data and scene lights.
3. **P2.06** Verify every authored T2MK layer: axes and projections, ordering,
   per-area counts, mask formats and resolutions, and the world-to-texture mapping.
   Survey the archive, not one area, and make unsupported combinations fail with the
   area and layer named.

Deliver: the variant/state table and the layer survey as findings files, production
integration points, native addresses for each consequential constant and comparison,
regressions (variant selection, mask blending, lightmap pair/tile arithmetic, layer
axis projections), exact commands and results.

Confirmation block (one per point):
`Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps`.

## Brief for Agent 2-C — assets, specials and views (P2.07, P2.08, P2.09)

You own how terrain textures are sourced and sampled, the special surface cases in
client data, and terrain's behavior across every view. You do not own shader
arithmetic or mesh/LOD construction.

Required reading first: `Findings/Eight-phase-reassessment-2026-11-02.md` (Phase 2),
`Findings/Unsupported-client-paths.md`, `Findings/Roadmap.md` (`Terrain and
full-world visibility`), `Findings/Research-log-02-rendering-and-lighting.md`, then
`src/terrain_gpu.rs` (texture and sampler creation, `terrain_pass_fog`),
`src/material.rs` (DDS access), `src/texture_atlas.rs`, `src/views.rs` or the
minimap/map path in `src/gpu.rs`, `src/bin/terrain-lod-gpu-check.rs`.

Work:

1. **P2.07** Match texture sourcing and sampling: source dimensions, mip chain
   generation, addressing modes, anisotropic filtering, per-material sampler
   settings, and every source format present in the archive. Where the client keeps
   an authored mip chain, keep it instead of regenerating; where it generates, match
   the filter.
2. **P2.08** Implement terrain specials found in client data: holes and visibility
   flags, special surface cases, and maximum-quality road/city detail including
   Archosaur's high-resolution surfaces. Survey the data first and report what exists;
   do not invent holes that no record describes.
3. **P2.09** Keep terrain consistent across main, reflection, clipped refraction,
   minimap and the full-world map with one per-frame snapshot, and keep boundary,
   variant, layer-selection and near/far LOD-continuity regressions running in CI,
   including wiring `terrain-lod-gpu-check` into the GPU program set if it is still
   outside it.

Deliver: production call sites, the asset/format survey, native evidence for each
sampling and view rule, regressions (mip/addressing equivalence, hole/visibility
flags, per-view submission and snapshot reuse), exact commands and results, and any
format that stays unsupported as an explicit error plus register entry.

Confirmation block (one per point):
`Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps`.

## Integration rules for the three briefs

* Report to `Findings/Phase-2-Agent-<A|B|C>-report.md`; keep the nine Phase 2 rows
  and checkboxes untouched until the integrated review flips them.
* One lighting/playback snapshot per frame is shared by every view; a secondary view
  must never advance time or re-consume publications.
* Paths, files and functions named here are the current state. Read them before
  changing them; earlier research entries may be superseded.
