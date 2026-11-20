# Phase 3 — nine-point plan and the three agent briefs

Phase 3 is **Complete buildings, rocks and other static models**. It was re-cut on
2 November 2026 into nine non-overlapping work points: the scope is unchanged, but
each point is now independently verifiable and owns one lane of the static-model
pipeline. Per-point decisions stay in
[Eight-phase-reassessment-2026-11-02.md](Eight-phase-reassessment-2026-11-02.md) and
the checkboxes in [Eight-phase-status.md](Eight-phase-status.md).

| ID | Point | Owner |
|---|---|---|
| P3.01 | BMD container and format coverage | Agent 3-A |
| P3.02 | Material and texture-stage state | Agent 3-B |
| P3.03 | Texture reference resolution (missing/shared/animated) | Agent 3-A |
| P3.04 | Culling, winding and sidedness | Agent 3-B |
| P3.05 | Depth, bias, ordering and overlap | Agent 3-B |
| P3.06 | Visibility, distance selection, LOD and regional residency | Agent 3-C |
| P3.07 | Reflection, fog, minimap and map passes | Agent 3-C |
| P3.08 | Atlas packing and bounded texture residency | Agent 3-A |
| P3.09 | Regression coverage in CI | Agent 3-C |

## Shared boundaries (all three agents)

* Target is the 1.4.2 executable and assets only. Later client source is research
  material, never a porting source, and no proprietary implementation is copied.
* Static BMD scenery only: buildings, rocks, walls, props. Terrain, water, sky,
  foliage/grass, animated ECM/SKI models, effects, gameplay and networking belong to
  other phases.
* No substitute geometry, silent skips or guessed material behavior. Every
  unrecovered version, stage or state must be an explicit error or a row in
  [Unsupported-client-paths.md](Unsupported-client-paths.md).
* A green build or a synthetic shader test does not complete a point; each point
  needs a recovered native rule plus an executable check.
* Lane ownership in `src/gpu.rs` (the shared renderer): Agent 3-B owns the
  static-model pipeline and state functions, Agent 3-C owns pass/view wiring and
  submission, Agent 3-A owns texture creation, upload and residency. Announce an edit
  that crosses a lane instead of reformatting or restructuring another lane.
* Do not flip Phase 3 rows or phase-level gates. Supply the change and the evidence
  for integrated review.

## Brief for Agent 3-A — format, assets and memory (P3.01, P3.03, P3.08)

You own what a BMD file is, which assets a placement resolves to, and how much
texture data is resident. You do not own render state, culling, depth or view
submission.

Required reading first: `Findings/Eight-phase-reassessment-2026-11-02.md` (Phase 3),
`Findings/Unsupported-client-paths.md`, the BMD/material sections of
`Findings/Research-log-02-rendering-and-lighting.md`, `Findings/Roadmap.md`
(`Models, materials and lighting`), then the source: `src/bmd.rs`, `src/objects.rs`,
`src/world_objects.rs`, `src/model_dds.rs`, `src/texture_atlas.rs`, and the existing
surveys `src/bin/bmd-check.rs`, `src/bin/bmd-survey.rs`,
`src/bin/bmd-material-survey.rs`.

Work:

1. **P3.01** Finish BMD container and format coverage: every model and mesh version,
   chunk layout, transform encoding, texture reference list, baked vertex-color
   arrays and per-mesh/per-material flags. Survey the whole archive and publish the
   version/flag census; unsupported versions and truncation stay explicit errors.
2. **P3.03** Implement the client's own texture reference resolution: missing,
   shared, paginated and animated references, the lookup order between the local
   directory and archives, and every fallback the client applies before failing.
   Animated texture records must either be implemented or rejected by name.
3. **P3.08** Bound texture residency: atlas packing, cross-page deduplication,
   eviction and streaming so full-resolution world assets are not all resident at
   once, with measured page counts and memory rather than estimates.

Deliver: production call sites and changed files; the version/flag census and the
residency measurement as findings files; native addresses for each consequential
lookup or packing rule; regressions for version/truncation handling, reference
resolution order, dedup and eviction; exact commands and results.

Confirmation block (one per point):
`Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps`.

## Brief for Agent 3-B — render state, culling and depth (P3.02, P3.04, P3.05)

You own how a parsed static model reaches the device: material and texture-stage
state, culling/sidedness and depth/order. You do not own file parsing, texture
residency or view/pass selection.

Required reading first: `Findings/Eight-phase-reassessment-2026-11-02.md` (Phase 3),
`Findings/Unsupported-client-paths.md`, the BMD material, sidedness and
depth-precision sections of `Findings/Research-log-02-rendering-and-lighting.md`,
then `src/bmd.rs`, `src/gpu.rs` (static-model draw and state helpers),
`src/world_objects.rs`, `src/bin/bmd-sided-gpu-check.rs`,
`src/bin/bmd-baked-gpu-check.rs`, `src/bin/bmd-overlap-survey.rs`.

Work:

1. **P3.02** Recover every static-model material and texture-stage state as a table:
   stage setups, alpha test, blending, transparency, material colors, two-sided
   fields and the baked-light combination. Replace per-asset special cases with the
   recovered rule, and make unrepresentable combinations explicit errors.
2. **P3.04** Recover the client's culling and two-sided behavior: how cull mode is
   chosen per mesh/material, how winding is derived, and what happens for mirrored
   or negative-determinant placements. The current global approximation is not
   evidence.
3. **P3.05** Recover the real causes of building Z-fighting and overlap: the client's
   depth bias and projection precision, its draw order, duplicate placements and
   overlapping geometry handling. Prove each fix against the overlaps the survey
   reports instead of moving geometry.

Deliver: the state table, production integration points, native addresses for each
consequential constant and comparison, regressions (stage selection, alpha/transparent
materials, cull-mode selection including mirrored placements, depth-conflict cases),
exact commands and results.

Confirmation block (one per point):
`Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps`.

## Brief for Agent 3-C — visibility, secondary views and CI (P3.06, P3.07, P3.09)

You own when a static model is drawn at all, in which views, and the automated
coverage that keeps Phase 3 honest. You do not own parsing, material state or
texture residency.

Required reading first: `Findings/Eight-phase-reassessment-2026-11-02.md` (Phase 3),
`Findings/Unsupported-client-paths.md`, `Findings/Roadmap.md` (`Terrain and
full-world visibility`, `Models, materials and lighting`), then
`src/world_objects.rs`, `src/gpu.rs` (pass and view submission, `render_minimap`,
`minimap_matrix`), `src/view_settings.rs`, and the CI definitions in
`.github/workflows/pr-checks.yml` plus `tools/`.

Work:

1. **P3.06** Recover static-model visibility, distance selection, LOD/detail
   reduction and the regional load/evict lifecycle. Per-placement visibility flags
   must be read, not inferred from distance alone; a model class the client hides
   must not be drawn.
2. **P3.07** Complete reflection, fog, minimap and full-map rendering for every
   supported static-model material, including alpha and special stages, reusing one
   per-frame snapshot so a secondary view never advances time or re-consumes scene
   light publications.
3. **P3.09** Keep regression coverage running in CI: BMD versions and truncation,
   culling/sidedness, texture stages, real depth conflicts and day/night baked
   lighting, with every static-model GPU diagnostic wired into the program set.

Deliver: production call sites, native evidence for visibility/LOD/region rules and
for each pass's material behavior, regressions plus the CI wiring, exact commands and
results, and an explicit list of material/pass combinations still unsupported.

Confirmation block (one per point):
`Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps`.

## Integration rules for the three briefs

* Report to `Findings/Phase-3-Agent-<A|B|C>-report.md`; keep the nine Phase 3 rows
  and checkboxes untouched until the integrated review flips them.
* The compiled viewer is the deliverable, not a test harness: every recovered rule
  must be reachable from `src/main.rs`'s normal world path.
* Paths and functions named here reflect the current tree. Read them before changing
  them; earlier research entries may be superseded.
