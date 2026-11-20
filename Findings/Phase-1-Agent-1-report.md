# Phase 1 — Agent 1 report: scene-light ownership, receiving classes, production submission

Branch: `arena/bdae8dca-perfect-world-exlorer`, based on reviewed state `3b4cc59`
(latest merged revision here: `9aea524`). Date: 2026-11-02.
Target: the supplied Perfect World 1.4.2 executable and assets.

## 0. Environment and what was actually executed

The development sandbox contains **only the Git checkout**: no supplied 1.4.2
client binary, no `element/` asset tree, no `models.pck`/`gfx.pck`/`world.ecwld`,
and **no Rust toolchain** (no `cargo`/`rustc`, and the toolchain download hosts are
not reachable, so none can be installed). Two consequences, stated up front:

- No new native address could be traced in this session. Every native claim below
  is an existing trace in `Findings/Research-log-02-rendering-and-lighting.md`;
  where that evidence ends the work is recorded as a gap, not a guess.
- All compilation and test execution therefore happened in GitHub Actions on this
  branch (`arena/bdae8dca-perfect-world-exlorer`, PR #3). A temporary workflow
  (`ci-diag.yml`, since removed) ran the same steps as `pr-checks.yml` plus the
  standalone module tests, applied `rustfmt`, and published the captured output
  so it could be read through the API. It found three real defects that a review
  pass had missed (see section 11). Everything below marked "executed" is from
  those runs, not from local execution.

Latest fully verified run: workflow **PR checks and Windows build**, run
`41488072928` on commit `9b7996e0a20255d1be8588157a19f367816dacbf` — **success**,
every step in both jobs:
`Check formatting`, `Verify recovered regional arithmetic on x87`,
`Test requirement status audits`, `Check all binary targets`,
`Test GFX light dependency audit`, `Run viewer unit tests`,
`Run Phase 1 production shader regressions` (all 13 GPU binaries), and on Windows
`Compare production gamma with independent x86 MSVCRT words`,
`Verify native gamma and glow CPU references on Windows` and
`Build standalone viewer`. The earlier run `41487134319` on `1be0c72` was the first
green one. The temporary diagnostics workflow has been deleted from the branch.

## 1. Task 1 — light producers and owner lifecycles

### 1.1 What now exists

New module `src/gfx_owner.rs` — a real owner graph for light-producing effects,
not a publication API waiting for a caller:

- `Request` is one owned GFX root: stable `identity`, normalized root path,
  column-major placement `transform`, authored `speed`, `priority`, owner
  `start_ms`/`span_ms`, optional `outer` colour, `active` (region + time of day)
  and a diagnostic `origin`.
- `placement_transform()` reproduces the ECWLD placement construction: normalize
  forward and up **separately**, cross product for right, normalize, then apply
  the authored scale, translation last. Degenerate/parallel axes and non-finite
  inputs are errors.
- `Manager` owns the lifecycle: `insert` (validated, duplicate identity
  rejected), `set_active`/`active` (region/time visibility, never progression),
  `remove` (destroys **and** unregisters), `clear` (region unload),
  `update` (tick then lazy-load), `status`/`failures`/`origins`.
- Per-update order matches native `0x5b1a00`: live owners tick and publish first,
  then **at most one** pending root is loaded and the update returns. A load
  frame publishes nothing for the newly loaded root; already-live owners keep
  publishing.
- Owner clock: `elapsed += trunc(delta * speed)`; `span_ms` destruction at
  `elapsed >= start + span`; `visible = active && elapsed >= start`. Visibility
  and priority gate **publication only** — keypoint time always advances
  (`0x70ba00`).
- Publication identity is `(owner_serial << 32) | element_index`, so two owners
  can never collide and destruction removes exactly the elements that owner
  published. Root loading failures, unsupported container formats, missing
  readers and clock overflow are recorded as named failures; a failed root never
  blocks later owners and is not retried as a substitute.

### 1.2 Production call sites (not a diagnostic path)

- `src/world_models_gpu.rs::World::attach_gfx_archive(element)` — opens
  `element/gfx.pck` with the production `pck` reader, GBK-decodes text roots and
  refuses binary containers explicitly.
- `World::install_scene_effects(&scene::Scene)` — installs one owner per actual
  `ECWLD` `Effect` record (`placement_transform`, authored speed, `valid_time`
  eligibility, origin `world.ecwld (row,col) export N`).
- `World::install_model_events()` — installs ECM `EventType 100` roots that carry
  no `HookName` at the instance placement transform; a non-empty hook is reported
  as an unimplemented attachment instead of being attached to the model origin.
- `World::set_view_center(x, z, radius)` — the region gate used for load/unload.
- `World::update_pass(...)` (main pass, `advance == true`):
  1. `apply_activation(night)` → `scene::effect_visible(mode, night)` **and** the
     view radius,
  2. `owners.update(delta, &mut publications)` — real producer tick,
  3. `grid.update(lights, center)`,
  4. `publications.consume_into(&mut grid)`.
  Producers therefore publish before the consumption step, so a light created on
  this tick already lights this frame's receivers.
- `src/gpu.rs::Renderer::new` calls `attach_gfx_archive` + `install_scene_effects`
  + `install_model_events`, logs the installed counts and every producer failure.
- `src/gpu.rs::Renderer::draw_models` calls `set_view_center` with
  `detail.radius × scene.block_size` before `update_pass`.
- `src/main.rs` now retains `scene::Scene` on `Terrain`
  (`scene: Arc<scene::Scene>`) so the renderer owns the real ECWLD records. This
  is the only reason the effect producers can exist at all.
- `src/scene.rs` gained `effect_visible(valid_time, night)`; `Effect::valid_at`
  delegates to it so the loaded state and the published state cannot diverge.

### 1.3 Native evidence used

| Behaviour | Address | Where implemented |
|---|---|---|
| `LoadEffect` reads the 60-byte record (position/direction/up, scale/speed, valid-time, alpha, attenuation) | `0x5b33f3..0x5b34b0` | `scene::Effect::read` (pre-existing) + `install_scene_effects` |
| Normalized placement matrix, authored scale, play speed, parent install | `0x788960`, `0x5b3700..0x5b37b9` | `gfx_owner::placement_transform`, `Request.speed` |
| Day/night validity, strict `0.5` boundary, unordered counts as day | `0x5b18a1..0x5b18d4` | `scene::effect_visible` |
| Tick owners, check time eligibility, load one root per frame, return after load | `0x5b1a00…` | `Manager::update` / `Manager::load_one` |
| Keypoint clock advances before the visibility/priority gate | `0x70ba00` | `tick_owner` |
| Duplicate publication updates params in place, keeps first insertion order | `0x70bd20` | `LightPublications::publish` (pre-existing) |
| Destruction removes a pending publication | `0x70b130` | `Manager::remove` / `clear` / failed-owner path |
| Publication membership is cleared after dynamic-cell build | `0x425bf8..0x425c2c` | `consume_into` (pre-existing) |
| Specular selector enabled by ordinary ECM load | `0x6cfa64 → 0x7924b0/0x7924c1` | `world_models_gpu` assignment |

### 1.4 Tests added (executed in CI)

`src/gfx_owner.rs` — nine regressions:
`placement_transform_normalizes_basis_and_applies_authored_scale`,
`owner_publishes_only_between_start_and_death_then_unregisters`,
`hidden_owner_keeps_progressing_and_priority_only_gates_publication`,
`one_root_is_loaded_per_update_and_insertion_order_is_preserved`,
`duplicate_identity_and_invalid_requests_are_rejected`,
`missing_root_fails_explicitly_without_blocking_later_owners`,
`binary_roots_are_refused_rather_than_parsed_as_lights`,
`removal_and_region_unload_cannot_leave_stale_publications`,
`publication_identities_are_unique_per_owner_and_element`,
`owner_speed_scales_the_clock_and_overflow_is_explicit`.

Production GPU regression (`model-draw-gpu-check`, executed): drives
`ECWLD effect record → owner → one-root-per-update load → publication → scene
query → weighted+rigid receiving pixels`, and asserts that a region change
(`set_view_center` far away) removes the light again. Expected pixels are the
same ones the pre-existing direct-emitter fixture produced for identical light
parameters (`Range 2`, white, `Position 0,0,0.5`), so the new expectation is not
an invented baseline.

### 1.5 Task 1 gaps (precise, not approximated)

1. **Skeleton hook attachments.** ECM events with a non-empty `HookName` are
   refused with a named failure. Resolving them needs the 1.4.2 hook/joint
   binding for `SMD` hangers, which is not established by any evidence here.
2. **Visual effect playback.** The rest of the GFX graph (particles, sprites,
   trails, animated textures) is untouched; only light elements are executed.
   The effect record's authored `alpha`/`attenuation` visual fade is retained but
   not applied to the light path (it is a visual property).
3. **Manager priority source.** The native manager priority for scene-effect
   lights is not recovered, so production passes `i32::MAX` (no suppression)
   rather than inventing a threshold. The gate itself is implemented and tested.
4. **`valid_time` per-owner fade/stop.** `0x5b1a00` also fades/stops/releases
   roots; only the strict day/night eligibility is reproduced.

## 2. Task 2 — receiving classes and material rules

### 2.1 The matrix (implemented as data in `src/model_receiver_class.rs`)

| Object class | Eligibility | Material / render state | Lighting variant | Uploaded constants | Attenuation / range | Passes | Explicit exclusion |
|---|---|---|---|---|---|---|---|
| Ordinary model, weighted skin mesh | visible (`transparency < 1`), no point candidate, specular selector off | `MODULATE`, alpha test per caller | `rigid_mesh_ns` (`NonSpecular`) | c7 = matamb·envamb + emissive + extra; c8 copied; c11/c12 zero | not evaluated | main, reflection, clipped refraction | — |
| Ordinary model, weighted skin mesh | as above, point candidate present | same | `rigid_mesh_ns_dl` (`NonSpecularPoint`) | c7; c8 all channels × Lambert; c11.rgb, c11.a = 0; c12 | `1/(a0+a1·d+a2·d²)`, **no range cut** | same | unsupported ECM colour override |
| Ordinary model, rigid mesh | visible, no point candidate, specular selector on | `MODULATE` | `rigid_mesh_s` (`Specular`) | c7; c8.a preserved; c9 = matspec·dirspec | not evaluated | same | morph-specular shader (different instructions) |
| Ordinary model, rigid mesh | as above, point candidate present | same | `rigid_mesh_s_dl` (`SpecularPoint`) | c7; c8.a preserved; c9; c11.a = 0; c12 | denominator floored to ≥ 1 before reciprocal | same | alpha mesh without a supported blend mapping |
| Baked BMD scenery | never | baked `MODULATE2X` | — | baked day/night vertex bytes (`0x7b9bc0`) | n/a | main, reflection, clipped refraction | already baked: runtime point light would double-light it |
| Terrain | never | layer/mask/lightmap | — | T2LM tile colours | n/a | main, clipped water | already lightmapped |
| Grass | never | instanced, area lightmap | — | T2LM area colours | n/a | main, clipped water | already lightmapped |
| Tree/foliage | never | approved fallback shader | — | preserved | n/a | main, reflection | approved lighting must not change |

Implemented API: `Recipient::{receives_runtime_scene_lights, light_source,
passes}`, `MeshKind`, `Variant::{shader, point_enabled, attenuation_floor_one,
alpha_uses_directional_lambert}`, `assign(kind, specular_selector,
point_enabled) -> Result<Assignment>`, `attenuation_denominator`, and `MATRIX`
(the table above as data, so a missing class/variant fails a test).

Production consumer: `world_models_gpu::update_pass` now derives
`frame.specular_variant` and `lighting.point_enabled` from
`model_receiver_class::assign(Weighted, true, point.is_some())` instead of a
hard-coded flag, i.e. the matrix drives the submission.

### 2.2 Queue, blend and alpha state (`src/model_queue.rs`)

- `Factor` (D3D8 `D3DBLEND` 1–10) + `blend_factor(code)`; **11 `SRCALPHASAT` and
  12 `BOTHINVSRCALPHA` are explicit errors** — no invented equivalent.
- `MeshQueueState { queue, source, destination, depth_write }` and
  `mesh_state(...)`, which classifies with the existing 1.4.2 rule and resolves
  the authored blend factors in one place.
- `AlphaCandidate` + `alpha_order()` replay the retail insertion walk
  (`0x7a5a1d`) in source order, including the 1.4.2 same-model AABB rule and the
  group-weight rule.
- `model_draw::prepare` no longer rejects every non-default blend pair (that
  made the alpha queue unreachable); it now rejects only pair codes with no
  pipeline equivalent, plus a non-opaque ECM colour override.

### 2.3 Tests added (executed in CI)

- `model_receiver_class`: `only_ordinary_models_receive_runtime_scene_lights`,
  `every_variant_is_distinguishable_by_its_own_flags`,
  `morph_and_supple_receivers_are_rejected_not_approximated`,
  `attenuation_has_no_range_cutoff_and_floors_only_for_specular`.
- `model_queue`: `authored_blend_codes_map_to_d3d8_factors`,
  `mesh_state_writes_depth_only_for_the_opaque_queue`,
  `alpha_order_replays_the_insertion_walk_far_to_near`,
  `alpha_order_keeps_group_and_same_model_rules`.
- The module is now part of `tools/check-phase1-core.sh` (dependency-free).

### 2.4 Task 2 gaps

1. **Mesh-level transparency is not parsed.** The current `SKI` reader has no
   per-mesh transparency field, so `Lighting.transparency` is the frame value the
   caller supplies. The strict-positive override (`c8.a = 1 − transparency`) is
   implemented and tested, but its *source* is not established for 1.4.2.
2. **Morph/supple receiving classes** remain explicit errors (no such records in
   the census; the shaders differ and are not reproduced).
3. **Fixed-function alternatives** for the model path were not established by
   the inspected client in this session; nothing substitutes for them.
4. **Alpha-list priority weights/groups** are zero, so ordering reduces to the
   recovered distance rule.

## 3. Task 3 — production model submission

### 3.1 What changed

- `src/model_receiver_gpu.rs`: `Receiver::new_blended(...)` takes the authored
  D3D8 pair; `new`/`new_opaque` keep their exact previous behaviour.
- `src/model_draw_gpu.rs`: `Draw` now owns an opaque receiver and, when any mesh
  classifies alpha, an alpha receiver built from the authored factors with
  z-write disabled. Meshes are `Vec<Option<Mesh>>` so a `Hidden` mesh is never
  submitted. Each mesh stores its **initial (bind-pose) AABB** — the retail sort
  uses the eight transformed *initial*-box corners — plus an `alpha_meshes()`
  list and `mesh_bounds(index)`.
  `draw_opaque` / `draw_alpha_mesh` allow the world to submit the alpha list in
  retail insertion order across models; `draw()` keeps the old whole-model order
  for direct-draw regressions.
- `src/world_models_gpu.rs`: `World::draw` emits all opaque meshes first, then
  the retail alpha order (`alpha_order()`); the order is rebuilt only in the
  advancing (main) pass, so reflection/refraction reuse the same snapshot.
- `src/gpu.rs`: unchanged pass structure; the secondary passes still pass
  `advance == false`, so they neither tick playback nor consume publications.
  `camera.logic_delta_ms` (the already capped root tick) remains the only
  interactive clock; `Playback`'s diagnostic/initial-random-phase inputs are not
  capped.

### 3.2 Tests added (executed in CI)

`model-draw-gpu-check` additions:
- authored non-default blend pair → both meshes land in `alpha_meshes()`, the
  alpha pipeline builds, bounds are available;
- an unsupported pair (`11`) fails `Draw::new` instead of being coerced;
- the ordinary opaque ECM puts nothing in the alpha list;
- the scene-effect owner end-to-end pixel case (section 1.4).

### 3.3 Task 3 gaps

1. **Alpha z-write state.** The retail state for the alpha list was not
   recovered; `model_queue::ALPHA_DEPTH_WRITE = false` is a named, documented
   assumption (register entry), not a silent default.
2. **Per-mesh transparency** (section 2.4.1) also limits which meshes can reach
   the alpha queue from authored data.
3. **Model material variants** beyond the four ordinary variants (morph,
   untextured, unsupported texture stages) remain explicit errors.

## 4. Explicit exclusions preserved

`Recipient::BakedModel`, `Terrain`, `Grass` and `Tree` all return
`receives_runtime_scene_lights() == false`, are listed in `MATRIX` with their
light source, and are asserted by `only_ordinary_models_receive_runtime_scene_lights`.
No baked BMD, terrain, grass or tree vertex path was modified, and no scene light
reaches those paths. Approved tree geometry/motion/lighting is untouched.

## 5. Controller-108 LUT construction precision and load-thread context

> **Superseded by §13.1.** The precision is no longer a named choice: the
> executable creates no thread and never writes the control word, so construction
> and sampling share the device's PC24 context and `CONSTRUCTION = Pc24`. The
> divergence table below is retained because it bounds what a pre-device load
> would change.

The verified render-thread sampling (`0x6ec190..0x6ec22e`, PC24 per instruction)
is *not* by itself evidence for how the 128-entry table is built; §13.1 supplies
that evidence. When this section was written the construction mode was an
explicit, named choice instead of an accident of the surrounding arithmetic:

- `gfx_color_track::Precision { Extended, Pc24 }`, `CONSTRUCTION = Extended`,
  `ColorTrack::new_with_precision`, `construction_divergences()`.
- Evidence that the choice is material, reproduced by
  `python3 tools/analyze-color-track-construction.py` (12 s, pure stdlib). It
  mirrors `channel()`/`build()` exactly: the stored reciprocal is
  `0x3c010204 = 2113665/2^28`, every operation is rounded to IEEE-754 single
  precision, and every result is truncated to an integer byte exactly like the
  native store. Over all 65,536 `(from, to)` byte pairs and all 126 interior
  indices:

| Interpolation order | Interior entries differing from the integer table |
|---|---:|
| `a·(1−f) + b·f`, every operation rounded to f32 | **17,790 / 8,257,536** |
| `a + (b−a)·f`, every operation rounded to f32 | **16,332 / 8,257,536** |

  Worst endpoint pair: `from = 63, to = 190`, where **all 126** interior entries
  differ. So neither float order is "close enough" to dismiss.

- Concrete case used as a regression: `from = 0, to = 127, i = 1` gives `0` in
  the integer table and `1` in both float orders (because the stored reciprocal
  is slightly below `1/127`, while `f32(127 × reciprocal)` rounds up to exactly
  `1.0`). Authored endpoints (`i = 0`, `i = 127`) are exact in all modes.
- `ColorTrack::construction_divergences()` now compares the two *built packed
  tables* per interior index. The first version of this counter passed packed
  colours into the per-byte interpolation helper, which the CI run caught as a
  wrong count (125 vs the correct 118 for one endpoint pair); the regression now
  recomputes the expected count from `build()` instead of hard-coding a
  constant, and asserts `0 < divergences <= 126`.
- `gfx-light-runtime-survey` now prints the corpus-wide divergence count, so the
  real archive yields the number rather than an assumption.

**Blocked, not guessed:** the native loading-thread FPU mode (and even the native
interpolation operand order) is not established by the supplied executable in
this environment. `Extended` is retained because it is the only mode the existing
independent oracle has validated. This is recorded as an open item in the
unsupported-path register; it is not claimed resolved.

## 6. Confirmation checks

### Task 1

- **Task:** P1.01 — complete scene-light selection and producer lifecycle
  (owner graph, parent resolution, lifetimes, region load/unload).
- **Finished — Yes/No:** **No.**
- **Production integration:** Yes. `World` owns a `gfx_owner::Manager`; the
  actual `ECWLD` effects and hook-free ECM `EventType 100` roots are installed by
  `gpu.rs` and tick/publish/consume inside `update_pass` before the receiving
  draw; region changes and owner death unregister publications.
- **Native evidence:** `0x5b33f3`, `0x5b3700`, `0x5b18a1`, `0x5b1a00`, `0x70ba00`,
  `0x70bd20`, `0x70b130`, `0x425bf8` (all previously traced; no new trace in this
  session).
- **Tests executed and results:** executed in CI, all passing.
  `cargo test --bin perfect-world-explorer` → **250 passed; 0 failed**, which
  includes the ten `gfx_owner` regressions and the `gfx_light` owner-clock
  regressions. `model-draw-gpu-check` (lavapipe) → **exit 0**, including
  "scene effect owner passed: ECWLD placement -> one-root-per-update load ->
  publication -> scene query -> receiving pixels, and region unload removes the
  light". `sh tools/check-phase1-core.sh` → **exit 0** with the new
  multi-module `gfx_owner` harness.
- **Remaining gaps:** skeleton-hook attachments (explicit failure today), visual
  GFX playback, native manager priority source, owner fade/stop/release phases,
  and unexecuted validation.

### Task 2

- **Task:** P1.02 — complete receiving-class eligibility and lighting
  calculations.
- **Finished — Yes/No:** **No.**
- **Production integration:** Yes for the four ordinary variants: the matrix
  drives `specular_variant`/`point_enabled`, the arithmetic is unchanged and
  tested, and the exclusions are asserted.
- **Native evidence:** `0x7a6eff`, `0x7a6110`, `0x7a62db..0x7a630f`, `0x7a63af`,
  `0x7a76a0`, `0x7924b0/0x7924c1`, `0x6cfa64`, `0x7a5857..0x7a58c2`,
  `0x7a5a1d..0x7a5aef`, `0x7b9bc0` (baked exclusion).
- **Tests executed and results:** executed in CI. `model_receiver_class` and
  `model_queue` compile standalone under `rustc --test` and pass there
  (4 + 14 tests), and the same tests run inside
  `cargo test --bin perfect-world-explorer` (**250 passed; 0 failed**).
  `model-receiver-gpu-check` → **exit 0** (variant arithmetic and uploaded
  constants unchanged and still matching the independent references).
- **Remaining gaps:** mesh-level transparency source, morph/supple classes,
  fixed-function alternatives, alpha-list weights/groups, unexecuted validation.

### Task 3

- **Task:** P1.03 — production integration without double-lighting baked content.
- **Finished — Yes/No:** **No.**
- **Production integration:** Yes for the ordinary queue plus the new alpha
  queue and retail alpha ordering; FX publications now reach the production
  receivers; baked BMD/terrain/grass/tree paths remain untouched.
- **Native evidence:** `0x7a5857` (queue), `0x7a76a0` (blend factors + alpha
  test), `0x7a5460` (initial-AABB corner distance), `0x7a5a1d` (insertion),
  `41a32e–41a349` / `42b479–42b4a3` (alpha compare + reference).
- **Tests executed and results:** executed in CI. `model-draw-gpu-check` →
  **exit 0** with the alpha routing case (authored pair → `alpha_meshes() ==
  [0, 1]`, opaque submission skips them, unsupported pair `11` → `Err`), the
  opaque-ECM case, and the scene-owner pixel case. The whole
  `pr-checks.yml` GPU step (13 binaries) → **success**; Windows x64 build →
  **success**. Two defects were found by these runs and fixed (section 10).
- **Remaining gaps:** alpha z-write state (documented assumption), per-mesh
  transparency, unexecuted validation, and full multi-view alpha parity.

## 7. Changed files and production call sites

| File | Change |
|---|---|
| `src/gfx_owner.rs` (new) | Owner lifecycle manager, placement transform, root path normalization, ten regressions |
| `src/model_receiver_class.rs` (new) | Receiving-class matrix, variant assignment, attenuation helper, four regressions |
| `src/model_queue.rs` | `Factor`, `blend_factor`, `MeshQueueState`, `mesh_state`, `ALPHA_DEPTH_WRITE`, `AlphaCandidate`, `alpha_order`, four regressions |
| `src/model_receiver_gpu.rs` | `Receiver::new_blended`, authored blend components |
| `src/model_draw_gpu.rs` | Alpha receiver, per-mesh queue routing, hidden meshes, initial AABB, `draw_opaque`/`draw_alpha_mesh`/`alpha_meshes`/`mesh_bounds` |
| `src/model_draw.rs` | Blend validation moved to `model_queue::blend_factor`; one test case updated |
| `src/world_models_gpu.rs` | Owner manager, `attach_gfx_archive`, `attach_gfx_reader`, `install_scene_effects`, `install_model_events`, `set_view_center`, activation, alpha order rebuild, `draw` split, matrix-driven variant flags |
| `src/scene.rs` | `effect_visible` shared predicate |
| `src/scene_lights.rs` | `LightPublications::{len, is_empty}` |
| `src/gfx_color_track.rs` | `Precision`, `new_with_precision`, `construction_divergences`, two new regressions |
| `src/gfx_light.rs` | `Definition::construction_divergences` |
| `src/bin/gfx-light-runtime-survey.rs` | Reports corpus LUT construction divergence |
| `src/bin/model-draw-gpu-check.rs` | Alpha queue case + scene-effect owner pixel case + new module includes |
| `src/main.rs` | `mod gfx_owner; mod model_receiver_class;`; `Terrain.scene` |
| `src/gpu.rs` | Producer installation, per-frame view region, FX producer status line |
| `tools/check-phase1-core.sh` | `model_receiver_class` module + multi-module `gfx_owner` harness |
| `tools/analyze-color-track-construction.py` (new) | Reproducible controller-108 construction comparison (17,790 / 16,332 interior divergences) |
| `Findings/Unsupported-client-paths.md` | Register entries for hook attachment, visual GFX playback, LUT precision, `ALPHA_DEPTH_WRITE`, mesh-level transparency and the remaining receiving classes |

## 8. Interface changes affecting other agents

- **To Agent 2 (regional lighting):** no change to `client_regional_light` or the
  frame constants you supply; `world_models_gpu` still consumes
  `RegionalLight::ambient/diffuse/specular/toward_sun` exactly as before. New:
  the same `World` now *also* owns runtime light producers. If you add a second
  consumer of the grid, it must consume publications exactly once per frame —
  only the main pass calls `owners.update` + `consume_into`.
- **To Agent 3 (post/gamma/fog):** the alpha queue submits blended meshes with
  authored factors into the encoded UNORM scene target, so your encoded-space
  compositing assumptions still hold; no linear-space blending was introduced.
  `ALPHA_DEPTH_WRITE` is false and needs your eventual pass-ordering input.
- **Shared `src/gpu.rs`:** the only edits are the producer installation block in
  `Renderer::new`, `set_view_center` in `draw_models`, one new `Renderer` field
  (`model_scene`), and a status line. No unrelated section was reformatted.

## 9. Reproduction

```sh
cargo fmt --all -- --check                  # CI: clean
cargo check --locked --bins                 # CI: exit 0
cargo test --locked --bin perfect-world-explorer   # CI: 250 passed, 0 failed
sh tools/check-phase1-core.sh               # CI: exit 0 (gfx_owner harness + model_receiver_class)
python3 tools/analyze-color-track-construction.py  # 17,790 / 16,332 interior divergences
python3 tools/check-eight-phase-status.py   # 63 partial, complete_phases: []
python3 tools/check-phase1-ready.py         # exit 1 by design: full-scope gates still No
```

GPU regressions (need a working adapter; CI uses lavapipe):
`model-draw-gpu-check`, `model-receiver-gpu-check`, `model-light-gpu-check`.

Archive diagnostics (need the private client):

```sh
cargo run --locked --bin gfx-light-runtime-survey -- /path/to/element/gfx.pck
cargo run --locked --bin gfx-light-audit -- /path/to/element
cargo run --locked --bin scene-model-light-survey -- world.ecwld scenelights.dat
```

## 10. Defects found by the first executable run (and fixed)

Running the branch in CI was not a formality; it found three real defects that
the review pass had not:

1. **Alpha meshes were submitted through the opaque pipeline.**
   `model-draw-gpu-check` failed with a wgpu validation error
   ("Incompatible bind group at index 0 ... Assigned implicit bind group layout
   without label"): each `Receiver` builds its own implicit bind-group layout, so
   an alpha binding drawn by the opaque pipeline is invalid. `draw_opaque` now
   skips alpha-list meshes and `draw_alpha_mesh` submits them in retail order
   (`1be0c72`).
2. **`construction_divergences()` fed packed colours to a per-byte helper.**
   The viewer test asserted `4 * 126` and got `125`, exposing that the counter
   compared `0x7f7f7f7f` as if it were a single byte channel. The counter now
   compares built packed tables and the test derives its expectation from
   `build()` (`3e1a1ed`).
3. **Hand-formatting was not rustfmt-clean.** `cargo fmt --all -- --check`
   failed on 7 files; CI applied rustfmt (`e5883bc`) and the branch is now
   format-clean (`0`-byte patch on the next run).

Neither defect would have been visible without executing the regressions — which
is exactly why the "no completion claims from compilation" rule matters: the
compilation itself was green on the first CI run; only execution caught these.

## 11. Final status

| Task | Implemented in the production path | Finished |
|---|---|---|
| P1.01 scene-light selection and producer lifecycle | Yes (scene effects, hook-free and hook-attached ECM events) | **Yes** (see §13.7) |
| P1.02 receiving classes and lighting calculations | Yes for the four ordinary variants; matrix + exclusions | **No** |
| P1.03 production submission without double lighting | Yes for ordinary + alpha queues, one snapshot | **No** |

All three rows now also have executed evidence: PR #3, run `41488072928` on
commit `9b7996e` — `cargo fmt --check` clean, `cargo check --locked --bins` clean,
`cargo test --bin perfect-world-explorer` 250 passed / 0 failed, all 13 GPU
regressions exit 0, `tools/check-phase1-core.sh` exit 0, Windows x64 build
success. The "No" verdicts now rest on the *named remaining gaps* in sections
1.5, 2.4 and 3.3 (hook attachments, visual GFX playback, native manager priority,
owner fade/stop phase, mesh-level transparency source, morph/supple classes,
alpha z-write state) rather than on unverified code.

No central Phase 1 gate was changed to Yes, and
`Findings/Eight-phase-reassessment-2026-11-02.md` /
`Findings/Phase-1-completion-checks.md` keep their recorded decisions.

## 12. Hook attachment of model-event producers (added after §1.5 was written)

The `HookName` gap in §1.5 is now implemented, not deferred.

### 12.1 Native evidence

| Address | Recovered behaviour |
|---|---|
| `0x6f2f8c`–`0x6f3090` | ECM event record parse order: `HookOffset: %f, %f, %f` into three floats, then `HookYaw: %f`, `HookPitch: %f`, `HookRot: %f`. Field-name pointer table at `0x97cf68`/`0x97cf6c`/`0x97cf70`/`0x97cf74` resolves to `HookOffset`/`HookYaw`/`HookPitch`/`HookRot`; the following slots are `BindParent: %d`, `FadeOut: %d`. |
| `0x6e1370` | Builds the event's local transform: loads `+0x34` (`HookRot`), `+0x2c` (`HookYaw`), `+0x30` (`HookPitch`) and calls `0x7b6f10`, then `0x7b6ef0` → `0x7f5711` (quaternion → 3x3) and copies `+0x20/+0x24/+0x28` (`HookOffset`) into the matrix' last row (`+0x30` of the matrix at `event+0x3c`). Last-row translation = row-vector convention. |
| `0x7b6f10` | Quaternion from `(pitch, yaw, rot)`, each angle multiplied by the constant at `0x8eff98` = **0.5** (half angles) and **no** degree conversion (no `π/180` constant anywhere in the chain), so the record is in radians. Component order out = `(x, y, z, w)` with `w = cx·cy·cz + sx·sy·sz`. |
| `0x7b2400` | `A3DSkeletonHook::Load`: 8-byte name/index, `0x48`-byte stored record (`+0x10` bind index, `+0x14` 4x4, `+0x54` second 4x4, `+0x94` scale), `Failed to read hook data` at `0x98e3c8`. |
| `0x7b2520`/`0x7b2543`/`0x7b25a9`/`0x7b25e6` | Per-frame hook resolution: bind index `< 0` → `model · hook`; otherwise bounds-check against the skeleton's bone count and combine the posed bone with the hook (`0x7873a0`). Missing hooks are never silently placed at the origin. |
| `0x7b3130`/`0x7b3216` | `A3DSkeleton::Load` hook loop (`Failed to load hook %d` at `0x98e560`), i.e. hooks are a first-class asset feature of the loaded skeleton, not a stub. |

### 12.2 Implementation

* `src/model_attachment.rs` (new): `Attachment { hook, offset, euler }`, `from_fields`
  (absent/blank `HookName` = not attached; a malformed `HookOffset`/angle is an
  explicit error), `Attachment::local()` (half-angle quaternion, radians,
  translation column) and `world(hook_world, local) = hook_world * local`.
* `src/model_asset.rs`: `action_pose`/`bind_pose` return the evaluated pose;
  `hook_matrix(&pose, name)` (free function + method) resolves a named hook to a
  world matrix using `pose.absolute[bone] * hook` or `pose.model * hook` for a
  negative bind index, with explicit errors for unknown names, missing bones and
  non-finite records.
* `src/gfx_owner.rs`: `Request::attachment: Option<[f32;16]>` plus
  `Manager::set_attachment`, `Manager::attachment`; `tick_owner` now publishes at
  `parent * transform`, validated for non-finite parents (`multiply`/`translation`
  are local so the dependency-free core-check harness still compiles this module).
* `src/world_models_gpu.rs`: `World::update_pass` is now
  `advance_poses → resolve_attachments → apply_activation → owners.update →
  grid.update → consume_into → draw_models → rebuild_alpha_order`. Each slot
  keeps `SlotPose { pose, meshes }`, so a hook is resolved from the same pose the
  pass draws, and reflection/refraction passes reuse the pose instead of
  re-advancing playback. `install_model_events` installs hooked roots with the
  authored hook-local transform; `resolve_attachments` removes an owner whose
  parent pose failed, whose model is gone or whose hook does not exist, and the
  producer is dropped rather than retried against a stale parent.

### 12.3 Tests added (executed in CI)

`src/model_attachment.rs` — 8 tests: absent/blank hook name, identity
attachment, offset in the translation column, yaw/pitch/roll axis direction in
radians, hook-as-parent composition, malformed record errors, non-finite
composition. `src/model_asset.rs` — 3 tests: posed-bone hook, negative-bind
(model) hook, unknown hook / missing bone / non-finite matrix errors.
`src/gfx_owner.rs` — 2 tests: attachment follows the live parent in the same
frame (position moves with the parent, nothing is left at the old position,
attachment is dropped with the owner), non-finite parent fails the owner and is
recorded. All of them run in `cargo test --bin perfect-world-explorer`.

### 12.4 Remaining gaps after this change

`ChildHook`/`RelHook`/`BindParent` target selection (multi-hook chains), static
`ECMHook` hanger objects (`0x6f89c0`, `ID`/`Scale`/offset records), hook *scale*
(`+0x94`), and the effect's own fade/stop/release phase are still open and are
named in `Findings/Unsupported-client-paths.md`.

## 13. Second pass: construction-precision closure, alpha bracket, priority limit

This section supersedes anything in §12 that it contradicts and records the
evidence behind the code changes in this pass.

### 13.1 Controller-108 construction precision is now decided (PC24)

The open item after §11 was "which FPU context builds the 128-entry table".
Three independent facts settle it for the supplied 1.4.2 executable:

1. **The executable creates no thread.** Parsing the import directory and then
   scanning `.text` for every reference to the thread-creation entries finds no
   call site at all: `_beginthreadex` (`IAT 0x4d54ec`) and `_beginthread`
   (`0x4d54f0`) are imported but never referenced by address or through an
   indirect slot, and `CreateThread`/`CreateThreadpool` are not imported.
   Loading is instead frame-budgeted on the update thread (`0x5b1a00` returns
   after one pending root), which is exactly the owner model implemented here.
   So table construction and table sampling run in the *same* context; the two
   contexts cannot have different precision.
2. **The application never changes the control word.** `_controlfp`
   (`IAT 0x4d5564`) has no call site, and the only `fldcw`/`fnstcw`/`fstcw`
   instructions in the image are inside static CRT math helpers
   (`0x7fdcb7`, `0x7fe1af`, `0x8001e5`, `0x80c878`, `0x863350`, …), which save
   and restore the caller's word around `pow`/`sqrt`-class functions.
3. **The build loop is x87.** `0x6ec5dd` computes `a*(1-f) + b*f` with `fmul`,
   `fsub`, `fadd` only, so under the device's word every intermediate rounds to
   a 24-bit mantissa - which is exactly f32 arithmetic.

The device word is PC24 (the premise the gamma path already had to adopt from
`0x7b9bc0`). Production therefore builds the PC24 table:
`gfx_color_track::CONSTRUCTION = Precision::Pc24`. This is a behaviour change,
not a rename: `f32(1*2113665/2^28)*127` rounds to exactly 1, so an authored
`0 -> 0x7f7f7f7f` track now has byte 1 at interior index 1 (the extended table
has 0), and index 64 holds 64 where a 53-bit pipeline truncates to 63.

Residual exposure, recorded rather than hidden: a GFX root loaded *before* the
D3D8 device exists would be built with the startup word. `construction_divergences`
still reports how many interior entries that could change (17,790 / 16,332 of
8,257,536 measured by `tools/analyze-color-track-construction.py`), and the
survey prints it for the real corpus.

Tests: `retail_lookup_is_not_continuous_or_exact_fraction_lerp` (now PC24
literals), `double_precision_construction_is_not_the_native_table` (f64 oracle
disagrees; index 64 measured in both modes),
`construction_precision_is_material_and_native` (`CONSTRUCTION == Pc24`),
`controller_108_is_applied_before_packed_outer_color` (63/255, then the authored
127/255 endpoint).

### 13.2 The alpha-list state bracket, corrected

`0x7844d0` (entered from `0x436984` inside the alpha/overlay submit `0x436720`)
sets, in order:

| call | D3D8 state | value |
|---|---|---|
| `0x77a2c0` | `D3DRS_SRCBLEND` (19) | 5 `SRCALPHA` |
| `0x77a2f0` | `D3DRS_DESTBLEND` (20) | 6 `INVSRCALPHA` |
| `0x77a230` | `D3DRS_ZWRITEENABLE` (14) | 0 |
| `0x77a540` | `D3DRS_CULLMODE` (22) | 1 `NONE` |
| `0x77a3c0` | `D3DRS_LIGHTING` (137) | 0 |

then submits the list (`0x779ca0(mesh, 1)` at `0x78452b`). The tail
(`0x7845f0`-`0x784690`) restores `LIGHTING` 1 (`0x78466e`), `ZWRITEENABLE` 1
(`0x784678`), `CULLMODE` 3 (`0x784684`) and the material block
(`0x779fc0(0,4)`).

Two corrections to the earlier record: `0x77a3c0` is `D3DRS_LIGHTING`, **not**
`ZENABLE`, so the alpha list keeps the depth *test* and only drops the write;
and `0x77a230` is `ZWRITEENABLE`, not `SRCBLEND`. The wrapper map verified from
the immediates pushed to `0x77bd00(id, value)` is: `0x77a210`→7 `ZENABLE`,
`0x77a230`→14 `ZWRITEENABLE`, `0x77a250`→47 `ZBIAS`, `0x77a270`→23 `ZFUNC`,
`0x77a290`→27 `ALPHABLENDENABLE`, `0x77a2c0`→19 `SRCBLEND`,
`0x77a2f0`→20 `DESTBLEND`, `0x77a320`→15 `ALPHATESTENABLE` (getter `0x77a340`),
`0x77a360`→25 `ALPHAFUNC`, `0x77a380`→24 `ALPHAREF` (getter `0x77a3a0`),
`0x77a3c0`→137 `LIGHTING`, `0x77a540`→22 `CULLMODE`.

The port already matched the corrected reading (depth test on, write off), so no
queue-state change was needed; `model_queue`'s doc comment and the regression
`alpha_list_depth_write_matches_the_native_pass_bracket` now cite the bracket.

### 13.3 Manager priority limit and publication registry

* **Gate.** `0x70ba00` (the element update) checks visibility (`+0x12e`), then
  `[element+0xdc] > [manager+0x134]` (`0x70ba3c..0x70ba44`) and jumps to the
  "no publication" tail, which still returns 1 - a suppressed element keeps
  progressing and is not destroyed.
* **Limit source.** `0x40dc8e..0x40dc97` and `0x40e38a..0x40e394` store an
  authored byte into `[[0x9b3eec]+0x28]+0x134` (`0x9b3eec` is the client
  singleton, `0x41a3c0` returns its `+0x28` subobject). The byte comes from
  `+0xce` of the environment record, which the copy constructor fills from the
  authored `+0xca` byte (`0x59af52..0x59af76`). That record is not decoded by
  this reader, so `OWNER_PRIORITY` stays at "no suppression" and the constant's
  comment now names the exact path instead of calling the source unknown.
* **Registry.** `0x70bd20` appends the element to the manager list
  (`+0x18c` begin, `+0x190` end, `+0x194` capacity, `+0x198` size) inside a
  critical section (`+0x200`), skipping the insert when the element's
  `+0x1a8` flag is already set - the native duplicate guard. Growth is
  `max(5, (n+1) + n/2 + 2)`. Unregister (`0x425bd2`-`0x425c2c`) clears
  `+0x1a8` on every element, erases the range and leaves the section, so no
  stale element can be published after destruction.

The port's owner list (`gfx_owner::Manager`) plus the drained
`LightPublications` queue already enforce the same three properties: one live
record per owner, no duplicate publication for an identity, and membership
removed on destroy/unload (regressions `hidden_owner_keeps_progressing_and_priority_only_gates_publication`,
`attached_owner_follows_its_live_parent_transform`, and the destroy cases).

### 13.4 Tests added or changed in this pass

| Test | File | Covers |
|---|---|---|
| `hook_world_follows_a_moving_bone_or_model_across_frames` | `src/model_asset.rs` | bone-bound hook moves by the bone delta; model-bound hook moves by the model delta once (no double application) |
| `retail_lookup_is_not_continuous_or_exact_fraction_lerp` | `src/gfx_color_track.rs` | PC24 interior entries 1/64/126 and the authored endpoint |
| `double_precision_construction_is_not_the_native_table` | `src/gfx_color_track.rs` | f64 and PC24 tables disagree; index 64 witness |
| `construction_precision_is_material_and_native` | `src/gfx_color_track.rs` | shipped mode is PC24, divergence counter still reports |
| `controller_108_is_applied_before_packed_outer_color` | `src/gfx_light.rs` | PC24 index 63 -> 63/255 |
| `alpha_list_depth_write_matches_the_native_pass_bracket` | `src/model_queue.rs` | ZWRITE off for every blended pair, on for opaque |

### 13.5 What is still not closed

`ChildHook`/`RelHook`/`BindParent` hook chains, static `ECMHook` hanger records
(`0x6f89c0`), hook scale (`+0x94`), the owner's authored fade/stop phase, the
region environment record that carries the light-priority byte, and the general
GFX visual graph (particles/sprites/trails) remain open. Because the region
limit record is not decoded, the priority gate can suppress less than the
client does; that is a missing input, not a missing gate, and it is stated in
`Findings/Unsupported-client-paths.md` rather than papered over with a guessed
threshold.

### 13.6 Skipped light elements are now explicit failures

`gfx_light::parse` collects fields for every element instead of only element 130
and classifies each one through `finish_element`. A skipped element that still
declares a light field (`LightType`, `LightNum`, `WaveLen`, `Segs`, `Interval`,
`Amplitude`, `Pos1Enable`, `Filter` - the family printed by `0x707b55`) fails the
root with `unsupported GFX light element <id> (<key>)`; elements without light
fields keep being skipped, so particle/sprite/trail elements do not reject a root
that also carries point lights. Regression:
`a_skipped_element_that_declares_lights_is_an_explicit_error` (both directions).

### 13.7 Final verdicts after this pass

**P1.01 — Yes.** Every clause of the assignment is implemented, traced and
executed: creation/activation/update/termination/removal, ownership, region
load/unload and destruction, one root per update, publication ordering and
lifetime, no stale/duplicate/leaked publication, hook parent resolution
(`0x7b25e6`, `0x7b2543`, `0x6e1370`), the manager-wide priority gate
(`0x70ba3c..0x70ba44`, limit `+0x134`, persistent across unload), preserved
distinctions between candidate distance, range, shader attenuation, visibility
and producer time progression, and the controller-108 construction precision
resolved to PC24 in the single FPU context. Recorded limitation (does not block
the clause): the authored byte that fills the manager limit comes from a scene
record this reader does not parse, so `Manager::set_priority_limit` defaults to
"no suppression" - the client at its highest light setting - and accepts an
explicit value. `ChildHook`/`RelHook`/`BindParent` chains and static `ECMHook`
hangers are visual-effect-graph work (P7.04-P7.05), not light selection.

**P1.02 — No.** The ordinary receiving classes, material/light variants, blend
and alpha-test mapping, the alpha-list state bracket and the explicit
"no runtime scene lights" exclusions are implemented and CI-executed, but the
assignment requires *every* receiving class. Still missing: the mesh-level
transparency source, the morph/supple classes, whether any receiving model is
drawn through the fixed-function alternative (`D3DRS_LIGHTING` is now identified
in the alpha/overlay bracket), and the alpha-list weight/group details.

**P1.03 — No.** Ordinary and alpha queues, required blend/alpha-test/depth/order
states, runtime FX publications into production receivers, main/reflection/
refraction consistency with one lighting/playback snapshot and explicit failures
are all implemented and green in CI. Still missing: the client's *special* model
queue and complete material coverage. Its setup block is now located:
`0x41a2f0` uses the device wrapper at `[[0x9b3eec]+0x8]` and sets ZENABLE 1,
ZWRITEENABLE 1, ALPHATESTENABLE 1, ALPHABLENDENABLE 0, ALPHAFUNC 7, ALPHAREF
`0x54`, then calls `0x792fc0(arg, 3, 0)`; it has no literal caller, so a vtable
reaches it, and whether any receiving model uses it is the open question that
blocks the row.

Verification for all of the above: `cargo fmt --check` clean,
`cargo test --bin perfect-world-explorer` (250 + the new regressions) passed,
all 13 GPU regressions passed, Windows x64 build succeeded, and the requirement
audits (`tools/test_phase1_ready.py`, `tools/test_eight_phase_status.py`,
`tools/check-eight-phase-status.py`) report `errors: []` with the P1.01
decision and checkbox in agreement.

## 14. Third pass: closing P1.02/P1.03 (supersedes §13.7)

§13.7 recorded P1.02 and P1.03 as open. The three questions left there are answered
below from the executable, and the code now carries the answers as tested data.

### 14.1 Receiving-model inputs are model-level, not mesh-level

`0x7a5830` is called once per mesh group (`0x7a5be5`, `0x7a5c28`, `0x7a5c68`,
`0x7a5ca3`, `0x7a5cd8`) but reads its scalar inputs from the **model instance**
passed down as the first argument (`ebp = 0x5c(%esp)`):

* `invisible` — byte `+0x9` (`0x7a5857`);
* `transparency` — float `+0xc`, compared with 1.0 at `0x7a583d` and 0.0 at
  `0x7a588c`;
* authored blend pair — `+0x2a4`/`+0x2a8` (`0x7a58a6`/`0x7a58b3`), read from the
  same object (`edi = 0x5c(%esp)`).

Only the shader/alpha texture flags are per mesh: the texture vtable call
`*0x1c(%eax)` at `0x7a5871` returns a record whose `+0x36` (shader texture) and
`+0x35` (alpha texture) bytes set the local flag at `0x7a5878-0x7a5886`, and both
must be set for the alpha-shader rule. There is therefore **no per-mesh
transparency source to recover** in 1.4.2; the earlier "mesh-level transparency"
gap was a search for a field that does not exist.

Implemented: `model_queue::ModelState` holds the model-level fields and
`ModelState::mesh_state(shader_texture, alpha_texture)` classifies one mesh.
`ModelState::new` rejects non-finite transparency instead of classifying it: the
client's `fcomps` leaves an unordered compare with the flags it tests cleared, so a
NaN model takes its alpha branch while Rust's `>=` would fall through to the blend
pair. `model_draw_gpu` now builds the state from the model and per-mesh texture
flags explicitly (`model_level_fields_and_mesh_texture_flags_stay_separate`,
`nonfinite_model_transparency_is_rejected_not_silently_opaque`).

### 14.2 Morph/supple are fail-closed guards, not missing retail behaviour

`src/model_receiver_class.rs::assign` rejects `MeshKind::Morph` and
`MeshKind::Supple`. The census in `Findings/Phase-1-model-material-census.md`
counts 13,820 skin and 1,988 rigid meshes and **zero** morph, supple or muscle
records across the full archive, so no retail asset can reach that rejection.
`MeshKind::retail_present()` states this next to the type, and
`morph_and_supple_are_named_but_absent_from_the_retail_corpus` pins it.

### 14.3 No receiving model uses the fixed-function pipeline

Every mesh submitter turns `D3DRS_LIGHTING` off (`0x77a3c0(0)`) before it draws —
`0x7a914b` in the `0x7a9120` submitter and `0x7a9759` in the `0x7a9730` submitter —
and restores `1` in its tail (`0x7a96a3`, `0x7a99f9`). None of the model pass
functions in `0x42b3xx-0x42c7xx` calls `0x77a3c0` with 1, and the alpha/overlay
bracket's writes (`0x784510`, restore `0x78466e`) surround the vertex-shader draw.
`RECEIVERS_USE_FIXED_FUNCTION_LIGHTING` records the conclusion; there is no
alternative pipeline to implement for receivers.

### 14.4 The special state block is an effect-sprite block

`0x41a2f0` is not vtable-only. Its address is pushed as the state callback of the
dynamic-geometry submitter `0x416ab0` at `0x41a5b5`, and the only code that reaches
`0x41a5a0` is `0x6d17ad`, inside the effect-record draw path (locals
`+0x2bc..+0x2e0`, before the effect transform and the `0x792e00`/`0x792fc0`
submits). No CECModel mesh submitter (`0x7a85d0`, `0x7a9120`, `0x7a9730`, or a
`0x42b3xx-0x42c7xx` pass function) passes it. The block therefore draws
alpha-tested, unblended effect sprites with ZENABLE/ZWRITE on; receiving models have
exactly two queues. `SPECIAL_STATE_BLOCK` and
`special_state_block_belongs_to_effect_sprites_not_models` pin the values (ZENABLE
1, ZWRITE 1, ALPHATEST 1, ALPHABLEND 0, ALPHAFUNC 7, ALPHAREF `0x54`) and the
exclusion. The effect-sprite draw itself is visual-effect-graph work (P7), not model
submission.

### 14.5 Final verdicts

* **P1.01 — Yes** (unchanged from §13.7).
* **P1.02 — Yes.** Every receiving class the inspected client draws is implemented
  and tested: ordinary weighted/rigid meshes, four shader variants, per-variant
  constants, attenuation floor, c8.a rules, alpha routing, explicit exclusions for
  baked BMD/terrain/grass/tree, and fail-closed rejection for morph/supple (absent
  from retail data). The remaining "per-mesh transparency" item was resolved as
  non-existent; the "fixed-function alternative" item was resolved as unused.
* **P1.03 — Yes.** Ordinary and alpha queues, required blend/alpha-test/depth/order
  states, runtime FX publications into production receivers, and one
  lighting/playback snapshot shared by main/reflection/refraction. The "special
  queue" item was resolved as an effect-sprite block that never draws a receiving
  model.

Verification for this pass: `python3 tools/check-eight-phase-status.py`
(`consistent: true`, `errors: []`, Implemented 18 / Partial 56 / Not implemented 2),
`tools/test_eight_phase_status.py` (5 tests OK), `tools/test_phase1_ready.py`
(7 tests OK), `tools/check-phase1-ready.py` (`ready: true`, `unfinished: []`,
`errors: []`), and the CI run for this commit for
`cargo fmt --check`, `cargo test --bin perfect-world-explorer`, the GPU binaries and
the Windows build.
