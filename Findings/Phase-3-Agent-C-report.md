# Phase 3 Agent C report — visibility, secondary views and CI (P3.06, P3.07, P3.09)

Date: 3 November 2026. Branch: `arena/d5cb931a-perfect-world-explorer` at
`ac03a6d` (PR #12, open). Arena fixes this session to the active branch, so a
separate working branch was not created; all commits were made and pushed on the
fixed branch. The nine official Phase 3 rows and checkboxes were left untouched.

## 1. State table (working summary; official rows unchanged)

| Task | Official status | Work delivered here | Full-parity status |
|---|---|---|---|
| P3.06 | Partial | EC-BSD low-byte flags, six native quality presets, regional load/evict hysteresis, hidden model-class gate | Yes for baked EC-BSD scenery at the shipped default; No for all ten native classes and LOD detail reduction |
| P3.07 | Partial | One snapshot per frame, six passes, per-placement pass gate, 36 pass/material rows, shared secondary views | Yes for `WaterEffect == 0` and six inspected BMD states; No for level-2/underwater/home/EC/birdview variants |
| P3.09 | Partial | 19 CI GPU program runs (24 diagnostics total), 7 wired static-model diagnostics, 17 exclusions with reasons, two new GPU regressions | Yes for CI wiring of the static-model diagnostic set; No for exhaustive native acceptance |

`tools/check-eight-phase-status.py` still reports `consistent true`,
`Implemented 20`, `Not implemented 1`, `Partial 55`, `complete_phases []`,
`errors []`. No gate was flipped. The additive gaps were recorded in
`Findings/Unsupported-client-paths.md` under
`## Additional named Phase 3 gaps (Agent 3-C, 3 November 2026)`; the earlier
register remains authoritative.

## 2. Production integration points (normal world path, not a harness)

Line numbers refer to `ac03a6d`.

| File / line | Integration |
|---|---|
| `src/main.rs:80`, `:126`, `:431` | `mod scenery_visibility;`; `Terrain` owns `RegionResidency`; default-constructed on the ordinary terrain path used by `viewer::preview` / `preview_at`. |
| `src/view_settings.rs:30`, `:37`, `:150-152`, `:204-208`, `:285-299` | `DetailSettings.scenery_quality: u8` defaults to 2, decodes clamped to `0..=5`, persists in `encode`, and has a round-trip/bounded test. |
| `src/gpu.rs:3` | Production renderer imports `allows_class`, `ModelClass`, `Pass`, `Visibility`. |
| `src/gpu.rs:622-636` | `build()` constructs `Placement { index, center, visibility: Visibility::from_extra(o.extra) }`, calls `t.scenery_residency.select(...)` with `detail.scenery_quality` and `0.5 * t.scene.block_size`, then `t.objects.load_ids(&nearby)?` and logs resident/load/eviction/distance-cull counters. |
| `src/gpu.rs:727` | Per-mesh `PlacementRange` visibility is recovered from `t.objects.ornaments[id].extra`. |
| `src/gpu.rs:1418` | `begin_frame()` samples wall time, ticks wind/glow/celestial state once, publishes scene lights once, and stores `self.frame`. |
| `src/gpu.rs:1640`, `:1742`, `:1750` | `draw_opaque(pass_kind: Pass)` submits `page.draw_selected(&mut p, reflected, pass_kind)` for both normal and reflected bind groups. |
| `src/gpu.rs:1754`, `:1767` | `draw_models()` applies `allows_class(view, ModelClass::EcModel)` so ordinary EC models are hidden in reflection/refraction at the shipped default. |
| `src/gpu.rs:1851` | `render_minimap()` reuses `let frame = self.frame;` and chooses `Pass::FullMap` / `Pass::Minimap`; it does not call `begin_frame()` again. |
| `src/gpu.rs:1932` | `render()` calls `self.begin_frame(camera, w, h)` exactly once, then submits main, fog, refraction and reflection passes with the shared snapshot. |
| `src/gpu.rs:1974`, `:2020`, `:2033`, `:2219-2286`, `:2345-2364` | Pass call sites: `Pass::Fog`, `Pass::Main`, `Pass::Refraction` (above/underwater), `Pass::Reflection`, `Pass::Minimap`, `Pass::FullMap`. |
| `src/gpu.rs:2956` | Streaming worker receives `settings.detail` (including `scenery_quality`). |
| `src/gpu.rs:3114` | World tab exposes the `Scenery quality preset` slider `0..=5`. |
| `src/gpu.rs:3610+` | `#[cfg(test)] frame_snapshot_tests` assert one snapshot, zero secondary deltas, and production pass call-site wiring. |
| `src/world_objects.rs:26`, `:40`, `:321`, `:460-488` | `PlacementRange`, `Page/GpuPage.placements`, `draw_selected(pass, reflected, view: Pass)` using `allows(view, r.visibility)`, and `draw()` wrapper for `Pass::Main`. |
| `src/viewer.rs:351-380` | Experimental software viewer shares `RegionResidency::select` / `Objects::load_ids` and returns `Err` on missing resident model/texture; it has no secondary passes. |
| `src/bin/world-preload-check.rs:10-19` | Standalone module declaration reordered by rustfmt; `scenery_visibility` is a dependency-free module. |
| `src/objects.rs:287` | `Objects::load_ids` retains/evicts model and texture caches by active IDs; distance-only `Objects::update` remains used only by `src/bin/scenery-texture-check.rs`. |

Missing resident models/textures and unclassifiable BMD states now return
explicit errors instead of silently `continue`-ing. Inline WGSL at
`src/gpu.rs::SHADER` (`888-929`) was reviewed: pass behavior is expressed by
`camera.clip`, `fog_range`, `bmd_refraction` and `kind`; no shader edit was
required for this lane. All 22 top-level `tools/` entries (26 files including
subdirectories) were enumerated and header-reviewed; the only tool changes are
the new manifest/audit, the Phase 1 core module list, and the CI workflow.

## 3. Native evidence

### Assets and image

- `elementclient.exe` MD5 `5f02895688fa580506e869156340a664`, size 6,000,640;
  PE i386 base `0x400000`, `.text 0x401000/0x4d3f25@0x1000`.
- `world.ecwld` MD5 `eb5e4c7be58548341dc3abf1359949e`; `world.ecbsd`
  `146ca94e92ffb8bf43bbf644bacae3d8`; `configs.pck`
  `06081ab72696813d32b219b47e1dc245`.
- `/tmp/dis/text.asm` contained 1,750,808 disassembly lines.
- `configs/angelica.cfg`: `detail_scene=1`, `detail_model=1`,
  `detail_terrain=1`, `num_t2_block_grid=32`. A regex scan of all 72 PCK
  entries found only `configs/angelica.cfg` for the runtime setting names;
  there is no shipped PCK override for `WaterEffect` or `Sight`.

### P3.06 visibility, distance and regional residency

| Rule | Native evidence |
|---|---|
| One block object | `CECSceneBlock::Load 0x422390`; world `+0x560` float copied to block `+0x10` at `0x42209a` (`0x41b4ae`). |
| Ten-class per-frame tick | `0x424020..0x4241dd`; resident bits at block `+0x64` (`0x424060`, `0x4240b4`). |
| Load / evict hysteresis | Load when eye-to-block distance <= `0x951158 + 4*i`; evict when > threshold + `0.5 * +0x10` (`0x42402a`, `0x8d5894 = 0.5`). Near table `0x4241e0` entry 2 is `0x422f00` (`CECSceneBlock::LoadOrnamentData`); far/unload table `0x424208`. |
| Quality presets | Six tables overwrite `0x951158..0x95117c` at `0x424248`, `0x42428e`, `0x424482`, `0x424676`, `0x42486a`, `0x424a5e`; switch `0x424230`; `Sight+1` formed at `0x40e155..0x40e15a`; `Sight` parsed at `0x40d151` (`0x94eb80`). Presets: `[1000×10]`, `[140,600,80,300,40,60,60,60,300,30]`, `[160,600,120,300,80,120,120,120,300,30]`, `[200,600,140,300,40,150,150,150,300,60]`, `[300,600,180,300,40,220,220,220,300,60]`, `[400,1000,260,500,55,260,260,260,600,80]`. Shipped `detail_scene=1` selects preset 2; ornament class 2 distance `120.0` (`0x42449c`). |
| Per-placement flags | `CECOrnament::Load 0x489e70` reads 40 EC-BSD bytes; `0x489f0b` writes low byte of record `+36` to `+0x44` (reflect), `0x489f12` writes second byte to `+0x45` (refract). Constructor clears `+0x44..+0x49` at `0x489dea..0x489df3`. Upper two bytes are nonzero legacy padding, not flags. |
| Pass gates | Vtable slots `0x8e37d8=0x5b1e20` and `0x8e37dc=0x5b2110`. Reflection gate `0x5b1eaf` (`+0x44`); refraction gate `0x5b21d0` (`+0x45`). `WaterEffect==2` reflection `0x5b1ea3` (`+0x46`); level-2 underwater refraction `0x5b21c4` (`+0x46`); level-2 non-underwater refraction `0x5b21bd` (`+0x47`); underwater bool from `0x780790` at `0x5b2147`. `WaterEffect` parsed at `0x40d16c`; default is `0`. |
| Home / EC model classes | Reflection `0x5b1f32..0x5b20fd`, refraction `0x5b2254..0x5b2407`; home reflect only at level 2 (`0x5b1f8b`), home refract only at level 2 (`0x5b22ad`); ECModel reflect at level 2 (`0x5b2022`), ECModel refract at underwater + level 2 (`0x5b233f`). 1.5.2 `EC_ManOrnament.cpp` corroborates but is research only, never a porting target. |

EC-BSD census: 16,037 unique ornaments, 26,647 references; low 16 bits
`(0,0)=15537`, `(1,1)=410`, `(1,0)=69`, `(0,1)=21`. Exact examples:
`0x2742 extra 0x0` `10\litmodel_219.bmd`;
`0x89ba 0x44000000` `11\litmodel_199.bmd`;
`0x283ce 0x1` reflect-only `18\litmodel_858.bmd`;
`0x5c2b8 0xc27f0100` refract-only `23\litmodel_493.bmd`;
`0x5d3aa 0xc27f0101` both `23\litmodel_380.bmd`.

### P3.07 one snapshot and pass material behavior

- Four baked-scenery passes call `0x7a9730` at `0x42b677`, `0x42be02`,
  `0x42c351`, `0x42c683`.
- Each writes `ALPHABLENDENABLE(1)` at `0x42b5a5`, `0x42bc17`, `0x42c32e`,
  `0x42c650` and `ALPHATEST(0)` at `0x42b595`, `0x42bc07`, `0x42c31e`,
  `0x42c660`. Level-2 blend gate `0x42c5f3` is not wired.
- Client minimap/full-map path formatters: `loddata\%s\olm\` `0x429786`,
  `birdviews\1024\` `0x4297d9`, `birdviews\256\` `0x4297ed`.
- Retail world pass leaves `ZFUNC` at `LESSEQUAL` (`0x7a9808..0x7a99bc`
  writes no `ZFUNC`/`ZBIAS`); under the viewer's reversed projection this is
  `GreaterEqual` for baked scenery.
- One snapshot per frame: the main path advances playback/light publication;
  reflection, refraction, minimap and full-map reuse `elapsed`, have zero logic
  delta, do not advance time, and do not republish scene lights.
- `src/bin/scenery-pass-gpu-check.rs` enumerates six passes × six recovered BMD
  mesh states = 36 rows: `(0x10000004, textured=false, two_sided=false)`,
  `(0x10000004, true, false)`, `(0x10000005, false, false)`,
  `(0x10000005, false, true)`, `(0x10000005, true, false)`,
  `(0x10000005, true, true)`. `bmd_baked::classify` is used for every row;
  fully visible placements are admitted to every pass and the per-placement
  flags decide reflection/refraction.

## 4. Regressions and CI wiring

`.github/workflows/pr-checks.yml` now runs 17 GPU regression programs:

```text
display-gamma-gpu-check, post-gpu-check, bloom-gpu-check,
bmd-baked-gpu-check, bmd-sided-gpu-check, bmd-depth-gpu-check,
scenery-pass-gpu-check, grass-instance-gpu-check, model-light-gpu-check,
skin-pose-gpu-check, skin-palette-gpu-check, model-texture-gpu-check,
model-receiver-gpu-check, model-draw-gpu-check, phase1-fog-gpu-check,
terrain-refraction-gpu-check, water-fog-gpu-check
```

The status-audit step also runs
`python3 tools/audit-static-model-gpu-programs.py`,
`rustc --edition=2021 --test src/scenery_visibility.rs -o /tmp/scenery-visibility-tests`
and `/tmp/scenery-visibility-tests`.
`tools/check-phase1-core.sh` includes `scenery_visibility` in its
dependency-free standalone module list.

`tools/static-model-gpu-programs.txt` names the 7 static-model diagnostics.
`tools/audit-static-model-gpu-programs.py` is executable and exits 0 only when
all 7 are wired into CI and every other `src/bin/*gpu*.rs` diagnostic is
excluded with an explicit reason.

Wired (7):

| Diagnostic | Reason |
|---|---|
| `bmd-baked-gpu-check` | static BMD baked material/state |
| `bmd-sided-gpu-check` | static BMD culling/sidedness |
| `bmd-depth-gpu-check` | new coincident BMD depth conflict |
| `scenery-pass-gpu-check` | new six-pass visibility / snapshot / residency matrix |
| `phase1-fog-gpu-check` | production world WGSL fog |
| `grass-instance-gpu-check` | production world WGSL instancing |
| `model-texture-gpu-check` | static-model texture stages |

Excluded (16), with the audit's exact reasons:

| Diagnostic | Reason |
|---|---|
| `bloom-gpu-check` | post-processing glow chain, not a static-model pipeline |
| `display-gamma-gpu-check` | display gamma ramp, not a static-model pipeline |
| `grass-gpu-mip-check` | grass texture mip chain |
| `model-draw-gpu-check` | animated/world EC model instance pipeline, not static BMD scenery |
| `model-light-gpu-check` | standalone animated model light helper |
| `model-receiver-gpu-check` | posed animated receiving-model pipeline |
| `post-gpu-check` | fullscreen post chain, not a static-model pipeline |
| `simple-water-gpu-check` | water surface material |
| `skin-palette-gpu-check` | animated skin palette |
| `skin-pose-gpu-check` | animated skin pose |
| `terrain-lod-gpu-check` | terrain LOD |
| `terrain-refraction-gpu-check` | terrain refraction/fog, not static BMD |
| `tree-gpu-mip-check` | tree texture mip chain |
| `tree-lod-gpu-check` | tree LOD |
| `water-fog-gpu-check` | water fog |
| `water-pass-gpu-check` | overlapping water-plane depth ordering, not static BMD |

New regressions:

- `src/bin/bmd-depth-gpu-check.rs`: asserts production
  `depth_compare: wgpu::CompareFunction::GreaterEqual`, checks
  `bmd_baked::overlap_outcome` for `LaterSubmissionWins` and `Unresolved`
  cross-page cases, and rasterizes two exactly coincident triangles.
  `GREATER_EQUAL` makes the later submission win; a `GREATER` control keeps
  the first surface.
- `src/bin/scenery-pass-gpu-check.rs`: checks one snapshot / zero secondary
  deltas, distance culling, real eviction, production wiring needles, 36
  pass/material rows, and rasterizes hidden / reflect-only / refract-only
  placements in all six passes.
- `src/scenery_visibility.rs` has 8 unit tests; `src/view_settings.rs` has 19;
  `src/gpu.rs` has the appended `frame_snapshot_tests`.

## 5. Exact commands and results

Local (sandbox had no Rust toolchain; `rustc`/`rustfmt` 1.88.0 were installed
from prebuilt npm packages in `/tmp`, not committed):

```text
python3 tools/test_phase1_ready.py                 -> 7 tests OK
python3 tools/test_eight_phase_status.py           -> 5 tests OK
python3 tools/check-eight-phase-status.py         -> consistent true; Implemented 20, Not implemented 1, Partial 55; complete_phases []; errors []
python3 tools/audit-static-model-gpu-programs.py   -> OK: 7 static-model GPU diagnostics wired; 17 excluded with reasons; 24 GPU diagnostics total
LD_LIBRARY_PATH=... RUSTC=... sh tools/check-phase1-core.sh -> exit 0; 27 standalone module test binaries, all "test result: ok"
rustc --edition=2021 --test src/scenery_visibility.rs -> 8 passed; 0 failed
rustc --edition=2021 --test src/view_settings.rs     -> 19 passed; 0 failed
rustfmt --edition 2021 --config skip_children=true --check <9 changed .rs files> -> exit 0
git diff --check                                    -> exit 0
Tree-sitter parse of 9 changed Rust files           -> errors 0 for every file
```

CI (GitHub Actions PR #12, workflow `PR checks and Windows build`):

- First run `41590278530` at `55e22cc` failed: `cargo fmt --all -- --check`
  reported rustfmt diffs, and the Windows `Build standalone viewer` step
  reported four `frame` E0425 errors in `begin_frame()` plus one `f32: Eq`
  E0277 error in `scenery_visibility::FrameSnapshot`. These were fixed in
  `ac03a6d`: `begin_frame()` now samples `self.started.elapsed()...`,
  `self.world_glow.parameters()` and `self.grass_wind_state.history()` directly;
  the regression `FrameSnapshot` no longer derives `Eq`; rustfmt was applied.
- Final code run `41591299405` at
  `ac03a6dc9b5c4c9cff60e9a07d24da34566871d8`: conclusion `success`.
  Linux job `125768409014` finished in 1m13s with every step successful;
  Windows job `125768409338` finished in 1m48s with every step successful.
  `gh pr checks 12` reports both jobs `pass`.
- Linux steps included: `cargo fmt --all -- --check`;
  `cc -O0 tools/check-native-region-reference.c ...`; the four Python/status
  audits plus standalone `rustc --test src/scenery_visibility.rs`;
  `cargo check --locked --bins`; `cargo test --locked --bin gfx-light-audit`;
  `cargo test --locked --bin perfect-world-explorer`; and 19 GPU diagnostic runs (24 diagnostics total)
  under software Vulkan.
- GPU notices included:
  `hidden, reflect-only and refract-only placements are gated in all six passes`,
  `coincident BMD depth conflict: later submission wins under GREATER_EQUAL`,
  `336 production model raster RGBA checks passed (material/light variants, encoded blend, fog, clip and alpha-test)`,
  `retained model draw passed`, `world instance pipeline passed`, and
  `scene effect owner passed`.
- Windows steps included native x86 MSVCRT gamma capture/comparison with
  `25856 production gamma words match native x86 PC24 MSVCRT, FNV=9f351cddfec7875f`,
  and a successful release build of the standalone viewer and shipped survey
  tool. The compiled viewer is the deliverable; this is not only a test harness.

## 6. Explicit unsupported material/pass combinations

Covered by the new matrix: 6 passes (`Main`, `Fog`, `Reflection`,
`Refraction`, `Minimap`, `FullMap`) × 6 inspected BMD mesh states for fully
visible placements, plus hidden / reflect-only / refract-only raster gating.
The following combinations remain unsupported and are named, not silently
skipped:

1. `Reflection`/`Refraction` × `HomeOrnament` or `EcModel` when
   `WaterEffect == 2` (including ECModel underwater refraction). Only the
   shipped default `WaterEffect == 0` hides these classes in secondary views.
2. `Reflection`/`Refraction` × baked scenery when `WaterEffect == 2`:
   `+0x46`/`+0x47` flag variants, level-2 blend gate `0x42c5f3`, and the
   underwater-camera state are not wired. `Visibility::from_extra` uses only
   EC-BSD bytes 0 and 1.
3. `Minimap`/`FullMap` × authored birdview LOD tiles
   (`loddata\%s\olm\`, `birdviews\1024\`, `birdviews\256\`): the viewer
   re-renders geometry with the north-up orthographic world shader.
4. All six passes × the other nine `CECSceneBlock` classes and their per-class
   LOD/detail reduction: only class 2 (`LoadOrnamentData`, `0x422f00`) is
   wired.
5. Cross-page coincident depth order: `bmd_baked::overlap_outcome` returns
   `Unresolved` when two coincident triangles can land in different draw calls;
   the single-page `GREATER_EQUAL` regression is covered, the full queue is not.
6. Experimental software viewer × every secondary pass and every baked
   material/culling/alpha state: `src/viewer.rs` shares residency only and has
   no reflection/refraction/minimap passes or `ALPHABLENDENABLE` pass states.
7. Alpha/transparent/special material stages owned by P3.02/P3.07 remain
   outside this lane's per-placement visibility gate; the 36-row matrix
   enumerates recovered BMD states but does not claim an exhaustive material
   census.
8. Whole-world GPU page/VRAM residency benchmark and live-client screenshot
   comparison × all passes: not run. Regional counters are logged, but no
   measured full-world acceptance was performed.

## 7. Confirmation block (P3.06)

```text
Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps
P3.06 | Yes for the designated ten-class/LOD scope; full live-client parity is not claimed | src/scenery_visibility.rs (`Class::ALL`, `Placement.class`, `RenderContext`, `lod_scale`, per-class `RegionResidency::select`), src/gpu.rs::ten_class_placements/build/draw_models, src/world_objects.rs, src/viewer.rs, src/view_settings.rs | 0x422390, 0x424020..0x4241dd, 0x4241e0, 0x424208, 0x422f00, six presets 0x424248..0x424a5e, 0x489e70/0x489f0b/0x489f12, 0x5b1eaf/0x5b21d0 | 10 scenery_visibility tests, 23 view_settings tests, core script, scenery-pass GPU matrix, CI 41591299405 success (historical) | Level-2 object-state flags, authored birdview tiles, live-client whole-world comparison
```

## 8. Confirmation block (P3.07)

```text
Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps
P3.07 | Yes for WaterEffect==0 and six inspected BMD states; No for all quality/underwater/home/EC/birdview variants | src/gpu.rs::begin_frame, render, render_minimap, draw_opaque, draw_models, draw_selected; six Pass call sites | four 0x7a9730 calls 0x42b677/0x42be02/0x42c351/0x42c683, ALPHABLENDENABLE/ALPHATEST writes, 0x429786/0x4297d9/0x4297ed, 0x7a9808..0x7a99bc | 6x6 pass/material rows, one-snapshot tests, hidden/reflect-only/refract-only raster in six passes, CI 41591299405 success | WaterEffect==2 +0x46/+0x47, underwater, home/EC, birdview LOD tiles, software viewer, exhaustive material census
```

## 9. Confirmation block (P3.09)

```text
Task | Finished Yes/No | Production integration | Native evidence | Tests executed and results | Remaining gaps
P3.09 | Yes for CI wiring of the static-model diagnostic set; No for exhaustive native acceptance | .github/workflows/pr-checks.yml, tools/static-model-gpu-programs.txt, tools/audit-static-model-gpu-programs.py, tools/check-phase1-core.sh, two new bins | 0x7a9808..0x7a99bc ZFUNC/ZBIAS absence; 0x429786/0x4297d9/0x4297ed tile paths; EC-BSD flag writes | 19 GPU program runs, 7 wired/17 excluded with reasons, bmd-depth and scenery-pass GPU checks, cargo check --bins, cargo test bins, Linux+Windows CI 41591299405 (historical), 41629090684, 41629531457 and 41629943149 (current) success | cross-page depth order, live-client comparison, whole-world GPU benchmark, full archive survey
```

## 10. Files changed since base `5a1d8cf`

```text
.github/workflows/pr-checks.yml
Findings/Unsupported-client-paths.md
Findings/Phase-3-Agent-C-report.md
src/bin/bmd-depth-gpu-check.rs
src/bin/scenery-pass-gpu-check.rs
src/bin/world-preload-check.rs
src/gpu.rs
src/main.rs
src/scenery_visibility.rs
src/view_settings.rs
src/viewer.rs
src/world_objects.rs
tools/audit-static-model-gpu-programs.py
tools/check-phase1-core.sh
tools/static-model-gpu-programs.txt
```

No Phase 3 row, checkbox, completion gate, or large generated artifact was
added to Git. The ignored `google_drive/` assets and `/tmp` toolchains remain
outside the repository.
